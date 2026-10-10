# Commit: Silicon Accounts migration decisions

Decisions taken while moving Commit from Silicon IAM (organizations) to Silicon Accounts (personal accounts) and
Silicon Apps. Each entry maps an old concept to the new behaviour and says why. Root `decisions.md` keeps the
IAM-era log; D-056 there points here and marks what this supersedes.

## Words

- **Account**: a Silicon Accounts account, keyed by its permanent, case-sensitive `uuid` (3 to 12 characters of
  a-z, A-Z, 0-9). Its `c:`/`si:` id is display data that can change.
- **Circle**: circle(Carbon C) = C and every Silicon whose custodian is C. circle(Silicon S) = S, its custodian, and
  every other Silicon with the same custodian. The relation is symmetric (`commit.in_circle`).
- **Custodian rule**: a Carbon manages its Silicons' work in Commit (read, change, delete, settings) and is recorded
  as itself in history. It never acts as the Silicon (notes, messages and authorship stay its own).

## Concept map (old → new)

| IAM era | Silicon Accounts era |
|---|---|
| Organization (`org_id`, `X-Org-ID`, `organization_id` on every row) | No organizations. Rows are keyed by account uuids. `organization_id` and the `*_principal_id` columns stay on IAM-era rows as provenance (nullable; NULL on new rows). `X-Org-ID` answers 400 `retired_header`. |
| IAM principal / `membership_id` / `commit.actor_projection` | `commit.accounts` (uuid, kind, current id, display name, photo, shared email, custodian, status, revocation cutoff, Accounts version). The projections stay as read-only provenance; the runtime roles can no longer read them. |
| Org roles (owner/admin/member), IAM capabilities, tags, trust, reports_to | Removed. Authority comes from ownership, explicit sharing (assignment, project membership) and the custodian rule. Project `tags` stay stored but are ignored. |
| Todos "public within the organization" | A todo is visible to the circles of its owner (`assigned_by`) and its assignee, and to whoever can read its project. Only the owner (or the owner's custodian) changes content or deletes it; the assignee (or its custodian) may also move its status; owner, assignee and their custodians may add notes. |
| Public project (organization-visible, organization-editable) | Visible to the owner's circle, to its members and to the custodians of member Silicons. Changing it needs membership or the custodian rule; a circle reader who is not a member gets 403 `project_not_writable`. |
| Private project (members or IAM tags) | Members by `c:`/`si:` id or uuid only (`silicon_ids`, `carbon_ids`), plus the custodians of member Silicons. Tags are gone. |
| Assignment / participation "inside the organization" | Carbons can be named by anyone. A Silicon takes work and project invitations only from its circle and from accounts on its allow-list (`/api/v1/silicons/{silicon}/allowed-accounts`), managed by the Silicon or its custodian. Refusal: 403 `silicon_not_reachable` naming the call that fixes it. |
| IAM OBO / ATA headers (`X-IAM-OBO-Access-Token`, `X-App-ID`, proof hashes) | `Authorization: Proof sap_…` User verification proofs on the normal routes. The scope is the route's action id (the IAM-era ids, `commit.todos.list` …, unchanged). Commit verifies with `POST /v1/proofs/verify`, requires `receiving_app == commit` and an issuer allowed for that scope by `COMMIT_PROOF_ISSUERS`, acts as `user.uuid`, and records `via_app` on what it writes. |
| IAM webhook on `POST /webhook/` | Same path, Silicon Accounts app webhook (`X-Accounts-Timestamp` / `X-Accounts-Signature`, 5-minute tolerance, dedupe on `event_id`). |
| Login popup, SLT exchange, session cookies, `/api/v1/sessions…`, context switching | Removed. The web becomes a BFF that signs in at Silicon Accounts and sends bearer tokens; the CLI signs in at Accounts itself (later stages). |
| Honeycomb testing environments (`/api/v1/test-environments…`, `/internal/honeycomb/…`, `X-Testing-Environment-Key`, `X-Testing-App-Secret`) | Removed. Their tables and rows stay (dormant, unreachable by the runtime roles). The headers answer 400 `retired_header`. |
| Contract 1 (IAM, `org_id`, tags) | Contract 2: no `org_id`/tags, every account reference is `{type, id, uuid}`, Silicon webhooks use payload version 3. `X-Commit-API-Version: 1` answers 406 `unsupported_contract`; contract 1 is marked deprecated and sunsets after seven idle days like any replaced contract. |
| Email preference per (organization, principal) | One preference per account. Without a saved preference the address defaults to the email the Carbon shared with Commit at sign-in (`email` scope); without one, the API says how to share one. Silicons have no email. |
| Notification delivery payload v2 (`org_id`, principal ids) | v3: `silicon {type, id, uuid}`, account references with uuid, `via_app` when another app acted. Delivery stays a direct HTTPS POST to the Silicon's webhook URL. |
| IAM directory lookups (members, tags) | Silicon Accounts lookups (`GET /v1/accounts/{uuid}`, `/v1/accounts/by-id/{id}`) with app credentials, cached 60 s (Accounts allows 600 a minute). Unknown or malformed names are 422 with the field named. |

## Decisions

**A-01 Account-keyed schema with placeholders (data kept).** Migration 0033 adds `commit.accounts`, NOT NULL
`*_account` columns (deferrable foreign keys) beside every IAM-era principal column, and one unlinked placeholder
account per IAM principal (`iam:<organization_id>:<principal_id>`, kind and id from the projection). Every IAM-era
row points at its placeholder until `commit-migrate link-identities` links it. No value is deleted or rewritten;
the upgrade test compares every IAM-era column of 22 tables before and after. Why: production data is tiny but
threaded through ~25 tables with composite organization keys; placeholders make the cutover reversible and testable
before any real uuid is known.

**A-02 `commit-migrate link-identities`.** `--file MAPPING.csv [--dry-run] [--offline]` re-points rows in one
transaction and prints a JSON report; `--plan` prints the production principals as a template. A line names a
principal by IAM principal UUID or IAM-era `c:`/`si:` id (an id matches production principals only; former testing
sandboxes reused production ids, so they are linked only by principal UUID). Rows move only while they still point
at the principal's previous account, so post-cutover changes are never clobbered; re-running with another file
re-points again; an empty uuid unlinks. Where an account may own one row (email preference, Silicon settings, active
membership, idempotency key, bug-report key) the newest legacy row wins and the rest stay on placeholders (reported).
A Carbon links only to a Carbon and a Silicon only to a Silicon; a principal mapped twice is refused. The report also
lists formerly organization-wide projects whose former readers lack access now (`public_projects_needing_shares`).
Lookups (`ACCOUNTS_URL`, `COMMIT_APP_SECRET`) are read-only; `--offline` skips them.

**A-03 Project UIDs must be unique across former organizations.** UIDs were unique per organization; without
organizations they are global locators. 0033 refuses to run while duplicates exist and prints the query that lists
them (rename the dormant copy's uid, which is only a locator). Production had one organization, so none exist.

**A-04 Project owner.** New column `owner_account` (initially the creator). On `account.deleted` an owned project
passes to its longest-standing active member, recorded in history; with no other member it is soft-deleted
(`deleted_at`, visible to nobody). The owner must stay an active member (deferred constraint trigger).

**A-05 Visibility and change rules live in SQL.** `commit.in_circle`, `circle_of`, `project_writable`,
`project_access`, `todo_access` and `may_reach` define the policy once; Rust authorizes with them inside the same
transaction that reads or writes (no time-of-check gap). Hidden resources answer 404, readable-but-unchangeable 403.

**A-06 The custodian rule is bounded.** A custodian acts for its Silicons in Commit (`VerifiedActor::acts_for`) but
always as itself: audit actor, note author and project collaborator are the custodian. Notification settings of a
Silicon are managed by the Silicon or its custodian with `?silicon=si:…`; anyone else gets 403 `not_custodian`.

**A-07 Authentication.** Every API route accepts exactly one `Authorization` header. Bearer tokens are EdDSA JWTs
verified locally against a cached JWKS (`aud` = `COMMIT_APP_ID`, `iss` = `ACCOUNTS_URL` exactly, `exp`/`nbf` with
30 s leeway; an unknown `kid` refetches the JWKS at most every 30 s; a failed fetch keeps the last good set). Each
refusal is a 401 with the client library's precise code (`token_expired`, `token_wrong_audience`,
`token_wrong_issuer`, `token_unknown_key`, `token_bad_signature`, `token_malformed`, …). Account details are
refreshed from `userinfo` (which carries the shared email) at most every 10 minutes, else from a lookup.

**A-08 Revocation.** `membership.signed_out` (any reason but `app_revoked`) and `membership.access_removed` record a
cutoff on the account; bearer tokens issued (`iat`) before it are refused with 401 `session_ended`. `app_revoked`
means Commit itself ended one sign-in (a CLI logout or the web's sign-out): other sign-ins stay valid. A deleted
account is refused with `account_deleted`. Commit holds no proofs or sessions of its own to end; proof and
introspection results are cached at most 30 s.

**A-09 Introspection only where access widens.** `PATCH /api/v1/projects/{id}` when it changes `private`,
`silicon_ids` or `carbon_ids`, and `PUT`/`DELETE` on a Silicon's allow-list, also introspect the bearer token online
(cached 30 s); inactive → 401 `token_revoked`. Projects have no delete route.

**A-10 Proof issuers default to deny.** `COMMIT_PROOF_ISSUERS` is a comma-separated list of `scope=app_id`
(`*=app_id` for every scope); scopes are validated at boot against Commit's action ids. Empty (the default) refuses
every proof with 403 `proof_issuer_not_allowed`. Other refusals: `proof_invalid` (401), `proof_wrong_receiver`
(401), `proof_without_account` (401, App verification proofs speak for no account), `proof_scope_missing` (403),
`proof_malformed` (401, e.g. a `sapr_…` refresh token). Production should list the Silicon Interface's app id.

**A-11 `/api/v1/obo/*` aliases stay for one release.** Each performs exactly its canonical route's action and accepts
the same Bearer or Proof credential, so Interface can switch credentials before switching paths.

**A-12 New routes.** `GET /api/v1/accounts` (public sign-in metadata: app id, Accounts URLs, scopes),
`GET /api/v1/me` (the caller as Commit sees it, its custodian or its Silicons, `via_app`),
`GET /api/v1/silicons/{silicon}/allowed-accounts` and `PUT`/`DELETE …/allowed-accounts/{account}`.
Paths that did not carry an organization are unchanged.

**A-13 Accounts webhook.** `POST /webhook/` verifies the signature over the raw body (401
`invalid_webhook_signature` for a bad signature or a timestamp outside 5 minutes; 400 `invalid_webhook_body` for a
body that is not an event; 503 when `COMMIT_ACCOUNTS_WEBHOOK_SECRET` is unset), deduplicates on `event_id`
(`commit.accounts_webhook_events`, body hash only), then: `account.id_changed` (new id unless fresher data is
stored), `account.updated` (fields Commit may see; only a higher Accounts `version` applies), `silicon.custodian_changed`
(new custodian, so circles change at once), sign-outs (A-08), `account.deleted` (A-14), `ping` and unknown types
(recorded, 200).

**A-14 Account deletion.** `commit.forget_account` (one SECURITY DEFINER step): personal todos (owned by and assigned
to the account) are deleted as tombstones with the normal retention; memberships end; owned projects pass on or are
deleted (A-04); notification settings, todo subscriptions, the email preference and allow-list entries are removed;
pending webhooks to it are dead-lettered and pending emails suppressed; the account row is anonymised (status
`deleted`, empty id, no name/photo/email) and every earlier token refused. Work it delegated to others or received
from others stays, shown as a deleted account (`id` = "").

**A-15 No Ting adapter.** D4 applies to apps that deliver through Ting. Commit never did: Silicon notifications are
direct HTTPS webhooks to the URL each Silicon (or its custodian) configures, and email goes through Postmark. Adding a
Ting channel would be new product behaviour, so `COMMIT_TING_URL` is not introduced. Revisit if notifications should
also reach Ting.

**A-16 Environment.** New: `ACCOUNTS_URL` (default `https://accounts.teamofsilicons.com`), `ACCOUNTS_API_URL`
(defaults to `ACCOUNTS_URL`), `COMMIT_APP_ID` (default `commit`), `COMMIT_APP_SECRET` (API, required),
`COMMIT_ACCOUNTS_WEBHOOK_SECRET` (`whsec_…`, required in production), `COMMIT_PROOF_ISSUERS`. Plain http is accepted
only for `localhost`/`127.0.0.1`/`::1` and never in production. Retired variables (`COMMIT_AUTH_MODE`,
`COMMIT_IAM_*`, `COMMIT_WEBHOOK_SIGNING_SECRET`, `COMMIT_WEBHOOK_KEY_VERSION`, `COMMIT_HONEYCOMB_*`,
`COMMIT_TEST_ENVIRONMENT_ENCRYPTION_KEY`, `COMMIT_TEST_KEY`) are ignored with a warning at boot. The AWS bootstrap
stops copying them and refuses to deploy without `COMMIT_APP_SECRET` and `COMMIT_ACCOUNTS_WEBHOOK_SECRET`.

**A-17 Runtime grants.** The API gets `commit.accounts` (SELECT, INSERT, UPDATE), the webhook inbox (SELECT,
INSERT), the allow-list (SELECT, INSERT, DELETE) and EXECUTE on the policy functions and `forget_account`; it loses
the IAM projections, testing-environment, IAM-webhook and Honeycomb objects. The worker reads only
`accounts(uuid, public_id)` of a delivery's recipient. `tests/postgres_runtime_grants.sql` asserts both.

**A-18 Dependency policy.** `silicon-accounts-client 0.4.0` brings `rsa` through `jsonwebtoken`. RUSTSEC-2023-0071
(Marvin) concerns RSA private-key operations; Commit verifies EdDSA tokens with public keys only, so `deny.toml`
ignores it with that reason (as Hook does). The vendored IAM client and its exceptions are gone.

**A-19 Account names in requests.** `c:`/`si:` ids are case-insensitive and looked up by id; anything else must look
like an Accounts uuid (3 to 12 alphanumerics) and is looked up exactly. Malformed or unknown names never reach the
database: 422 naming the field. A deleted account is unknown; a Silicon waiting for its custodian is refused with its
reason.

**A-20 Testing environments and Honeycomb plumbing removed.** Routes, worker jobs, pairing secrets and the AWS
testing-credentials helper are gone; their tables keep their rows. Packaging (`honeycomb.yaml`, release scripts) is
left to the packaging stage.

## Pre-existing oddities left as they are

- Stored delivery failure codes are spelled `webwebhook_unavailable` etc. (since before the migration). They are
  persisted values, so they are not renamed here.
