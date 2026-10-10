#!/usr/bin/env python3
"""Run Commit on this machine against a local Silicon Accounts stack.

    scripts/dev-accounts.sh [--build] [--fresh]   # dev_accounts.py up: start (idempotent)
    scripts/dev-accounts-stop.sh [--keep-webhook]  # dev_accounts.py down: stop what up started
    python3 scripts/dev_accounts.py restart        # restart the API and worker (same database, same secret)
    python3 scripts/dev_accounts.py status         # what runs, as JSON (never secrets)
    python3 scripts/dev_accounts.py env            # shell exports that point the commit CLI here

`up` does, each step only when needed:
  1. checks that Silicon Accounts answers on a loopback URL (it never talks to a deployed one);
  2. with --build, builds commit-api, commit-worker, commit-migrate and the commit CLI;
  3. creates the database (with --fresh: drops it first) and applies Commit's migrations;
  4. points Commit's webhook at Silicon Accounts to http://127.0.0.1:<base+1>/webhook/ with Commit's own app
     credentials (PUT /v1/apps/commit/webhook), remembering the URL it replaced so `down` can put it back;
  5. starts commit-api on 127.0.0.1:<base+1> and commit-worker, logs in <dir>/logs, pids in <dir>/pids;
  6. proves a test delivery (POST /v1/apps/commit/webhook/test) reaches the API and verifies; a stale signing
     secret is rotated (POST …/webhook/rotate-secret) and the API restarted with the new one.
The webhook's signing secret is kept in <dir>/webhook-secret (mode 0600, never in git) and reaches the service only
through its environment.

Configuration (environment):
  COMMIT_TEST_STACK        JSON file describing the stack: accounts_public_url, accounts_api_url,
                           apps.commit.app_secret, apps.commit.webhook_secret_seeded (the testkit's stack file shape)
  ACCOUNTS_URL             Silicon Accounts public URL, the token issuer (default: stack file, else http://localhost:9590)
  ACCOUNTS_API_URL         where Commit calls Silicon Accounts (default: stack file, else ACCOUNTS_URL)
  COMMIT_APP_SECRET        Commit's app secret at that stack (default: stack file)
  COMMIT_DEV_BASE          port block base (default 4140: web 4140, API 4141)
  COMMIT_DEV_DATABASE_URL  default postgres://postgres@127.0.0.1:5460/commit_e2e (its user owns the schema)
  COMMIT_DEV_DIR           state directory (default .mig): logs/, pids/, webhook-secret, webhook-previous-url
  COMMIT_DEV_BIN_DIR       where the binaries are (default $CARGO_TARGET_DIR/debug, else target/debug)
  COMMIT_DEV_PROOF_ISSUERS COMMIT_PROOF_ISSUERS for the API (default: the actions production allows the app
                           `interface`, the testkit's stand-in for the Silicon Interface)
  PSQL_BIN_DIR             directory with psql/createdb/dropdb (default: PATH, then Homebrew's postgresql@16)
"""
import argparse
import base64
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import time
from urllib.error import HTTPError, URLError
from urllib.parse import unquote, urlencode, urlparse
from urllib.request import ProxyHandler, Request, build_opener
import uuid

ROOT = Path(__file__).resolve().parents[1]
APP_ID = "commit"
WEBHOOK_PATH = "/webhook/"
LOOPBACK = {"localhost", "127.0.0.1", "::1"}
# The actions production allows the Silicon Interface (docs/migration/cutover.md, step 4).
INTERFACE_ACTIONS = (
    "commit.todos.list", "commit.todos.create", "commit.todos.read", "commit.todos.update", "commit.todos.delete",
    "commit.todo_notes.list", "commit.todo_notes.create", "commit.projects.list", "commit.projects.read",
    "commit.projects.update", "commit.project_tasks.list", "commit.project_tasks.create",
    "commit.project_tasks.update", "commit.project_tasks.claim",
)
PROCESSES = ("api", "worker")
OPENER = build_opener(ProxyHandler({}))


class Failure(Exception):
    """A precise, user-facing reason the command cannot continue."""


def http(method, url, body=None, basic=None, bearer=None, headers=None, timeout=15, form=None):
    """One request; returns (status, parsed JSON body or None, response headers). Network errors raise Failure."""
    options = {"Accept": "application/json", **(headers or {})}
    data = None
    if body is not None:
        data = json.dumps(body).encode()
        options["Content-Type"] = "application/json"
    if form is not None:
        data = urlencode(form).encode()
        options["Content-Type"] = "application/x-www-form-urlencoded"
    if basic:
        options["Authorization"] = "Basic " + base64.b64encode(":".join(basic).encode()).decode()
    if bearer:
        options["Authorization"] = "Bearer " + bearer
    return send(method, url, data, options, timeout)


def send(method, url, data, headers, timeout=15):
    """Send exact body bytes; returns (status, parsed JSON body or None, response headers)."""
    try:
        with OPENER.open(Request(url, data=data, method=method, headers=headers), timeout=timeout) as response:
            status, raw, got = response.status, response.read(), dict(response.headers)
    except HTTPError as error:
        with error:
            status, raw, got = error.code, error.read(), dict(error.headers)
    except (URLError, OSError) as error:
        raise Failure(f"{method} {url} failed: {getattr(error, 'reason', error)}") from None
    if not raw:
        return status, None, got
    try:
        return status, json.loads(raw), got
    except ValueError:
        return status, {"raw": raw[:300].decode(errors="replace")}, got


def loopback_url(name, value):
    parsed = urlparse(value)
    if parsed.scheme not in ("http", "https") or (parsed.hostname or "") not in LOOPBACK:
        raise Failure(f"{name} must be a Silicon Accounts stack on this machine (localhost or 127.0.0.1); got {value!r}. "
                      "This script never talks to a deployed Silicon Accounts.")
    return value.rstrip("/")


def absolute(path):
    path = Path(path)
    return path if path.is_absolute() else ROOT / path


def load_config():
    env = os.environ.get
    stack_file = env("COMMIT_TEST_STACK")
    stack = {}
    if stack_file:
        try:
            stack = json.loads(Path(stack_file).read_text())
        except (OSError, ValueError) as error:
            raise Failure(f"COMMIT_TEST_STACK={stack_file} is not a readable JSON stack file: {error}") from None
    app = (stack.get("apps") or {}).get(APP_ID) or {}
    accounts_url = loopback_url("ACCOUNTS_URL", env("ACCOUNTS_URL") or stack.get("accounts_public_url")
                                or "http://localhost:9590")
    accounts_api_url = loopback_url("ACCOUNTS_API_URL", env("ACCOUNTS_API_URL") or stack.get("accounts_api_url")
                                    or accounts_url)
    app_secret = env("COMMIT_APP_SECRET") or app.get("app_secret")
    if not app_secret:
        raise Failure("Commit's app secret at the local stack is unknown: set COMMIT_TEST_STACK to the stack file "
                      "(apps.commit.app_secret) or COMMIT_APP_SECRET.")
    base = int(env("COMMIT_DEV_BASE") or 4140)
    target = absolute(env("CARGO_TARGET_DIR") or "target")
    directory = absolute(env("COMMIT_DEV_DIR") or ".mig")
    database_url = env("COMMIT_DEV_DATABASE_URL") or "postgres://postgres@127.0.0.1:5460/commit_e2e"
    parsed = urlparse(database_url)
    if (parsed.hostname or "") not in LOOPBACK:
        raise Failure(f"COMMIT_DEV_DATABASE_URL must be a database on this machine; got host {parsed.hostname!r}.")
    issuers = env("COMMIT_DEV_PROOF_ISSUERS")
    if issuers is None:
        issuers = ",".join(f"{action}=interface" for action in INTERFACE_ACTIONS)
    return {
        "stack": stack,
        "accounts_url": accounts_url,
        "accounts_api_url": accounts_api_url,
        "app_secret": app_secret,
        "seeded_webhook_secret": app.get("webhook_secret_seeded"),
        "base": base,
        "api_port": base + 1,
        "api_url": f"http://127.0.0.1:{base + 1}",
        "webhook_url": f"http://127.0.0.1:{base + 1}{WEBHOOK_PATH}",
        "dir": directory,
        "pids": directory / "pids",
        "logs": directory / "logs",
        "bin": absolute(env("COMMIT_DEV_BIN_DIR") or target / "debug"),
        "target": target,
        "database_url": database_url,
        "database": unquote(parsed.path.lstrip("/")),
        "database_user": unquote(parsed.username or "postgres"),
        "proof_issuers": issuers,
    }


# --- PostgreSQL ------------------------------------------------------------------------------------------------

def pg_tool(name):
    configured = os.environ.get("PSQL_BIN_DIR")
    candidates = [Path(configured) / name] if configured else []
    found = shutil.which(name)
    if found:
        candidates.append(Path(found))
    candidates.append(Path("/opt/homebrew/opt/postgresql@16/bin") / name)
    for candidate in candidates:
        if candidate.exists():
            return str(candidate)
    raise Failure(f"{name} was not found; set PSQL_BIN_DIR to the directory holding the PostgreSQL client tools.")


def pg_args(cfg):
    parsed = urlparse(cfg["database_url"])
    return ["-h", parsed.hostname or "127.0.0.1", "-p", str(parsed.port or 5432), "-U", cfg["database_user"]]


def database_exists(cfg):
    result = subprocess.run(
        [pg_tool("psql"), *pg_args(cfg), "-d", "postgres", "-tAc",
         f"SELECT 1 FROM pg_database WHERE datname = '{cfg['database']}'"],
        capture_output=True, text=True, timeout=30, check=False)
    if result.returncode != 0:
        raise Failure(f"PostgreSQL at {pg_args(cfg)} did not answer: {result.stderr.strip()}")
    return result.stdout.strip() == "1"


def ensure_database(cfg, fresh):
    if not cfg["database"].startswith("commit_"):
        raise Failure(f"refusing to manage database {cfg['database']!r}: development databases are named commit_…")
    if fresh and database_exists(cfg):
        subprocess.run([pg_tool("dropdb"), *pg_args(cfg), "--force", cfg["database"]], check=True, timeout=60)
        print(f"dropped database {cfg['database']}")
    if not database_exists(cfg):
        subprocess.run([pg_tool("createdb"), *pg_args(cfg), cfg["database"]], check=True, timeout=60)
        print(f"created database {cfg['database']}")
    log = cfg["logs"] / "migrate.log"
    with log.open("ab") as out:
        result = subprocess.run([str(binary(cfg, "commit-migrate"))], env=service_env(cfg, None, migrator=True),
                                cwd=cfg["dir"], stdout=out, stderr=subprocess.STDOUT, timeout=600, check=False)
    if result.returncode != 0:
        raise Failure(f"commit-migrate failed (exit {result.returncode}); see {log}")
    print(f"migrations applied to {cfg['database']}")


# --- processes -------------------------------------------------------------------------------------------------

def binary(cfg, name):
    path = cfg["bin"] / name
    if not path.exists():
        raise Failure(f"{path} does not exist: build it (scripts/dev-accounts.sh --build) or set COMMIT_DEV_BIN_DIR.")
    return path


def service_env(cfg, webhook_secret, migrator=False):
    """A clean environment: nothing inherited but PATH/HOME/locale, so no live mail or telemetry key leaks in."""
    env = {key: os.environ[key] for key in ("PATH", "HOME", "TMPDIR", "LANG") if key in os.environ}
    env.update({
        "COMMIT_ENVIRONMENT": "development",
        "COMMIT_LOG": os.environ.get("COMMIT_DEV_LOG", "silicon_commit=info,tower_http=info"),
        "COMMIT_TELEMETRY": "off",
        "COMMIT_TELEMETRY_HOME": str(cfg["dir"] / "telemetry"),
        "COMMIT_POSTMARK_SERVER_TOKEN": "",
        "COMMIT_TELEMETRY_TABLE_KEY": "",
    })
    if migrator:
        env.update({"COMMIT_MIGRATOR_DATABASE_URL": cfg["database_url"], "COMMIT_SCHEMA_OWNER": cfg["database_user"]})
        return env
    env.update({
        "COMMIT_BIND_ADDR": f"127.0.0.1:{cfg['api_port']}",
        "COMMIT_PUBLIC_BASE_URL": f"{cfg['api_url']}/api/v1/",
        "COMMIT_CORS_ALLOWED_ORIGINS": f"http://localhost:{cfg['base']},http://127.0.0.1:{cfg['base']}",
        "COMMIT_DATABASE_URL": cfg["database_url"],
        "ACCOUNTS_URL": cfg["accounts_url"],
        "ACCOUNTS_API_URL": cfg["accounts_api_url"],
        "COMMIT_APP_ID": APP_ID,
        "COMMIT_APP_SECRET": cfg["app_secret"],
        "COMMIT_PROOF_ISSUERS": cfg["proof_issuers"],
    })
    if webhook_secret:
        env["COMMIT_ACCOUNTS_WEBHOOK_SECRET"] = webhook_secret
    return env


def pid_of(cfg, name):
    try:
        pid = int((cfg["pids"] / name).read_text().strip())
    except (OSError, ValueError):
        return None
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return None
    except PermissionError:
        return pid
    # A recycled pid belongs to another program: only claim our own binaries.
    command = subprocess.run(["ps", "-p", str(pid), "-o", "command="], capture_output=True, text=True,
                             check=False).stdout
    return pid if f"commit-{name}" in command else None


def start(cfg, name, webhook_secret):
    if pid_of(cfg, name):
        return False
    with (cfg["logs"] / f"{name}.log").open("ab") as log:
        process = subprocess.Popen([str(binary(cfg, f"commit-{name}"))], env=service_env(cfg, webhook_secret),
                                   cwd=cfg["dir"], stdout=log, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL,
                                   start_new_session=True)
    (cfg["pids"] / name).write_text(f"{process.pid}\n")
    return True


def stop(cfg, name):
    pid = pid_of(cfg, name)
    pid_file = cfg["pids"] / name
    if not pid:
        pid_file.unlink(missing_ok=True)
        return False
    os.kill(pid, signal.SIGTERM)
    for _ in range(100):
        if not pid_of(cfg, name):
            break
        time.sleep(0.1)
    else:
        os.kill(pid, signal.SIGKILL)
    pid_file.unlink(missing_ok=True)
    return True


def wait_ready(cfg, timeout=60):
    deadline = time.time() + timeout
    last = "no answer yet"
    while time.time() < deadline:
        if not pid_of(cfg, "api"):
            raise Failure(f"commit-api exited during start; see {cfg['logs'] / 'api.log'}")
        try:
            status, body, _ = http("GET", f"{cfg['api_url']}/readyz", timeout=3)
            if status == 200:
                return
            last = f"HTTP {status} {body}"
        except Failure as error:
            last = str(error)
        time.sleep(0.25)
    raise Failure(f"commit-api was not ready within {timeout} s (last: {last}); see {cfg['logs'] / 'api.log'}")


# --- the webhook at Silicon Accounts ---------------------------------------------------------------------------

def app_call(cfg, method, path, body=None):
    headers = {"Idempotency-Key": f"commit-dev-{uuid.uuid4()}"} if method != "GET" else {}
    return http(method, f"{cfg['accounts_api_url']}/v1/apps/{APP_ID}{path}", body=body,
                basic=(APP_ID, cfg["app_secret"]), headers=headers)


def expect(status, body, wanted, what):
    if status not in wanted:
        raise Failure(f"{what}: Silicon Accounts answered HTTP {status}: {json.dumps(body)[:400]}")
    return body


def save_secret(cfg, secret):
    path = cfg["dir"] / "webhook-secret"
    path.touch(mode=0o600, exist_ok=True)
    path.chmod(0o600)
    path.write_text(secret + "\n")


def known_secret(cfg):
    try:
        secret = (cfg["dir"] / "webhook-secret").read_text().strip()
    except OSError:
        secret = ""
    return secret or cfg["seeded_webhook_secret"] or None


def point_webhook(cfg):
    """Points Commit's webhook here; returns the signing secret to give the API (or None when it must rotate)."""
    current = expect(*app_call(cfg, "GET", "/webhook")[:2], (200,), "reading Commit's webhook")
    previous = cfg["dir"] / "webhook-previous-url"
    if current.get("url") != cfg["webhook_url"]:
        if not previous.exists():
            previous.write_text(json.dumps({"url": current.get("url")}) + "\n")
        answer = expect(*app_call(cfg, "PUT", "/webhook", {"url": cfg["webhook_url"]})[:2], (200,),
                        "setting Commit's webhook")
        print(f"Commit's webhook now posts to {cfg['webhook_url']} (was {current.get('url')})")
        if answer.get("secret"):
            save_secret(cfg, answer["secret"])
            return answer["secret"]
    return known_secret(cfg)


def rotate_secret(cfg):
    answer = expect(*app_call(cfg, "POST", "/webhook/rotate-secret")[:2], (200,), "rotating the webhook secret")
    save_secret(cfg, answer["secret"])
    print("rotated Commit's webhook signing secret (the one this script knew was not the stack's)")
    return answer["secret"]


def prove_delivery(cfg, timeout=45):
    """Queues a test ping and waits for its delivery; returns 'delivered', 'refused' (401) or a reason."""
    queued = expect(*app_call(cfg, "POST", "/webhook/test")[:2], (200, 202), "queueing a test delivery")
    event_id = queued.get("event_id")
    deadline = time.time() + timeout
    while time.time() < deadline:
        page = expect(*app_call(cfg, "GET", "/webhook/deliveries?limit=50")[:2], (200,), "reading deliveries")
        for item in page.get("items", []):
            if item.get("event_id") != event_id:
                continue
            if item.get("status") == "delivered":
                return "delivered"
            if item.get("last_status") in (401, 503):
                return "refused"
        time.sleep(0.5)
    return f"the test delivery {event_id} did not arrive within {timeout} s"


# --- commands --------------------------------------------------------------------------------------------------

def build():
    env = dict(os.environ)
    env.setdefault("CARGO_TARGET_DIR", str(ROOT / "target"))
    for args in (["-p", "silicon-commit", "--bins"], ["-p", "silicon-commit-cli", "--bin", "commit"]):
        subprocess.run(["cargo", "build", "--locked", *args], cwd=ROOT, env=env, check=True)


def ensure_stack(cfg):
    status, body, _ = http("GET", f"{cfg['accounts_api_url']}/.well-known/jwks.json", timeout=5)
    if status != 200 or not (body or {}).get("keys"):
        raise Failure(f"Silicon Accounts at {cfg['accounts_api_url']} did not serve its JWKS (HTTP {status}).")


def up(cfg, args):
    for directory in (cfg["dir"], cfg["pids"], cfg["logs"], cfg["dir"] / "telemetry"):
        directory.mkdir(parents=True, exist_ok=True)
    ensure_stack(cfg)
    if args.build:
        build()
    running = all(pid_of(cfg, name) for name in PROCESSES)
    if args.fresh or args.build:
        # A new database or new binaries: the running processes must not keep the old ones.
        for name in PROCESSES:
            stop(cfg, name)
        running = False
    if not running:
        ensure_database(cfg, args.fresh)
    secret = point_webhook(cfg) or rotate_secret(cfg)
    started = [name for name in PROCESSES if start(cfg, name, secret)]
    wait_ready(cfg)
    outcome = prove_delivery(cfg)
    if outcome == "refused":
        secret = rotate_secret(cfg)
        restart(cfg, secret)
        outcome = prove_delivery(cfg)
    if outcome != "delivered":
        raise Failure(f"Silicon Accounts could not deliver to Commit's webhook: {outcome}; see {cfg['logs'] / 'api.log'}")
    print(json.dumps({**status_of(cfg), "started": started, "webhook_test": outcome}, indent=1))


def restart(cfg, secret=None):
    secret = secret or known_secret(cfg)
    for name in PROCESSES:
        stop(cfg, name)
    for name in PROCESSES:
        start(cfg, name, secret)
    wait_ready(cfg)


def down(cfg, args):
    stopped = [name for name in PROCESSES if stop(cfg, name)]
    restored = None
    previous = cfg["dir"] / "webhook-previous-url"
    if previous.exists() and not args.keep_webhook:
        url = json.loads(previous.read_text()).get("url")
        if url:
            expect(*app_call(cfg, "PUT", "/webhook", {"url": url})[:2], (200,), "restoring Commit's webhook")
        else:
            expect(*app_call(cfg, "DELETE", "/webhook")[:2], (204,), "removing Commit's webhook")
        previous.unlink()
        restored = url
    print(json.dumps({"stopped": stopped, "webhook_restored_to": restored}, indent=1))


def status_of(cfg):
    return {
        "api": {"pid": pid_of(cfg, "api"), "url": cfg["api_url"], "log": str(cfg["logs"] / "api.log")},
        "worker": {"pid": pid_of(cfg, "worker"), "log": str(cfg["logs"] / "worker.log")},
        "database": cfg["database"],
        "accounts_url": cfg["accounts_url"],
        "accounts_api_url": cfg["accounts_api_url"],
        "webhook_url": cfg["webhook_url"],
        "proof_issuers": cfg["proof_issuers"],
    }


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    up_parser = commands.add_parser("up", help="start (idempotent)")
    up_parser.add_argument("--build", action="store_true", help="cargo build the binaries first")
    up_parser.add_argument("--fresh", action="store_true", help="drop and recreate the database")
    down_parser = commands.add_parser("down", help="stop what up started and restore the webhook URL")
    down_parser.add_argument("--keep-webhook", action="store_true", help="leave Commit's webhook pointing here")
    commands.add_parser("restart", help="restart the API and worker")
    commands.add_parser("status", help="print what runs as JSON")
    commands.add_parser("env", help="print shell exports for the commit CLI")
    args = parser.parse_args(argv)
    try:
        cfg = load_config()
        if args.command == "up":
            up(cfg, args)
        elif args.command == "down":
            down(cfg, args)
        elif args.command == "restart":
            restart(cfg)
            print(json.dumps(status_of(cfg), indent=1))
        elif args.command == "status":
            print(json.dumps(status_of(cfg), indent=1))
        else:
            print(f"export COMMIT_API_URL={cfg['api_url']}\nexport ACCOUNTS_URL={cfg['accounts_url']}")
    except Failure as error:
        print(f"dev-accounts: {error}", file=sys.stderr)
        return 1
    except subprocess.CalledProcessError as error:
        print(f"dev-accounts: {' '.join(map(str, error.cmd))} failed with exit {error.returncode}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
