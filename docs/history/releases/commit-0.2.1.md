# Commit 0.2.1 recovery release

Released September 20, 2026. The recovery fixes IAM authentication and canonical
membership handling, CLI session refresh and organization discovery, report
backups, and linked task/todo subtree deletion.

## Publication and deployment

- Recovery implementation: `f001bcd`.
- Versioned backend source: `3e90f9eacd2beec99864621a031a2d5491261568`.
- Publication and native CI source, including safe deployment ordering:
  `97c4eb4ab8cab6f743b664b16f32ae2917282bec`.
- Deployment instructions and documentation version display:
  `eac844e8b8e012c88d5e5d729b93c7285615daca`.
- Honeycomb `tos>commit`: public and active, latest version `0.2.1`; release
  `ef5b0a04-8925-47eb-aaf3-6730d2cb6bf8`.
- Published Rust crates: `silicon-commit-client` and `silicon-commit-cli` 0.2.1.
  Registry API and sparse-index checksums match both uploaded crate archives.
- Production API and worker use the same ARM64 image:
  `234951665042.dkr.ecr.us-east-1.amazonaws.com/silicon-commit@sha256:1b14af8bceac6e6b6ce8f394ae3f8a366bd1dd92596783b079379fb9e3bf0cd9`.
- Production version endpoint reports 0.2.1 and the backend source revision above.
  Health/readiness passed; both processes had zero restarts and no warnings or
  errors in the post-rollout inspection.
- Frontend: Vercel deployment `dpl_3vLTNMpwKJU2DPEAkNAn8cWrE8nR`, aliased to
  <https://commit.teamofsilicons.com>. Public assets match the build, and the IAM
  authorization redirect and anonymous session endpoint passed.
- Docs: <https://docs.commit.teamofsilicons.com>; all 27 public files match the
  generated site. The previous site remains in the Caddy configuration volume.

Honeycomb archive SHA-256:

```text
96ad1e7529e3ef9b2cc1e12da8782d583f59906efbee7bbae083eb133cecb64e  commit-0.2.1.tar.gz
```

Anonymous installation succeeded. Its macOS ARM executable matches the package
checksum `ba43ff791730c44e8269e331a401d3a318819429dd0a311872e1654217237547`.
The affected workspace's existing Honeycomb installation was updated to 0.2.1 and the actual
managed command passed version, login status, todo list and project list checks.

## Data recovery and verification

The old API and worker were stopped before migration. A quiesced custom-format
PostgreSQL dump was created and its restore index checked before bootstrap ran.
RDS snapshot `silicon-commit-before-0-2-1-20260920` reached `available`.
Protected dump and prior runtime configuration:
`/opt/commit/backups/before-0.2.1-20260920` on the existing Commit host.
Quiesced dump SHA-256:
`9a6b9a81428e3142ba616cca030192b0787ff9439863122eabe54db75d11d2cc`.

Migrations 0028–0030 completed. Before live verification mutations, actor, todo,
project, task, testing organization mapping and idempotency record counts were
unchanged. Every retained membership became canonical `actor[org]`.
The sandbox encryption key was unchanged. Runtime grant checks passed.

The affected production Silicon session refreshed its legacy tokens, persisted
expiry and selected its authorized organization automatically. Live CLI checks passed for
assigned todo creation, idempotent replay, reading, status update, listing without
an organization flag, deletion, and rejection of reads after deletion. The
verification todo was removed; no real work was changed.

The missing Honeycomb lifecycle transport was provisioned using the deployment
operator's credentials. Only Commit's participant entry and paired token were
added to Honeycomb's existing registry. Its other participants and live settings
were preserved. Commit's API and worker reloaded their transport configuration
without changing their image or stable sandbox encryption key. The runtime
instance received no secret-store write permissions. A dedicated application-owned
sandbox reached `ready` for Commit, Honeycomb and IAM with Commit release 0.2.1.

Fresh Carbon and Silicon identities both logged in through the published CLI.
Live project checks covered a three-level task tree, independent sibling updates,
task-to-todo status synchronization, diary writes and reads, and retained history.
Deleting a parent linked todo removed all five tasks in its assigned and unassigned
subtree and all three linked todos. Independent sibling tasks and their linked todo
survived, deleted todo reads returned 404, and the earlier history snapshot remained
unchanged.

Local coverage includes 189 Rust tests with real PostgreSQL workflow and retained
data migration regressions, 12 frontend tests, and seven deployment ordering and
failure-recovery tests. Strict Clippy, formatting, dependency policy, runtime
permissions, OpenAPI validation and documentation checks passed.

- [All regular CI jobs passed](https://github.com/teamofsilicons/silicon-commit/actions/runs/35521121257).
- [The deployment documentation and docs version follow-up also passed CI](https://github.com/teamofsilicons/silicon-commit/actions/runs/35523075793).
- [All six native builds, executable smoke tests and package validation passed](https://github.com/teamofsilicons/silicon-commit/actions/runs/35521266306).
  CI used published Honeycomb CLI 0.2.3; the public archive was validated locally
  with the installed Honeycomb CLI 0.2.4.

The backend was cross-compiled for Linux ARM64 with a glibc 2.28 baseline, then
assembled in the existing Debian runtime on the production ARM64 host. Shared
library resolution and native Linux ARM CLI execution passed there. Temporary
repository-scoped ECR push permission was removed after upload. The private,
encrypted release staging bucket expires `staging/` objects after seven days.

## IAM sandbox compatibility

Sandbox verification exposed an IAM application-selector transport inconsistency:
OAuth directory responses contained UUID memberships while root-key responses
contained canonical memberships. Compatible IAM patch
`08cf3df040e740f9c27339cf71acfd2ef04e7a47` restores the missing transport layer.
Its [full CI passed](https://github.com/teamofsilicons/silicon-iam/actions/runs/35523310549),
and both public IAM APIs now report that revision. API, scoped API and worker
stability checks passed. All four runtime environment hashes and both database
schema fingerprints were unchanged; no migrations ran. The deployed image is:
`234951665042.dkr.ecr.us-east-1.amazonaws.com/silicon-iam-production@sha256:f4b035ed59f96eb5465c11e870c72ada61ec5fb2cd74202d848812e8bcb8ad0b`.
The exact SDK 2 application-selector/OAT directory request now returns canonical
Carbon and Silicon memberships. The published CLI passed Carbon-to-Silicon and
Silicon-to-Carbon assignment, standalone and self-assigned todo creation,
bidirectional task/todo status synchronization, and the mixed subtree deletion
regression. Independent tasks and todos survived; deleted linked todos returned
404. Both test sessions refreshed successfully. The affected production workspace's
installed CLI also passed its checksum, version, authenticated status, todo list
and project list checks after the IAM rollout.

The deployed browser UI passed sandbox sign-in, organization selection, standalone
self-todo creation, title and description edits, status update to in progress,
persistence after a full page reload, and deletion. The verification todo was absent
after deletion. The browser was logged out and exited testing mode afterward.

The dedicated environment was then deleted. Honeycomb read-back reported `deleted`
with no pending operation and all three participants ready on the deletion revision;
Commit's persisted operation receipt reported `completed`. Previously valid IAM
test application credentials and both Commit test sessions returned 401 afterward.
Temporary credential files were removed. Sanitized workflow results and browser
screenshots were retained locally. No other testing environments were changed.

## Remaining upstream boundary

OBO verification is bound to the real HTTP method, path and body. Self-assignment
uses the verified caller snapshot. Assigning to other actors through OBO requires
IAM delegated-directory authority; it currently returns 403. Ordinary bearer
assignments are supported and were verified live.
