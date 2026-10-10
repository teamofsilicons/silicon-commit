# Commit: Silicon Accounts + Silicon Apps migration progress

Branch `migrate/accounts-apps-20261010` (based on `origin/main` at `acd7a8e`). Each stage appends a dated section:
what it did, commits, test commands with results, what is left, gotchas. Decisions live in
[`decisions.md`](decisions.md).

## 2026-10-10 — Stage 1 (service): baseline

Recorded before any change, from a clean worktree at `acd7a8e`.

Environment: rustc/cargo 1.98.0, PostgreSQL 16.11 on `127.0.0.1:5460` (no Docker), disposable database
`commit_baseline`.

```sh
export CARGO_TARGET_DIR=$PWD/target/mig CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=3
createdb -h 127.0.0.1 -p 5460 -U postgres commit_baseline
COMMIT_TEST_DATABASE_URL=postgres://postgres@127.0.0.1:5460/commit_baseline \
COMMIT_IAM_APP_SECRET=local-test-only-encryption-key-not-an-iam-credential \
  cargo test --workspace --all-targets --locked --no-fail-fast
```

Result: **285 passed, 0 failed, 0 ignored** (exit 0). The PostgreSQL suites really ran (the database had 32 applied
migrations and rows afterwards; nothing printed `skipping`).

| target | passed |
|---|---|
| `silicon_commit` unit tests (src/) | 157 |
| `tests/iam_api_workflow.rs` | 1 |
| `tests/postgres_integration.rs` | 11 |
| `tests/postgres_membership_migration.rs` | 1 |
| `tests/postgres_public_id_migration.rs` | 1 |
| `tests/postgres_subtree_deletion.rs` | 1 |
| `cli/tests/commands.rs` | 26 |
| `client/tests/transport.rs` | 9 |
| vendored `silicon_iam_client` (unit 47, application_contracts 27, canonical_cutover 2, social_signup 2) | 78 |

The vendored IAM client is a workspace member only because it is a path dependency inside the repository; its 78
tests leave with it. Everything else is the baseline later stages compare against.

## 2026-10-10 — Stage 1 (service): Silicon Accounts

The service now signs people in with Silicon Accounts only, has no organizations, keeps every IAM-era value, and
passes every test that passed at the baseline (except those that tested deleted IAM/Honeycomb code).

What changed:

- **Dependencies.** `silicon-accounts-client = "0.4.0"` replaces the vendored `silicon-iam-client` (directory,
  path dependency and its 78 tests removed). `chacha20poly1305`, `rand` and `subtle` went with the testing-environment
  secrets. `deny.toml` ignores RUSTSEC-2023-0071 (Marvin; `rsa` via `jsonwebtoken`), unreachable because Commit only
  verifies EdDSA tokens with public keys (decision A-18).
- **Configuration.** `ACCOUNTS_URL`, `ACCOUNTS_API_URL`, `COMMIT_APP_ID`, `COMMIT_APP_SECRET`,
  `COMMIT_ACCOUNTS_WEBHOOK_SECRET`, `COMMIT_PROOF_ISSUERS`; plain http only for loopback, never in production; IAM,
  Honeycomb and testing variables are ignored with a boot warning. `.env.example`, the AWS bootstrap (and its tests)
  and CI follow.
- **Authentication.** One extractor for every route: Bearer JWT verified locally (JWKS cache, rate-limited refetch on
  an unknown `kid`) or `Proof sap_…` verified online and checked for receiver, scope and allowed issuer.
  Introspection on visibility/member changes and allow-list changes. Sign-outs and removed access refuse earlier
  tokens (`iat` vs a per-account cutoff). IAM routes, sessions, OBO verification, the IAM webhook and testing
  environments are gone; `/api/v1/obo/*` stay one release as aliases.
- **Accounts and the circle.** `commit.accounts` (uuid, kind, id, name, photo, shared email, custodian, status,
  cutoff), the SQL policy functions (`in_circle`, `circle_of`, `project_writable`, `project_access`, `todo_access`,
  `may_reach`), the Silicon allow-list (`/api/v1/silicons/{silicon}/allowed-accounts`), `/api/v1/me` and
  `/api/v1/accounts`. Lookups by id or uuid are cached; malformed or unknown names are 422.
- **Data.** Migration 0033 adds account columns beside every principal column, one unlinked placeholder per IAM
  principal, `commit_private.identity_links`, the webhook inbox, the allow-list, `owner_account`/`deleted_at` on
  projects, `commit.forget_account`, and contract 2. `commit-migrate link-identities --file … [--dry-run]
  [--offline]` / `--plan` re-points rows (decision A-02).
- **Webhook.** `POST /webhook/` verifies `X-Accounts-Signature`, dedupes on `event_id` and applies the six account
  events (A-13, A-14).
- **Cross-app.** No Ting adapter: Commit never delivered through Ting (A-15). No outgoing calls to other apps.
- **Grants, OpenAPI, docs.** Runtime grants and their contract test, `openapi.yaml` (bearer + proof security,
  account schemas, removed org/IAM/testing paths), API/Accounts/projects/notifications/contracts/deployment docs (and
  the bundled CLI copies), README service sections, root `decisions.md` D-056, and this folder's `decisions.md`,
  `cutover.md` and `understanding-proposal.md`.

Bugs found and fixed while testing: an ambiguous `kind` in the email trigger (0033), a trigger condition that read a
field missing on one of its two tables (0033), `link-identities` failing on the migrator connection (enum decoding
without the `commit` search path, and re-enabling triggers with pending constraint events), a 2000-byte limit for
deletion outcomes, malformed account names mapped to 503 instead of 422 (found by the live run), and `--plan`/id lines
matching former sandbox principals (now production only).

Test commands (all on PostgreSQL 16.11 at `127.0.0.1:5460`, fresh databases):

```sh
export CARGO_TARGET_DIR=$PWD/target/mig CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=3
cargo fmt --all --check                                                       # ok
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings # ok
COMMIT_TEST_DATABASE_URL=postgres://postgres@127.0.0.1:5460/commit_service_full \
  cargo test --workspace --all-targets --locked --no-fail-fast                # 174 passed, 0 failed (exit 0)
psql … -f deploy/postgres_runtime_grants.sql && psql … -f tests/postgres_runtime_grants.sql   # both exit 0
cargo deny --all-features check                                               # advisories, bans, licenses, sources ok
npx --yes @redocly/cli@2.49.0 lint openapi.yaml                               # valid; 9 warnings (baseline 19)
python3 deploy/aws/test_bootstrap.py                                          # 8 tests OK
```

| target | baseline | now |
|---|---|---|
| `silicon_commit` unit tests | 157 | 113 (66 removed with IAM, sessions, Honeycomb, testing-environment and IAM-capability code; 22 new) |
| `tests/accounts_api_workflow.rs` (was `iam_api_workflow.rs`) | 1 | 4 |
| `tests/postgres_integration.rs` | 11 | 11 (ported to accounts) |
| `tests/postgres_accounts.rs` (new: circle, allow-list, webhook events, deletion) | – | 4 |
| `tests/postgres_accounts_migration.rs` (new: empty DB, IAM-era upgrade + link-identities, CLI) | – | 3 |
| `tests/postgres_membership_migration.rs` | 1 | 1 |
| `tests/postgres_public_id_migration.rs` (upgrade pinned to ≤ 32) | 1 | 1 |
| `tests/postgres_subtree_deletion.rs` (legacy repair on a ≤ 32 database + account-keyed variant) | 1 | 2 |
| `cli/tests/commands.rs` | 26 | 26 |
| `client/tests/transport.rs` | 9 | 9 |
| vendored `silicon_iam_client` | 78 | removed with the crate |

The HTTP tests sign EdDSA tokens with a test key served from a local Accounts double (correct, wrong `aud`, wrong
`iss`, expired, unknown `kid`, tampered, malformed), deliver webhooks signed with `sign_webhook` (bad signature, stale
timestamp, bad body, duplicate, sign-out, deletion, no secret), and stub proof verification (scope, issuer,
receiver, invalid, App verification, refresh token).

Live run against the shared local Accounts stack (`ACCOUNTS_URL=http://localhost:9590`, Commit's development app
secret and seeded webhook secret, database `commit_live`, API on `127.0.0.1:4141`): a Carbon signed in to `commit`
through the hosted pages (`mint.mts app-signin --exchange --scope email`) and a Silicon through an SLT; `/me` showed
name, photo, shared email and the custodian link; the Silicon created a todo for its custodian and a project; the
custodian read and set the Silicon's notification settings; the fake `interface` app issued a User verification
proof that listed and created todos (`via_app` recorded) and was refused outside its scopes; real `account.updated`
and `account.id_changed` deliveries (fetched from the stack's delivery log) were replayed to `/webhook/` and applied;
completing the delegated todo queued a payload-v3 outbox event that the worker claimed; a Silicon of another Carbon
refused work with `silicon_not_reachable`. Nothing outside Commit's own app and the test identities
`commit-live-258*@example.test` was touched; the stack's webhook URL for Commit was left as it was.

Commits: `c1223f2` Move the Commit service to Silicon Accounts · `f504f4b` Configure deployment for Silicon
Accounts · `cc297f4` Document the Silicon Accounts API contract · then this folder (decisions, cutover, proposal,
progress).

Left for later stages or a Carbon: see `remaining` in the stage result (CLI, client, web, packaging, the production
cutover in [`cutover.md`](cutover.md), the UNDERSTANDING.md proposal, and whether the docs site should publish this
folder).

## 2026-10-10 — Stage 2 (client crate and CLI): Silicon Accounts

`silicon-commit-client` and `silicon-commit-cli` 0.5.0 speak Silicon Accounts and API contract 2 only. Decisions
A-21 to A-39 in [`decisions.md`](decisions.md); the CLI's cutover steps are 15 to 19 in [`cutover.md`](cutover.md).

What changed:

- **Client crate.** Bearer (Accounts access token for `commit`) or `with_proof(sap_…)` credentials; `accounts()`,
  `me()`, the Silicon allow-list, `notification_settings_of`/`update_notification_settings_of` for custodians;
  contract 2 negotiation; typed `Error`/`ApiError` keeping `code`, `message`, `hint`, `details`, `request_id`,
  `Retry-After` (foreign bodies never echoed); plain http only for loopback. New `auth::AccountsAuth`: device flow
  (interval, `slow_down`, expiry, transient retries), short-lived token exchange with `client_id` only, rotating
  refresh, revocation, `SignInRefusal` classification, `peek_claims`. Removed: organizations, IAM session routes,
  testing-environment methods, crates.io update checks.
- **CLI.** `login` (device flow; `--scope`, `--open`, `--json` progress), `login --slt|--slt-stdin|<SLT>`,
  `login status [--json] [--offline]`, `logout [--force]`, `accounts [--json]` (+ hidden `iam`), `me`,
  `silicons allowed-accounts|allow|disallow`, `notifications --silicon`. Session v2 in the profile directory
  (0600/0700, atomic), refreshed once under `session.lock`, ended on `invalid_grant`; replay after 401 with the same
  idempotency key and body; old IAM and damaged files reported, never crashed on. Removed: `testing`,
  `test-environments`, `daemon`, `config updates`, `--org-id`, `--test`, `--no-update`. Help pages say what each
  command is for, how it combines, examples with `c:`/`si:` ids.
- **Docs.** START, CLI, CLIENT rewritten; API/PROJECTS without "circle"; CONTRACTS names the 0.5 clients;
  TELEMETRY without testing environments; crate READMEs; the CLI bundles ACCOUNTS and drops IAM and
  TEST_ENVIRONMENTS (`cli/docs` stays identical to `docs/`).
- **Service fix found live** (A-39): `/me` named a Silicon's custodian with an empty id until the custodian used
  Commit; now looked up (cached, not stored). Regression test fails before the fix (shown) and passes after.

Commits: `2389b38` Move the Commit client and CLI to Silicon Accounts · `97ae518` Document the Silicon Accounts CLI
and Rust client · `cb65d7b` Name a Silicon's custodian before the custodian uses Commit · `d2ace3b` Do not store a
custodian learned only from a lookup · then this folder (decisions, cutover, proposal, progress).

Tests (PostgreSQL 16.11 on `127.0.0.1:5460`, fresh databases):

```sh
export CARGO_TARGET_DIR=$PWD/target/mig CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=3
cargo fmt --all --check                                                         # ok
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings   # ok
COMMIT_TEST_DATABASE_URL=postgres://postgres@127.0.0.1:5460/commit_cli_full \
  cargo test --workspace --all-targets --locked --no-fail-fast                  # 194 passed, 0 failed, nothing skipped
cargo test -p silicon-commit-client --doc                                       # 1 passed
cargo deny --all-features check                                                 # advisories, bans, licenses, sources ok
cargo package -p silicon-commit-client                                          # builds from the packaged crate
cargo package -p silicon-commit-cli --list --allow-dirty                        # sources, docs, tests, README
npm ci --prefix docs-site && npm run build --prefix docs-site && npm run check --prefix docs-site
                                                                                # 26 pages, 633 local links ok
for g in cli/docs/*.md; do cmp "$g" "docs/$(basename "$g")"; done               # identical (CI check)
```

| target | before (service stage) | now |
|---|---|---|
| service (unit + HTTP + PostgreSQL suites) | 139 | 140 (+ `me_names_a_custodian_that_never_used_commit`) |
| `client/tests/transport.rs` | 9 (IAM-era) | 9 (rewritten: credentials, proofs, contract 2, envelope, URLs) |
| `client/tests/auth.rs` (new: stub Accounts) | – | 7 (device pending/slow_down/denied/expired/transient, SLT ok + every refusal, refresh + reuse, revoke, URL rules, claims) |
| CLI unit tests (new: storage classes, atomic 0600 writes, exclusive lock, URL comparison, argument parsing, clap tree) | – | 8 |
| `cli/tests/discovery.rs` (golden `accounts --json`, `iam` alias, signed-out status, help tree, retired flags, guides) | – | 5 |
| `cli/tests/login.rs` (device flow pending→slow_down→success, denied, expired; SLT via stdin/flag/positional; refusals; replace + revoke previous; `--no-save`) | – | 6 |
| `cli/tests/sessions.rs` (single-flight refresh across 2 processes, 401 replay, uncertain refresh, reuse ends session, logout + `--force`, legacy/damaged files, profiles, `signed_in_elsewhere`, offline, `--token`, custodian id) | – | 11 |
| `cli/tests/commands.rs` (sign-in required, todo validation, 422 details, filters/selectors/allow-list, email hint, reports, homes) | 26 (IAM-era) | 8 |

Live run against the shared stack (`ACCOUNTS_URL=http://localhost:9590`) and the migrated service on
`127.0.0.1:4141` (database `commit_cli_live`, `.mig/cli-live-env.sh` = the service stage's development settings),
identities `commit-cli-c1-126@example.test`/`si:commit-cli-s1-126` and `commit-cli-c2-127@example.test`/
`si:commit-cli-s2-127`. Outputs trimmed only where marked.

```text
$ mint.mts silicon --custodian-email commit-cli-c1-126@example.test --handle commit-cli-s1-126
{'uuid': 'C66', 'id': 'si:commit-cli-s1-126', 'kind': 'silicon', 'stk': '<hidden>', 'custodian': {'uuid': '0Nn', 'id': 'c:commit-cli-c1-126'}}
$ SLT=$(mint.mts slt --silicon si:commit-cli-s1-126 --stk … --app commit)        # slt_… (47 chars)
$ printf %s "$SLT" | commit login --slt-stdin                                     # fresh SILICON_HOME and HOME
Signed in to Commit as si:commit-cli-s1-126 (commit-cli-s1-126), a Silicon. Custodian: c:commit-cli-c1-126.
uuid          C66
verified      yes, the Commit API accepted it                                     (rows trimmed)
SLT occurrences in stdout, stderr and session.json: 0, 0, 0
$ commit login status --json
{"accounts_url":"http://localhost:9590","api_url":"http://127.0.0.1:4141","authenticated":true,
 "custodian":{"id":"c:commit-cli-c1-126","uuid":"0Nn"},"display_name":"commit-cli-s1-126",
 "expires_at":"2026-10-10T03:14:30Z","id":"si:commit-cli-s1-126","kind":"silicon","profile":"default",
 "refresh_expires_at":"2029-03-28T02:44:30Z","uuid":"C66","verified":true}           (exit 0; before the A-39
                                                                                    fix the custodian id was "")
$ commit todos create --data '{"title":"CLI live: review the release","assigned_to":"c:commit-cli-c1-126"}'
{"assigned_by":{"id":"si:commit-cli-s1-126","type":"silicon","uuid":"C66"},
 "assigned_to":{"id":"c:commit-cli-c1-126","type":"carbon","uuid":"0Nn"},"status":"yet_to_do",…}
$ commit todos list --view delegated_by_me                                        # count 1, the todo above
$ commit projects create --data '{"name":"CLI live project 126",…,"tasks":[{"title":"Draft","assigned_to":"c:commit-cli-c1-126"}]}'
{"id":"01a123b5-85a4-…","owner":{"id":"si:commit-cli-s1-126","type":"silicon","uuid":"C66"},"private":false,…}
$ commit me                    # {"uuid":"C66",…,"custodian":{"id":"c:commit-cli-c1-126","type":"carbon","uuid":"0Nn"}}
# forced refresh through the real token endpoint: expires_at set to 1, then
$ commit todos list --view all # items: 2; refresh token rotated: True, access token valid for 1800 s, marker cleared
$ printf %s "$SLT" | commit login --slt-stdin --json                              # the same SLT again
{"error":{"code":"invalid_grant","reason":"already_used","message":"The short-lived token was already used; each one
 works once. Get a new one.","hint":"Sign in again. … `silicon-accounts login --app commit -q | commit login
 --slt-stdin`.","request_id":"01a123b5-bbd3-…","status":400}}                      (exit 1, nothing saved)
$ printf %s "$REMIND_SLT" | commit login --slt-stdin                              # minted for app remind
commit: The short-lived token was issued for the app 'remind', not for 'commit'; … (HTTP 400, invalid_grant, request ID …)
$ commit logout --json         # {"id":"si:commit-cli-s1-126","revoked":true,"signed_out":true,"uuid":"C66"}
$ commit login status --json   # {"authenticated": false}
# the revoked refresh token at Silicon Accounts: invalid_grant "…revoked at 2026-10-10T02:48:03.568Z (app_revoked)…"

$ commit login --json &        # Carbon, fresh SILICON_HOME; stderr:
{"browser_opened":false,"event":"device_code","expires_at":"2026-10-10T02:58:16Z","expires_in":600,"interval":5,
 "user_code":"JYN9-6F64","verification_uri":"http://localhost:9590/device",
 "verification_uri_complete":"http://localhost:9590/device?code=JYN9-6F64"}
$ mint.mts approve --email commit-cli-c1-126@example.test --code JYN9-6F64      # {"approved":"JYN9-6F64","status":204}
# commit login finished; stdout:
{"authenticated":true,"display_name":"Commit Cli C1 126","id":"c:commit-cli-c1-126","kind":"carbon",
 "method":"device","uuid":"0Nn","verified":true,…}
$ commit todos list            # the Silicon's two todos (assigned_by si:commit-cli-s1-126)
$ commit notifications --silicon si:commit-cli-s1-126     # custodian reads its Silicon's settings (version 0)
$ commit silicons allowed-accounts si:commit-cli-s1-126   # {"allowed":[],"silicon":{…"uuid":"C66"}}
$ commit email                 # saved false, shared_email null, then on stderr:
No email is shared with Commit yet. Set one with `commit email --data '{"email":"you@example.com"}'`, or sign in
again and share your email: `commit login --scope email`.

# second pair after A-39 (not storing lookup-only custodians):
$ commit login --scope email   # text mode; approved with mint.mts approve
Signed in to Commit as c:commit-cli-c2-127 (Commit Cli C2 127), a Carbon.
$ commit login status --json   # display_name "Commit Cli C2 127", verified true (kept after /me)
$ commit email                 # "shared_email": "commit-cli-c2-127@example.test"
$ commit logout                # Signed out …; the sign-in was ended at Silicon Accounts.

# discovery in an EMPTY HOME and SILICON_HOME:
$ commit --help                # exit 0, 92 lines
$ commit accounts --json       # {"accounts_url":"https://accounts.teamofsilicons.com","api_url":
                               #  "https://backend.commit.teamofsilicons.com","app_id":"commit",…,"version":"0.5.0"} exit 0
$ commit login status --json   # {"authenticated": false} exit 0; the home stayed empty
```

Nothing outside Commit's app and the test identities above was touched; Commit's sign-in setup and webhook on the
stack were only read (`device_flow` and `public_client` were already on). All processes this stage started are
stopped (`.mig/pids/cli-*`); its databases `commit_cli_live`, `commit_cli_test` and `commit_cli_full` are dropped.

Found, not fixed here (for the fix or e2e stage): an account row made from a lookup (someone assigned it a todo or
invited it before it used Commit) carries `refreshed_at = now`, so that account's own sign-in within ten minutes skips
`userinfo` and Commit misses its display name (lookups carry none) and shared email until the next refresh. A
separate "refreshed from userinfo" marker, or treating never-self-refreshed rows as stale, would fix it.

Left for later stages: packaging (`apps.yaml`, release workflow, `honeycomb.yaml` removal; the binary already answers
the three discovery commands in an empty home), moving `docs/IAM.md`, `HONEYCOMB.md`, `TEST_ENVIRONMENTS.md`,
`RELEASES.md`, `install.sh` and the IAM-era notes to `docs/history/` with the docs-site navigation, the root README's
CLI section, and publishing the crates (client first). When `silicon-accounts-client` releases
`exchange_slt_public_client` and a public revoke for app ids, `auth` can drop its two direct calls (A-21).

Gotchas: the harness refuses `rm -rf "$VAR"`; use fresh `mktemp -d` homes instead. PostgreSQL client tools are in
`/opt/homebrew/opt/postgresql@16/bin`. The device-flow integration test takes ~8 s because RFC 8628 adds 5 s after
`slow_down`. The local stack's issuer is `http://localhost:9590`, which also serves the API used by the CLI.

Addendum (same stage, after the record above): `fbecc6b` Answer login status even when the session cannot be read
(`--json` now also exits 0 for an unreadable session path, `reason: io_error`, and for `--token` with an unusable
`COMMIT_API_URL`, `verified: false` + warning; the new assertions fail without the fix, shown) and `f8da0bc` Keep
retired words out of the client's and CLI's doc comments. Re-run: `cargo test -p silicon-commit-client -p
silicon-commit-cli` 54 passed + 1 doctest, workspace clippy clean, `cargo fmt --all --check` ok.

## 2026-10-10 — Stage 3 (packaging, CI, deployment, documentation): Silicon Apps

Everything around the code now says and does Silicon Accounts and Silicon Apps. Nothing was pushed, released,
uploaded or deployed; a release is one tag away and each production step is one reviewed command in
[`cutover.md`](cutover.md). Decisions A-40 to A-54 in [`decisions.md`](decisions.md); root `decisions.md` D-057.

What changed:

- **Packaging.** `packaging/apps.yaml.in` and `scripts/package-apps.sh VERSION TARGET BINARY` (Python behind a bash
  wrapper, `scripts/package_apps.py`): one target per archive, `bin/commit[.exe]`, native-binary check per target
  (glibc ≤ 2.28 for Linux; the ABI checker now reads ELF32 for linux-i686/armv7hf), the three discovery commands
  with Silicon Apps' own pass rules in an empty home (plus `--version` = manifest version), `silicon-apps validate`
  and `pack` with an empty packer home, archive members checked byte for byte and validated again, `.sha256`.
  Receipts carry a discovery run from the target's own machine to the packer (`--receipt`, `--discovery
  require`); `version` checks manifests and tag; `checksums` writes SHA256SUMS. `build-release-macos.sh` packs each
  target the same way. Deleted: `honeycomb.yaml`, `scripts/package-release.py`, `docs-site/release.py`,
  `.github/workflows/release-package.yml`.
- **CI.** `release.yml` (tags `v*` and by hand): version/tag check, the six existing targets on the same runners and
  toolchains, discovery natively on each runner and on Linux again in the glibc 2.28 Debian image with no network,
  one packaging job (`cargo install --locked silicon-apps-cli --version 0.2.0`), artifact
  `commit-silicon-apps-release` (archives, `.sha256`, `SHA256SUMS`, `SOURCE_REVISION`); publishes nothing. `ci.yml`:
  a `package` job (Linux CLI for glibc 2.28, packed with discovery required) and the tools job runs the packager,
  ABI, bootstrap, cutover and host tests. No IAM env or Honeycomb step remains; the Postgres service container was
  already there.
- **Deployment.** `deploy/aws/host.py` (SSM copy/run, SHA-256-checked copies under `/opt/commit` only) and
  `deploy/aws/cutover.py` (on the host: `queues`, and `link-identities` plan/dry-run/apply in the deployed image with
  a root-only env file removed even on failure). The AWS README describes the current procedure. Caddy needed no
  change (it proxies `/webhook/` with everything else; the API sets no CSP; the Next.js web's CSP comes from the web
  kit). `cutover.md` rewritten as the full ordered runbook.
- **Docs.** New `docs/RELEASES.md`; `install.sh` installs with Silicon Apps at the same address; START/DEVELOPMENT
  (and their CLI copies), README top, README/OpenAPI wording ("the accounts close to" instead of "circle", no
  "organizations"). IAM/Honeycomb-era records moved to `docs/history/` unchanged with an index; the three retired
  stubs removed. Docs site: excludes `docs/history` and `docs/migration`, redirects the removed pages' addresses,
  new navigation, contract 2 footer, generic preview banner, fails on a missing nav/redirect page.
- **Service wording.** The retired `X-Org-ID` refusal, the contract 1 refusal and `/api/v1/contracts` no longer name
  the previous identity service or organizations; three stale doc comments fixed.
- **Records.** `understanding-proposal.md` gains the Updates/docs/wording paragraphs; decisions A-40 to A-54; D-057.

Commits: `dbdc046` Package the CLI for Silicon Apps instead of Honeycomb · `631c622` Build Silicon Apps archives in
the release workflow · `e867598` Prepare the production cutover for Silicon Accounts and Silicon Apps · `5ae8416`
Document releasing and installing through Silicon Apps · `f346f79` Keep retired words out of the service's own
messages · then this folder (decisions, proposal, progress).

### Tests and proofs

```sh
export CARGO_TARGET_DIR=$PWD/target/mig CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=3
cargo fmt --all --check                                                        # ok (after rustfmt on one line)
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings  # ok
COMMIT_TEST_DATABASE_URL=postgres://postgres@127.0.0.1:5460/commit_ship_full \
  cargo test --workspace --all-targets --locked --no-fail-fast                 # 194 passed, 0 failed, none skipped
cargo test -p silicon-commit-client --doc --locked                             # 1 passed
python3 scripts/test_package_apps.py                                           # 20 tests OK
python3 scripts/test_linux_abi.py                                              # 8 tests OK (2 new: ELF32, bad class)
python3 deploy/aws/test_bootstrap.py; python3 deploy/aws/test_cutover.py; python3 deploy/aws/test_host.py  # 8, 7, 4 OK
npm ci --prefix docs-site && npm run build --prefix docs-site && npm run check --prefix docs-site
                                     # 13 pages built; 22 pages (with 9 redirects) and 354 local links verified
for g in cli/docs/*.md; do cmp "$g" "docs/$(basename "$g")"; done             # identical
npx --yes @redocly/cli@2.49.0 lint openapi.yaml                                # valid, 9 warnings (as before)
python3 (PyYAML 6.0.3) parse + structural lint of all workflows                # no problems (needs, matrix keys, outputs)
psql … -c "$(cutover.py QUEUES)" on a migrated scratch database                # runs; (0 rows)
```

Mutation checks on the packager tests (each reverted): accepting `login status` JSON without
`"authenticated": false` (survived at first; two cases added, now caught), accepting `accounts --json` with any
`app_id`, dropping the `--version` check, skipping the receipt check: all fail the suite.

Packaging proof on this Mac (silicon-apps 0.2.0 from `~/.apps/bin`, used only for the local `validate`/`pack`
with an empty scratch `--home`):

```text
$ cargo build --locked --release -p silicon-commit-cli --bin commit                 # 59.8 s, 5,992,704 bytes
$ SILICON_APPS=~/.apps/bin/silicon-apps scripts/package-apps.sh 0.5.0 macos-aarch64 target/mig/release/commit --discovery require
packaged …/dist/apps/commit-0.5.0-macos-aarch64.tar.gz (2808091 bytes)
sha256 09cbc11771e4ed5563d1587600a5506829e11db24ab2c47783b31fee294dc00b
discovery ran here
$ tar -tvzf dist/apps/commit-0.5.0-macos-aarch64.tar.gz
-rw-r--r--  0 0      0         113  1 Jan  1970 apps.yaml
-rwxr-xr-x  0 0      0     5992704  1 Jan  1970 bin/commit
$ silicon-apps validate EXTRACTED_DIR --home EMPTY --json      # {"errors": [], "manifest": {… "macos-aarch64": {"binary": "bin/commit"}}, "version": "0.5.0"}, "valid": true}
$ silicon-apps validate dist/apps/commit-0.5.0-macos-aarch64.tar.gz --home EMPTY --json   # "valid": true
# from the extracted archive, env -i PATH=/usr/bin:/bin HOME=EMPTY SILICON_HOME=EMPTY:
$ bin/commit --help                 # exit 0, 92 lines
$ bin/commit accounts --json        # exit 0: {"accounts_url": "https://accounts.teamofsilicons.com", "api_url": "https://backend.commit.teamofsilicons.com", "app_id": "commit", … "version": "0.5.0"}
$ bin/commit login status --json    # exit 0: {"authenticated": false}
# files left in the empty home: 0
$ cargo zigbuild --locked --release -p silicon-commit-cli --bin commit --target x86_64-unknown-linux-gnu.2.28   # 53 s (cargo-zigbuild 0.23.4, Zig 0.15.2)
$ python3 scripts/check_linux_abi.py target/mig/x86_64-unknown-linux-gnu/release/commit   # requires glibc 2.28 (maximum 2.28)
$ scripts/package-apps.sh 0.5.0 linux-x86_64 …/commit --discovery require
package-apps: the discovery commands are required, but this machine cannot run the linux-x86_64 binary (…); pass the receipt from `discover --receipt-out` on a linux-x86_64 machine
$ scripts/package-apps.sh 0.5.0 linux-x86_64 …/commit --discovery require --receipt <macos receipt>
package-apps: the discovery receipt … is for other target, sha256 (…)
$ scripts/package-apps.sh 0.5.0 linux-aarch64 …/commit     # the ELF executable is not built for linux-aarch64
$ scripts/package-apps.sh 0.5.0 linux-x86_64 …/commit      # auto: note, then packaged (sha256 0aebd970…), "validation worker runs them at upload"
$ python3 scripts/package_apps.py checksums dist/apps --version 0.5.0 --expect linux-x86_64 linux-aarch64
package-apps: commit-0.5.0-linux-aarch64.tar.gz is missing from dist/apps; every release target must be packed
$ (cd dist/apps && shasum -a 256 -c SHA256SUMS)              # both OK
```

The release workflow's hand-off was replayed locally for macos-aarch64: build-job receipt, a copy without the exec
bit (as `download-artifact` delivers it), packaging with `--discovery require --receipt`: the same archive digest
`09cbc117…` as the direct run (packing is deterministic).

### Sweep

`git grep -n -i -E 'iam|honeycomb|org_id|organi[sz]ation|\borg\b|tenant'` (106 files; no `vendor/` left). Every
remaining hit is intentional:

| where | why it stays |
| --- | --- |
| `migrations/0001`–`0032` | applied migrations are immutable history (sqlx checksums them) |
| `migrations/0033_silicon_accounts.sql` | keeps IAM-era columns as provenance, creates `iam:` placeholders, re-keys, drops org-qualified keys |
| `src/infrastructure/postgres/identity_links.rs`, `src/bin/commit_migrate.rs` | the operator command that links IAM-era principals (`iam_principal_id,accounts_uuid,org_id`) |
| `src/domain/ids.rs`, `src/domain/actor.rs` | the `iam:<organization>:<principal>` placeholder ids and their test |
| `src/api/auth.rs`, `src/api/mod.rs` | the retired-header list (`x-org-id`, `x-iam-obo-*`) refused with 400, the OBO-alias comment, the test that retired routes are gone |
| `src/api/contracts.rs`, `src/application/{authorization,scopes}.rs`, `src/infrastructure/clients/webhook.rs`, `src/infrastructure/postgres/todos.rs` | doc comments stating what changed (scope ids kept from the IAM era, payload v3 without `org_id`) and a test asserting `org_id` is absent |
| `src/config.rs` | the retired variables boot warns about |
| `cli/src/{main,api,login}.rs` | the hidden `commit iam --json` alias (brief: one minor release) |
| `cli/src/session.rs`, `cli/tests/sessions.rs` | recognising IAM-era session files (`org_id`, `actor`) to answer `legacy_session` |
| `cli/tests/discovery.rs` | asserts help never shows retired words, and the hidden alias |
| `tests/*.rs`, `tests/*.sql` | IAM-era fixtures for the upgrade tests; grant assertions that runtime roles cannot read IAM-era objects; assertions that new rows carry no organization |
| `deploy/postgres_runtime_grants.sql` | comments on the revoked IAM-era objects |
| `deploy/aws/bootstrap.py`, `test_bootstrap.py`, `test_cutover.py` | retired secret keys that are never copied, and tests proving it |
| `deploy/aws/cutover.py` | the mapping header `iam_principal_id,accounts_uuid[,org_id]` in an error message |
| `deploy/aws/edge.json` | AWS IAM (the EC2 instance role), unrelated to Silicon IAM |
| `docs-site/build.mjs` | old addresses (`/iam/`, `/honeycomb/`, `/iam5-…/`) redirected to the pages that replaced them |
| `docs/history/**` | historical records, kept unchanged |
| `docs/migration/**` | this migration's notes (D9 allows them here) |
| `decisions.md` | the append-only engineering log; D-056 and D-057 supersede |
| `UNDERSTANDING.md` | Carbon-only; changes proposed in `understanding-proposal.md` |
| `frontend/**` | the previous SolidJS web and its gateway (IAM popup, `X-Org-ID`): the web stages replace it with the Next.js web and delete `frontend/`; it is not rewritten here and must not be deployed with 0.5.0 (README says so) |

User-facing copy also checked for "circle", "AI agent", "human", "user account", "team": none in current docs, help or
OpenAPI.

### Left for later stages or a Carbon

- **Web stages**: replace `frontend/` (and its CI job) with the Next.js web; then update the README's web line, the
  web env names in cutover step 5 if `web/.env.example` differs from the kit's (`APP_ID`, `APP_SECRET`,
  `ACCOUNTS_URL`, `APP_API_URL`, `SESSION_SECRET`, `PUBLIC_URL`), and the Vercel root directory.
- **E2E stage**: scenario 7 can use `scripts/package-apps.sh 0.5.0 macos-aarch64 target/mig/release/commit` and run
  the three commands from the extracted archive (shown above).
- **Production** (a Carbon, by the runbook): tag `v0.5.0`, image, sign-in setup, webhook secret, deployment secret,
  drain, bootstrap, link-identities, web, Interface, docs, Silicon Apps upload/release/promote (Linux first), crates.
- Not run here: the release workflow itself (needs GitHub; nothing may be pushed), the Linux discovery commands
  (no Docker or Linux on this Mac; CI runs them natively and in the glibc 2.28 image), actionlint (not installed;
  PyYAML parse plus a structural check instead).

Blocked on: nothing in this repository. Interface's proof release and the Silicon runtime's switch to Silicon
Accounts tokens are outside it (cutover steps 15 and 24).

Gotchas: zsh treats `echo ======` as a command lookup; `bash -n a.sh b.sh` checks only `a.sh` (CI loops);
an unquoted YAML scalar must not contain `: `; `upload-artifact` drops the exec bit (the packager chmods its staged
copy); `commit-migrate`'s tracing layer writes JSON logs to stdout, so `cutover.py` sets
`COMMIT_LOG=silicon_commit=warn` (in practice `--plan` printed only the CSV); `silicon-apps validate` also accepts an
archive path.

## 2026-10-10 — Stage 4 (end to end against Silicon Accounts)

The migrated service and CLI were run against the shared local Silicon Accounts stack (`http://localhost:9590`) with
real Carbons and Silicons. The eight scenarios are now a script; the runs found three defects and a fourth while
fixing one of them, all fixed with regression tests that fail without the fix. Decisions A-55 to A-64 in
[`decisions.md`](decisions.md). Nothing outside Commit's app and the `commit-e2e-*` test identities was changed on
the stack; Commit's webhook URL there was pointed at the local API during runs and put back afterwards.

### Setup: `scripts/dev-accounts.sh` and `scripts/dev-accounts-stop.sh`

`scripts/dev_accounts.py` (behind the two wrappers): migrates `commit_e2e` on `127.0.0.1:5460`, starts
`commit-api` (`127.0.0.1:4141`) and `commit-worker` with a clean environment, points Commit's webhook at the stack
(`PUT /v1/apps/commit/webhook`, Commit's own credentials), proves a test delivery, and rotates the secret only if
the test is refused. State in `.mig/` (`pids/api`, `pids/worker`, `logs/`, `webhook-secret` 0600,
`webhook-previous-url`); `.mig/` is now in `.gitignore`. `restart`, `status` and `env` subcommands; `--build`
rebuilds and restarts, `--fresh` recreates the database. First start (real output, trimmed):

```text
$ COMMIT_TEST_STACK=…/test-stack.json scripts/dev-accounts.sh --fresh
created database commit_e2e
migrations applied to commit_e2e
Commit's webhook now posts to http://127.0.0.1:4141/webhook/ (was http://127.0.0.1:9593/commit/webhooks)
{"api": {"pid": 85176, "url": "http://127.0.0.1:4141", …}, "worker": {"pid": 85177, …}, "database": "commit_e2e",
 "accounts_url": "http://localhost:9590", "accounts_api_url": "http://127.0.0.1:9589",
 "webhook_url": "http://127.0.0.1:4141/webhook/", "proof_issuers": "commit.todos.list=interface,…(14 actions)",
 "started": ["api", "worker"], "webhook_test": "delivered"}
$ scripts/dev-accounts-stop.sh
{"stopped": ["api", "worker"], "webhook_restored_to": "http://127.0.0.1:9593/commit/webhooks"}
```

The seeded secret from the stack file was still the stack's (no rotation was needed in any run).

### Scenarios: `scripts/e2e-accounts.sh`

`tests/e2e/accounts_e2e.py` runs the real binaries; identities come from `tests/e2e/mint.mts` (the testkit's sign-in
pages and development mail, `SILICON_ACCOUNTS_DIR`), the Silicon's own actions from the stack's `silicon-accounts`
CLI. It starts the stack when the API is not running and stops what it started. Each run writes
`.mig/e2e/run-<n>/result.json` and a transcript with tokens, STKs, proofs and secrets redacted (checked: no match
for JWT/`sar_`/`slt_`/`whsec_`/`sa_app_` patterns).

| # | what runs (all against the stack, real tokens) |
| --- | --- |
| 1 | `mint app-signin --app commit --email commit-e2e-c1-<n>@example.test --redirect http://localhost:4140/auth/callback --exchange`; `/me` (uuid, id, shared email); todo create (and idempotent retry), list, read, update, note, delete (then 404); project create, update, read by UID; email settings default to the shared address, saved, project completed, the worker picks up the email for that address; `X-Org-ID` 400, contract 1 406, contract 2 200; no token, the account-site token (wrong audience) and a tampered token 401 |
| 2 | `mint silicon` + `mint slt` → `commit login --slt-stdin` in a fresh home (SLT nowhere in output or session file, file 0600) → `login status --json` (uuid, id, kind, custodian, verified) → `todos create` for the custodian, `projects create` with a task, `projects tasks`, `todos add-note`, `todos list --view delegated_by_me`, `me` → `logout --json` (revoked) → `{"authenticated": false}`; then positional `commit login <SLT>` and logout again |
| 3 | `commit login --json` (device code on stderr) → `mint approve` → signed in as the Carbon → `todos list` shows its Silicon's todo; access token expired by hand → the next command refreshes at Silicon Accounts and saves the rotated pair |
| 4 | Carbon named by another before it used Commit: its first `/me` shows its name and shared email; custodian reads and changes its Silicon's project and todo; the custodian's other Silicon reads the public project (403 `project_not_writable` on change); an unrelated Carbon gets 404 until shared by `c:` id, can change it as a member, 404 again after unsharing; private project hidden from the other Silicon, visible to the member's custodian; the unrelated Carbon cannot assign or invite the Silicon (403 `silicon_not_reachable`) or allow itself (403 `not_custodian`), the same-custodian Silicon can; the custodian allows the Carbon → work assigned → the Silicon removes it from its list → refused again, earlier work stays visible |
| 5 | custodian changes the Silicon's id (`POST /v1/me/silicons/{uuid}/id`) → Commit shows it; the stack replays the delivery and the same body is posted again → applied once; `PATCH /v1/me` display name → shown; forged (other secret, unsigned, 10-minute-old, not an event) → 401/400, not applied; custodian transfer (`…/transfer` + accept) → the new custodian sees the Silicon's todo, the former one 404; the Silicon signs in to the local `silicon-accounts` CLI, `silicon-accounts login --app commit -q \| commit login --slt-stdin`, then `silicon-accounts apps remove commit` → its earlier token 401 `session_ended`, `commit login status --json` false, commands say `session_ended`; STK rotation → `membership.signed_out` reason `stk_rotated` → old token refused, a sign-in with the new STK works at once; the web's sign-out (refresh token revoked) → reads still pass (local verification), a visibility change 401 `token_revoked`; a Silicon deleted by its custodian → its token 401 `account_deleted`, its project passes to the remaining member, its own todo 404 |
| 6 | `interface` issues User verification proofs (subject token from `mint app-signin --app interface --exchange`): list and create todos as the Carbon (`via_app` in the todo history), the `/api/v1/obo/todos/list` alias; 403 `proof_scope_missing`, 403 `proof_issuer_not_allowed` (`commit.projects.create` is not in the allowed list), 401 `proof_invalid` (revoked; for receiving app `remind`), 401 `proof_without_account` (App verification), 401 `proof_malformed` (a `sapr_` token); a refreshed proof works; revoked after use → refused once the 30-second cache has passed |
| 7 | release CLI → `scripts/package-apps.sh 0.5.0 macos-aarch64 … --output-dir … --discovery require` → archive holds `apps.yaml` + `bin/commit` (one target) → in an empty `HOME`/`SILICON_HOME`: `--help` exit 0, `accounts --json` (`app_id` commit, production URLs, version 0.5.0), hidden `iam --json` identical, `login status --json` = `{"authenticated": false}`; the home stays empty |
| 8 | before/after `dev_accounts.py restart` (new pid): the same access token works, the CLI's saved session works, an event id seen before is acknowledged but not applied (also the scenario 5 id change), the Silicon that removed Commit stays refused, a forged delivery is refused |

Scenario 6's issuer half does not apply: Commit issues no proofs (A-64).

### Bugs found and fixed

1. **A token issued just before a sign-out, in the same second, kept working** (found live, scenario 5). The CLI
   signed in at 04:15:10.193 (`iat` 1791605710) and the Silicon removed Commit at 04:15:10.219; the cutoff check
   compared `iat` with the floored cutoff, so after the removal:
   ```text
   FAIL after: commit login status --json says signed out (exit 0): {… "authenticated": true, "id":
   "si:commit-e2e-s1x-605702", "kind": "silicon", … "verified": true}
   ```
   Fix `6c1b4ab` (A-59): a token from the cutoff's own second is introspected. Regression test
   `a_token_from_the_cutoffs_own_second_is_decided_by_silicon_accounts`, on the old comparison:
   `Error: a token from before the removal, in the same second: 200 OK {…}`; with the fix: ok.
2. **An "active" answer cached for 30 s let a change widen access after a sign-out** (found live, scenario 5).
   C2's token was introspected during an allow-list change in scenario 4; after the web-style sign-out its
   visibility change was not refused (`HTTP 404` from the project lookup instead of `401 token_revoked`).
   Fix `777aa8b` (A-60): widening changes never reuse a cached positive answer (introspection or proof
   verification); documented in ACCOUNTS.md. Regression test `changes_that_widen_access_never_rest_on_a_cached_answer`,
   with the cache: `Error: the second change asked again: 200 OK {…}`; with the fix: ok.
3. **An account first named by someone else had no name or shared email at its own first sign-in** (found live with
   a probe; the CLI stage had flagged it). After `c1` assigned `c3` a todo:
   ```text
   row after lookup:  | <null> | 2026-10-10 09:35:43.165721+05:30      (display name, email, refreshed_at)
   c3 /me: 200 {"id": "c:commit-e2e-c3-605142", "display_name": "", "email": null}
   ```
   Fix `6af47a3` (A-58): a lookup-only row (`accounts_version` 0) is refreshed from `userinfo` on the account's
   first bearer request. Regression test `an_account_named_before_it_used_commit_reads_its_own_view_at_sign_in`,
   before: `Error: {… "email":null …}`; after: ok. Also covered live in scenario 4.
4. **A lookup answered from the cache could undo a newer webhook event** (found reading the code while fixing 3).
   `remember` stamped every stored lookup `refreshed_at = now`, so a lookup cached before a custodian transfer or an
   id change, stored after it, restored the old custodian or id and made Commit ignore the event that followed.
   Fix `6af47a3` (A-57): `observed_at` on resolved accounts; rows take only newer data. Regression test
   `a_lookup_answered_from_the_cache_never_undoes_a_newer_event`, before: panics at the "custodian is now"
   assertion (the transfer event was ignored); after: ok.

Looked at and left as is (recorded): concurrent duplicate deliveries of one event are harmless because every
handler is idempotent (A-61); email stays opt-in as in the IAM era, the default-on question is in the
UNDERSTANDING proposal (A-62); ids of accounts that never signed in can go stale because Silicon Accounts sends no
events for them (A-63).

### Final run (real output, trimmed to section headers and a few checks)

```text
$ . .mig/e2e.env && scripts/e2e-accounts.sh          # stack stopped beforehand; the script starts and stops it
migrations applied to commit_e2e
Commit's webhook now posts to http://127.0.0.1:4141/webhook/ (was http://127.0.0.1:9593/commit/webhooks)
{… "started": ["api", "worker"], "webhook_test": "delivered"}
== scenario 1: Carbon on the API
  c1 is c:commit-e2e-c1-606857 (…)
  ok  /me shows the email the Carbon shared with Commit
  ok  the retry returns the same todo
  ok  read the deleted todo -> 404
  ok  the email goes to the shared address (Postmark is not configured here, so it stays queued)
  ok  contract 1 -> 406 unsupported_contract
  ok  the Carbon's account-site token (another audience) -> 401 token_wrong_audience
== scenario 2: Silicon on the CLI
  ok  the short-lived token appears nowhere
  ok  login status says who is signed in
  ok  logout ended the sign-in at Silicon Accounts
  ok  login status says exactly {"authenticated": false}
  ok  commit login <SLT> (the form the Silicon runtime runs) signs in
== scenario 3: device flow
  ok  the Carbon approved the code
  ok  the CLI is signed in as the Carbon
  ok  the CLI refreshed at Silicon Accounts and saved the rotated pair
== scenario 4: circle and sharing
  ok  its first sign-in shows its own name and the email it shared
  ok  but cannot change it without being a member -> 403 project_not_writable
  ok  the Carbon cannot read it any more -> 404
  ok  an unrelated Carbon cannot assign the Silicon a todo -> 403 silicon_not_reachable
  ok  work already assigned stays visible to the Carbon -> 200
== scenario 5: webhooks
  ok  Commit shows the Silicon's new id
  ok  the event id was applied once
  ok  signed with another secret -> 401 invalid_webhook_signature
  ok  the former custodian cannot -> 404
  ok  after: its earlier access token is refused -> 401 session_ended
  ok  after: commit login status --json says signed out (exit 0)
  ok  Silicon Accounts said why (stk_rotated)
  ok  a sign-in with the new STK works at once -> 200
  ok  changing who can see a project is checked online -> 401 token_revoked
  ok  its access token is refused -> 401 account_deleted
  ok  the project passed to its remaining member
== scenario 6: proofs
  ok  Commit recorded the acting app in the todo's history
  ok  a scope Commit does not accept from interface -> 403 proof_issuer_not_allowed
  ok  a proof for another receiving app -> 401 proof_invalid|proof_wrong_receiver
  ok  an App verification proof speaks for no account -> 401 proof_without_account
  ok  the revoked proof after the cache window -> 401 proof_invalid
== scenario 7: discovery from a packed archive
  ok  scripts/package-apps.sh 0.5.0 macos-aarch64 packs the CLI
  ok  the hidden commit iam --json prints exactly the accounts object
  ok  commit login status --json exits 0 and says signed out
  ok  the empty home stayed empty
== scenario 8: restart safety
  ok  the API restarted
  ok  after: the same access token works (stateless JWT) -> 200
  ok  after: the CLI's saved session works
  ok  after: the same event id is acknowledged but not applied
  ok  after: the Silicon that removed Commit stays refused -> 401 session_ended
{"stopped": ["api", "worker"], "webhook_restored_to": "http://127.0.0.1:9593/commit/webhooks"}
8/8 scenarios passed; details in …/.mig/e2e/run-606857
```

213 checks in 50 s of scenarios (scenario 6 waits 31 s for the proof cache); its 421-line transcript has no token, STK, proof or secret (grep for each pattern: 0 matches). Earlier full runs `run-606247` (199 checks) and
`run-606606` (213) also passed 8/8. The API log shows each event applied by Commit itself (for example
`account.id_changed | id is now si:commit-e2e-s1x-…`, `silicon.custodian_changed | custodian is now c:…`,
`membership.access_removed | access removed: every earlier sign-in is refused`), and the cleanup's revocations as
`membership.signed_out | one Commit sign-in ended; other sign-ins stay valid` (`app_revoked`).

### Test commands and results

```sh
export CARGO_TARGET_DIR=$PWD/target/mig CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=3
cargo fmt --all --check                                                        # ok
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings  # ok
COMMIT_TEST_DATABASE_URL=postgres://postgres@127.0.0.1:5460/commit_e2e_full \
  cargo test --workspace --all-targets --locked --no-fail-fast                 # 198 passed, 0 failed, none skipped
python3 scripts/test_dev_accounts.py                                           # 9 tests OK (new)
python3 scripts/test_package_apps.py; python3 scripts/test_linux_abi.py        # 20, 8 OK
python3 deploy/aws/test_bootstrap.py; …/test_cutover.py; …/test_host.py        # 8, 7, 4 OK
npm run build --prefix docs-site && npm run check --prefix docs-site           # 13 pages; 22 pages, 354 links ok
for g in cli/docs/*.md; do cmp "$g" "docs/$(basename "$g")"; done             # identical
CI tools steps replayed locally (py_compile of the new scripts, bash -n of the wrappers; workflow parsed with PyYAML)
. .mig/e2e.env && scripts/e2e-accounts.sh                                      # 8/8 scenarios, 213 checks
```

| target | before (stage 3) | now |
| --- | --- | --- |
| `tests/accounts_api_workflow.rs` | 5 | 8 (cutoff second, own view at first sign-in, no cached answer for widening changes) |
| `tests/postgres_accounts.rs` | 4 | 5 (a cached lookup never undoes a newer event) |
| everything else | 185 | 185 |

Commits: `6af47a3` Keep account details from going backwards and read a new account's own view · `6c1b4ab` Refuse
a token from the second of a sign-out once Silicon Accounts ended it · `777aa8b` Check with Silicon Accounts every
time a change widens access · `2eed4a6` Run Commit against a local Silicon Accounts stack, end to end · `7a9efdd`
Cover email defaults, transition aliases and contract checks end to end · `7ec3a99` Record the end-to-end stage
(decisions A-55 to A-64, the UNDERSTANDING proposal's email question, the cutover rehearsal step, this log) ·
Log the end-to-end stage's service decisions in the engineering record (root `decisions.md` D-058, with this
line).

Blocked on: nothing. No defect was found in Silicon Accounts or another app.

Left for later stages or a Carbon: the web stages (the Next.js web and its browser tests); running the packed
Linux archives' discovery commands (CI does it natively and in the glibc 2.28 image); the production cutover per
[`cutover.md`](cutover.md); Interface's proof release (outside this repository); the email default (A-62).

Gotchas: `PUT /v1/apps/{app}/webhook` keeps a stored secret and answers `"secret": null`, so the stack file's seeded
secret is the one to use; app lookups return only uuid, kind, id, status (and a Silicon's custodian), no name or
photo; Silicon Accounts answers `{"valid": false}` to a proof verified by an app that is not its receiver, so with a
real stack Commit says `proof_invalid`, never `proof_wrong_receiver`; the stack allows ten email codes per address in
ten minutes (reuse each Carbon's first-party token for approvals, Silicon creation, id changes and transfers); STKs
look like `stk-<hex>` (hyphen); `python3 -I` hides the user site-packages, so PyYAML is only in
`/usr/local/bin/python3`; zsh has no `PIPESTATUS`.

## Review fixes — 10 October 2026

Closed the todo patch path that silently restored removed private-project members: implicit sharing now runs only when assignment/project changes, with fresh Accounts confirmation and project write authorization. Late profile events cannot revert a newer custodian transfer. Linking rejects different legacy public identities that collapse into one Accounts uuid. JWKS outage retries are bounded for aged caches. Added real PostgreSQL regression coverage for removed-member todo edits, late custodian profile events and ambiguous mappings. Full workspace tests passed except the migration refusal-message assertion; after correcting validation order, all three migration tests passed. `cargo clippy --locked --all-targets -- -D warnings` passed.

Remaining review items include pre-first-use revocation events, stale custodians on ordinary reads, proof access removal, notification defaults/delivery mapping, operator/documentation checks and genuine local Accounts end-to-end validation. The new web directory is currently the shared kit skeleton, not a completed product.

## UUID128 compatibility and checked backfill — 10 October 2026

Added canonical UUID acceptance, an explicit identity-column mapping consumer with transactional dry-run/apply/reapply and a retired-subject fence. Populated clone verification preserved resource IDs, provider paths, native credentials and signed bodies; 307 accounts and 0 encrypted proof grants migrated. See [uuid128.md](uuid128.md) for the cutover sequence and evidence. Production and original checkouts remain untouched.

## Completed backend recovery — 10 October 2026

Migrations 0034–0035 preserve pre-first-use sign-out/access-removal/deletion, refuse proofs after access removal, require confirmed fresh sign-in before restoring access, and bound custody authority to 10 minutes. Late directory responses cannot override newer custody events. Definitive userinfo refusals fail closed. Managed-Silicon reads refresh stale known relationships; SQL grants no authority to expired custody. Default shared email now queues project completion without requiring a saved preference; unlinked legacy delivery jobs stay pending until linked. Input cardinality is checked before repeated directory work, and lookups are deduplicated. Authorization errors no longer echo unsupported schemes, CLI state uses exclusive temporary creation, identity-link applies require credentials or explicit offline mode, and OpenAPI advertises the correct backend/webhook origins.

Verification: complete workspace suite passed after the email-recipient lookup fix (`.mig/lifecycle-workspace.log`); focused Accounts authority 15/15 (`.mig/lifecycle-final.log`), shared-email queue regression 1/1 (`.mig/default-email-test.log`), all-target/all-feature clippy with warnings denied, and real Accounts/API/CLI scenario suite 8/8 (`.mig/e2e/run-619947/result.json`). Subsequent UUID fence and restore-order changes are covered by the focused PostgreSQL authority suite. The stored notification contract remains per-Silicon HTTPS webhooks and Carbon email; recovered prior-agent notes claiming a Ting replacement do not override the existing product contract.

## Completed Next.js product workspace — 10 October 2026

The new `web/` application uses the Accounts/Apps Arc components and hosted Carbon sign-in. It implements Todo creation, assignment, notes, filters, status changes and deletion; private projects with explicit membership, tasks/subtasks, diary edits with conflict preservation, updates/blockers, version history and confirmed permanent completion. Custodians can manage Silicon allow-lists, HTTPS notification destinations and subscriptions, including per-todo settings for the Silicon that delegated that todo. Carbon shared-email preferences, account menu, report flow, public documentation and responsive light/dark layouts are included. Server-side token storage, refresh, CSRF protection and revocation follow the shared web kit. `web/README.md` describes configuration and the active Vercel root; the older Solid frontend remains a reference.

Verification: `pnpm test` 44/44; typecheck, lint and production build passed. The hosted Accounts auth/accessibility suite passed 24/24, covering desktop/phone light/dark navigation and the Todo creation dialog. A real backend product journey exercised Todo and private-project operations, subtasks, diary, versions and completion. A separate real Silicon journey created a Silicon through Accounts, admitted its exchanged token through Commit, and exercised allow-list add/remove, notification save/reload and a delegated Todo subscription; it passed after correcting the fixture to use an actual delegating Silicon. Populated project screenshots in `web/screens/` were visually reviewed. Evidence: `.mig/web-e2e.log`, `.mig/web-product-final.log` (first product test), `.mig/web-silicon-final.log`, `.mig/web-unit-final.log`, `.mig/web-build.log`. Browser output includes development theme/reduced-motion notices; this does not claim an absence of browser warnings.

The full frontend and backend are verified locally. Production cutover, publish and deployment remain coordinated root tasks.

## UUID cutover replay and coverage verification — 10 October 2026

The checked CSV consumer now refuses incomplete legacy-account coverage before changing data and parks unaccepted prepared notifications without rewriting historical body/event identities. Dry-run, apply, idempotent reapply, conflicting-map refusal and missing-map refusal passed populated PostgreSQL clones. Explicit pending notification fixtures verified immutable-body preservation and replay exclusion. See [uuid128.md](uuid128.md) for schema-specific preservation rules and local evidence. No production data or original checkout changed.

Verified webhook handling now ignores retired top-level subjects and embedded custodian identities before writing account state, and records an acknowledged delivery without reintroducing the retired account reference. Repeated delayed deliveries remain harmless. Regression covers signed-out subjects, custodian changes and profile updates; the Accounts authority suite passed 16/16 (`.mig/uuid-event-suite.log`).

The final local macOS ARM64 CLI was rebuilt after UUID/auth fixes, then repackaged with required native signed-out discovery and extracted-archive validation. Evidence: `.mig/uuid-final-package.log`; artifacts remain local under `dist/apps/`. This is local release preparation, not publication.

## Coordinated local UUID backfill — 10 October 2026

With API/worker writers stopped and full database/private-state backups captured, applied the shared Accounts export (211 rows, SHA256 `750423f3117e11f5eb42025b457ff0f1bf42bd463c31b7e106d9cd10978ca4d9`) to `commit_e2e`: 24 cached account rows moved and 0 held proof grants were authenticated/resealed. Dry-run, apply and unchanged replay passed. Exact resource IDs, archive/prepared payloads, Hook secret ciphertext and decrypted DM/Extend proof hashes matched their pre-apply snapshots. The live store had no unaccepted prepared deliveries left to park; separate populated queue regressions cover that case. Evidence: Commit `.mig/cutover/applied.json` and `retention-verified.json`. Production remains untouched.

## Post-cutover verification and shutdown — 10 October 2026

After Accounts restarted with the applied shared map, a previously valid old-subject bearer returned 401 and a fresh hosted sign-in returned 200 with the exact exported canonical UUIDv4 subject. The retained todo still has the same ID, title and description, and its owner resolves to the mapped UUID. Evidence: Commit `.mig/cutover/verification.json` and `verify.log`.

Stopped every development API/worker/provider owned by this app and preserved the post-cutover database plus required private runtime state as mode-0600 archives. All four owned port blocks (4120–4159 and 4200–4239) have no listeners. Evidence: Commit `.mig/cutover/final-cleanup.json`, including backup SHA256 values. Shared Accounts/PostgreSQL cleanup is coordinated by the root task; unrelated pre-existing services were untouched.

The local macOS ARM64 archive is a **development-profile candidate** (debug symbols disabled, unoptimized), with native discovery and extracted archive validation completed. It is not an optimized or published release; other target release builds and production deployment remain separate gates.
