"""Provision Honeycomb/Commit testing credentials during managed deployment.

This credential is deployment plumbing, never an app-registration or user setup step.
Existing credentials are preserved; a partially completed update is safe to resume.
"""
import copy
import hmac
import json
import os
from pathlib import Path
import re
import secrets
import subprocess
import tempfile
from urllib.parse import urlsplit

TOKEN = "COMMIT_HONEYCOMB_SERVICE_TOKEN"
REGISTRY = "HONEYCOMB_LIFECYCLE_PARTICIPANTS"


def https_origin(value, name, allow_path=False):
    """Use only an explicitly configured service URL, never a derived domain."""
    try:
        if not isinstance(value, str) or any(c.isspace() for c in value):
            raise ValueError()
        parsed = urlsplit(value)
        if (parsed.scheme != "https" or not parsed.hostname or parsed.username is not None
                or parsed.password is not None or parsed.query or parsed.fragment
                or (not allow_path and parsed.path not in ("", "/"))):
            raise ValueError()
        parsed.port  # Reject malformed ports even when no request is being made.
        return "https://" + parsed.netloc
    except (TypeError, ValueError):
        raise ValueError(f"{name} must contain an explicit HTTPS service URL") from None


def valid_app_id(value):
    return isinstance(value, str) and re.fullmatch(
        r"[a-z][a-z0-9_-]{0,79}", value
    ) is not None


def desired_credentials(honeycomb, commit, commit_public_base_url=None):
    """Return updated copies without rotating an existing service credential."""
    honeycomb = copy.deepcopy(honeycomb)
    commit = copy.deepcopy(commit)
    backend = honeycomb["backend"]
    app_id = commit.get("COMMIT_IAM_APP_ID")
    if not valid_app_id(app_id):
        raise ValueError("COMMIT_IAM_APP_ID must contain the configured application identity")
    runtime_url = commit.get("COMMIT_PUBLIC_BASE_URL")
    if runtime_url is not None and commit_public_base_url is not None:
        if https_origin(runtime_url, "COMMIT_PUBLIC_BASE_URL", True) != https_origin(
                commit_public_base_url, "commit_public_base_url", True):
            raise ValueError("Configured Commit API origins differ; refusing to overwrite")
    base = https_origin(runtime_url if runtime_url is not None else commit_public_base_url,
                        "COMMIT_PUBLIC_BASE_URL", True)
    https_origin(commit.get("COMMIT_HONEYCOMB_URL"), "COMMIT_HONEYCOMB_URL")
    try:
        entries = json.loads(backend.get(REGISTRY, "[]"))
    except (TypeError, ValueError):
        raise ValueError("Invalid lifecycle participant registry") from None
    if not isinstance(entries, list):
        raise ValueError("Invalid lifecycle participant registry")
    registered = set()
    existing = None
    for entry in entries:
        if (not isinstance(entry, dict) or set(entry) != {"app_id", "base_url", "token_env"}
                or not valid_app_id(entry["app_id"])
                or entry["app_id"] in registered
                or not isinstance(entry["token_env"], str)
                or not re.fullmatch(r"[A-Z_][A-Z0-9_]{0,127}", entry["token_env"])):
            raise ValueError("Invalid or duplicate lifecycle participant entry")
        registered.add(entry["app_id"])
        origin = https_origin(entry["base_url"], "Lifecycle participant base_url")
        if entry["app_id"] == app_id:
            if origin != base or entry["token_env"] != TOKEN:
                raise ValueError("Commit lifecycle participant configuration differs; refusing to overwrite")
            existing = entry
        elif entry["token_env"] == TOKEN:
            raise ValueError("Commit lifecycle credential is assigned to another identity")

    tokens = [value for value in (backend.get(TOKEN), commit.get(TOKEN)) if value]
    for token in tokens:
        if not isinstance(token, str) or len(token) < 32 or not all(33 <= ord(c) <= 126 for c in token):
            raise ValueError("Existing testing service credential has invalid format")
    if len(tokens) == 2 and not hmac.compare_digest(*tokens):
        raise ValueError("Testing service credentials differ; refusing to overwrite either service")
    token = tokens[0] if tokens else secrets.token_urlsafe(48)
    backend[TOKEN] = commit[TOKEN] = token
    if existing is None:
        entries.append({"app_id": app_id, "base_url": base, "token_env": TOKEN})
        backend[REGISTRY] = json.dumps(entries, separators=(",", ":"))
    return honeycomb, commit


def ensure_testing_credentials(profile, honeycomb_region, honeycomb_secret,
                               commit_region="us-east-1",
                               commit_secret="silicon-commit/production",
                               commit_public_base_url=None):
    def aws(region, *command):
        result = subprocess.run(
            ["aws", *(["--profile", profile] if profile else []), "--region", region, "secretsmanager",
             *command, "--output", "json"], capture_output=True, text=True,
        )
        if result.returncode:
            # AWS error text can contain request content; never echo it with secrets.
            raise RuntimeError("Testing credential secret-store operation failed")
        return json.loads(result.stdout)

    def read(region, name):
        result = aws(region, "get-secret-value", "--secret-id", name)
        return result["VersionId"], json.loads(result["SecretString"])

    def write_if_changed(region, name, version, previous, desired):
        if previous == desired:
            return False
        latest_version, _ = read(region, name)
        if latest_version != version:
            raise RuntimeError("Service secret changed during deployment; retry with current configuration")
        fd, filename = tempfile.mkstemp(prefix="honeycomb-testing-credential-")
        try:
            os.fchmod(fd, 0o600)
            with os.fdopen(fd, "w") as stream:
                json.dump(desired, stream)
            aws(region, "put-secret-value", "--secret-id", name,
                "--secret-string", "file://" + filename)
        finally:
            Path(filename).unlink(missing_ok=True)
        return True

    hc_version, hc = read(honeycomb_region, honeycomb_secret)
    bc_version, bc = read(commit_region, commit_secret)
    next_hc, next_bc = desired_credentials(hc, bc, commit_public_base_url)
    # Commit first: if the second write fails, retry reuses its credential.
    changed_bc = write_if_changed(commit_region, commit_secret, bc_version, bc, next_bc)
    changed_hc = write_if_changed(honeycomb_region, honeycomb_secret, hc_version, hc, next_hc)
    _, verified_hc = read(honeycomb_region, honeycomb_secret)
    _, verified_bc = read(commit_region, commit_secret)
    if not hmac.compare_digest(verified_hc["backend"].get(TOKEN, ""), next_hc["backend"][TOKEN]) or not hmac.compare_digest(verified_bc.get(TOKEN, ""), next_bc[TOKEN]):
        raise RuntimeError("Testing credential changed during verification; deployment stopped")
    return {"honeycomb_secret_updated": changed_hc, "commit_secret_updated": changed_bc}


if __name__ == "__main__":
    import argparse
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile")
    parser.add_argument("--honeycomb-region", default="us-east-1")
    parser.add_argument("--honeycomb-secret", required=True)
    parser.add_argument("--commit-region", default="us-east-1")
    parser.add_argument("--commit-secret", required=True)
    parser.add_argument("--commit-public-base-url", required=True)
    args = parser.parse_args()
    print(json.dumps(ensure_testing_credentials(args.profile, args.honeycomb_region,
        args.honeycomb_secret, args.commit_region, args.commit_secret,
        args.commit_public_base_url)))
