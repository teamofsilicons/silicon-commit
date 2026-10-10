# Parallel Accounts production release

The user narrowed the production rollout on 2026-10-10 to the apps themselves. Existing IAM Silicons and custody are not imported, linked, re-enrolled or updated through Honeycomb. This plan supersedes the in-place production steps in `cutover.md`; that document remains historical reference for the earlier migration design.

- Accounts API origin: `https://api.commit.teamofsilicons.com`.
- Existing IAM API origin: `https://backend.commit.teamofsilicons.com`; its service, database, credentials, data keys and client traffic remain unchanged.
- New website: `https://commit.teamofsilicons.com`, using the new Accounts BFF and API origin.
- New package version: `0.5.0`; its CLI/SDK defaults target the new API origin. No existing IAM installation is automatically changed by this operation.
- New database: `silicon_commit_accounts`. New role names: `commit_accounts_migrator / commit_accounts_api / commit_accounts_worker`. Never point the Accounts binary or installer at the IAM database or reuse a role whose credentials an IAM service uses.

## Deployment boundary

Separate API and worker containers on the existing host; API loopback 8081, distinct env files and telemetry/state directories.

Apply migrations through 0036 to the new empty store and grant only the new roles. The new Accounts store contains only Accounts-created data. Old conversations, devices, todos and hooks do not appear in the new account interface without a separately authorized identity/data migration; they remain accessible to existing IAM clients and recoverable from their original stores and backups.

Accounts registrations are already active with the public website callback and device/public-client flows. First-use webhook signing secrets and subscriptions must be configured only for the new API. Existing IAM webhooks and delivery workers remain untouched. The new website uses a distinct session secret.

DNS is centrally managed by the rollout coordinator. Current authoritative DNS is Namecheap, not Route53. Add the new API hostname without replacing the old backend hostname or any mail records. Preserve the previous Vercel deployment for website rollback.

## Verification and rollback

Before exposure, prove the new database name and role, migrate it, verify health and anonymous auth refusal, then sign in with an existing Accounts identity. Create and read a resource in the new app; confirm the corresponding IAM store has not changed. Check both API origins independently. Native Extend acceptance includes new-device pairing, screenshot/file transfer and the real Briefcase integration, while an existing IAM device remains connected to its existing service.

Rollback changes only the new deployment, new DNS alias and website promotion. No restore or destructive changes are needed on the still-running IAM store. Keep the new Accounts store and any data written there for diagnosis; do not drop it during rollback.

## Build and inventory evidence

The deployment runs from isolated migration branches and draft PRs; the Accounts services are deployed against new empty databases while the existing IAM services and stores remain unchanged. Read-only infrastructure, database provenance, Accounts configuration before-states and backup availability are recorded privately under the workspace `.migration/live/`. Optimized release builds run in GitHub Actions, and package publication remains coordinated with the platform rollout. The earlier local archives were development builds and are not the production artifacts.

## Verified production rollout (2026-10-10)

The new API and public Accounts website are live. The old IAM API, its store, keys, clients and devices remain intact. No IAM identity or Silicon was imported. All new Accounts webhook signed ping deliveries were delivered. Anonymous protected calls and invalid bearer tokens return401.

The website now uses official Silicon UI registry source at `https://ui.teamofsilicons.com/r/{name}.json`, with source hashes in `web/silicon-ui-registry.json`. App-owned numeric layout aliases preserve existing spacing; Commit and Hook also distinguish deliberate keyboard confirmation from pointer double-click suppression. Authenticated browser checks use genuine app CLI sessions wrapped in the existing encrypted BFF cookie format; this is separate from the verified hosted sign-in redirect contract.

Optimized CLI archives were executed on all six native platforms (including Windows ARM and Intel Mac), with actual help, Accounts discovery, signed-out status and version output. Each GitHub release tag is pinned to its binary source; later website changes have a separate revision. Private source, archive, CI and deployment receipts are under `.migration/live/parallel/commit/` in the operator workspace. The earlier local development archives are not production releases.

Production functional proof: Carbon assigned a todo to the new temporary Silicon. The authenticated browser added a note and deleted the fixture by keyboard confirmation, checked desktop/mobile and light/dark views, and had no page errors after stable UTC timestamp rendering. The BFF API base includes `/api`.
