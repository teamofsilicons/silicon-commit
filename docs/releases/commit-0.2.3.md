# Commit 0.2.3 canonical IAM compatibility release

Released September 21, 2026. Commit accepts IAM 3 canonical identities while
retaining existing ownership, assignments, history, and idempotency records. It
also accepts IAM 2 responses containing the former extra principal UUID.
`silicon-iam-client` 3.0.0 is the registry dependency.

Canonical actor type, public ID, membership, and selected organization resolve
to Commit's existing private row keys. New actors receive private Commit UUIDs;
IAM UUIDs are neither trusted as storage keys nor sent back to IAM. Migration
0031 changes webhook aggregate metadata to text without rewriting event bodies,
signatures, hashes, ownership, or credentials. Login and refresh no longer expose
the undocumented `actor.principal_id`; supported clients use `actor.type` and
`actor.public_id`.

Keep this compatible consumer after it serves traffic, even if IAM rolls back.
Older Commit binaries cannot authenticate newly allocated private actor mappings.
Reverting requires the predeployment database backup and previous runtime, with
explicit handling of later writes; prefer retaining or repairing this consumer.
See the repository's `deploy/iam-3-cutover.md` for the complete cutover contract.

## Linux package compatibility

The 0.2.2 native Linux CLI artifacts accidentally required glibc 2.39. They passed
smoke tests on newer CI runners but failed on the production Amazon Linux host.
The 0.2.2 backend was never deployed. Published immutable 0.2.2 artifacts are
superseded by 0.2.3.

Both Linux builds now use pinned cargo-zigbuild 0.23.4 and Zig 0.15.2 with an
explicit glibc 2.28 target. The packager reads actual ELF dynamic version
requirements and rejects a requirement above 2.28. Both architectures must pass
native execution in a pinned Debian 10/glibc 2.28 container in remote CI.
Regression checks accept both real 0.2.1 Linux binaries and reject both broken
0.2.2 binaries. Six ABI-gate tests cover version ordering, metadata validation,
and packaging rejection.

## Publication and deployment

- Source: `c9cdb234861c184416caaa2a3284017add2baa28`.
- IAM adapter implementation: `60018275a51c45b428f68867420eefeaf79187d1`.
- [Regular CI passed](https://github.com/teamofsilicons/silicon-commit/actions/runs/35581162280).
- [All six native builds and packaging passed](https://github.com/teamofsilicons/silicon-commit/actions/runs/35581162968), using published Honeycomb packager 0.2.3.
- Honeycomb `tos>commit` is public and active at 0.2.3, release
  `9aac9e6c-7612-4347-9574-bf61413ae8b8`.
- Rust crates `silicon-commit-client` and `silicon-commit-cli` 0.2.3 are published.
  Registry API, sparse index, and downloaded archives agree on checksums and
  embed the source revision above.
- Production API and worker use the same ARM64 image:
  `234951665042.dkr.ecr.us-east-1.amazonaws.com/silicon-commit@sha256:caeaf3c3523ef774420ad877b229d8232d2327b4917bc446ea6f09805256d6f2`.
- Frontend Vercel deployment: `dpl_6eAMoiFANrnZb2sRUL8erfe48DFw`, aliased to
  <https://commit.teamofsilicons.com>. Public JavaScript and CSS match the local
  build. Anonymous session and IAM authorization redirects passed.

Honeycomb archive SHA-256:

```text
583e4eccd5366f819ec9770bcc6b3848c7cd7e38e891d18f4894a9db36d76c66  commit-0.2.3.tar.gz
```

All six archive binaries match the native CI artifacts. A clean anonymous
installation passed version, help, and daemon status checks. Its macOS ARM binary
SHA-256 is `b626aa855394caa72a5405422346cc51938642cbfa3c0e353ec0edfa9f2753c5`.
The published Linux ARM CLI also passed version, help, daemon status, and
unauthenticated login-status checks on the actual production host.

## Migration and runtime verification

The existing API and worker were drained before migration. RDS snapshot
`silicon-commit-before-0-2-2-20260921` was available, and a fresh quiesced custom
PostgreSQL dump was created and its restore index verified. The protected backup
is `/opt/commit/backups/before-0.2.2-20260921` on the existing Commit host. Its
quiesced dump SHA-256 is
`740cc6f0cf58af66bd02c9f399d35b8cef3b7cf110953ea6fe3137dbf9ed64ab`.

Migrations 1–31 and the webhook aggregate text type were verified. All 27
application tables had identical row counts and complete row-content fingerprints
before and after migration, checked while both services were stopped. Runtime
permission checks passed, including expected denial of forbidden writes.

The sandbox encryption key and effective API/worker environments were unchanged.
Both containers run without privileges on read-only filesystems; post-rollout
inspection showed zero restarts and zero logged errors. Temporary ECR push
permission and deployment-only credential files were removed. Public health,
readiness, and version checks passed; anonymous todo and project reads with the required organization header return 401.

Rust checks include real PostgreSQL workflows for retained ownership and canonical
IAM-only responses, exact idempotent replay, strict Clippy, and formatting.
Frontend tests, dependency policy, OpenAPI, documentation, runtime grants, and
deployment failure-handling checks also passed.

## Live compatibility checks

Maharaj's managed package was updated through Honeycomb to 0.2.3, preserving its
launcher symlink. Its checksum matches the release, and the existing `chef:bricks`
session passed login status, organization selection, todo listing, and project
listing without modifying real work.

A dedicated application-owned environment
`036b5c48-0aaf-4bdf-83ca-8f1928b88d55` reached ready with Commit 0.2.3. Before the IAM cutover, against
IAM 2, three fresh Carbon/Silicon actors authenticated through the exact
published CLI and the SDK 3 directory returned canonical memberships. Live tests
passed Carbon-to-Silicon and Silicon-to-Carbon assignments, standalone and self
todos, linked-todo updates, diary, and history. Private project owner and assignee
reads returned 200; an outsider received 404 for the project, task list, linked
todo, notes, versions, and snapshot, and could not find the project in listings.
Three original idempotency keys and request bodies replayed with identical
responses and `Idempotency-Replayed: true`, without adding history entries.

The same environment, saved sessions, resources, version snapshot, and idempotency
keys were retained for verification across IAM's canonical cutover.

Before the IAM cutover, browser E2E also passed against IAM 2:
sandbox selection, Carbon login, standalone self-todo creation, title/description
editing, status change to In progress, persistence after a full page reload, and
deletion of that separate UI fixture. The retained project, tasks, history,
sessions, and original todos were unchanged. The signed-in browser was retained;
its testing-context cookie has the existing 15-minute deadline, so a later browser
check may require reauthentication without changing retained CLI sessions.

The documentation site serves version 0.2.3. All 28 public files were compared
against the generated site and matched byte-for-byte.


## Verification after the IAM 3 cutover

IAM 3.0.0 went live at source
`deea75e3d8f9b331bf9ef25e5d39c6546ed5a9fd`, image
`sha256:f799c1172e16be44c1959159f599bc312e5cfcbbf9e45e9ced75132117b2fd7a`.
Before the pause, the three retained Commit token families and the IAM management
family were refreshed normally, without logging in again. Immediately after the
cutover, all three exact pre-pause Commit sessions authenticated with unchanged
tokens and no automatic refresh or replacement login.

The canonical directory passed. Owner and assignee project, task, linked todo,
diary, history, and version-6 snapshot responses exactly matched the pre-cutover
baseline. All six outsider reads remained 404 and the private project stayed
absent from its list. All three original idempotency keys and bodies replayed with
identical responses and replay headers, without adding history. New writes by
the original owner and worker succeeded; the old snapshot remained unchanged.

A new canonical Silicon was created after the cutover with its job description.
It authenticated, received an assignment from the original owner, gained access
to the existing private project, updated its linked todo, and created a self todo.
The retained IAM management session refreshed successfully as part of this test.

Finally, only the worker CLI's local expiry hint was set to zero to exercise its
actual refresh path. Its existing family rotated both access and refresh tokens,
persisted them and the new expiry, retained API, organization, and test-environment
bindings, and passed both CLI and direct authorized reads. The project response
was unchanged by refresh. Maharaj's actual managed 0.2.3 binary and existing
production `chef:bricks` session also passed authentication, organization selection,
and todo/project reads without a new login or real-work mutations.


Browser checks also passed against canonical IAM 3. The test selection/session
had expired, so the same Carbon identity authenticated again in the same retained
environment. This is a reauthentication check, not old-cookie continuity proof.
The retained todo and private project remained visible after reload. A separate
standalone self todo was created, edited, moved to In progress, reloaded to verify
persistence, and deleted; the original baseline todo remained. All retained CLI
session and replay continuity checks preceded these independent UI operations.


Post-cutover cleanup exposed a separate Honeycomb control-plane compatibility gap:
application-owned environment reads rejected the canonical IAM application
identity, and retained application ownership needs compatible resolution. No
cleanup mutation was attempted through an alternate authority. The environment
and its private credentials remain held until the Honeycomb fix is live, after
which deletion and participant completion/credential-denial checks will run.
