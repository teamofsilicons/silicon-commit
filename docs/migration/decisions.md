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

## Packaging, CI, deployment and documentation (stage 3)

**A-40 One archive per target, from a template.** `packaging/apps.yaml.in` is rendered by
`scripts/package-apps.sh VERSION TARGET BINARY` into an `apps.yaml` that lists only that target; the archive holds
exactly `apps.yaml` and `bin/commit` (`bin/commit.exe`), is packed by `silicon-apps pack` (deterministic: the same
binary gives the same archive) and is validated again as an archive. Output: `dist/apps/commit-VERSION-TARGET.tar.gz`
and `.sha256`. Why: Silicon Apps uploads and validates per target, so each archive must stand on its own; `honeycomb.yaml`
and the six-target single archive are gone.

**A-41 The six targets Commit already shipped.** The release builds linux-x86_64 and linux-aarch64 (glibc 2.28 with
cargo-zigbuild 0.23.4 and Zig 0.15.2), windows-x86_64 and windows-aarch64 (static C runtime), macos-x86_64 and
macos-aarch64, on the same runners as before. The packager also accepts linux-i686, linux-armv7hf and windows-i686
(the glibc check now reads ELF32), so adding one is a matrix row; they are not built now because Commit never
shipped them and the Silicons run x86_64 and aarch64 (the brief: keep the targets the app already ships).

**A-42 Discovery is checked where the binary runs, and bound to its bytes.** The three commands use Silicon Apps'
own pass rules: `--help` exits 0 with text; `accounts --json` exits 0 with one JSON object whose `app_id` is
`commit`; `login status --json` exits 0 with one JSON object whose `authenticated` is `false`, signed out, in an
empty home with none of the caller's environment. The packager also requires `commit --version` to print the
manifest's version (packing the wrong build is caught). Each release build job runs them on the target's own
runner (Linux again in the pinned glibc 2.28 Debian image with no network) and writes a receipt: target, version,
SHA-256 of the binary, results. The single packaging job, on Linux, runs them itself where it can and otherwise
accepts only a receipt for the same bytes; `--discovery require` refuses without either. Why: one packer install on
one Linux runner instead of six (two of them Windows), without weakening the guarantee.

**A-43 The packer never sees a sign-in.** `silicon-apps validate` and `pack` are local; the script runs them with an
empty `--home`, `SILICON_APPS_NO_DAEMON=1`, and without `APPS_TOKEN`, `APPS_URL`, `ACCOUNTS_URL` or `SILICON_HOME`,
and requires silicon-apps 0.2.x (CI installs `silicon-apps-cli` 0.2.0 with `cargo install --locked`).

**A-44 One version, and a tag that names it.** The root, client and CLI manifests (and the CLI's dependency on the
client) must agree on a strict `x.y.z` (Silicon Apps refuses prerelease suffixes); a release tag must be exactly
`v<version>`. `workflow_dispatch` builds from any branch without the tag check. The release publishes nothing:
artifact `commit-silicon-apps-release` with the archives, `SHA256SUMS` and `SOURCE_REVISION`.

**A-45 CI packs on every change.** A `package` job in `ci.yml` builds the Linux CLI for glibc 2.28 and packs it with
the discovery commands required, and the tools job runs the packager's tests, so a release stays one tag away.

**A-46 Both crates stay on crates.io.** `silicon-commit-client` is a library dependency; `silicon-commit-cli` stays
publishable as a source mirror for `cargo install` (as Silicon Apps does for its own CLI), and 0.5.0 replaces the
0.4.1 crate that stops working at the cutover. Every doc installs with `silicon-apps install commit`, the only
channel that updates the CLI. (The survey suggested stopping the CLI crate; the CLI stage's runbook kept it.)

**A-47 Production steps are scripts, not hand-typed SSM payloads.** `deploy/aws/host.py` copies a file under
`/opt/commit` (checked by SHA-256 on the host, names limited to letters, digits, `.`, `_`, `-`) or runs a command as
root through Systems Manager; `deploy/aws/cutover.py` runs on the host the two steps that need the private database:
`queues` (waiting webhook deliveries and emails, read-only, as the migrator) and `plan`/`dry-run`/`apply` of
`commit-migrate link-identities` in the deployed image, with a root-only environment file that is removed even on
failure. Why: the database accepts only the host, and bootstrap deletes the migrator's environment after migrating.

**A-48 Drain before 0033.** Migration 0033 asks for a drained outbox. The runbook stops only the API, lets the worker
empty the queues (`cutover.py queues`), then runs bootstrap. A delivery still waiting on a failing destination may be
carried over; the new worker sends it as stored.

**A-49 The webhook secret exists before the webhook.** `POST /v1/apps/commit/webhook/generate-secret` makes the
`whsec_…` before the URL is set, so bootstrap starts with it and no event is refused while the old service runs;
`silicon-accounts app webhook set` afterwards keeps that secret.

**A-50 History moves out of the current docs.** Release notes, verification reports, cutover contracts and design
notes of the IAM and Honeycomb era moved to `docs/history/` unchanged (paths inside them fixed); the three retired
stubs (`IAM.md`, `HONEYCOMB.md`, `TEST_ENVIRONMENTS.md`) are gone. Neither `docs/history/` nor `docs/migration/` is
published on the docs site or bundled into the CLI; the site sends the old addresses of removed pages to the pages
that replaced them.

**A-51 Docs site.** Navigation lists Silicon Accounts, releases and deployment; the footer says contract 2; the
release-preview banner no longer names a retired integration; the build fails if the navigation or a redirect names
a missing page.

**A-52 `install.sh` keeps its address.** `https://docs.commit.teamofsilicons.com/install.sh` now runs
`silicon-apps install commit` (passing options through) and explains how to install Silicon Apps when it is missing,
the same shape as before.

**A-53 Production allows Interface only what it used.** The runbook builds `COMMIT_PROOF_ISSUERS` from the action ids
Interface called in the IAM era (todos, notes, projects, tasks) for Interface's app id, rather than `*=`; a missing one
answers 403 `proof_issuer_not_allowed` naming it.

**A-54 Words in the reference.** The OpenAPI description and the README say "the accounts close to" an account
(defined once) instead of "circle", and no longer say "organizations".

## End to end against Silicon Accounts (stage 4)

**A-55 A local stack in two commands.** `scripts/dev-accounts.sh [--build] [--fresh]` migrates `commit_e2e` on
`127.0.0.1:5460`, starts `commit-api` on `127.0.0.1:4141` (Commit's block: web 4140, API 4141) and `commit-worker`,
points Commit's webhook at Silicon Accounts to `http://127.0.0.1:4141/webhook/` with Commit's own app credentials
and proves a test delivery; `scripts/dev-accounts-stop.sh` stops both and puts the webhook URL it found back (the
shared stack had it on the testkit's fake app). The signing secret stays in `.mig/webhook-secret` (0600, ignored by
git): `PUT …/webhook` keeps a stored secret, so the script uses the stack file's seeded one and rotates only when a
test delivery is refused. Both scripts refuse a non-loopback Silicon Accounts or database, and the services get a
clean environment (no inherited Postmark or telemetry key). The development API allows `interface` exactly the
actions production will (cutover step 4), which a unit test keeps in step with the runbook.

**A-56 The scenarios are a script, not a cargo test.** `scripts/e2e-accounts.sh` (`tests/e2e/accounts_e2e.py`,
Python standard library) needs a running stack, the testkit and the `silicon-accounts` CLI, which `cargo test`
cannot assume; CI unit-tests the scripts' guards and compiles them instead. Identities come from
`tests/e2e/mint.mts`, which drives the testkit's sign-in pages and development mail. A run signs each Carbon in
once and reuses first-party tokens for approvals, Silicon creation and account-site actions, because the stack
allows ten email codes per address in ten minutes. Transcripts redact tokens, STKs, proofs and secrets.

**A-57 Stored account details only move forward.** Supersedes the part of A-07/A-13 where any lookup refreshed a
row "as of now". A resolved account carries `observed_at` (when Silicon Accounts said it; a cached lookup keeps
its fetch time), and `commit.accounts` takes it only when it is at least as new as `refreshed_at`, which now means
"the newest information stored was true at this time" (fetch time or event time). Found by reading the code
while fixing A-58, and proven by a regression test that fails without the fix: a lookup cached before a custodian
transfer, stored right after it, restored the former custodian and made Commit ignore the
`silicon.custodian_changed` event, so the former custodian kept its powers. Commit's and Silicon Accounts'
clocks are compared directly (NTP-level skew is accepted, as before).

**A-58 An account's first sign-in reads its own view.** Resolves the CLI stage's open finding. A row known only
from lookups (someone named the account first) has `accounts_version` 0; its first bearer request refreshes from
`userinfo`, which now also stores the account's version with the shared email (`remember_own_view`; an equal or
newer version from `account.updated` wins). Found live: such a Carbon's `/me` had no display name or email, so the
email default of the decisions file had nothing to use. No migration was needed (the column existed).

**A-59 A token from the sign-out's own second is decided by Silicon Accounts.** Refines A-08. `iat` has whole
seconds; the cutoff has milliseconds. Earlier seconds are refused and later ones accepted as before; a token from
the cutoff's own second is introspected (a cached answer from before the cutoff is not trusted), so a sign-in made
right after the event works and one made just before it does not. Found live: a Silicon signed the CLI in and
removed Commit 30 ms later, and the CLI kept working. Only tokens from that one second cost an introspection.

**A-60 Changes that widen access never use a cached answer.** Supersedes "cached 30 s" in A-09 for these checks.
Visibility and member changes and allow-list changes introspect the access token every time, and a proof on those
routes is verified again; other requests keep the 30-second caches. Found live: a token introspected during an
allow-list change was accepted for a visibility change after the web had signed the Carbon out (that sign-out is
Commit's own, `app_revoked`, so no cutoff applies).

**A-61 Concurrent duplicates of one event need no lock.** The dedupe row is written after the event is applied in
the same transaction, so two deliveries of one `event_id` in flight at once can both apply. Every handler is
idempotent (id and custodian changes are guarded by time, profile updates by version, cutoffs use `greatest`,
`forget_account` locks the row and skips what is already forgotten), so the cost is one extra "applied" log line.
Kept as is.

**A-62 Email stays opt-in.** Only Carbons who saved an email preference get email, as in the IAM era (0026); the
address offered by default is the one shared with Commit (checked end to end). UNDERSTANDING.md reads as if
project-completion emails were on by default; making them so would start emailing people who never asked, so it is
left as a product change for a Carbon (noted in `understanding-proposal.md`).

**A-63 Accounts that never signed in can show an old id.** Silicon Accounts sends app events only for accounts
with a membership, so an account Commit knows only from lookups (assigned work, never signed in) keeps the id it
had when named until it signs in or is named again by its new id. Uuids stay right and are what Commit keys on;
filters by `c:`/`si:` id use the stored current id. Accepted: refreshing every displayed account would spend the
600 lookups a minute.

**A-64 Commit issues no proofs.** The issuer half of the proof scenario does not apply (A-15 stands: no Ting, no
other outgoing app calls); the receiver half runs against the stack's `interface` app.
