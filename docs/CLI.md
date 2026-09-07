# Commit CLI

Build with `cargo build -p commit`; invoke the resulting binary as `commit`. Run `commit --help` or `commit <command> --help` for the complete grammar. Output is pretty JSON and is suitable for scripting.

Configure `COMMIT_API_URL`, `COMMIT_ACCESS_TOKEN`, and `COMMIT_ORG_ID`, or pass `--api-url`, `--token`, and `--org-id`. `commit login <slt>` exchanges a Silicon IAm short-lived token. Tokens are saved with mode `0600` under `~/.commit/session.json`; `--no-save` prints them without writing. The saved API URL is reused on later invocations. `--idempotency-key` reuses a write key after a transport failure and `--if-match VERSION` sends an ETag precondition.

Commands:

- `health`, `ready`, `version`
- `todos list [--view assigned_to_me|delegated_by_me|all] [--status STATUS] [--assigned-to ID] [--assigned-by ID] [--created-from RFC3339] [--created-to RFC3339] [--limit 1..100] [--cursor CURSOR]`; `get`, `create`, `update`, `delete`, `notes [--limit N] [--cursor CURSOR]`, `add-note`, `subscription`, and `set-subscription`.
- `projects list [--status STATUS] [--silicon-id ID] [--limit 1..100] [--cursor CURSOR]`; `get`, `create`, `update`, `diary`, `set-diary`, `tasks [--limit N] [--cursor CURSOR]`, `create-task`, `update-task`, `blocker`, `create-update`, and `complete`.
- `notifications` reads settings; `notifications --data JSON` replaces them.
- `test-environments list`, `create`, `key`, `rotate`, `clean`, `restore`, and `delete` manage organization-owned sandboxes.

Write payloads use `--data '<json>'` or `--data @path/to/file`. Prefix any ordinary resource command with `--test <32-character-key>` (or set `COMMIT_TEST_KEY`) to run it in an isolated test environment; the command and API surface stay identical to production. Test environments enforce the server's 10-project and 100-todo limits.

The CLI records the last crates.io update check in `~/.commit/last-update-check` and checks at most once per hour. After a command completes, a newer release is installed with `cargo install --locked --force commit`; a failed update is non-fatal and retried on the next hourly check. Use `--no-update` to disable checks and installation for a run. Update messages go to stderr so automation's JSON stdout remains clean.
