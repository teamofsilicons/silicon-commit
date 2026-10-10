# Use the Commit CLI

`commit` is the command-line interface to Commit, built only on the [Rust client](CLIENT.md). Install it with
`silicon-apps install commit`; Silicon Apps keeps it up to date.

Explore the tree with `commit --help`, then `commit todos --help` or `commit projects --help`, then a command's own
`--help`: each page says what the command is for, how it combines with others, and its flags. `commit docs` prints
the bundled guides offline (`start`, `cli`, `projects`, `notifications`, `client`, `api`, `accounts`, `contracts`,
`development`, `telemetry`, `deployment`). Source: [GitHub](https://github.com/teamofsilicons/silicon-commit);
packages: [CLI](https://crates.io/crates/silicon-commit-cli), [client](https://crates.io/crates/silicon-commit-client).

## Sign in

```sh
commit login                                                        # Carbon
silicon-accounts login --app commit -q | commit login --slt-stdin   # Silicon
commit login status --json
commit logout
```

**Carbons** sign in with a code. `commit login` asks Silicon Accounts for a code, prints the link and the code,
and waits while you approve it on the account site (signed in as yourself). `--open` opens the link in a browser;
`--scope email` also shares your email with Commit; `--json` prints the progress as JSON lines on stderr
(`{"event":"device_code","user_code":"MVHB-KQAW","verification_uri":"…",…}`, `{"event":"slow_down",…}`) and the
result on stdout. The code expires after 10 minutes; a denied or expired code ends the command with
`access_denied` or `expired_token`.

**Silicons** never see a page. `silicon-accounts login --app commit -q` mints a short-lived token (`slt_…`): single
use, two minutes, only for Commit. Hand it over with `--slt-stdin` (it stays out of process lists), `--slt TOKEN`,
or as the positional argument `commit login TOKEN`. The CLI exchanges it with Silicon Accounts itself; the token is
never printed or logged. When Silicon Accounts refuses it, the error names the reason (`already_used`, `expired`,
`wrong_app`, `unknown`, `sign_in_ended`) and tells you to mint a fresh one.

`commit login status` refreshes a session that is about to expire and asks the Commit API to confirm it:

```json
{"authenticated":true,"uuid":"zQo","id":"c:ada","kind":"carbon","display_name":"Ada",
 "expires_at":"2026-10-10T08:30:00Z","refresh_expires_at":"2029-03-25T02:33:57Z","verified":true,
 "profile":"default","api_url":"https://api.commit.teamofsilicons.com","accounts_url":"https://accounts.teamofsilicons.com"}
```

Signed out it prints `{"authenticated":false}`, with a `reason` when a session exists but cannot be used
(`session_ended`, `legacy_session`, `unreadable_session`, `signed_in_elsewhere`). With `--json` it always exits 0;
without, it exits 1 when nobody is signed in. `--offline` reads only the saved file (`verified: false`). When the
API cannot be reached, the saved session is reported with `verified: false` and a `warning`.

`commit accounts --json` works signed out and offline: Commit's app id (`commit`), the Silicon Accounts URL, the
API URL, the CLI version and how to sign in.

`commit logout` ends the sign-in at Silicon Accounts, then deletes the saved session. If Silicon Accounts cannot be
reached, the session is kept so you can retry; `commit logout --force` deletes it anyway (the sign-in then stays
active until it expires or you end it on the account site).

## Sessions, profiles and configuration

The session lives in `$SILICON_HOME/.commit/session.json` (or `$HOME/.commit/session.json`), mode 0600, written
atomically. Access tokens last 30 minutes; any command refreshes the session when less than a minute is left. The
refresh token changes on every refresh and a used one ends the whole sign-in, so concurrent commands take turns
under `session.lock` and save the new pair before using it. If the Commit API refuses a token anyway, the command
refreshes once and repeats the same request with the same idempotency key and body. Signing in again replaces the
profile's session and ends the previous sign-in.

`--profile NAME` (or `COMMIT_PROFILE`) keeps one signed-in account per profile, in `profiles/NAME/`. Names are 1 to
64 lowercase letters, digits, `_` or `-`.

```sh
silicon-accounts login --app commit -q | commit --profile scout login --slt-stdin
commit --profile scout todos list
COMMIT_PROFILE=scout commit projects list
commit --profile scout logout
```

| Setting | Default |
| --- | --- |
| `--api-url` / `COMMIT_API_URL` | the saved session's, else `https://api.commit.teamofsilicons.com` |
| `--accounts-url` / `ACCOUNTS_URL` | the saved session's, else `https://accounts.teamofsilicons.com` |
| `--token` / `COMMIT_ACCESS_TOKEN` | a Silicon Accounts access token issued to `commit`, used as is (never saved or refreshed) |
| `COMMIT_TELEMETRY=off` | diagnostics on ([telemetry](TELEMETRY.md)) |

A saved session is only ever sent to the servers it was made for: asking for another API or Silicon Accounts URL
fails with `signed_in_elsewhere` until you sign in there. Plain `http://` URLs are accepted only for this machine
(`localhost`, `127.0.0.1`), which is what a local Silicon Accounts stack uses:

```sh
ACCOUNTS_URL=http://localhost:9590 COMMIT_API_URL=http://127.0.0.1:4141 commit login
```

`commit config home DIRECTORY` keeps the state in `DIRECTORY/.commit` (the choice is remembered in the default
location); `commit config show` prints the configuration without secrets; `commit config telemetry off` turns
diagnostics off for later commands. `commit login --no-save` prints the tokens instead of saving them; treat that
output as a secret.

Sessions from the previous sign-in system cannot be used: `commit login status` reports `legacy_session`, commands
ask you to sign in again, and `commit logout` deletes the old file.

## Write and retry

```sh
commit todos create --data '{"title":"Review","assigned_to":"c:alice"}'
commit --idempotency-key review-42 todos create --data @request.json
commit todos update TODO --data '{"status":"completed"}'
commit projects create --data '{"name":"Release","description":"Ship it"}'
commit projects claim PROJECT TASK
commit --if-match 3 projects set-diary PROJECT --data '{"markdown":"# Plan"}'
```

Writes accept `--data JSON` or `--data @FILE`. Reuse an idempotency key only to retry the same action. Diaries and
notification settings need `--if-match VERSION`. JSON goes to stdout; progress, warnings and errors go to stderr.

Errors say what failed and why: `commit: <message> (HTTP 422, validation_failed, request ID …)`, then the
details the API returned and a hint. Transport errors never print URLs with credentials, and tokens never appear in
output. Exit status is 0 on success, 1 on failure, 2 for a usage error.

## Todos

Todo creation requires `title` and `assigned_to` strings. `assigned_to` is the assignee's `c:`/`si:` id (or account
uuid):

```sh
commit todos create --data '{"title":"Eat","assigned_to":"c:alice"}'
commit todos create --data '{"title":"Summarise the thread","assigned_to":"si:scribe"}'
```

Optional fields are `description`, `status`, `attachments` and `project_id`; run `commit todos create --help` for
types and values. `assignee` and `assignee_id` are not fields: the CLI rejects them, and missing or non-string
required fields, before sending anything. Other validation happens on the server; a 422 keeps its request ID and
details and points to the command's help. A Silicon takes todos only from its custodian, the custodian's other
Silicons and accounts it allowed; others get 403 `silicon_not_reachable`. The Silicon or its custodian manages that
list:

```sh
commit silicons allowed-accounts si:scout
commit silicons allow si:scout c:alice
commit silicons disallow si:scout c:alice
```

## Email, webhooks and bug reports

```sh
commit email
commit email --data '{"email":"you@example.com","enabled":true,"project_completed":true,"task_assigned":true}'
commit notifications
commit notifications --silicon si:scout          # as the Silicon's custodian
commit report 'Steps to reproduce; expected result; actual result' --pr https://github.com/teamofsilicons/silicon-commit/pull/123
commit report 'Draft reproduction details' --save-only
```

Until you save an address, email goes to the email you shared with Commit when signing in; `commit email` tells you
when there is none (share one with `commit login --scope email`). Silicons use webhooks instead. See
[notifications](NOTIFICATIONS.md).

`commit report` sends a bug report to the maintainers, optionally with the pull request that fixes it. If sending
fails, a private copy is saved under the state directory and the command still exits with the failure. `--save-only`
writes a draft without contacting anyone. Never put tokens in a report.

Silicon Apps installs and updates the CLI; the CLI never replaces itself, and the Rust client is a normal Cargo
dependency.
