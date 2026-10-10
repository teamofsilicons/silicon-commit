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

## Client crate and CLI (stage 2)

**A-21 The CLI uses only the Rust package.** `silicon-commit-client` 0.5.0 gained an `auth` module
(`AccountsAuth`) that signs Commit's own tools in at Silicon Accounts as a public client (`client_id=commit`, no
secret), so the CLI keeps depending on nothing but the client crate (UNDERSTANDING: "CLI is built using the Rust
Package only"). It wraps `silicon-accounts-client` 0.4.0 for the device flow (`app_device_authorize` /
`app_device_poll`) and public refresh (`refresh_app_public_client`). Two calls are direct HTTP with the same error
mapping: the short-lived token exchange (the published 0.4.0 has no `exchange_slt_public_client`; it exists only in
the unreleased Accounts repository) and revocation (0.4.0's public revoke helper accepts only first-party client
ids, as the brief notes). Replace both with the crate's helpers when a release has them.

**A-22 Errors keep the API envelope, including details.** `Error::Api` carries `code`, `message`, `hint`,
`details`, `request_id` and `Retry-After`; the CLI prints all of them. The IAM-era client withheld `details`; the
service marks them safe to show and an agent needs them to fix a request. Bodies that are not Commit's envelope
(proxy pages) are never echoed, and `details` over 4 KiB are dropped. Silicon Accounts refusals keep their code and
description; `SignInRefusal` classifies `invalid_grant` descriptions (already used, expired, wrong app, unknown,
malformed, sign-in ended, account inactive) because the service gives no machine-readable reason.

**A-23 Plain http only for this machine.** Both the API client and `AccountsAuth` refuse `http://` except for
`localhost`, `*.localhost`, `127.0.0.0/8` and `::1`. There is no override flag: production is https, and the local
Accounts stack is on loopback.

**A-24 Session file, version 2.** `<state>/session.json` (or `profiles/<name>/session.json`), mode 0600 in a 0700
directory, written atomically: tokens, `expires_at`, `refresh_expires_at`, scope, the account (`uuid`, `id`, `kind`,
display name, shared email, custodian), the Accounts and API URLs, how it was made (`device`/`slt`), and markers for
an in-flight refresh and an ended sign-in. IAM-era files (no `version`, with `org_id`/`actor`/tokens) are reported as
`legacy_session`, anything else unknown as `unreadable_session`; neither is ever rewritten by status or commands
(serde's messages are never shown because they can quote file contents); `commit logout` deletes them and signing in
replaces them. Leftover `test-*.json`, `testing-selection` and `auto-update` files are ignored.

**A-25 Refresh once, under a lock.** Commands refresh when less than 60 s are left: take `session.lock`, read the
file again (another command may have rotated already), mark `refresh_started_at`, refresh, save the new pair, then
use it. An uncertain refresh is never retried automatically (public-client refresh is not idempotent and a used
refresh token ends the sign-in); the session is kept and the next command tries again, and if that ends in
`invalid_grant` the error explains the lost answer. `invalid_grant` marks the session ended, so later commands say
`session_ended` without contacting Silicon Accounts again.

**A-26 Replay after 401.** A request the API refuses with 401 is repeated once after a forced refresh (unless another
command already replaced the token), with the same idempotency key and the same body (`--data @FILE` is read once).
If the profile signed in as another account meanwhile, nothing is replayed (`profile_changed`).

**A-27 `commit login status`.** Default: refresh if needed, then ask the API (`GET /api/v1/me`) to confirm
(`verified: true`) and remember the id, name and custodian it reports. If the API cannot be reached the saved session
is reported with `verified: false` and a `warning`; only a refused token or an ended sign-in turns it into
`authenticated: false` (with `reason`). `--offline` reads the file only. `--json` always exits 0; text exits 1 when
signed out. The app decision "offline from the saved session (refresh first)" is read as: the answer comes from the
saved session after a refresh, not from a backend status route as before; the `/me` check only sets `verified`.
Extra keys: `profile`, `api_url`, `accounts_url`, `custodian` (Silicons), `reason`/`message` (signed out), `source:
"token"` with `--token`.

**A-28 `commit login` (Carbons).** Device flow with `client_id=commit`, label `Commit CLI on <os>`. The link and code
go to stderr (JSON lines with `--json`: `device_code`, `slow_down`, warnings), the result to stdout. Nothing opens
unless `--open`. By default no extra details are requested (the app's required fields always come with the sign-in);
`--scope email` asks to share the email, which is the decisions file's "sign in again to share one". Ctrl-C exits 130.

**A-29 `commit login --slt/--slt-stdin/<SLT>` (Silicons).** The positional form stays (the Silicon runtime still runs
`commit login <SLT>`) and is documented; `--slt-stdin` refuses a terminal and empty input. A value without the `slt_`
prefix is refused before any request and described without echoing it (refresh token, proof, STK, JWT, a token of
the previous sign-in system); a lowercase word like `statuss` is answered as a mistyped subcommand. `--scope`/`--open`
with a token is a usage error. Commit had no `COMMIT_SLT` variable, so none was added.

**A-30 Signing in again replaces the session.** No `--force` (unlike `silicon-accounts login`): re-consent for email
and the runtime's repeated `commit login <SLT>` must simply work. The new session is saved first, then the previous
sign-in is revoked at the Silicon Accounts it came from (best effort; a failure is a warning). After every sign-in the
CLI asks `GET /api/v1/me` once (best effort) so a wrong `--api-url` shows at once (`verified: false` + warning).

**A-31 `commit logout`.** Revokes at the session's Silicon Accounts (`POST /v1/oauth/revoke`, `client_id=commit`),
then deletes the file. If Silicon Accounts cannot be reached the session is kept and the command exits 1 so it can be
retried; `--force` deletes it anyway with a warning. Ended, legacy and damaged sessions are deleted without a network
call. `--token`/`COMMIT_ACCESS_TOKEN` is ignored by logout (it ends the saved session only). Not signed in:
`{"signed_out":false,"reason":"not_signed_in"}`, exit 0.

**A-32 Where requests go.** `--api-url`/`COMMIT_API_URL` and `--accounts-url`/`ACCOUNTS_URL` (all global flags), else
the saved session's URLs, else production. A saved session is never sent to other servers: asking for another API or
Accounts URL fails with `signed_in_elsewhere` (status reports it as `authenticated: false`). URLs compare by origin;
an `/api/v1` suffix is ignored.

**A-33 `commit accounts [--json]`.** Prints the same object with or without `--json`, never fails and never touches
the network or the disk beyond reading the session: `app_id`, `accounts_url`, `api_url` (effective values),
`version`, `command`, `profile`, `login` (`carbon`, `silicon`, `status` commands), `docs`, `repository`,
`rust_client`. The hidden `commit iam [--json]` prints exactly the same object for one minor release (the runtime
still calls it); it appears in no help or user doc.

**A-34 Removed commands and flags.** `testing`, `test-environments`, `daemon` (it only removed the old updater),
`config updates`, `--org-id`/`COMMIT_ORG_ID`, `--test`/`COMMIT_TEST_KEY` and `--no-update` are gone (usage error,
exit 2). `--no-update` was already a no-op and the Silicon runtime never passes it (it runs `iam --json`,
`login <SLT>` and `login status --json`). Manual removal of old updater units is in the cutover runbook.

**A-35 Kept.** `--profile` (one account per profile), `config home|show|telemetry`, `report` (with the local copy on
failure and `--save-only`), `docs`, `health`/`ready`/`version` (no credentials), `--token` (used as is, never saved
or refreshed; status reads its claims unverified for display and confirms it with `/me`), `--no-save` (prints the
tokens with a warning).

**A-36 New CLI commands.** `commit me`, `commit silicons allowed-accounts|allow|disallow`, and `--silicon` on
`commit notifications` for custodians, mirroring the service's new routes. Carbons without a shared email get a
hint after `commit email` (`commit login --scope email`).

**A-37 Bundled guides.** `commit docs` topics: start, cli, projects, notifications, client, api, accounts,
contracts, development, telemetry, deployment (`cli/docs` stays byte-identical to `docs/`, which CI checks). The IAM
and testing-environment guides left the bundle; their `docs/` files are left for the packaging/docs stage to move to
`docs/history/`. User-facing guides no longer say "circle": "the accounts close to" an account, defined once.

**A-38 Versions.** `silicon-commit-client` and `silicon-commit-cli` 0.4.1 → 0.5.0 (breaking), speaking contract 2
(`X-Commit-Supported-Versions: 2`; any other `X-Commit-API-Version` answer is `Error::UnsupportedContract`).
Telemetry stays server-side (Space Station in the worker); the CLI only sends `X-Commit-Telemetry: on|off` and
`X-Accounts-Telemetry: off` when diagnostics are off.

**A-39 `/me` names a custodian Commit has not seen.** Found in the live run: a Silicon's `/me` showed its custodian
with an empty id until the custodian used Commit. `/me` now looks the custodian up (cached a minute) and does not
store the answer, because a row made from a lookup would delay the custodian's own first `userinfo` refresh (name,
shared email) by up to ten minutes. The CLI also keeps a known custodian id when an answer omits it.
