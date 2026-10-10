#!/usr/bin/env python3
"""End-to-end scenarios against a local Silicon Accounts stack, with real tokens and the real binaries.

    COMMIT_TEST_STACK=/path/to/test-stack.json SILICON_ACCOUNTS_DIR=/path/to/silicon-accounts \\
      scripts/e2e-accounts.sh [--only 1,2,…] [--keep-running]

Starts the development stack (scripts/dev_accounts.py up) unless the API already answers, then runs:
  1. a Carbon signs in on the hosted pages and drives the API: todos (create, list, read, update, notes, delete),
     a project, and the refusals for a missing or foreign token;
  2. a Silicon signs the CLI in with a short-lived token in a fresh home, works with todos and projects, signs out;
  3. a Carbon signs the CLI in with the device flow, uses it, and the CLI refreshes an expired token;
  4. the custodian rule, the custodian's other Silicons, an unrelated Carbon refused until a project is shared with
     it by c: id and again after unsharing, and a Silicon that takes work only from accounts it allowed;
  5. webhooks: a custodian changes a Silicon's id, a profile change, a replayed event id, forged deliveries, a
     custodian transfer, a Silicon removing Commit (its earlier token and CLI session are refused), an STK rotation,
     a sign-out checked online where access widens, and a deleted Silicon whose project passes on;
  6. User verification proofs from `interface` on the scopes it is allowed; revoked, foreign, App verification,
     missing-scope and not-allowed proofs are refused;
  7. the three Silicon Apps discovery commands from a freshly packed archive, in an empty home;
  8. restart safety: access tokens, CLI sessions, refused sign-ins and webhook dedupe survive a restart.
Every identity is new per run (`commit-e2e-*-<run>`). Results go to <dev dir>/e2e/run-<run>/ (result.json and
transcript.jsonl, tokens redacted); sign-ins made here are ended at the end, and what this run started is stopped
unless --keep-running.

Environment: everything scripts/dev_accounts.py reads, plus
  SILICON_ACCOUNTS_DIR   a silicon-accounts checkout with testkit dependencies installed (for tests/e2e/mint.mts)
  SILICON_ACCOUNTS_CLI   the silicon-accounts CLI built from it (default $SILICON_ACCOUNTS_DIR/target/debug/silicon-accounts)
  COMMIT_E2E_TSX         the TypeScript runner (default $SILICON_ACCOUNTS_DIR/testkit/node_modules/.bin/tsx)
  SILICON_APPS           silicon-apps 0.2 for scenario 7's local validate/pack (default: PATH)
The stack file must also hold apps.interface.app_secret (scenario 6 issues proofs as that app).
"""
import argparse
import hashlib
import hmac
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tarfile
import tempfile
import threading
import time
import tomllib
import uuid

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))
import dev_accounts  # noqa: E402  (shared configuration, HTTP helper and process control)

http = dev_accounts.http
SECRET_KEYS = {"access_token", "refresh_token", "stk", "slt", "proof_token", "proof_refresh_token", "secret",
               "subject_token", "id_token", "token", "device_code"}
SECRET_PATTERN = re.compile(r"(eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+"
                            r"|(?:sar|slt|sap|sapr|whsec|sa_app)_[A-Za-z0-9_-]+|stk-[A-Za-z0-9]{8,})")


def redact(value):
    if isinstance(value, dict):
        return {k: ("<redacted>" if k in SECRET_KEYS and v else redact(v)) for k, v in value.items()}
    if isinstance(value, list):
        return [redact(v) for v in value]
    if isinstance(value, str):
        return SECRET_PATTERN.sub("<redacted>", value)
    return value


class Check(AssertionError):
    """A scenario expectation that did not hold."""


class Run:
    def __init__(self, args):
        self.cfg = dev_accounts.load_config()
        self.args = args
        self.n = str(int(time.time()) % 1_000_000)
        self.dir = self.cfg["dir"] / "e2e" / f"run-{self.n}"
        self.dir.mkdir(parents=True, exist_ok=True)
        self.transcript = (self.dir / "transcript.jsonl").open("a")
        self.results = []
        self.current = None
        self.started_stack = False
        self.ids = {}
        self.state = {}
        self.revoke_later = []  # (app_id, app secret, refresh token) of API sign-ins made here
        env = os.environ.get
        self.bin = self.cfg["bin"] / "commit"
        sa_dir = env("SILICON_ACCOUNTS_DIR")
        if not sa_dir:
            raise SystemExit("e2e-accounts: set SILICON_ACCOUNTS_DIR to a silicon-accounts checkout (its testkit "
                             "mints identities).")
        self.sa_dir = Path(sa_dir)
        self.tsx = env("COMMIT_E2E_TSX") or str(self.sa_dir / "testkit/node_modules/.bin/tsx")
        self.sa_cli = env("SILICON_ACCOUNTS_CLI") or str(self.sa_dir / "target/debug/silicon-accounts")
        for path, what in ((self.bin, "COMMIT_DEV_BIN_DIR"), (Path(self.tsx), "COMMIT_E2E_TSX"),
                           (Path(self.sa_cli), "SILICON_ACCOUNTS_CLI")):
            if not path.exists():
                raise SystemExit(f"e2e-accounts: {path} does not exist (set {what}).")
        apps = self.cfg["stack"].get("apps") or {}
        self.interface_secret = (apps.get("interface") or {}).get("app_secret")
        self.accounts = self.cfg["accounts_api_url"]
        self.api_base = f"{self.cfg['api_url']}/api/v1"
        self.redirect = f"http://localhost:{self.cfg['base']}/auth/callback"

    # --- recording ------------------------------------------------------------------------------------------

    def log(self, kind, **fields):
        entry = {"t": round(time.time(), 3), "scenario": self.current, "kind": kind, **redact(fields)}
        self.transcript.write(json.dumps(entry) + "\n")
        self.transcript.flush()

    def say(self, text):
        print(f"  {text}", flush=True)
        self.log("note", text=text)

    def check(self, label, condition, detail=""):
        self.log("check", label=label, ok=bool(condition), detail=detail)
        if not condition:
            raise Check(f"{label}: {redact(detail) if detail else 'expectation failed'}")
        print(f"  ok  {label}", flush=True)

    def expect_status(self, label, response, *wanted):
        status, body, _ = response
        self.check(f"{label} -> {'/'.join(map(str, wanted))}", status in wanted, f"HTTP {status}: {json.dumps(body)[:600]}")
        return body

    def expect_error(self, label, response, status, *codes):
        got, body, _ = response
        code = ((body or {}).get("error") or {}).get("code") if isinstance(body, dict) else None
        self.check(f"{label} -> {status} {'|'.join(codes)}", got == status and (not codes or code in codes),
                   f"HTTP {got}: {json.dumps(body)[:600]}")
        return body

    # --- calls ----------------------------------------------------------------------------------------------

    def api(self, method, path, token=None, proof=None, body=None, headers=None, key=None):
        options = dict(headers or {})
        if token:
            options["Authorization"] = f"Bearer {token}"
        if proof:
            options["Authorization"] = f"Proof {proof}"
        if method in ("POST", "PATCH", "PUT", "DELETE"):
            options.setdefault("Idempotency-Key", key or f"commit-e2e-{uuid.uuid4()}")
        response = http(method, f"{self.api_base}{path}", body=body, headers=options)
        self.log("api", method=method, path=path, request=body, status=response[0], response=response[1])
        return response

    def accounts_call(self, method, path, bearer=None, basic=None, body=None, key=True):
        headers = {"Idempotency-Key": f"commit-e2e-{uuid.uuid4()}"} if key and method != "GET" else {}
        response = http(method, f"{self.accounts}{path}", body=body, bearer=bearer, basic=basic, headers=headers)
        self.log("accounts", method=method, path=path, request=body, status=response[0], response=response[1])
        return response

    def mint(self, *args):
        """Runs tests/e2e/mint.mts; waits out the stack's per-network code limit once if it bites."""
        env = {**os.environ, "SILICON_ACCOUNTS_DIR": str(self.sa_dir),
               "COMMIT_TEST_STACK": os.environ.get("COMMIT_TEST_STACK", ""),
               "ACCOUNTS_API_URL": self.accounts}
        for attempt in (1, 2):
            result = subprocess.run([self.tsx, str(ROOT / "tests/e2e/mint.mts"), *args], capture_output=True,
                                    text=True, env=env, cwd=self.dir, timeout=180, check=False)
            if result.returncode == 0:
                value = json.loads(result.stdout.strip().splitlines()[-1])
                self.log("mint", args=[a if not a.startswith(("eyJ", "stk", "slt_")) else "<redacted>" for a in args],
                         result=value)
                return value
            if attempt == 1 and re.search(r"too many|rate.?limit|429", result.stderr, re.I):
                self.say("the stack's sign-in code limit was reached; waiting 45 s for it to reset")
                time.sleep(45)
                continue
            break
        raise Check(f"mint {args[0]} failed: {redact(result.stderr.strip())[-1200:]}")

    def cli(self, home, *args, stdin=None, timeout=90, extra_env=None):
        env = {"PATH": "/usr/bin:/bin", "HOME": str(home), "SILICON_HOME": str(home),
               "COMMIT_API_URL": self.cfg["api_url"], "ACCOUNTS_URL": self.cfg["accounts_url"], **(extra_env or {})}
        result = subprocess.run([str(self.bin), *args], input=stdin, capture_output=True, text=True, env=env,
                                cwd=home, timeout=timeout, check=False)
        self.log("cli", args=list(args), exit=result.returncode, stdout=result.stdout[-4000:],
                 stderr=result.stderr[-4000:])
        return result

    def cli_json(self, label, home, *args, stdin=None):
        result = self.cli(home, *args, stdin=stdin)
        self.check(f"commit {' '.join(args)} exits 0 ({label})", result.returncode == 0,
                   f"exit {result.returncode}: {result.stderr.strip()[-800:]}")
        try:
            return json.loads(result.stdout)
        except ValueError:
            raise Check(f"commit {' '.join(args)} printed no JSON: {result.stdout[-400:]}") from None

    def silicon_accounts(self, home, *args, stdin=None):
        """The local stack's silicon-accounts CLI in its own home (never the production one on PATH)."""
        env = {"PATH": "/usr/bin:/bin", "HOME": str(home), "ACCOUNTS_HOME": str(home)}
        result = subprocess.run([self.sa_cli, "--url", self.cfg["accounts_url"], "--home", str(home), *args],
                                input=stdin, capture_output=True, text=True, env=env, timeout=60, check=False)
        self.log("silicon-accounts", args=list(args), exit=result.returncode, stdout=result.stdout[-800:],
                 stderr=result.stderr[-800:])
        return result

    def home(self, name):
        path = Path(tempfile.mkdtemp(prefix=f"{name}-", dir=self.dir))
        path.chmod(0o700)
        return path

    # --- identities (created once per run, reused: the stack limits email codes per address) ----------------

    def carbon(self, key):
        """A Carbon with a first-party session (for the account site's actions): uuid, id, email, access_token."""
        if key not in self.ids:
            email = f"commit-e2e-{key}-{self.n}@example.test"
            self.ids[key] = {"email": email, **self.mint("carbon", "--email", email)}
            self.say(f"{key} is {self.ids[key]['id']} ({self.ids[key]['uuid']})")
        return self.ids[key]

    def carbon_app(self, key, fresh=False):
        """The Carbon's tokens for Commit, from the hosted sign-in pages (code + PKCE, exchanged with the secret)."""
        carbon = self.carbon(key)
        if fresh or "app" not in carbon:
            tokens = self.mint("app-signin", "--app", "commit", "--email", carbon["email"], "--redirect",
                               self.redirect, "--scope", "email", "--exchange")
            carbon["app"] = tokens
            self.revoke_later.append(("commit", self.cfg["app_secret"], tokens["refresh_token"]))
        return carbon["app"]["access_token"]

    def silicon(self, key, custodian="c1"):
        if key not in self.ids:
            owner = self.carbon(custodian)
            self.ids[key] = self.mint("silicon", "--token", owner["access_token"], "--handle",
                                      f"commit-e2e-{key}-{self.n}")
            self.say(f"{key} is {self.ids[key]['id']} ({self.ids[key]['uuid']}), custodian {owner['id']}")
        return self.ids[key]

    def slt(self, key):
        silicon = self.silicon(key)
        return self.mint("slt", "--silicon", silicon["id"], "--stk", silicon["stk"], "--app", "commit")["slt"]

    def exchange_slt(self, slt):
        """What the commit CLI does: a public-client exchange with client_id=commit and no secret."""
        status, body, _ = http("POST", f"{self.accounts}/v1/oauth/token",
                               form={"grant_type": "urn:silicon:params:oauth:grant-type:slt", "slt": slt,
                                     "client_id": "commit"})
        self.log("accounts", method="POST", path="/v1/oauth/token (slt, public client)", status=status, response=body)
        self.check("short-lived token exchanged as a public client", status == 200, f"HTTP {status}: {body}")
        self.revoke_later.append(("commit", None, body["refresh_token"]))
        return body

    def silicon_app(self, key, fresh=False):
        silicon = self.silicon(key)
        if fresh or "app" not in silicon:
            silicon["app"] = self.exchange_slt(self.slt(key))
        return silicon["app"]["access_token"]

    # --- webhooks -------------------------------------------------------------------------------------------

    def webhook_secret(self):
        secret = dev_accounts.known_secret(self.cfg)
        if not secret:
            raise Check("the webhook signing secret is unknown (start the stack with scripts/dev-accounts.sh)")
        return secret

    def deliveries(self, limit=100):
        status, body, _ = http("GET", f"{self.accounts}/v1/apps/commit/webhook/deliveries?limit={limit}",
                               basic=("commit", self.cfg["app_secret"]))
        if status != 200:
            raise Check(f"reading Commit's webhook deliveries: HTTP {status} {body}")
        return body.get("items", [])

    def wait_delivery(self, event_type, account_uuid, since, timeout=40):
        """Waits until Silicon Accounts delivered an event of this type about this account to Commit."""
        deadline = time.time() + timeout
        seen = None
        while time.time() < deadline:
            for item in self.deliveries():
                if item.get("type") == event_type and item.get("account_uuid") == account_uuid \
                        and item.get("created_at", "") >= since:
                    seen = item
                    if item.get("status") == "delivered":
                        self.log("delivery", delivery=item)
                        return item
            time.sleep(0.5)
        raise Check(f"no delivered {event_type} for {account_uuid} within {timeout} s (last seen: {seen})")

    def delivery_payload(self, delivery_id):
        status, body, _ = http("GET", f"{self.accounts}/v1/apps/commit/webhook/deliveries/{delivery_id}",
                               basic=("commit", self.cfg["app_secret"]))
        if status != 200:
            raise Check(f"reading delivery {delivery_id}: HTTP {status} {body}")
        return body

    def post_webhook(self, raw, secret=None, timestamp=None, signature=None):
        timestamp = str(timestamp or int(time.time()))
        if signature is None:
            mac = hmac.new((secret or self.webhook_secret()).encode(), f"{timestamp}.".encode() + raw, hashlib.sha256)
            signature = f"v1={mac.hexdigest()}"
        headers = {"Content-Type": "application/json", "X-Accounts-Timestamp": timestamp,
                   "X-Accounts-Signature": signature}
        response = dev_accounts.send("POST", f"{self.cfg['api_url']}/webhook/", raw, headers)
        self.log("webhook", body=raw.decode(errors="replace")[:800], status=response[0], response=response[1])
        return response

    @staticmethod
    def now_iso():
        return time.strftime("%Y-%m-%dT%H:%M:%S", time.gmtime(time.time() - 2)) + ".000Z"

    def poll(self, label, probe, timeout=30):
        """Repeats probe() until it returns a true value; returns it."""
        deadline = time.time() + timeout
        last = None
        while time.time() < deadline:
            last = probe()
            if last:
                return last
            time.sleep(0.5)
        raise Check(f"{label}: not true within {timeout} s")

    # --- scenario 1 -----------------------------------------------------------------------------------------

    def scenario_1(self):
        """A Carbon on the API."""
        c1 = self.carbon("c1")
        token = self.carbon_app("c1")
        account = self.ids["c1"]["app"].get("account") or {}
        self.check("the hosted sign-in returned the Carbon's account", account.get("uuid") == c1["uuid"]
                   and account.get("kind") == "carbon", account)
        me = self.expect_status("GET /me as the Carbon", self.api("GET", "/me", token=token), 200)
        self.check("/me names the Carbon by uuid and id", me.get("uuid") == c1["uuid"] and me.get("id") == c1["id"], me)
        self.check("/me shows the email the Carbon shared with Commit", me.get("email") == c1["email"], me)

        key = f"commit-e2e-todo-{self.n}"
        body = {"title": "E2E: plan the release", "assigned_to": c1["id"], "description": "First draft"}
        todo = self.expect_status("create a todo", self.api("POST", "/todos", token=token, body=body, key=key), 201)
        self.check("the todo belongs to the Carbon", todo["assigned_by"]["uuid"] == c1["uuid"]
                   and todo["assigned_to"]["uuid"] == c1["uuid"], todo)
        again = self.expect_status("the same create retried with its idempotency key",
                                   self.api("POST", "/todos", token=token, body=body, key=key), 201, 200)
        self.check("the retry returns the same todo", again["id"] == todo["id"], again)
        listed = self.expect_status("list todos", self.api("GET", "/todos?view=all", token=token), 200)
        self.check("the list holds the todo once", [t["id"] for t in listed["items"]].count(todo["id"]) == 1, listed)
        read = self.expect_status("read the todo", self.api("GET", f"/todos/{todo['id']}", token=token), 200)
        self.check("the todo reads back", read["title"] == body["title"], read)
        updated = self.expect_status("update the todo", self.api(
            "PATCH", f"/todos/{todo['id']}", token=token,
            body={"status": "in_progress", "title": "E2E: plan the release (v2)"}), 200)
        self.check("the update applied", updated["status"] == "in_progress"
                   and updated["title"].endswith("(v2)"), updated)
        self.expect_status("add a note", self.api("POST", f"/todos/{todo['id']}/notes", token=token,
                                                   body={"body": "Started on the outline."}), 201)
        notes = self.expect_status("list notes", self.api("GET", f"/todos/{todo['id']}/notes", token=token), 200)
        self.check("the note is there", len(notes["items"]) == 1 and notes["items"][0]["author"]["uuid"] == c1["uuid"],
                   notes)
        self.expect_status("delete the todo", self.api("DELETE", f"/todos/{todo['id']}", token=token), 200, 204)
        self.expect_error("read the deleted todo", self.api("GET", f"/todos/{todo['id']}", token=token), 404)

        project = self.expect_status("create a project", self.api(
            "POST", "/projects", token=token,
            body={"name": f"E2E Carbon project {self.n}", "description": "Ship version two"}), 201)
        self.check("the Carbon owns the project", project["owner"]["uuid"] == c1["uuid"], project)
        patched = self.expect_status("update the project", self.api(
            "PATCH", f"/projects/{project['id']}", token=token, body={"description": "Ship version two by Friday"}), 200)
        self.check("the project update applied", patched["description"] == "Ship version two by Friday", patched)
        self.expect_status("read the project by its UID", self.api("GET", f"/projects/{project['uid']}", token=token), 200)

        self.expect_error("no credential", self.api("GET", "/me"), 401)
        self.expect_error("the Carbon's account-site token (another audience)",
                          self.api("GET", "/me", token=c1["access_token"]), 401, "token_wrong_audience")
        self.expect_error("a tampered token", self.api("GET", "/me", token=token[:-4] + "AAAA"), 401,
                          "token_bad_signature", "token_malformed")
        self.state["c1_project"] = project

    # --- scenario 2 -----------------------------------------------------------------------------------------

    def scenario_2(self):
        """A Silicon on the CLI."""
        c1 = self.carbon("c1")
        s1 = self.silicon("s1")
        home = self.home("s1-cli")
        slt = self.slt("s1")
        login = self.cli(home, "login", "--slt-stdin", stdin=slt)
        self.check("commit login --slt-stdin exits 0", login.returncode == 0, login.stderr[-800:])
        self.check("login names the Silicon and its custodian", s1["id"] in login.stdout and c1["id"] in login.stdout,
                   login.stdout)
        session_file = home / ".commit" / "session.json"
        stored = session_file.read_text()
        self.check("the short-lived token appears nowhere", slt not in login.stdout + login.stderr + stored)
        self.check("the session file is private", (session_file.stat().st_mode & 0o777) == 0o600,
                   oct(session_file.stat().st_mode))
        status = self.cli_json("signed in", home, "login", "status", "--json")
        self.check("login status says who is signed in", status.get("authenticated") is True
                   and status.get("uuid") == s1["uuid"] and status.get("id") == s1["id"]
                   and status.get("kind") == "silicon" and status.get("verified") is True, status)
        self.check("login status names the custodian", (status.get("custodian") or {}).get("uuid") == c1["uuid"],
                   status)

        todo = self.cli_json("todo for the custodian", home, "todos", "create", "--data", json.dumps(
            {"title": "E2E: review the release notes", "assigned_to": c1["id"]}))
        self.check("the Silicon delegated a todo to its custodian", todo["assigned_by"]["uuid"] == s1["uuid"]
                   and todo["assigned_to"]["uuid"] == c1["uuid"], todo)
        project = self.cli_json("project", home, "projects", "create", "--data", json.dumps({
            "name": f"E2E Silicon project {self.n}", "description": "Release checklist",
            "tasks": [{"title": "Draft the notes", "assigned_to": s1["id"]}]}))
        self.check("the Silicon owns a public project", project["owner"]["uuid"] == s1["uuid"]
                   and project["private"] is False, project)
        tasks = self.cli_json("tasks", home, "projects", "tasks", project["id"])
        self.check("the project has its task", [t["title"] for t in tasks["items"]] == ["Draft the notes"], tasks)
        self.cli_json("note", home, "todos", "add-note", todo["id"], "--data", json.dumps({"body": "Drafted."}))
        delegated = self.cli_json("delegated list", home, "todos", "list", "--view", "delegated_by_me")
        self.check("todos list shows the delegated todo", todo["id"] in [t["id"] for t in delegated["items"]],
                   delegated)
        me = self.cli_json("me", home, "me")
        self.check("commit me is the Silicon", me.get("uuid") == s1["uuid"], me)

        logout = self.cli_json("logout", home, "logout", "--json")
        self.check("logout ended the sign-in at Silicon Accounts", logout.get("signed_out") is True
                   and logout.get("revoked") is True, logout)
        after = self.cli(home, "login", "status", "--json")
        self.check("login status --json exits 0 signed out", after.returncode == 0, after.stderr)
        self.check("login status says exactly {\"authenticated\": false}", json.loads(after.stdout) ==
                   {"authenticated": False}, after.stdout)
        self.state.update(s1_todo=todo, s1_project=project)

    # --- scenario 3 -----------------------------------------------------------------------------------------

    def scenario_3(self):
        """Device flow: a Carbon signs the CLI in by approving a code."""
        c1 = self.carbon("c1")
        home = self.home("c1-cli")
        env = {"PATH": "/usr/bin:/bin", "HOME": str(home), "SILICON_HOME": str(home),
               "COMMIT_API_URL": self.cfg["api_url"], "ACCOUNTS_URL": self.cfg["accounts_url"]}
        process = subprocess.Popen([str(self.bin), "login", "--json"], env=env, cwd=home, stdout=subprocess.PIPE,
                                   stderr=subprocess.PIPE, text=True)
        events = []
        found = threading.Event()

        def read_stderr():
            for line in process.stderr:
                events.append(line)
                if '"device_code"' in line:
                    found.set()
        reader = threading.Thread(target=read_stderr, daemon=True)
        reader.start()
        try:
            self.check("commit login printed a device code", found.wait(30), "".join(events)[-800:])
            prompt = json.loads(next(line for line in events if '"device_code"' in line))
            self.log("cli", args=["login", "--json"], stderr_event=prompt)
            self.check("the code points at the account site", prompt["verification_uri"].startswith(
                self.cfg["accounts_url"]) and re.fullmatch(r"[A-Z0-9]{4}-[A-Z0-9]{4}", prompt["user_code"]), prompt)
            approved = self.mint("approve", "--token", c1["access_token"], "--code", prompt["user_code"])
            self.check("the Carbon approved the code", approved.get("status") == 204, approved)
            stdout, _ = process.communicate(timeout=60)
        finally:
            if process.poll() is None:
                process.kill()
        reader.join(5)
        self.log("cli", args=["login", "--json"], exit=process.returncode, stdout=stdout)
        self.check("commit login exits 0 after approval", process.returncode == 0, "".join(events)[-800:])
        result = json.loads(stdout)
        self.check("the CLI is signed in as the Carbon", result.get("authenticated") is True
                   and result.get("uuid") == c1["uuid"] and result.get("method") == "device", result)
        todos = self.cli_json("first command", home, "todos", "list")
        if "s1_todo" in self.state:
            self.check("the Carbon sees the todo its Silicon assigned it",
                       self.state["s1_todo"]["id"] in [t["id"] for t in todos["items"]], todos)
        session_file = home / ".commit" / "session.json"
        session = json.loads(session_file.read_text())
        before = (session["access_token"], session["refresh_token"])
        session["expires_at"] = 1  # as if the 30-minute access token had run out
        session_file.write_text(json.dumps(session))
        self.cli_json("after the access token ran out", home, "todos", "list", "--view", "all")
        after = json.loads(session_file.read_text())
        self.check("the CLI refreshed at Silicon Accounts and saved the rotated pair",
                   (after["access_token"], after["refresh_token"]) != before
                   and after["access_token"] != before[0] and after["refresh_token"] != before[1]
                   and after["expires_at"] > time.time() + 600, {"expires_at": after["expires_at"]})
        self.state["c1_home"] = home

    # --- scenario 4 -----------------------------------------------------------------------------------------

    def scenario_4(self):
        """The custodian rule, the custodian's other Silicons, sharing by id, and Silicons not open to the world."""
        c1, c2, s1, s2 = self.carbon("c1"), self.carbon("c2"), self.silicon("s1"), self.silicon("s2")
        t_c1 = self.carbon_app("c1")
        if "app" not in c2:
            self.say("an account named before it used Commit")
            self.expect_status("the Carbon assigns a todo to another Carbon that never used Commit", self.api(
                "POST", "/todos", token=t_c1, body={"title": "E2E: welcome", "assigned_to": c2["id"]}), 201)
        t_c2 = self.carbon_app("c2")
        me = self.expect_status("the other Carbon signs in and reads /me", self.api("GET", "/me", token=t_c2), 200)
        self.check("its first sign-in shows its own name and the email it shared", me.get("email") == c2["email"]
                   and me.get("display_name"), me)
        t_s1, t_s2 = self.silicon_app("s1"), self.silicon_app("s2")
        project = self.expect_status("the Silicon creates a project", self.api(
            "POST", "/projects", token=t_s1,
            body={"name": f"E2E circle project {self.n}", "description": "Release plan"}), 201)
        pid = project["id"]
        todo = self.expect_status("the Silicon creates a todo for itself", self.api(
            "POST", "/todos", token=t_s1, body={"title": "E2E: S1's own work", "assigned_to": s1["id"]}), 201)
        tid = todo["id"]

        self.say("custodian rule")
        self.expect_status("the custodian reads the Silicon's project", self.api("GET", f"/projects/{pid}", token=t_c1), 200)
        edited = self.expect_status("the custodian edits it", self.api(
            "PATCH", f"/projects/{pid}", token=t_c1, body={"description": "Release plan, checked by the custodian"}), 200)
        self.check("the custodian's edit applied", edited["description"].endswith("custodian"), edited)
        self.expect_status("the custodian reads the Silicon's todo", self.api("GET", f"/todos/{tid}", token=t_c1), 200)
        moved = self.expect_status("the custodian moves its status", self.api(
            "PATCH", f"/todos/{tid}", token=t_c1, body={"status": "in_progress"}), 200)
        self.check("the custodian's change applied", moved["status"] == "in_progress", moved)
        listed = self.expect_status("the custodian lists all work", self.api("GET", "/todos?view=all", token=t_c1), 200)
        self.check("the Silicon's todo is in the custodian's list", tid in [t["id"] for t in listed["items"]], listed)

        self.say("the custodian's other Silicon")
        self.expect_status("a Silicon with the same custodian reads the project", self.api(
            "GET", f"/projects/{pid}", token=t_s2), 200)
        self.expect_error("but cannot change it without being a member", self.api(
            "PATCH", f"/projects/{pid}", token=t_s2, body={"description": "x"}), 403, "project_not_writable")
        self.expect_status("and reads the todo", self.api("GET", f"/todos/{tid}", token=t_s2), 200)

        self.say("an unrelated Carbon")
        self.expect_error("cannot read the project", self.api("GET", f"/projects/{pid}", token=t_c2), 404)
        self.expect_error("cannot read the todo", self.api("GET", f"/todos/{tid}", token=t_c2), 404)
        page = self.expect_status("lists projects", self.api("GET", "/projects", token=t_c2), 200)
        self.check("the project is not in its list", pid not in [p["id"] for p in page["items"]], page)
        shared = self.expect_status("the owner shares the project by c: id", self.api(
            "PATCH", f"/projects/{pid}", token=t_s1, body={"carbon_ids": [c2["id"]]}), 200)
        self.check("the Carbon is a member now", c2["uuid"] in [m["uuid"] for m in shared["members"]], shared)
        self.expect_status("the Carbon reads the shared project", self.api("GET", f"/projects/{pid}", token=t_c2), 200)
        self.expect_status("and, as a member, changes it", self.api(
            "PATCH", f"/projects/{pid}", token=t_c2, body={"description": "Release plan, reviewed by c2"}), 200)
        self.expect_status("the owner unshares it", self.api(
            "PATCH", f"/projects/{pid}", token=t_s1, body={"carbon_ids": []}), 200)
        self.expect_error("the Carbon cannot read it any more", self.api("GET", f"/projects/{pid}", token=t_c2), 404)
        private = self.expect_status("the owner makes it private", self.api(
            "PATCH", f"/projects/{pid}", token=t_s1, body={"private": True}), 200)
        self.check("the project is private", private["private"] is True, private)
        self.expect_error("the custodian's other Silicon no longer sees it", self.api(
            "GET", f"/projects/{pid}", token=t_s2), 404)
        self.expect_status("the custodian of the member Silicon still does", self.api(
            "GET", f"/projects/{pid}", token=t_c1), 200)

        self.say("Silicons are not open to the world")
        self.expect_error("an unrelated Carbon cannot assign the Silicon a todo", self.api(
            "POST", "/todos", token=t_c2, body={"title": "E2E: from outside", "assigned_to": s1["id"]}),
            403, "silicon_not_reachable")
        self.expect_error("nor add it to a project", self.api(
            "POST", "/projects", token=t_c2, body={"name": f"E2E outside {self.n}", "silicon_ids": [s1["id"]]}),
            403, "silicon_not_reachable")
        self.expect_error("nor allow itself", self.api(
            "PUT", f"/silicons/{s1['id']}/allowed-accounts/{c2['id']}", token=t_c2), 403, "not_custodian")
        self.expect_status("the custodian's other Silicon can assign it work", self.api(
            "POST", "/todos", token=t_s2, body={"title": "E2E: from S2", "assigned_to": s1["id"]}), 201)
        allowed = self.expect_status("the custodian allows the Carbon", self.api(
            "PUT", f"/silicons/{s1['id']}/allowed-accounts/{c2['id']}", token=t_c1), 200)
        self.check("the allow-list names the Carbon", [a["account"]["uuid"] for a in allowed["allowed"]] == [c2["uuid"]],
                   allowed)
        outside = self.expect_status("now the Carbon assigns the Silicon a todo", self.api(
            "POST", "/todos", token=t_c2, body={"title": "E2E: allowed work", "assigned_to": s1["id"]}), 201)
        mine = self.expect_status("the Silicon lists its work", self.api("GET", "/todos", token=t_s1), 200)
        self.check("the Silicon sees the todo", outside["id"] in [t["id"] for t in mine["items"]], mine)
        self.expect_status("the custodian sees it too", self.api("GET", f"/todos/{outside['id']}", token=t_c1), 200)
        self.expect_status("the Silicon itself takes the Carbon off its list", self.api(
            "DELETE", f"/silicons/me/allowed-accounts/{c2['id']}", token=t_s1), 200)
        self.expect_error("the Carbon is refused again", self.api(
            "POST", "/todos", token=t_c2, body={"title": "E2E: after disallow", "assigned_to": s1["id"]}),
            403, "silicon_not_reachable")
        self.expect_status("work already assigned stays visible to the Carbon", self.api(
            "GET", f"/todos/{outside['id']}", token=t_c2), 200)
        self.state.update(circle_project=project, s1_own_todo=todo)

    # --- scenario 5 -----------------------------------------------------------------------------------------

    def sql(self, query):
        """One read-only query against the development database; returns the first column of the first row."""
        result = subprocess.run([dev_accounts.pg_tool("psql"), *dev_accounts.pg_args(self.cfg), "-d",
                                 self.cfg["database"], "-tAc", query], capture_output=True, text=True, timeout=30,
                                check=False)
        if result.returncode != 0:
            raise Check(f"psql: {result.stderr.strip()}")
        return result.stdout.strip()

    def scenario_5(self):
        """Webhooks from Silicon Accounts."""
        c1, c2, s1, s2 = self.carbon("c1"), self.carbon("c2"), self.silicon("s1"), self.silicon("s2")
        t_c1, t_c2 = self.carbon_app("c1"), self.carbon_app("c2")
        t_s1, t_s2 = self.silicon_app("s1"), self.silicon_app("s2")
        project = self.state.get("circle_project") or self.expect_status("the Silicon creates a project", self.api(
            "POST", "/projects", token=t_s1, body={"name": f"E2E webhook project {self.n}"}), 201)

        self.say("the custodian changes the Silicon's id")
        new_id = f"si:commit-e2e-s1x-{self.n}"
        since = self.now_iso()
        self.expect_status("POST /v1/me/silicons/{uuid}/id as the custodian", self.accounts_call(
            "POST", f"/v1/me/silicons/{s1['uuid']}/id", bearer=c1["access_token"], body={"id": new_id}), 200)
        changed = self.wait_delivery("account.id_changed", s1["uuid"], since)
        shown = self.expect_status("the custodian reads the project", self.api(
            "GET", f"/projects/{project['id']}", token=t_c1), 200)
        self.check("Commit shows the Silicon's new id", shown["owner"]["id"] == new_id
                   and shown["owner"]["uuid"] == s1["uuid"], shown["owner"])
        me = self.expect_status("the Silicon's earlier token still works", self.api("GET", "/me", token=t_s1), 200)
        self.check("and /me shows the new id", me["id"] == new_id, me)
        s1["id"] = new_id

        self.say("a replayed event id is ignored")
        before = self.delivery_payload(changed["id"])
        self.expect_status("Silicon Accounts replays the delivery", self.accounts_call(
            "POST", "/v1/apps/commit/webhook/replay", basic=("commit", self.cfg["app_secret"]),
            body={"delivery_ids": [changed["id"]]}), 200)
        replayed = self.poll("the replay was delivered", lambda: (
            lambda d: d if d.get("manual_replays", 0) > before.get("manual_replays", 0)
            and d.get("status") == "delivered" else None)(self.delivery_payload(changed["id"])))
        attempts = replayed.get("attempts") or []
        self.check("Commit answered the original and the replay with 2xx", len(attempts) >= 2
                   and all(a.get("status_code") in (200, 204) for a in attempts), attempts)
        event_id = changed["event_id"]
        self.check("the event id was applied once", self.sql(
            f"SELECT count(*) FROM commit.accounts_webhook_events WHERE event_id = '{event_id}'") == "1")
        raw = json.dumps(before["payload"], separators=(",", ":"), sort_keys=True).encode()
        status, body, _ = self.post_webhook(raw)
        self.check("the same event sent again is acknowledged but not applied", status == 200
                   and body.get("applied") is False, f"HTTP {status}: {body}")
        self.state["replay_raw"] = raw

        self.say("a profile change")
        since = self.now_iso()
        name = f"Commit E2E Carbon {self.n}"
        self.expect_status("the Carbon renames itself", self.accounts_call(
            "PATCH", "/v1/me", bearer=c1["access_token"], body={"display_name": name}), 200)
        self.wait_delivery("account.updated", c1["uuid"], since)
        me = self.expect_status("the Carbon reads /me", self.api("GET", "/me", token=t_c1), 200)
        self.check("Commit shows the new display name", me["display_name"] == name, me)

        self.say("forged deliveries are refused")
        forged = json.dumps({"app_id": "commit", "data": {"uuid": c1["uuid"], "kind": "carbon", "old_id": c1["id"],
                             "new_id": "c:forged", "membership_id": f"commit:{c1['uuid']}"},
                             "event_id": str(uuid.uuid4()), "occurred_at": self.now_iso(), "silicon": None,
                             "type": "account.id_changed"}, separators=(",", ":")).encode()
        self.expect_error("signed with another secret", self.post_webhook(forged, secret="whsec_not-the-real-one"),
                          401, "invalid_webhook_signature")
        self.expect_error("unsigned", self.post_webhook(forged, signature=""), 401, "invalid_webhook_signature")
        self.expect_error("signed 10 minutes ago", self.post_webhook(forged, timestamp=int(time.time()) - 600),
                          401, "invalid_webhook_signature")
        self.expect_error("not an event", self.post_webhook(b'{"hello":"world"}'), 400, "invalid_webhook_body")
        me = self.expect_status("the Carbon reads /me", self.api("GET", "/me", token=t_c1), 200)
        self.check("the forged id change was not applied", me["id"] == c1["id"], me)

        self.say("a custodian transfer moves the custodian rule")
        work = self.expect_status("the second Silicon creates a todo", self.api(
            "POST", "/todos", token=t_s2, body={"title": "E2E: S2's work", "assigned_to": s2["id"]}), 201)
        self.expect_status("its custodian reads it", self.api("GET", f"/todos/{work['id']}", token=t_c1), 200)
        self.expect_error("the other Carbon cannot", self.api("GET", f"/todos/{work['id']}", token=t_c2), 404)
        since = self.now_iso()
        request = self.expect_status("the custodian transfers the Silicon", self.accounts_call(
            "POST", f"/v1/me/silicons/{s2['uuid']}/transfer", bearer=c1["access_token"], body={"to": c2["id"]}), 201)
        self.expect_status("the other Carbon accepts", self.accounts_call(
            "POST", f"/v1/me/custodian-requests/{request['request']['id']}/accept", bearer=c2["access_token"]), 204)
        self.wait_delivery("silicon.custodian_changed", s2["uuid"], since)
        self.expect_status("the new custodian reads the Silicon's todo", self.api(
            "GET", f"/todos/{work['id']}", token=t_c2), 200)
        self.expect_error("the former custodian cannot", self.api("GET", f"/todos/{work['id']}", token=t_c1), 404)
        me = self.expect_status("the Silicon reads /me", self.api("GET", "/me", token=t_s2), 200)
        self.check("/me names the new custodian", (me.get("custodian") or {}).get("uuid") == c2["uuid"], me)

        self.say("the Silicon removes Commit")
        self.expect_status("before: the Silicon's token works", self.api("GET", "/me", token=t_s1), 200)
        accounts_home = self.home("s1-accounts")
        login = self.silicon_accounts(accounts_home, "-q", "login", "--silicon", s1["id"], "--stk-stdin",
                                      stdin=s1["stk"])
        self.check("silicon-accounts login --silicon exits 0", login.returncode == 0, login.stderr[-800:])
        slt = self.silicon_accounts(accounts_home, "-q", "login", "--app", "commit")
        self.check("silicon-accounts login --app commit -q prints a short-lived token",
                   slt.returncode == 0 and slt.stdout.strip().startswith("slt_"), slt.stderr[-800:])
        cli_home = self.home("s1-cli-pipeline")
        piped = self.cli(cli_home, "login", "--slt-stdin", stdin=slt.stdout.strip())
        self.check("commit login --slt-stdin takes it (the documented pipeline)", piped.returncode == 0,
                   piped.stderr[-800:])
        status = self.cli_json("before removal", cli_home, "login", "status", "--json")
        self.check("the CLI is signed in as the Silicon", status.get("authenticated") is True
                   and status.get("uuid") == s1["uuid"], status)
        since = self.now_iso()
        removed = self.silicon_accounts(accounts_home, "--json", "apps", "remove", "commit")
        self.check("silicon-accounts apps remove commit exits 0", removed.returncode == 0, removed.stderr[-800:])
        self.wait_delivery("membership.access_removed", s1["uuid"], since)
        self.expect_error("after: its earlier access token is refused", self.api("GET", "/me", token=t_s1), 401,
                          "session_ended")
        status = self.cli(cli_home, "login", "status", "--json")
        answer = json.loads(status.stdout or "{}")
        self.check("after: commit login status --json says signed out (exit 0)", status.returncode == 0
                   and answer.get("authenticated") is False, status.stdout + status.stderr)
        listed = self.cli(cli_home, "todos", "list")
        self.check("after: a command explains the sign-in ended", listed.returncode == 1
                   and "session_ended" in listed.stderr + listed.stdout, listed.stderr[-600:])
        self.state["s1_removed_token"] = t_s1

        self.say("an STK rotation ends the Silicon's sign-ins")
        self.expect_status("before: the Silicon's token works", self.api("GET", "/me", token=t_s2), 200)
        since = self.now_iso()
        rotated = self.expect_status("its custodian rotates the STK", self.accounts_call(
            "POST", f"/v1/me/silicons/{s2['uuid']}/stk", bearer=c2["access_token"], body={}), 200)
        s2["stk"] = rotated["stk"]
        delivery = self.wait_delivery("membership.signed_out", s2["uuid"], since)
        reason = (self.delivery_payload(delivery["id"]).get("payload", {}).get("data") or {}).get("reason")
        self.check("Silicon Accounts said why (stk_rotated)", reason == "stk_rotated", reason)
        self.expect_error("after: the earlier access token is refused", self.api("GET", "/me", token=t_s2), 401,
                          "session_ended")
        t_s2 = self.silicon_app("s2", fresh=True)
        self.expect_status("a sign-in with the new STK works at once", self.api("GET", "/me", token=t_s2), 200)

        self.say("a sign-out at Silicon Accounts is enforced at once where access widens")
        c2_tokens = self.ids["c2"]["app"]
        self.expect_status("the web's sign-out revokes the Carbon's refresh token", self.revoke(
            "commit", self.cfg["app_secret"], c2_tokens["refresh_token"]), 200, 204)
        self.expect_status("a read still works until the access token expires (verified locally)", self.api(
            "GET", "/todos", token=t_c2), 200)
        self.expect_error("changing who can see a project is checked online", self.api(
            "PATCH", f"/projects/{project['id']}", token=t_c2, body={"private": False}), 401, "token_revoked")
        del self.ids["c2"]["app"]

        self.say("a deleted Silicon is forgotten")
        s3 = self.silicon("s3")
        t_s3 = self.silicon_app("s3")
        owned = self.expect_status("the Silicon creates a project shared with its custodian", self.api(
            "POST", "/projects", token=t_s3, body={"name": f"E2E handed on {self.n}", "carbon_ids": [c1["id"]]}), 201)
        mine = self.expect_status("and a todo for itself", self.api(
            "POST", "/todos", token=t_s3, body={"title": "E2E: S3's own work", "assigned_to": s3["id"]}), 201)
        since = self.now_iso()
        self.expect_status("its custodian deletes the Silicon", self.accounts_call(
            "DELETE", f"/v1/me/silicons/{s3['uuid']}", bearer=c1["access_token"], body={"confirm": s3["id"]}), 204)
        self.wait_delivery("account.deleted", s3["uuid"], since)
        self.expect_error("its access token is refused", self.api("GET", "/me", token=t_s3), 401, "account_deleted")
        handed = self.expect_status("the project lives on", self.api("GET", f"/projects/{owned['id']}", token=t_c1),
                                    200)
        self.check("the project passed to its remaining member", handed["owner"]["uuid"] == c1["uuid"]
                   and s3["uuid"] not in [m["uuid"] for m in handed["members"]], handed)
        self.expect_error("its personal todo is gone", self.api("GET", f"/todos/{mine['id']}", token=t_c1), 404)

    # --- scenario 6 -----------------------------------------------------------------------------------------

    def issue_proof(self, subject_token, scopes, receiving_app="commit"):
        body = self.expect_status(f"interface issues a User verification proof for {receiving_app} {scopes}",
                                  self.accounts_call("POST", "/v1/proofs/user-verification",
                                                     basic=("interface", self.interface_secret),
                                                     body={"subject_token": subject_token,
                                                           "receiving_app": receiving_app, "scopes": scopes}), 201)
        self.check("the proof is a sap_ token for the Carbon", body["proof_token"].startswith("sap_"), body)
        return body

    def scenario_6(self):
        """User verification proofs: Commit as the receiving app."""
        if not self.interface_secret:
            raise Check("the stack file has no apps.interface.app_secret; scenario 6 issues proofs as `interface`")
        c1 = self.carbon("c1")
        tokens = self.mint("app-signin", "--app", "interface", "--email", c1["email"], "--redirect",
                           "http://127.0.0.1:9593/interface/callback", "--exchange")
        self.revoke_later.append(("interface", self.interface_secret, tokens["refresh_token"]))
        subject = tokens["access_token"]

        proof = self.issue_proof(subject, ["commit.todos.list", "commit.todos.create", "commit.projects.create"])
        listed = self.expect_status("list todos with the proof", self.api("GET", "/todos", proof=proof["proof_token"]), 200)
        self.check("the list is the Carbon's", all(c1["uuid"] in (t["assigned_to"]["uuid"], t["assigned_by"]["uuid"])
                                                   for t in listed["items"]), listed)
        created = self.expect_status("create a todo with the proof", self.api(
            "POST", "/todos", proof=proof["proof_token"],
            body={"title": "E2E: created through interface", "assigned_to": c1["id"]}), 201)
        self.check("Commit acted as the Carbon", created["assigned_by"]["uuid"] == c1["uuid"], created)
        self.check("Commit recorded the acting app in the todo's history", self.sql(
            f"SELECT count(*) FROM commit.todo_activity WHERE todo_id = '{created['id']}' "
            "AND changes->>'via_app' = 'interface'") == "1")
        self.expect_error("a route the proof has no scope for", self.api("GET", "/me", proof=proof["proof_token"]),
                          403, "proof_scope_missing")
        self.expect_error("a scope Commit does not accept from interface", self.api(
            "POST", "/projects", proof=proof["proof_token"], body={"name": f"E2E via interface {self.n}"}),
            403, "proof_issuer_not_allowed")

        unused = self.issue_proof(subject, ["commit.todos.list"])
        self.expect_status("interface revokes a proof", self.accounts_call(
            "POST", "/v1/proofs/revoke", basic=("interface", self.interface_secret),
            body={"proof_id": unused["proof_id"]}), 204)
        self.expect_error("the revoked proof", self.api("GET", "/todos", proof=unused["proof_token"]), 401,
                          "proof_invalid")
        foreign = self.issue_proof(subject, ["remind.schedules.read"], receiving_app="remind")
        self.expect_error("a proof for another receiving app", self.api("GET", "/todos", proof=foreign["proof_token"]),
                          401, "proof_invalid", "proof_wrong_receiver")
        app_proof = self.expect_status("interface issues an App verification proof", self.accounts_call(
            "POST", "/v1/proofs/app-verification", basic=("interface", self.interface_secret),
            body={"receiving_app": "commit", "scopes": ["commit.todos.list"]}), 201)
        self.expect_error("an App verification proof speaks for no account", self.api(
            "GET", "/todos", proof=app_proof["proof_token"]), 401, "proof_without_account")
        self.expect_error("a proof refresh token sent as a proof", self.api(
            "GET", "/todos", proof=proof["proof_refresh_token"]), 401, "proof_malformed")
        refreshed = self.expect_status("interface refreshes the first proof", self.accounts_call(
            "POST", "/v1/proofs/refresh", basic=("interface", self.interface_secret),
            body={"proof_refresh_token": proof["proof_refresh_token"]}), 200)
        self.expect_status("the refreshed proof token works", self.api(
            "GET", "/todos", proof=refreshed["proof_token"]), 200)
        self.expect_status("interface revokes the proof Commit has verified", self.accounts_call(
            "POST", "/v1/proofs/revoke", basic=("interface", self.interface_secret),
            body={"proof_id": proof["proof_id"]}), 204)
        self.say("waiting 31 s: Commit caches a proof's verification for at most 30 s")
        time.sleep(31)
        self.expect_error("the revoked proof after the cache window", self.api(
            "GET", "/todos", proof=refreshed["proof_token"]), 401, "proof_invalid")

    # --- scenario 7 -----------------------------------------------------------------------------------------

    def scenario_7(self):
        """The Silicon Apps discovery commands from a freshly packed archive, in an empty home."""
        import platform
        version = tomllib.loads((ROOT / "cli/Cargo.toml").read_text())["package"]["version"]
        machine = {"arm64": "aarch64", "aarch64": "aarch64", "x86_64": "x86_64", "AMD64": "x86_64"}.get(
            platform.machine(), platform.machine())
        target = f"{'macos' if sys.platform == 'darwin' else 'linux'}-{machine}"
        binary = Path(self.args.release_bin) if self.args.release_bin else self.cfg["target"] / "release" / "commit"
        if not self.args.release_bin:
            self.say(f"building the release CLI for {target}")
            build = subprocess.run(["cargo", "build", "--locked", "--release", "-p", "silicon-commit-cli", "--bin",
                                    "commit"], cwd=ROOT, capture_output=True, text=True, timeout=1800, check=False,
                                   env={**os.environ, "CARGO_TARGET_DIR": str(self.cfg["target"])})
            self.check("the release CLI builds", build.returncode == 0, build.stderr[-1500:])
        out = self.dir / "dist"
        pack = subprocess.run([str(ROOT / "scripts/package-apps.sh"), version, target, str(binary), "--output-dir",
                               str(out), "--discovery", "require"], cwd=ROOT, capture_output=True, text=True,
                              timeout=600, check=False)
        self.log("package", exit=pack.returncode, stdout=pack.stdout[-2000:], stderr=pack.stderr[-2000:])
        self.check(f"scripts/package-apps.sh {version} {target} packs the CLI", pack.returncode == 0,
                   (pack.stdout + pack.stderr)[-1500:])
        archive = out / f"commit-{version}-{target}.tar.gz"
        extracted = self.home("archive")
        with tarfile.open(archive) as tar:
            names = sorted(tar.getnames())
            tar.extractall(extracted, filter="data")
        self.check("the archive holds apps.yaml and bin/commit only", names == ["apps.yaml", "bin/commit"], names)
        manifest = (extracted / "apps.yaml").read_text()
        self.check("apps.yaml names the app, its version and only this target", "app_id: commit" in manifest
                   and f"version: {version}" in manifest and f"{target}:" in manifest
                   and manifest.count("binary:") == 1, manifest)
        empty = self.home("empty")
        env = {"PATH": "/usr/bin:/bin", "HOME": str(empty), "SILICON_HOME": str(empty)}

        def run(*args):
            result = subprocess.run([str(extracted / "bin/commit"), *args], capture_output=True, text=True, env=env,
                                    cwd=empty, timeout=60, check=False)
            self.log("discovery", args=list(args), exit=result.returncode, stdout=result.stdout[-1500:],
                     stderr=result.stderr[-800:])
            return result
        helped = run("--help")
        self.check("commit --help exits 0 with text", helped.returncode == 0 and len(helped.stdout.strip()) > 100,
                   helped.stderr)
        accounts = run("accounts", "--json")
        found = json.loads(accounts.stdout) if accounts.returncode == 0 else {}
        self.check("commit accounts --json exits 0 with app_id commit", found.get("app_id") == "commit"
                   and found.get("accounts_url") == "https://accounts.teamofsilicons.com"
                   and found.get("version") == version, accounts.stdout + accounts.stderr)
        status = run("login", "status", "--json")
        self.check("commit login status --json exits 0 and says signed out", status.returncode == 0
                   and json.loads(status.stdout) == {"authenticated": False}, status.stdout + status.stderr)
        self.check("the empty home stayed empty", not any(empty.iterdir()), [p.name for p in empty.iterdir()])

    # --- scenario 8 -----------------------------------------------------------------------------------------

    def scenario_8(self):
        """Restart safety."""
        c1 = self.carbon("c1")
        t_c1 = self.carbon_app("c1")
        self.expect_status("before: the Carbon's access token works", self.api("GET", "/me", token=t_c1), 200)
        event_id = str(uuid.uuid4())
        raw = json.dumps({"app_id": "commit", "data": {}, "event_id": event_id, "occurred_at": self.now_iso(),
                          "silicon": None, "type": "ping"}, separators=(",", ":")).encode()
        status, body, _ = self.post_webhook(raw)
        self.check("before: a new event is applied", status == 200 and body.get("applied") is True, body)
        old_pid = dev_accounts.pid_of(self.cfg, "api")
        dev_accounts.restart(self.cfg)
        new_pid = dev_accounts.pid_of(self.cfg, "api")
        self.check("the API restarted", old_pid and new_pid and old_pid != new_pid, f"{old_pid} -> {new_pid}")
        me = self.expect_status("after: the same access token works (stateless JWT)", self.api("GET", "/me", token=t_c1),
                                200)
        self.check("after: it is still the Carbon", me["uuid"] == c1["uuid"], me)
        if "c1_home" in self.state:
            result = self.cli_json("after restart", self.state["c1_home"], "me")
            self.check("after: the CLI's saved session works", result.get("uuid") == c1["uuid"], result)
        status, body, _ = self.post_webhook(raw)
        self.check("after: the same event id is acknowledged but not applied", status == 200
                   and body.get("applied") is False, body)
        if "replay_raw" in self.state:
            status, body, _ = self.post_webhook(self.state["replay_raw"])
            self.check("after: the id change replayed again is not applied", status == 200
                       and body.get("applied") is False, body)
        if "s1_removed_token" in self.state:
            self.expect_error("after: the Silicon that removed Commit stays refused", self.api(
                "GET", "/me", token=self.state["s1_removed_token"]), 401, "session_ended")
        self.expect_error("after: a forged delivery is refused", self.post_webhook(raw, secret="whsec_forged"), 401,
                          "invalid_webhook_signature")

    # --- running --------------------------------------------------------------------------------------------

    def revoke(self, app_id, secret, token):
        """POST /v1/oauth/revoke for a refresh token: with the app secret, or as a public client without one."""
        form = {"token": token, "token_type_hint": "refresh_token"}
        if secret:
            response = http("POST", f"{self.accounts}/v1/oauth/revoke", form=form, basic=(app_id, secret))
        else:
            response = http("POST", f"{self.accounts}/v1/oauth/revoke", form={**form, "client_id": app_id})
        self.log("accounts", method="POST", path=f"/v1/oauth/revoke ({app_id})", status=response[0],
                 response=response[1])
        return response

    def cleanup(self):
        """Ends the sign-ins this run made (revoked refresh tokens send app_revoked sign-outs, which are harmless)."""
        if "c1_home" in self.state:
            self.cli(self.state["c1_home"], "logout", "--json")
        for app_id, secret, token in self.revoke_later:
            self.revoke(app_id, secret, token)

    def run(self, selected):
        names = {1: "Carbon on the API", 2: "Silicon on the CLI", 3: "device flow", 4: "circle and sharing",
                 5: "webhooks", 6: "proofs", 7: "discovery from a packed archive", 8: "restart safety"}
        try:
            dev_accounts.ensure_stack(self.cfg)
            if not dev_accounts.pid_of(self.cfg, "api"):
                dev_accounts.up(self.cfg, argparse.Namespace(build=False, fresh=False))
                self.started_stack = True
            for number in selected:
                self.current = number
                print(f"\n== scenario {number}: {names[number]}", flush=True)
                started = time.time()
                try:
                    getattr(self, f"scenario_{number}")()
                    outcome = {"scenario": number, "name": names[number], "ok": True}
                except Check as failure:
                    print(f"  FAIL {failure}", flush=True)
                    outcome = {"scenario": number, "name": names[number], "ok": False, "failure": str(failure)}
                outcome["seconds"] = round(time.time() - started, 1)
                self.results.append(outcome)
                self.log("scenario", **outcome)
        finally:
            self.current = None
            self.cleanup()
            if self.started_stack and not self.args.keep_running:
                dev_accounts.down(self.cfg, argparse.Namespace(keep_webhook=False))
        passed = sum(1 for r in self.results if r["ok"])
        summary = {"run": self.n, "passed": passed, "failed": len(self.results) - passed, "results": self.results,
                   "transcript": str(self.dir / "transcript.jsonl")}
        (self.dir / "result.json").write_text(json.dumps(summary, indent=1) + "\n")
        print(f"\n{passed}/{len(self.results)} scenarios passed; details in {self.dir}")
        return 0 if passed == len(self.results) else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--only", help="comma-separated scenario numbers (default: all, in order)")
    parser.add_argument("--keep-running", action="store_true", help="leave a stack this run started running")
    parser.add_argument("--release-bin", help="scenario 7: pack this commit binary instead of building one")
    args = parser.parse_args()
    selected = [int(x) for x in args.only.split(",")] if args.only else list(range(1, 9))
    try:
        return Run(args).run(selected)
    except dev_accounts.Failure as error:
        print(f"e2e-accounts: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
