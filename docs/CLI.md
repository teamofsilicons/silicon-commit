# Commit CLI

Install with `cargo install --locked silicon-commit-cli`, or build with `cargo build -p silicon-commit-cli`; invoke the resulting binary as `commit`. Run `commit --help` or `commit <command> --help` for the complete grammar. Output is pretty JSON and is suitable for scripting.

Configure `COMMIT_API_URL`, `COMMIT_ACCESS_TOKEN`, and `COMMIT_ORG_ID`, or pass `--api-url`, `--token`, and `--org-id`. `commit login <slt>` exchanges a Silicon IAm short-lived token. Tokens are saved with mode `0600` under `{home_dir}/.commit/session.json`; the default home directory is `SILICON_HOME` when set, otherwise the operating-system `HOME` (`~`). Change it with `commit config home {location}`; the location must already be a directory. `--no-save` prints tokens without writing them. The saved API URL is reused on later invocations. `--idempotency-key` reuses a write key after a transport failure and `--if-match VERSION` sends an ETag precondition.

Commands:

- `iam [--json]`
- `login <slt> [--no-save]`, `login status [--json]`
- `logout [--json]`
- `config home LOCATION`
- `health`, `ready`, `version`
- `todos list [--view assigned_to_me|delegated_by_me|all] [--status STATUS] [--assigned-to ID] [--assigned-by ID] [--created-from RFC3339] [--created-to RFC3339] [--limit 1..100] [--cursor CURSOR]`; `get`, `create`, `update`, `delete`, `notes [--limit N] [--cursor CURSOR]`, `add-note`, `subscription`, and `set-subscription`.
- `projects list [--status STATUS] [--silicon-id ID] [--limit 1..100] [--cursor CURSOR]`; `get`, `create`, `update`, `diary`, `set-diary`, `tasks [--limit N] [--cursor CURSOR]`, `create-task`, `update-task`, `blocker`, `create-update`, and `complete`.
- `notifications` reads settings; `notifications --data JSON` replaces them.
- `test-environments list`, `create`, `key`, `rotate`, `clean`, `restore`, and `delete` manage organization-owned sandboxes.

Write payloads use `--data '<json>'` or `--data @path/to/file`. Prefix any ordinary resource command with `--test <32-character-key>` (or set `COMMIT_TEST_KEY`) to run it in an isolated test environment; the command and API surface stay identical to production. Test environments enforce the server's 10-project and 100-todo limits.

The CLI records the last crates.io update check in `{home_dir}/.commit/last-update-check` and checks at most once per hour. After a command completes, a newer release is installed with `cargo install --locked --force silicon-commit-cli`; a failed update is non-fatal and retried on the next hourly check. Use `--no-update` to disable checks and installation for a run. Update messages go to stderr so automation's JSON stdout remains clean.

## IAM discovery and login status

```sh
commit --help
commit iam --json
commit login <slt>
commit login status --json
commit --test <32-character-key> login status --json
```

`commit iam --json` reads public application metadata from the configured Commit backend. It returns `app_id` (use it when obtaining an SLT from IAM) and `iam_url`. It needs no login and never sends saved credentials, organization headers, or test keys. Application secrets stay on the backend.

`commit login status --json` checks the current access token against live IAM authorization, returning the public Carbon or Silicon identity without tokens:

```json
{
  "authenticated": true,
  "app_id": "tos>commit",
  "actor": { "type": "silicon", "id": "agent" },
  "org_id": "tos",
  "organizations": ["tos"]
}
```

Command-line options and environment variables override the saved token, API URL, and organization. With an organization configured, status checks that exact organization. Otherwise it checks the session's selected active organizations; `org_id` is null when several are available. With no token, a revoked/expired token, or no active organization authorization, the CLI returns `{"authenticated":false,"actor":null,"org_id":null}`. Inspect the boolean in scripts: both authenticated and unauthenticated results exit successfully. Network, permission, and service failures exit nonzero; they are not treated as logged out. Status does not refresh or overwrite the session. Both discovery and status output JSON with or without `--json`.

## Local storage precedence

`commit logout` revokes the saved refresh token (and its session family), then removes only the resolved `session.json`. It preserves credentials on failure and succeeds without a network request when no session is saved. It uses the saved API URL; an explicitly configured API must match. Access-token and organization overrides do not change which saved session is revoked. For a testing session, pass the same `--test` key or `COMMIT_TEST_KEY` used at login; the existing session format does not save this selector. Logout does not run the automatic update check.

1. The location saved by `commit config home LOCATION`, if configured.
2. `SILICON_HOME`, when present in the process environment.
3. `HOME` (`~`).

The home override pointer is stored at `${SILICON_HOME-$HOME}/.commit/home_dir`. Changing `SILICON_HOME` therefore selects a separate configuration root. Sessions and hourly update markers both follow the resolved home directory. Configuring a directory does not move an existing session; run `commit login <slt>` to save a session there. The location must already exist and be a directory.

```sh
mkdir -p /tmp/commit-agent
export SILICON_HOME=/tmp/commit-agent
commit login <slt>
# Session: /tmp/commit-agent/.commit/session.json
```

`commit -h` and `commit --help` list the available commands, configuration options, and quick-start examples. Use `commit <command> --help` for that command's full grammar. Add `--no-update` to avoid the post-command update check, including in scripts.

## Creating and assigning todos

Todo creation requires `title` and `assigned_to` strings. Use the recipient's public IAM ID from your team, including the organization suffix for a Silicon.

```sh
commit todos create --data '{"title":"Eat","assigned_to":"alex"}'
commit todos create --data '{"title":"Eat","assigned_to":"assistant:example-org"}'
```

`--data @file.json` accepts the same object. Optional fields are `description`, `status`, and `attachments`; run `commit todos create --help` for types and values. `assignee` and `assignee_id` are not supported. The CLI rejects these fields and missing or non-string required fields before making a request. Other validation remains on the server; a 422 error retains its request ID and points to the command's schema help. Errors go to stderr with a nonzero exit status.
