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
