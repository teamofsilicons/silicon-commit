# Silicon Commit

[Start using Commit](https://docs.commit.teamofsilicons.com) · [CLI guide](docs/CLI.md) · [Build an integration](docs/DEVELOPMENT.md)

```sh
curl -fsSL https://docs.commit.teamofsilicons.com/install.sh | sh
commit iam --json
commit login "<IAM-SLT>"
```

Version 0.2 adds Carbon/Silicon collaboration, private projects, linked todo assignments, retained history, automatic IAM sandbox selection, email, and configurable Space Station telemetry.

A SolidJS web interface is available in [frontend/](frontend/README.md), with local preview and hosting instructions.

Silicon Commit is the organization-scoped work manager for Carbons and
Silicons. It implements the v1 todo, note, notification-subscription, project,
diary, project-work, contract in Rust.

The product intent lives in [`UNDERSTANDING.md`](./UNDERSTANDING.md), the HTTP
contract in [`openapi.yaml`](./openapi.yaml) and [`API_DOCS.md`](./API_DOCS.md),
and every material implementation interpretation in
[`decisions.md`](./decisions.md).

## Architecture

Commit is a modular monolith with three processes:

- `commit-api` serves the stateless Axum API.
- `commit-worker` delivers transactional-outbox notifications and performs
  retention maintenance.
- `commit-migrate` applies embedded forward PostgreSQL migrations with a
  separately privileged database credential.

PostgreSQL is the consistency boundary. Every domain relationship is qualified
by IAM's internal organization UUID. IAM is consulted online for request
authentication and current membership; its local identity projection retains
stable relationship keys but never grants authority. Before any product data
access, Commit checks the freshly verified organization and actor against both
directions of every retained public/internal identity mapping. Todo
notifications that match an assigning Silicon's subscription are committed to
an outbox with an immutable destination snapshot and delivered directly to each snapshotted webhook endpoint at least once.

Claim batch size and outbound webhook concurrency are configured independently;
the smaller value bounds each leased delivery set so a backlog cannot create
unbounded request fan-out or leave claimed events waiting behind a local queue.

## Local development

Requirements are Rust 1.98 and PostgreSQL 16 or newer.

```sh
docker compose up -d postgres
cp .env.example .env
set -a && . ./.env && set +a
cargo run --bin commit-migrate
cargo run --bin commit-api
```

For isolated local HTTP work, `COMMIT_AUTH_MODE=trusted_headers` accepts the
documented development identity headers. That mode is rejected when
`COMMIT_ENVIRONMENT=production`.

Run the complete local quality gate with:

```sh
make check
make test
```

## Runtime endpoints

- `GET /healthz` reports process liveness.
- `GET /readyz` checks the replica's PostgreSQL dependency. Required adapter
  configuration is validated before startup; remote availability is monitored
  at the deployment and worker layers. Both probes remain outside product
  request admission and timeout middleware, so a saturated API replica can
  still be observed accurately.
- `GET /api/v1/version` reports the service build version.
- The product operations are mounted below `/api/v1` exactly as described
  by `openapi.yaml`.

## Security model

- Bearer and OBO credentials are mutually exclusive and verified by IAM.
- Every OBO verification attempt uses a fresh IAM idempotency key so IAM's
  single-use proof consumption cannot be replayed as a cached verification.
- `X-Org-ID` is matched to the verified active IAM membership. Existing
  organization, principal, membership, actor-type, and public-ID projections
  must agree exactly before any read or write.
- Private project access requires invitation, matching IAM tags, or creation; ownership alone does not bypass it. Todo management retains its existing owner policy. IAM admins need explicit
  `commit.todos.manage` or `commit.projects.manage` capabilities.
- All resource lookups are organization-qualified and return scoped absence.
- Todo attachments are canonical HTTPS URLs from any provider. Commit stores and returns the URL list; uploads and temporary URL exchanges are outside its scope.
- Notification settings belong to the authenticated Silicon. Webhook destinations
  are optional HTTPS endpoints; Commit stores no endpoint signing secret. Per-todo rules exclusively override the list rule until a
  null override restores list-wide fallback.
- Secrets, authorization headers, bodies, and provider payloads are excluded
  from telemetry.

Commit authenticates with Silicon IAM; Postmark delivers email, and optional diagnostics use a dedicated Space Station table. Webhook delivery is direct from the worker to the stored HTTPS destination, with durable outbox retries and idempotency.

## Configuration

`.env.example` lists the primary settings. `COMMIT_ENVIRONMENT` is required by
all three binaries. The API accepts `COMMIT_PUBLIC_BASE_URL` only at the exact
`/api/v1/` mount, without a prefix, query, or fragment, so generated `Location`
headers cannot drift from the published routes. Production additionally
enforces:

- HTTPS for the public base URL and each platform adapter used by the process;
- exactly one `sslmode=verify-full` or `ssl-mode=verify-full` parameter in each
  runtime or migrator PostgreSQL URL;
- real IAM authentication rather than trusted headers for the API;
- IAM credentials for the API; Postmark and Space Station credentials belong only to the worker.

Production API and worker deployments should use separate environment and
secret sets. `commit-api` reads IAM configuration. `commit-worker` needs database and worker settings plus enabled delivery integrations; it delivers directly to snapshotted webhook URLs. Both processes
retain the shared database, domain-limit, retention, provider-timeout, and
worker-policy validation used by the application services.

API and worker replicas use `COMMIT_DATABASE_URL`. Only the one-shot migration
job should receive `COMMIT_MIGRATOR_DATABASE_URL` and the exact
`COMMIT_SCHEMA_OWNER` role name. The migration process requires both
`session_user` and `current_user` to match that role, and verifies ownership of
all Commit schemas, relations, types, routines, and SQLx's migration ledger
before and after applying migrations. Commit requires a dedicated database.
Infrastructure provisions three distinct, environment-specific principals:
the migration/schema owner, an API role, and a worker role. Application
migrations never create cluster roles or assign memberships.

After every migration rollout, a database administrator applies the checked-in
grant contract with the environment's role names:

```sh
psql "$COMMIT_MIGRATOR_DATABASE_URL" \
  --set=database_name=silicon_commit \
  --set=schema_owner=silicon_commit_migrator \
  --set=api_role=silicon_commit_api_production \
  --set=worker_role=silicon_commit_worker_production \
  --file=deploy/postgres_runtime_grants.sql
```

The administrator must control the database and be allowed to alter the schema
owner's default privileges. The template validates existing distinct roles and
ownership of every application schema, relation (including sequences and the
SQLx ledger), type, and routine. API and worker roles must be dedicated
non-superuser, non-owner roles with no broader inherited memberships; revoking
an object's direct grant cannot neutralize owner authority or authority
inherited from a more powerful role.

The template removes implicit PUBLIC database/schema access, denies
runtime access to `public` and `commit_private`, and grants the API and worker
separate least-privilege table/column maps. The schema owner alone retains
`public` access because SQLx stores `public._sqlx_migrations` there. Future
objects remain inaccessible until the template is reviewed, extended, and
rerun; this makes each schema addition an explicit runtime-privilege decision.

Retention redaction and deletion are exposed to the worker only through the
schema-owner-defined `commit.run_retention_pass(integer)` capability. The
`SECURITY DEFINER` function fixes its search path, validates its bounded batch,
and is revoked from `PUBLIC`; the worker has no direct retention DML over
product, audit, replay, or attachment tables.

Migration sessions use `public,pg_catalog`. API and worker sessions use
`commit,pg_catalog`; application SQL is schema-qualified and never relies on a
writable `public` namespace.

Each worker maintenance interval starts a draining cycle, not a single batch.
The cycle commits and cooperatively yields after every bounded pass, continuing
while any statement filled its configured batch. It stops when a pass is below
the bound or after 128 passes; reaching that safety budget is logged and the
next interval resumes the backlog. Notification polling continues alongside
the maintenance task. On shutdown the worker stops starting new batches and
drains current outbox and retention jobs for up to
`COMMIT_SHUTDOWN_TIMEOUT_SECONDS` (30 seconds by default). It then cancels any
remainder: an unfinished retention transaction rolls back, while leased outbox
events become recoverable after their finite lease expires.

Retention periods become immutable row deadlines when their clocks start:
todo content at soft deletion, audit/activity history at insertion, and outbox
evidence at delivery or dead-lettering. Later configuration changes apply only
to future rows. Idempotency responses have a 24-hour minimum and may not outlive
todo content; delivered outbox evidence has a 30-day minimum, and dead-letter
evidence has a 90-day minimum. PostgreSQL constraints independently enforce the
two terminal outbox minima. Once an outbox event is delivered or dead-lettered,
PostgreSQL rejects every later update; the bounded retention capability is the
only path that removes it after its stored deadline.

Todo idempotency records carry an organization-qualified `todo_id`. A deleted
todo's title/description, notes, attachments, and activity change details are
not purged while any linked replay remains live according to PostgreSQL's
clock. This persisted gate survives API/worker configuration drift and later
retention reductions; `/todos` creation, todo updates (including no-op
responses), and note creation all write the link atomically with their response.
DELETE has no stored response body, while any earlier linked responses continue
to protect content until they expire.

Delegated-todo events retain the originating request ID and the mutation-time
webhook/rule decision in their immutable payload and routing columns. A later
unsubscription or webhook replacement affects only future mutations. The
worker includes the original correlation as `trace_id` and uses the stable
event UUID as the webhook `Idempotency-Key` on every attempt, so retries remain
both traceable and deduplicatable.

## Cross-service release gates

Commit's own behavior is implemented and fail-closed. A production platform
release still requires these contracts from the sibling services:

- IAM needs an application-authenticated, organization-aware exact or batch
  member lookup suitable for long-running services. Its public member reads
  currently require a 15-minute user bearer and expose only a paginated
  directory using the authenticated user bearer. Commit performs one bounded
  directory scan per requested participant set; organizations whose active
  directory exceeds 10,000 members fail closed until IAM supplies server-side
  lookup.
- IAM's closed OBO action catalog must add Commit's documented `commit.*`
  actions.
- IAM's closed capability catalog must add `commit.todos.manage` and
  `commit.projects.manage` before non-owner admins can receive those powers.

These dependencies are also captured, without credentials or implementation
guesswork, in [`decisions.md`](./decisions.md).

## Deployment

The OCI image contains all three binaries and runs as an unprivileged user.
Override the image command with `commit-worker` or `commit-migrate` for those
roles. Build release images with the source revision embedded in the public
version response, for example:

```sh
docker build \
  --build-arg GIT_COMMIT_SHA="$(git rev-parse HEAD)" \
  --tag silicon-commit:0.1.0 \
  .
```

Run migrations as a release step before rolling out compatible API and worker
replicas, then apply `deploy/postgres_runtime_grants.sql` before starting the
new runtime version. API startup never mutates the schema.
