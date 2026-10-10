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
