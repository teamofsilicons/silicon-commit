# Silicon Commit engineering decisions

This append-only log records material product interpretations, architecture,
security, data-model, API, and operational decisions. A superseded decision is
kept and marked as such rather than silently rewritten.

## D-001 — Published intent and contract are authoritative together

**Status:** Accepted

`UNDERSTANDING.md` defines product intent, while `openapi.yaml` and
`API_DOCS.md` define the v1 HTTP surface. Commit implements all 20 documented
operations without inventing public CRUD endpoints for acknowledged contract
gaps. Where the documents are silent or inconsistent, this log supplies the
implementation rule. Contract changes must update the OpenAPI document, prose,
implementation, and contract tests together.

## D-002 — Public means authenticated organization-visible

**Status:** Accepted

Todos, notes, projects, diaries, and project work are visible to active members
of their organization, not to the public internet or other organizations.
Every domain query is qualified by the verified organization. `X-Org-ID` is a
requested context only and is never trusted without IAM confirmation.

## D-003 — Modular monolith with independently scalable processes

**Status:** Accepted

One Rust package contains domain, application, infrastructure, HTTP, and worker
modules. Thin `commit-api`, `commit-worker`, and `commit-migrate` binaries share
the library. PostgreSQL transactions remain the consistency boundary while API
and outbox workers scale independently. Internal microservices are deferred
until measured workload boundaries justify them.

## D-004 — PostgreSQL is authoritative

**Status:** Accepted

PostgreSQL 16 or newer stores work data, idempotency responses, audit records,
and notification outbox events. Foreign keys, checks, unique indexes, and
transactions enforce invariants where possible. Every tenant-owned query also
includes `org_id`; composite organization-qualified relationships prevent
cross-tenant attachment even if application policy regresses.

## D-005 — IAM is checked online and fail-closed

**Status:** Accepted

Commit never implements login or local production identities. Bearer tokens are
introspected with Silicon IAM application credentials on every request so token
revocation and current membership take effect immediately. OBO proofs are
verified with IAM and bound to the Commit audience, requested action, actor,
organization, resource when present, and expiry. `X-App-ID` is mandatory for
OBO despite its omission as a formal OpenAPI parameter.

A trusted-header identity provider exists only for test/development and startup
rejects it in production. External dependency failure does not grant access.
Known IAM directory states that remove authority (`disabled` organization or
`removed` membership) are treated as absence and therefore as `401` during
authentication. Structurally inconsistent identifiers and unknown enum or
status values remain invalid upstream responses rather than being mistaken for
revocation.
IAM credential endpoints reporting an absent, expired, consumed, or conflicting
grant are likewise normalized to `401`; dependency authorization failures remain
unavailable rather than being attributed to the caller.

## D-006 — Assignment and participation stay inside the organization

**Status:** Accepted

Every assignee must be a current Carbon or Silicon in the request organization.
Every project participant must be a current Silicon there. Commit resolves
these facts through IAM before mutation and stores actor type alongside the
public actor ID, even though the v1 Todo response exposes `assigned_to` as an
untyped string. Cross-organization assignment is rejected.

## D-007 — Mutation authorization is least privilege

**Status:** Accepted

Any active organization member may create a todo and append a note. The todo
assigner, assignee, or an organization owner/admin may change status. Only the
assigner or owner/admin may change content, attachments, or assignee, and only
the assigner or owner/admin may delete. A mixed patch requires authority for
every field it changes.

Only a Silicon may create a project and its creator is always a participant.
Current participant Silicons and organization owners/admins may mutate project
metadata, diary, tasks, blockers, updates, and completion. Other organization
members have read-only project visibility. Removing a participant preserves
historical authorship.

## D-008 — IDs, slugs, and project UIDs have separate purposes

**Status:** Accepted

Persistent resources use server-generated UUIDv7 IDs. A project slug is a
normalized display locator derived from its current creation name and remains
immutable across renames. The documented UID is
`{slug}:{creator_silicon_id}:{utc_unix_milliseconds}` and is stable. Project
path lookup accepts the UUID or exact UID, never a non-unique slug. Public IAM
identifiers are bounded by Unicode scalar count consistently in Rust and
PostgreSQL; the composed UID is bounded to 2,048 UTF-8 bytes. Because an opaque
creator ID can contain reserved path characters, clients percent-encode the
whole UID as exactly one path segment.

## D-009 — Lifecycle rules preserve the documented states

**Status:** Accepted

Missing todo and project-task status defaults to `yet_to_do`; missing project
status defaults to `yet_to_start`. The v1 API permits explicit reopening and
other documented status transitions because the product contract defines no
state graph. Project completion is exceptional: it atomically writes the one
immutable completion entry and sets status to `completed`. Generic project
PATCH cannot set `completed`; this prevents completion without its statement.

## D-010 — Project task hierarchy is acyclic and project-local

**Status:** Accepted

A subtask parent must exist in the same project. Because v1 cannot change a
parent after creation, a cycle cannot be introduced. Task creation and updates
require non-empty titles; descriptions may be empty but are always represented.

## D-011 — Diary replacement uses optimistic concurrency

**Status:** Accepted

Every project begins with an empty diary at version 1. `PUT` replaces the whole
Markdown document only when `If-Match` equals the current integer version, then
increments it once. A stale version returns the standard `409` error envelope.
The limit is 100,000 Unicode words as counted by Unicode word boundaries,
including words in Markdown syntax/content. This is deterministic across API
replicas.

## D-012 — Attachments store only allowlisted permanent URLs

**Status:** Accepted

Commit stores only canonical HTTPS permanent Briefcase URLs and never stores a
temporary CDN URL. Origins are allowlisted by configuration, URL credentials,
fragments, and non-default ports are rejected, and the Briefcase entry UUID is
parsed from the canonical path before the outbound request. Temporary URL
generation forwards the caller's bearer or OBO credentials so Briefcase remains
the resource authorization authority. Uploading bytes is out of scope.

The configured origin and base-path allowlist governs new attachment writes and
temporary-URL issuance. A stored URL is decoded using immutable HTTPS,
canonical-serialization, and `/entries/{uuid}` invariants instead of today's
allowlist, so rotating an origin cannot make historical todos unreadable or
undeletable. Removing an origin blocks future references and access issuance;
it does not corrupt already accepted aggregate state.

## D-013 — Creation and documented updates are transactionally idempotent

**Status:** Accepted

Every endpoint requiring `Idempotency-Key` binds the key to organization,
actor, operation, resource path, and a SHA-256 request fingerprint. The domain
mutation, audit record, outbox record, and replayable status/body commit in one
transaction. A retry with the same fingerprint returns the stored response; a
different request returns `409 idempotency_key_reused`. Records default to a
24-hour retention window.

## D-014 — Cursor pagination is opaque and stable

**Status:** Accepted

Todo, todo-note, project, and project-task pages order by
`(created_at DESC, id DESC)`. Cursors are versioned base64url-encoded JSON
carrying both keys; malformed cursors are validation errors. Date filters are
inclusive, interpreted as UTC timestamps, and `created_from` later than
`created_to` is rejected. Limits default to 50 and are bounded to 1–100.

## D-015 — Silicon notifications use a transactional outbox

**Status:** Accepted with external contract dependency

Changing or deleting a delegated todo, and appending its note, writes a
versioned outbox event in the same transaction when the assigner is a Silicon
and differs from the assignee. Creation does not notify the Silicon that just
created the todo. Workers claim events with leases, deliver at least once,
retry with capped exponential backoff and jitter, and dead-letter after a
bounded attempt count. Payloads contain stable event and todo IDs so consumers
can deduplicate. Database batch size and outbound delivery concurrency are
separate limits, but the effective claim is the smaller value so an event is
never leased merely to wait in an in-memory queue. One replica therefore
executes no more than the configured delivery concurrency at once (16 by
default and 256 at most), and every claimed event can begin before its lease
ages behind unrelated work.

The current Hook contract requires a per-Silicon endpoint key and signing secret
but exposes no discovery or internal publish operation to Commit; the Commit
brief instead names `/silicon/{silicon_id}/`. The adapter therefore uses a
configured URL template and HMAC secret, while its durable boundary remains
stable. Production deployment must supply a Hook-compatible route/credential;
the API never pretends synchronous delivery succeeded.

## D-016 — Deletes are soft internally and absent publicly

**Status:** Accepted

Todo DELETE immediately removes the resource from every public read and returns
204, while retaining a tombstone and audit history for operations and incident
response. There is no public restore operation because none is contracted. A
repeat DELETE remains a no-op only for the original assigner or a current todo
manager; an unknown ID returns 404.

## D-017 — Errors are stable, redacted, and correlated

**Status:** Accepted

Errors use the documented nested `error.code`, `error.message`, and
`error.request_id` shape. Status mappings distinguish 400 malformed protocol,
401 authentication, 403 authority, 404 organization-scoped absence, 409 state
or idempotency conflict, 422 semantic validation, 429 rate limiting, 502
invalid provider response, 503 dependency outage, and 500 internal failure.
Every response carries the same `X-Request-ID`; logs never record credentials or
request bodies.

## D-018 — Rust and operational quality baseline

**Status:** Accepted

The service uses Rust 2024 on pinned stable 1.98, forbids unsafe code, denies
panic-oriented shortcuts and debug macros, applies strict Clippy/rustfmt, and
checks dependency licenses/advisories. Axum/Tokio/Tower provide bounded bodies,
timeouts, concurrency, sensitive-header handling, structured tracing, health,
readiness, and graceful shutdown. Migrations run through a dedicated command,
never implicitly in each API replica. Release builds retain Rust's unwinding
panic strategy so the HTTP panic boundary can return a redacted `500` for an
isolated handler failure instead of aborting the entire API process; process
supervision remains responsible for failures outside that request boundary.

## D-019 — Audit records accompany domain mutations

**Status:** Accepted

Every mutation records organization, authenticated actor, action, resource,
resource ID, request ID, timestamp, and a minimal non-secret change summary in
the same transaction. Audit data is not exposed by v1 because no activity API
is contracted. Default retention is 2,555 days pending compliance review.

## D-020 — Strict validation closes permissive schema holes

**Status:** Accepted

Names and titles are trimmed and must remain non-empty. Projects must retain at
least one Silicon participant. Duplicate participant and attachment values are
rejected. Todo descriptions may be null as PATCH documents; create normalizes a
missing description to null. Unknown JSON fields are rejected. Notes, titles,
descriptions, attachment counts, and body sizes have documented configuration
limits to protect the service even where OpenAPI currently omits a maximum.
Every user-authored string destined for PostgreSQL rejects U+0000 at the domain
boundary, including formatting-preserving descriptions and diary Markdown, so
valid JSON cannot turn a semantic input error into a database-backed `500`.

## D-021 — Explicit IAM capabilities supersede generic admin privilege

**Status:** Accepted; supersedes the `owner/admin` shorthand in D-007

An organization owner is a Commit manager. An IAM admin is a Commit manager
only when IAM reports the explicit `commit.todos.manage` or
`commit.projects.manage` capability for the corresponding action. Generic admin
status, job-role text, tags, application identity, and OBO issuer never imply
Commit authority. OBO retains exactly the represented actor's authority.

Todo notes may be appended only by the assigner, assignee, or a todo manager;
organization-wide visibility is not organization-wide commenting authority.

## D-022 — Cross-service OBO proofs are newly delegated

**Status:** Accepted; supersedes credential forwarding in D-012

Commit never forwards a bearer token or an incoming Commit-audience OBO proof
to Briefcase. It asks IAM to exchange the authenticated actor grant for a new,
short-lived proof bound to audience `silicon-briefcase`, action
`briefcase.file.temporary_url`, organization, and parsed entry UUID. Commit then
sends its own app ID and that proof to Briefcase. Incoming OBO delegation needs
an IAM-supported child grant; if IAM cannot delegate it, the operation fails
closed rather than broadening or replaying authority.

Until that child-grant contract exists, the v1 temporary-URL operation declares
bearer-only security in OpenAPI. Incoming OBO remains supported for the other
product operations; it is not advertised for an operation that must reject it.

IAM's original published contract acknowledges that proof exchange was absent.
The platform's in-progress `/api/v1/obo-access/exchanges` operation is the
required deployment dependency and must be contract-tested before release.

## D-023 — Hook owns per-Silicon endpoint credentials

**Status:** Accepted; supersedes the URL-template/shared-secret detail in D-015

Commit does not provision or retain per-Silicon Hook endpoint keys and signing
secrets. Its worker publishes a minimal event to one authenticated internal Hook
ingress configured by URL and IAM service credential; Hook resolves the target
Silicon and owns endpoint routing. If that internal publish contract is absent,
events remain retryable/dead-lettered in Commit's outbox and production
readiness reports the integration unavailable. A global HMAC key is not used as
a substitute for Hook's per-endpoint secrets.

## D-024 — IAM internal identities back persistent relationships

**Status:** Accepted

Commit stores IAM's internal organization UUID, principal UUID, and membership
UUID in retained local projections. Public `org_id` and actor handles remain
immutable presentation snapshots. Domain foreign keys use the internal IDs and
are organization-qualified. This prevents handle ambiguity and preserves
authorship after membership removal without preserving authority.

## D-025 — Todo tombstones have bounded content retention

**Status:** Accepted; completes D-016

Deleted todos are hidden immediately. A worker purges their title, description,
notes, and attachment references after 45 days by default while retaining the
minimal redacted audit trail for the D-019 audit period. Tombstone retention and
audit retention are independently configurable. A repeat DELETE remains a
no-op only for the original assigner or a current todo manager; the tombstone
does not bypass the endpoint's mutation authority.

## D-026 — Browser origins are explicit and credential-safe

**Status:** Accepted

Cross-origin browser access is denied by default. Deployments may configure an
exact HTTPS origin allowlist; wildcard origins are not accepted with bearer/OBO
headers. CORS permits only the documented methods and request headers and does
not imply authentication or resource authority.

## D-027 — IAM integration follows published wire contracts and declares gaps

**Status:** Accepted with external contract dependencies

Commit sends IAM's exact published introspection, organization, membership,
membership-authorization, OBO verification, and OBO exchange documents. It
passes `X-Org-ID` wherever IAM accepts organization context, validates both
public and internal identity fields, bounds directory pagination, rejects
contradictory responses, and fails closed. An inbound OBO proof cannot be used
as the subject of the currently documented exchange, so child delegation for
that caller is intentionally rejected.

The checked-in IAM contract still lacks four facilities Commit needs for a
complete production rollout: an application-authenticated and renewable
organization directory lookup; catalog entries for Commit's `commit.*` OBO
actions and the downstream `briefcase.file.temporary_url` action; the
`commit.todos.manage` and `commit.projects.manage` capability entries; and
child delegation from an existing OBO grant. IAM's current closed `OboAction`
enum contains only IAM-owned actions, so every Commit-audience proof exchange
and Commit's Briefcase child exchange fails closed until that catalog is
extended. `COMMIT_IAM_DIRECTORY_TOKEN` is an integration bridge; the documented
15-minute user bearer cannot be treated as a durable service credential. Until
IAM publishes those contracts, only owners can receive organization-wide
Commit management and OBO-backed Commit requests cannot complete in production.
No local cache, generic admin role, forwarded credential, or fabricated identity
substitutes for those missing authorities.

## D-028 — Persistent IAM projections reject identity remapping

**Status:** Accepted; strengthens D-024

The tuple of internal organization UUID and public organization ID, and each
organization-qualified principal UUID, membership UUID, actor type, and public
actor ID, is immutable after first observation. Todo and project write paths
first read and compare an existing projection, inserting only when it is
absent. They do not update a tenant-global organization row or actor row on
every mutation; current authorization still comes from IAM, so per-request
freshness writes add contention without granting correctness. A concurrent
insert is re-read and compared. Any unique collision or contradictory mapping
is treated as an invalid provider response (`502`) instead of silently
relabeling historical work.

## D-029 — The migrator owns schema; runtime receives a group role

**Status:** Superseded by D-036

Migrations create a fixed NOLOGIN `silicon_commit_runtime` role, remove PUBLIC
access from Commit schemas and objects, and grant the role schema usage plus
ordinary table DML and sequence access in the public `commit` schema only.
Deployment DBAs grant that group role to API and worker login roles; those
logins receive neither schema creation nor `commit_private` access. The
migrator needs schema ownership and cluster permission to create the NOLOGIN
role. Its session pins `search_path` to `public,pg_catalog` so SQLx always uses
`public._sqlx_migrations`, including before the application schema exists;
runtime sessions use `commit,public`.

## D-030 — Authenticated HTTP is non-cacheable; readiness stays local

**Status:** Accepted; clarifies the readiness phrase in D-023

Every HTTP response includes `Cache-Control: no-store`, including errors and
health responses. This prevents a shared cache from reusing an
organization-scoped response whose identity is carried in authorization and
custom headers.

`/healthz` remains a process liveness check and `/readyz` checks the local
PostgreSQL dependency. Required production integration configuration is
validated before startup, while IAM, Briefcase, and Hook reachability is
reported through adapter failures, outbox state, metrics, and deployment-level
readiness. Probing remote services from every API readiness request would create
cascading eviction during a platform outage. In D-023, “production readiness”
therefore means the release/deployment gate, not the API replica's `/readyz`
response.

## D-031 — Project creation ownership and completion are permanent

**Status:** Accepted; narrows D-009 and strengthens D-007

The creating Silicon remains an active participant for the entire lifetime of
the project; participant replacement may remove other Silicons but never the
creator. A project completion is terminal: the dedicated operation atomically
adds the sole immutable completion statement and changes the status to
`completed`, after which no generic PATCH may reopen the project. Other
non-completed lifecycle states remain freely replaceable because v1 defines no
additional state graph. Domain validation, application checks, repository
guards, and deferred/immediate database triggers enforce these rules.

## D-032 — Committed idempotent responses precede mutable authorization

**Status:** Accepted; clarifies D-013

An exact retry belongs to the authenticated organization and principal that
originally committed it, so Commit returns that stored response before
re-evaluating mutable project participation or management authority. Every
idempotent project mutation first serializes and probes its full idempotency
scope, then a new request acquires the same scope in its write transaction and
authorizes again while holding the project row lock. Losing participation
cannot erase a successful response, while a changed fingerprint still returns
`409 idempotency_key_reused` and no fresh unauthorized mutation can commit.

## D-033 — Retention is explicit, independently bounded, and convergent

**Status:** Accepted; strengthens D-013, D-019, and D-025

Every new audit event and todo-activity row receives an explicit `retain_until`
derived from the deployment's audit-retention setting at write time; the
default remains 2,555 days. A later configuration change applies only to new
history. Tombstone maintenance clears only activity's user-derived `changes`
document after the content-retention window, preserving actor, action, request,
resource, and timestamp metadata until that row's stored audit deadline, when
the activity row is deleted. Redacting a todo's title and
description is maintenance rather than a new domain mutation, so its tombstone
version, update timestamp, identity, assignment, status, and deletion metadata
remain unchanged. Delivered and dead-lettered outbox rows have separate
defaults of 30 and 90 days so diagnostic failures outlive successful delivery
records without becoming permanent storage.

Each maintenance statement locks and affects at most the configured nonzero
batch size in deterministic timestamp-and-ID order and skips rows held by live
transactions. A pass is therefore bounded rather than exhaustive. Every
scheduled cycle commits and cooperatively yields between successive passes
until no statement fills its batch, allowing an expired-row rate above one
batch per interval to converge. A 128-pass safety budget prevents a permanently
saturated source from monopolizing the database; exhausting it is logged and
the next interval resumes. Delivery continues in parallel, shutdown cancels the
cycle, and only its current bounded transaction can require rollback. Delivery
and maintenance batches are capped at 10,000 rows so parsed configuration
cannot defeat the per-transaction bound. Startup also rejects durations that
cannot fit the signed PostgreSQL interval quantities used by persistence, and
requires the outbox lease to exceed the provider request timeout so a live
delivery cannot be reclaimed. Idempotency retention may not exceed todo
tombstone retention, ensuring a replay can never return content after its
aggregate is eligible for redaction. D-039 persists that invariant per todo, so
the startup comparison is defense in depth rather than the cross-process
consistency boundary. Runtime content and collection limits may
be lowered but cannot exceed the public v1 contract caps: 500 title
characters, 200 project-name characters, 20,000 description/note characters,
20 attachments, and 100 participants. The schema deliberately retains
100,000-character and
100-attachment physical headroom for forward migration, but configuration
cannot use that headroom while v1 advertises the lower maxima.

## D-034 — Operational HTTP metadata is part of the v1 contract

**Status:** Accepted; strengthens D-014, D-017, and D-020

Every success response declares `X-Request-ID` and `Cache-Control: no-store`.
Rate-limited responses alone require `Retry-After`; durable idempotent
mutations declare `Idempotency-Replayed`; only addressable todo, project, and
project-task creates declare `Location`; and diary reads and writes return a
strong, quoted, positive-version `ETag`. OpenAPI models those headers and the
standard error body explicitly.
Every `401` also carries the credential-free `WWW-Authenticate: Bearer`
challenge required by HTTP bearer clients; OBO remains a separately documented
alternative authentication mechanism.

`GET /version` is the sole unauthenticated v1 operation and is modeled with an
operation-level empty security requirement. It returns only public build
metadata, allowing deployment verification without weakening the global
authentication requirement for product operations.
OCI builds accept the source revision as a non-secret build argument so the
reported commit identifies the exact binary; an omitted value is explicitly
reported as `unknown` for local builds.

All collection reads use bounded opaque keyset pagination, including nested
notes and project tasks. PATCH properties that are non-nullable in OpenAPI
reject explicit JSON `null`; only the todo description uses null as a defined
clearing operation. Stable project UIDs are exact opaque locators and must be
percent-encoded as one path segment when used in a URL.

## D-035 — Runtime configuration is process-scoped and least-privileged

**Status:** Accepted; strengthens D-005, D-022, and D-023

The API and worker use explicit configuration-loading profiles. API replicas
load listener, IAM application/directory, and Briefcase settings, but never
read Hook's service token. Worker replicas load Hook's publication URL and
service token, but never read IAM credentials, Briefcase settings, API
authentication mode, or listener settings. A runtime profile marker prevents
one process from accepting settings loaded for the other. Database, domain
limits, idempotency/audit retention, provider transport bounds, and worker
policy remain shared because application and maintenance code rely on those
invariants.

Production validation remains fail-closed for every adapter a process
instantiates: the API forbids trusted-header authentication and requires its
IAM credentials and HTTPS IAM/Briefcase endpoints; the worker requires a paired
Hook URL and service token over HTTPS. Common PostgreSQL URLs accept SQLx's
`sslmode` and `ssl-mode` spellings, but exactly one may appear. Production
requires that sole effective value to be `require`, `verify-ca`, or
`verify-full`, preventing last-value-wins query parameters from disabling TLS.

## D-036 — Runtime database authority is deployment-owned and process-specific

**Status:** Accepted; supersedes D-029

Application migrations own schemas and their security baseline but never
create, alter, or grant membership to cluster roles. Infrastructure provisions
distinct environment-specific schema-owner, API, and worker principals. After
each migration, a database administrator applies the checked-in psql grant
contract using those names. This keeps global identity and login policy in the
deployment authority while keeping object privileges reviewable beside the
schema that uses them.

Runtime principals must be dedicated non-superuser, non-owner roles without
broad inherited memberships; object-level revocation cannot cancel authority
obtained through a more powerful parent role. The grant template rejects named
runtime roles that violate those directly observable preconditions, while
deployment remains responsible for any login role placed above a NOLOGIN group.

Commit requires a dedicated database. The grant contract removes implicit
PUBLIC connect, temporary-object, and schema access; only the schema owner keeps
`public` usage/create for SQLx's migration ledger. Runtime roles receive usage
of `commit`, never `commit_private` or `public`. API privileges cover online
projection and product transactions. Worker privileges cover queue delivery
and bounded retention, with column-level UPDATE grants for todo redaction,
activity redaction, outbox lifecycle fields, and row locking on database-
enforced immutable tables. Triggers may maintain timestamps without giving
runtime roles direct authority over private trigger functions.

Migration sessions use `public,pg_catalog`; runtime sessions use
`commit,pg_catalog`. The migrator's default privileges deny PUBLIC access, and
future objects remain unavailable to runtime roles until the explicit grant map
is extended and reapplied. That fail-closed release step is intentional: a new
table, enum, sequence, or function cannot silently inherit production access.

## D-037 — Stored deadlines and mutation clocks preserve temporal truth

**Status:** Accepted; strengthens D-028 and D-033

Todo activity now freezes its audit deadline in `retain_until`, just like the
external audit log. Because Commit is still pre-release, the column, default,
constraint, and retention index are part of the original activity-table
migration rather than an unsafe fictitious full-table backfill over seven years
of history. Maintenance orders and deletes by the stored deadline rather than
recomputing from current configuration. The application writes the configured
deadline explicitly, and the database default remains only a compatibility
fallback.

PostgreSQL's transaction clock predates row-lock waits, so it cannot represent
the time an older transaction finally mutates a row. Version/touch triggers and
direct lifecycle updates use the wall clock at execution and preserve
`updated_at` with `GREATEST` against the locked value. Todo deletion,
project/participant/diary/task transitions, and outbox leases, retries, and
terminal states follow the same non-regression rule. Identity projections are
insert-once comparisons and therefore avoid mutable freshness timestamps on
the hot path. Version increments remain unchanged, and retention-only todo
redaction still preserves the tombstone version and update time.

Nested note listing establishes active-parent visibility and reads its page in
one SQL statement. An absent or deleted parent therefore returns scoped
absence, an active parent with no notes returns an empty page, and a concurrent
delete cannot interleave between a separate visibility check and note read.

## D-038 — IAM participant resolution is batched and ambiguity-safe

**Status:** Accepted; strengthens D-024 and D-027

The identity boundary resolves a set of distinct public actor IDs in one
operation and returns one active member per ID in first-request order. Project
participant validation removes the already-verified Silicon caller, performs
at most one batch resolution for the remainder, and independently revalidates
organization UUID, public organization ID, actor type, principal UUID, and
membership UUID uniqueness before persistence. Todo assignment delegates its
single-ID lookup through the same boundary.

Untyped todo assignment never short-circuits just because the requested public
ID equals the caller's ID: the directory scan must still detect a Carbon/Silicon
label collision. The non-production trusted-header provider may fall back to
its already-authenticated caller only after its configured directory reports no
match; a configured collision still fails.

IAM does not yet publish an exact-ID or batch member lookup, so its adapter
performs one bounded walk of the active organization directory for the entire
requested set instead of one walk per participant. It validates every returned
member and exhausts the pagination walk before succeeding: returning as soon as
each ID first appears would miss a duplicate or a cross-type public-ID collision
on a later page. Missing or ambiguous identities fail the whole request without
returning a partial set. Existing limits remain 100 members per page, 100 pages,
bounded response bodies, validated non-repeating cursors, and exact public-ID
comparison. Consequently an organization directory larger than 10,000 active
members fails closed until IAM provides the production exact/batch lookup
already required by D-027.

## D-039 — Live todo replays are a persisted content-retention gate

**Status:** Accepted; strengthens D-013, D-025, and D-033

A startup comparison between idempotency TTL and tombstone retention cannot
protect historical responses after configuration is lowered or API and worker
replicas drift. Every todo-scoped idempotency record therefore stores a nullable
organization-qualified `todo_id` foreign key. The database requires every
`/todos` resource path to carry that link and non-todo operations to leave it
NULL. Creation (`/todos`), changed and no-op updates, and note creation persist
the link atomically with the complete replay body. Project idempotency remains
unlinked. DELETE has no stored response body; repeat deletion is content-free,
while all earlier linked responses continue to protect the tombstone.

Todo content maintenance may purge notes or attachments, clear activity change
details, or redact title/description only when no linked record has
`expires_at > transaction_timestamp()`. Replay lookup uses the same PostgreSQL
clock and strict expiry boundary, so equality is already unavailable and safe
to retain. The partial organization/todo/expiry index makes the gate bounded by
the still-live replay set. Because Commit is pre-release, the link, constraint,
foreign key, and index are part of the original operations schema instead of a
speculative response-body backfill that could leave historical records
unprotected or stall deployment.

## D-040 — Migration authority requires complete object ownership

**Status:** Accepted; strengthens D-036

`commit-migrate` requires an explicit `COMMIT_SCHEMA_OWNER`. Both PostgreSQL
`session_user` and `current_user` must equal that role before and after SQLx
runs, preventing an API credential, worker credential, or incidental DBA role
from silently becoming an application-object owner. The process checks every
Commit schema, relation (including indexes and sequences), defined type,
routine, and `public._sqlx_migrations` before and after migration. An existing
ownership mismatch stops the rollout rather than attempting an implicit
ownership repair.

The deployment grant contract independently repeats the complete catalog
ownership check before applying revokes and grants. This is necessary because
PostgreSQL owner authority is implicit and cannot be neutralized with `REVOKE`,
and because default privileges affect only objects later created by the named
owner. The administrator remains responsible for intentional, separately
reviewed ownership transfer; the application never changes role membership or
object ownership automatically.

## D-041 — Operational probes bypass product admission control

**Status:** Accepted; strengthens D-030 and D-034

Body limits, the concurrency semaphore, and the product request deadline wrap
only `/api/v1`. `/healthz` and `/readyz` retain request IDs, no-store policy,
CORS policy, panic containment, sensitive-header handling, and tracing, but do
not wait behind saturated product traffic. Readiness's database operation is
still bounded by the runtime pool-acquisition and PostgreSQL statement
deadlines. This lets an orchestrator distinguish a live saturated process from
a dead one instead of receiving a product timeout from its probe.

The CORS exposed-header set includes the credential-free
`WWW-Authenticate: Bearer` challenge as well as the other documented
operational headers. An allowlisted browser can therefore act on the same 401
contract as a non-browser HTTP client.

## D-042 — Versioned protocol tokens have one canonical spelling

**Status:** Accepted; strengthens D-013 and D-034

The public replay promise is a minimum, not merely a default:
`COMMIT_IDEMPOTENCY_TTL_SECONDS` cannot be less than 24 hours and still cannot
exceed todo tombstone retention. Deployments may retain responses longer, but
cannot silently weaken the documented retry guarantee.

Diary ETags are opaque strong validators whose sole accepted syntax is a
quoted positive canonical decimal (`"1"`, `"2"`, and so on). Alternate numeric
spellings such as `"01"` and `"+1"` are rejected even if an integer parser
would produce the same number; Commit only accepts the exact representation it
emits and OpenAPI declares.

## D-043 — Public strings and project locators are canonical at ingress

**Status:** Accepted; strengthens D-008, D-018, and D-034

Limits on public strings apply to the raw Unicode scalar sequence received from
the client, before any normalization. Required text rejects U+0000, counts
surrounding whitespace toward its configured limit, trims that whitespace for
storage, and must remain nonblank. Formatting-preserving optional text keeps
its whitespace but follows the same raw limit and U+0000 rule. Public actor and
organization IDs likewise enforce their 255-scalar limit before trimming, must
remain nonblank, and reject control characters. This ordering prevents padding
from bypassing the advertised input bounds and keeps persisted identity labels
stable.

A project slug is generated once from the validated creation name and never
changes with the display name. Symbol characters are excluded before
transliteration, the result is canonical lowercase ASCII words separated by
single hyphens, and the persisted slug is bounded to 200 bytes. A name with no
lexical letters or numbers receives the stable `project` fallback; an
overlong result is truncated at the bound and cannot end in a hyphen. The
stable UID remains the exact opaque
`{slug}:{creator-public-id}:{utc-unix-milliseconds}` value emitted at creation.
Ingress accepts it only when the decoded value is at most 2,048 UTF-8 bytes,
the slug is canonical, the actor component is nonblank and control-free, and
the millisecond component is its canonical signed decimal spelling. Project
paths accept a server UUID or that exact UID, never a bare slug or an alternate
numeric spelling.

## D-044 — Retention deadlines are frozen and maintenance authority is encapsulated

**Status:** Accepted; strengthens D-033, D-036, D-037, D-039, and D-040

Retention configuration is converted into an immutable row deadline at the
event that starts each retention period: todo content at soft deletion, audit
and activity history at insertion, and outbox evidence at delivery or
dead-lettering. Maintenance compares PostgreSQL's clock with those stored
deadlines; it never recomputes eligibility from the worker's current settings.
A later configuration change therefore affects only future rows. Startup keeps
idempotency for at least 24 hours and no longer than todo content, delivered
outbox evidence for at least 30 days, and dead-letter evidence for at least 90
days. Outbox constraints independently enforce the terminal-evidence minima,
and every later update of a delivered or dead-lettered row is rejected. Only
the owner-defined retention capability may delete that immutable evidence after
its stored deadline.

The schema owner exposes bounded maintenance as the
`commit.run_retention_pass(integer)` `SECURITY DEFINER` capability. It has a
fixed `pg_catalog, pg_temp` search path, rejects batch sizes outside 1–10,000,
and is unavailable to `PUBLIC`. The worker receives only `EXECUTE` on that
function for content redaction and deletion; it does not receive direct
retention DML over product, audit, idempotency, or attachment tables. Trigger
guards and live todo-replay checks remain effective inside the owner-defined
function. This preserves bounded cleanup while preventing a compromised worker
credential from directly removing arbitrary product/history rows or bypassing
their stored todo and audit deadlines.

## D-045 — Online identities must agree with both sides of retained history

**Status:** Accepted; strengthens D-004, D-027, D-028, and D-038

After IAM verifies a caller and before any product read or write, Commit checks
the verified identity against its insert-once projection. An existing internal
organization UUID may map only to its retained public organization ID, and an
existing public organization ID may map only to that UUID. Within the
organization, principal UUID, membership UUID, and the public
`(actor_type, actor_id)` identity must all describe the same retained actor.
Any contradiction fails as an upstream identity error before tenant data is
read. A genuinely unseen pair remains valid and is persisted on its first
write; the transactional insertion path repeats the exact comparison to close
concurrent first-observation races.

Every inbound OBO authentication attempt uses a fresh IAM verification
idempotency key. The key is deliberately independent of Commit's public
business idempotency key and of the proof contents: a deterministic verification
key would allow IAM to replay an earlier successful verification instead of
enforcing a proof's single-use consumption. Commit still verifies the complete
returned issuer, audience, action, organization, resource, actor, consumption,
and expiry tuple before accepting the represented actor.

## D-046 — Deployment mode and externally visible transport are explicit

**Status:** Accepted; strengthens D-005 and D-035

`COMMIT_ENVIRONMENT` is required by the API, worker, and migration processes;
none silently assumes development policy. The API's
`COMMIT_PUBLIC_BASE_URL` must be an absolute credential-free HTTP(S) URL whose
path is exactly `/api/v1/`, with no deployment prefix, query, or fragment. A
missing trailing slash is normalized before validation. Production additionally
requires HTTPS, ensuring every generated `Location` is rooted at the one public
v1 mount described by the contract.

Every production runtime or migrator PostgreSQL URL must contain exactly one
`sslmode` or `ssl-mode` parameter whose value is case-insensitively
`verify-full`.
Modes that encrypt without verifying the server hostname no longer satisfy
production policy, and duplicate spellings are rejected instead of relying on
driver-specific last-value precedence. Non-production URLs may use another
supported mode or omit it, but still cannot contain conflicting TLS-mode
parameters.

## D-047 — Worker shutdown is bounded and Hook preserves request correlation

**Status:** Accepted; strengthens D-015, D-017, D-023, and D-033

On a shutdown signal the worker stops scheduling new outbox and retention
batches, then allows the currently running jobs to finish for at most
`COMMIT_SHUTDOWN_TIMEOUT_SECONDS`. If the deadline expires it cancels both job
sets and closes the pool. An interrupted retention transaction rolls back;
claimed outbox rows retain finite leases and another worker recovers them after
lease expiry. The shutdown deadline therefore bounds process termination
without acknowledging unfinished delivery or abandoning durable events.

Every delegated-todo outbox payload records the originating validated request
ID. When a worker claims the immutable row, it promotes an explicit payload
`trace_id`, or otherwise that `request_id`, to Hook's optional top-level
`trace_id`. The same value survives retries and lease recovery. The stable
outbox event UUID is also the Hook `Idempotency-Key`, so downstream logs can
correlate one user mutation across Commit and Hook while Hook deduplicates
at-least-once delivery. Credentials and provider response bodies remain absent
from both the event and retained failure state.

## D-048 — The dependency policy distinguishes private code from distributable licenses

**Status:** Accepted; clarifies D-018

The workspace crate is unpublished proprietary application code, so the
dependency-license gate excludes that private root from third-party license
classification instead of assigning it a false SPDX open-source license. Every
registry dependency remains subject to the allowlist. The allowlist includes
CDLA-Permissive-2.0 for the public root-certificate data used by the platform
TLS verifier. Advisory and source checks remain denying gates; duplicate
transitive versions are reported for review but remain warnings when upstream
dependency graphs require them.

## D-049 — Local credentials never enter source or container contexts

**Status:** Accepted; strengthens D-017 and D-018

Git and Docker build contexts exclude every `.env` variant and common private
key/container formats (`*.key`, `*.pem`, and `*.p12`). The checked-in
`.env.example` is the sole exception and contains placeholders only. Runtime
credentials remain deployment inputs; they are never copied into the OCI image
or committed as developer-specific configuration.

## D-050 — Attachment storage is provider-neutral; temporary exchange is Briefcase-only

**Status:** Accepted; supersedes D-012

Todo attachments accept canonical absolute HTTPS URLs from any image provider,
not only configured Briefcase origins. Ingress rejects credentials, fragments,
control characters, surrounding whitespace, non-default ports, missing hosts,
and values longer than 2,048 bytes; query strings are permitted because image
providers commonly use them for stable transformations. Commit stores the
canonical URL and makes no provider request while creating or updating a todo.

`POST /attachments/temporary-url` remains a deliberately narrower capability.
The supplied `permanent_url` must be an attachment on a visible active todo and
must classify as the exact canonical `/entries/{uuid}` resource beneath a
configured Briefcase base URL. A provider-neutral URL that is not Briefcase is
rejected as semantic input before IAM child-proof exchange or a Briefcase
request. Commit still stores no temporary URL and uploading remains outside its
scope. D-022 continues to govern the Briefcase-audience OBO exchange.

## D-051 — Notification settings are optional, Silicon-owned versioned resources

**Status:** Accepted; supersedes the operation count in D-001 and extends D-034

The authenticated v1 product surface has 24 operations, plus the unauthenticated
`GET /version` operation. Four self-scoped operations expose Silicon
notification configuration: `GET` and `PUT /notification-settings`, and `GET`
and `PUT /todos/{todo_id}/notification-subscription`. Only an authenticated
Silicon may access its own settings. A per-todo resource is additionally
available only while the caller is that active delegated todo's assigner;
organization management authority does not transfer ownership of another
Silicon's notification destination.

The optional webhook is a canonical actor-bound Hook public ingress URL with
the exact `/silicon/{authenticated-silicon-id}/{UPPERCASE-HEX-KEY}` shape,
where the key is six characters. It must use HTTPS and a host, cannot contain
credentials, a query, a fragment, a non-default port, a localhost authority, or
a non-public literal address, and cannot identify another Silicon. Commit
stores the URL but never a Hook endpoint signing secret, and audit summaries
record only whether one is configured.

Both resource types use complete replacement and optimistic concurrency. An
absent resource is represented virtually with version zero, `updated_at: null`,
and `ETag: "0"`; every persisted replacement has a positive integer version.
`PUT` requires the one canonical strong quoted non-negative integer
`If-Match`, and nullable properties must be sent explicitly. An exact stale
retry whose desired configuration already matches returns the current
representation; another stale write returns `409`.

## D-052 — Per-todo notification rules exclusively override list rules

**Status:** Accepted; clarifies the subscription scope in UNDERSTANDING.md

A Silicon's list-wide rule remains active until explicitly replaced with
`null`. `any_update` selects a meaningful todo patch, note append, or deletion;
`status_updates` selects only an actual todo-status transition; and
`specific_statuses` selects only a transition whose resulting status is in its
non-empty unique status set. Creation, idempotent replay, and no-op replacement
do not produce a notification event.

An active non-null per-todo rule is the exclusive rule for that todo. If it
does not match an event, Commit does not fall back to the list-wide rule. A
per-todo `subscription: null` is retained as a versioned unsubscribe tombstone
but semantically removes the override, so future mutations fall back to the
current list-wide rule. The only selectable statuses are Commit's existing
todo states: `completed`, `canceled`, `in_progress`, `blocked`, and
`yet_to_do`; the prose example `failed` does not add an undocumented state.

## D-053 — Notification eligibility and routing are frozen at mutation commit

**Status:** Accepted; supersedes D-015 and D-023, and extends D-034

Commit considers a todo notification only when `assigned_by` is a Silicon and
the todo is delegated to a different actor. Patches use the resulting
assignment; notes and deletion use the locked current assignment. A todo's
creator is not notified of creation. The v1 personal-todo model has no subtodo
resource; project tasks and their nested subtasks are separate unassigned
project work and do not participate in delegated-todo notifications.

When an eligible mutation matches an effective rule and the assigning Silicon
has a webhook, the same database transaction stores payload version 2 plus an
immutable routing snapshot: canonical webhook URL, notification-settings
version, list-or-todo source, effective scope, and source-resource version.
Changing the webhook, replacing a rule, or unsubscribing affects only later
mutations; it neither redirects nor cancels already durable events. The worker
continues at-least-once delivery with the event UUID as its idempotency key and
the original request ID as correlation.

Commit's worker sends that snapshotted destination and rule metadata to one
service-authenticated Hook ingress and never retains or handles the public
endpoint's signing secret. The sibling Hook service does not yet publish that
internal route, so production release remains gated on a compatible ingress
contract accepting payload version 2. Absence or failure of that route leaves
the already-committed event retryable or dead-lettered; it does not roll back
the todo mutation.
