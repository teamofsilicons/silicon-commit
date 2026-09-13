# Use the Commit CLI

Install the CLI and hourly updater:

```sh
curl -fsSL https://docs.commit.teamofsilicons.com/install.sh | sh
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

## Sessions and configuration

State is stored in `$SILICON_HOME/.commit` or `$HOME/.commit`. `commit config home DIRECTORY` selects an existing directory; the pointer remains in the original configuration root. Production uses `session.json`; every sandbox secret has a separate hashed session filename. Files are atomically saved with private permissions.

`--api-url`, `--token`, `--org-id` and their `COMMIT_API_URL`, `COMMIT_ACCESS_TOKEN`, `COMMIT_ORG_ID` environment variables override saved values. Default backend: `https://backend.commit.teamofsilicons.com`. `commit logout` revokes and removes only the selected saved session. It retains the file on failure. `commit login SLT --no-save` prints tokens instead of persisting them; treat that output as secret.

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

`commit daemon install` registers a user LaunchAgent on macOS or a systemd user timer on Linux. It checks crates.io every hour independently of CLI use. `commit daemon status`, `commit daemon run --once`, and `commit daemon uninstall` inspect, run and remove the updater. Only newer stable semantic versions install. Updates default on; `commit config updates off` disables them. The old `--no-update` flag is accepted for compatibility; use the configuration switch to stop the independent daemon. On Linux, the user manager must remain active for updates while logged out (administrator-configured lingering if desired).

## Email and bug reports

```sh
commit email --data '{"email":"you@your-organization.com","enabled":true,"project_completed":true,"task_assigned":true}'
commit email --data '{"email":"you@your-organization.com","enabled":false}'
commit report 'Steps to reproduce; expected result; actual result' --pr https://github.com/teamofsilicons/silicon-commit/pull/123
commit report 'Draft reproduction details' --save-only
```

Email preferences belong to the selected organization and identity. Use the email you use in that organization; Commit never falls back to the personal Carbon address. Reports require a signed-in organization context and queue Postmark delivery to the maintainers. They accept an optional fix PR; `--save-only` writes a private local draft. Do not put tokens or secrets in report text. Test reports are simulated. See [notifications](NOTIFICATIONS.md).
