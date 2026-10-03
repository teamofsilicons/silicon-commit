# Use the Commit CLI

Install the CLI through Honeycomb:

```sh
honeycomb install 'commit'
commit iam --json
commit login '<IAM-SLT>'
commit login status --json
commit --org-id your-org todos list
```

Use `commit --help`, then `commit todos --help` or `commit projects --help`, then a leaf command's `--help`. `commit docs start`, `projects`, `testing`, `api`, `client`, `cli`, and `development` read bundled offline guides. Source: [GitHub](https://github.com/teamofsilicons/silicon-commit); packages: [CLI](https://crates.io/crates/silicon-commit-cli), [client](https://crates.io/crates/silicon-commit-client).

## Write and retry

```sh
commit todos create --data '{"title":"Review","assigned_to":"alice"}'
commit --idempotency-key review-42 todos create --data @request.json
commit todos update TODO --data '{"status":"completed"}'
commit projects create --data '{"name":"Release","description":"Ship it"}'
commit projects tasks PROJECT
commit projects claim PROJECT TASK
commit projects versions PROJECT
```

Writes accept `--data JSON` or `--data @FILE`. Reuse an idempotency key for a retry of the same action, not a different action. Diaries and webhook preferences use `--if-match VERSION`. JSON goes to stdout; diagnostic and testing messages go to stderr. Errors include HTTP status, a stable code and request ID; transport errors do not print credential-bearing URLs. See [projects](PROJECTS.md) and the [API](API.md).

## Organization selection and recovery

IAM 5 login returns one Carbon or Silicon and one organization. Use `--profile NAME` or `COMMIT_PROFILE` to save several independent accounts or organization logins. The default profile is `default`; names use 1–64 lowercase letters, numbers, underscores or hyphens. A saved profile's actor and organization never change during refresh; `--org-id` must match its saved organization. To use another organization, log in under another profile. Legacy unscoped or actorless session files require a fresh login. Explicit `--token` calls remain caller-managed and do not inherit or modify a saved session's organization.

```sh
commit --profile personal --org-id team-a login '<oac_code>'
commit --profile work --org-id team-b login '<oac_code>'
commit --profile personal todos list
COMMIT_PROFILE=work commit projects list
commit --profile work logout
```

`commit report "DETAILS"` saves a private local Markdown copy if submission fails, including when authentication or organization discovery fails. The command still exits with the original failure so automation can distinguish local saving from submission. Use `--save-only` to write a report without contacting the service.

## Sessions and configuration

State is stored in `$SILICON_HOME/.commit` or `$HOME/.commit`. `commit config home DIRECTORY` selects an existing directory; the pointer remains in the original configuration root. The default profile keeps `session.json` and hashed sandbox session filenames. Named profiles use `profiles/NAME/` with their own production session, sandbox sessions, and saved testing selector. Files are atomically saved with private permissions.

`--api-url`, `--token`, `--org-id` and their environment variables configure explicit calls; saved credentials cannot be sent to another API or organization. `--profile` takes precedence over `COMMIT_PROFILE`. Default backend: `https://backend.commit.teamofsilicons.com`. `commit logout` revokes and removes only the selected saved session. It retains the file on failure. `commit login SLT --no-save` prints tokens instead of persisting them; treat that output as secret.

```sh
commit config show
commit config updates off
commit config telemetry off
commit testing use '<testing-app-secret>'
commit login '<test-SLT-or-existing-public-ID>'
commit testing status
commit testing exit
```

`commit --test APP_SECRET <same command>` temporarily selects a sandbox. `COMMIT_TEST_KEY` is equivalent. Every command, including help and failures, ends with the selected sandbox on stderr. Stored selection is overridden by these explicit selectors; unset them to return to production. See [testing](TEST_ENVIRONMENTS.md).

## Background updates

Honeycomb owns CLI installation and updates. Commit never downloads or replaces its executable. Use `commit daemon uninstall` to remove a legacy LaunchAgent or systemd updater. `commit daemon status` reports update ownership. Legacy `daemon install`, `daemon run`, and `config updates on` commands return migration guidance; `--no-update` remains an accepted compatibility flag. The Rust client remains a normal Cargo dependency with no runtime self-update.

## Email and bug reports

```sh
commit email --data '{"email":"you@your-organization.com","enabled":true,"project_completed":true,"task_assigned":true}'
commit email --data '{"email":"you@your-organization.com","enabled":false}'
commit report 'Steps to reproduce; expected result; actual result' --pr https://github.com/teamofsilicons/silicon-commit/pull/123
commit report 'Draft reproduction details' --save-only
```

Email preferences belong to the selected organization and identity. Use the email you use in that organization; Commit never falls back to the personal Carbon address. Reports require a signed-in organization context and queue Postmark delivery to the maintainers. They accept an optional fix PR; `--save-only` writes a private local draft. Do not put tokens or secrets in report text. Test reports are simulated. See [notifications](NOTIFICATIONS.md).

## Creating and assigning todos

Todo creation requires `title` and `assigned_to` strings. Use the recipient's public IAM ID from your team, including the organization suffix for a Silicon.

```sh
commit todos create --data '{"title":"Eat","assigned_to":"alex"}'
commit todos create --data '{"title":"Eat","assigned_to":"assistant:example-org"}'
```

`--data @file.json` accepts the same object. Optional fields are `description`, `status`, `attachments`, and `project_id`; run `commit todos create --help` for types and values. `assignee` and `assignee_id` are not supported. The CLI rejects these fields and missing or non-string required fields before making a request. Other validation remains on the server; a 422 error retains its request ID and points to the command's schema help. Errors go to stderr with a nonzero exit status.

Authenticated commands, including `login status`, automatically rotate the saved session within 60 seconds of access-token expiry. Scoped IAM 5 session files without expiry refresh once; older actorless or unscoped sessions require reauthentication. Concurrent commands serialize rotation; retries after uncertain responses reuse the same key and retain the saved credentials until replacement tokens arrive. Refresh uses only the session's saved server, actor, organization, and selected test environment. An account change during a request stops automatic replay instead of sending the pending operation as the replacement account. An explicit `--token` remains caller-managed.
