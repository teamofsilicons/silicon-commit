# Silicon Commit API documentation

This guide describes the v1 HTTP surface. The machine-readable contract is
[`openapi.yaml`](./openapi.yaml), and product intent remains in
[`UNDERSTANDING.md`](./UNDERSTANDING.md).

## Conventions

The production base URL is:

```text
https://commit.teamofsilicons.com/api/v1
```

“Public” work is visible to authenticated members of the same organization; it
is never internet-public. Every product request requires `X-Org-ID`, and Commit
matches that public organization ID to current IAM authority before reading or
writing data. If Commit has previously retained either side of the verified
organization or actor identity, the internal UUIDs and public IDs must match in
both directions; a contradiction fails closed before product data is accessed.

`GET /version` is the sole unauthenticated v1 operation. It exposes only the
service name, API version, build version, and source revision so operators can
verify a deployment. It does not accept organization context or credentials.

Authenticate with exactly one of:

- `Authorization: Bearer <IAM access token>`; or
- `X-IAM-OBO-Access-Proof: <proof>` together with `X-App-ID: <issuer app>`.

OBO support is fail-closed behind IAM extending its currently IAM-only action
catalog with the `commit.*` actions in this contract. Bearer authentication is
the deployable path until that cross-service release gate is satisfied.

Notification settings are self-scoped: only an authenticated Silicon may read
or replace its own destination and subscriptions. Organization management
authority does not grant access to another Silicon's notification settings.

Supplying both mechanisms or only half of the OBO pair is a malformed request
and returns `400`. A missing, invalid, expired, or revoked credential returns
`401` with `WWW-Authenticate: Bearer`. OBO proofs are consumed for their exact
Commit audience, action, organization, and resource scope. Commit supplies IAM
a fresh verification idempotency key on every authentication attempt, so
retrying a consumed proof cannot replay IAM's earlier successful verification.
This provider key is unrelated to any business `Idempotency-Key` below.

Durable todo/project create and append operations, plus todo and project
aggregate updates, require an `Idempotency-Key` of 8–255 visible ASCII
characters. The operation sections and OpenAPI contract identify the exact
endpoints. A key is scoped to the organization, actor, operation, and resource
path. An exact retry within 24 hours replays the stored status and JSON and
returns `Idempotency-Replayed: true`; a fresh response carries `false`.
Reusing a key for different input returns `409`. Todo, project, and project-task
creates also return an absolute canonical `Location`; append-only entries do
not.

Deployments may retain replays longer, but configuration cannot reduce the
public 24-hour guarantee. Browser clients on an allowed CORS origin can read
all declared operational response headers, including `WWW-Authenticate`.

Every response carries `X-Request-ID`. A valid caller-supplied value is reused;
otherwise Commit generates a UUIDv7. Every response also carries
`Cache-Control: no-store`. A `429` response includes `Retry-After` as a minimum
number of whole seconds before retrying. Errors use this shape:

```json
{
  "error": {
    "code": "validation_failed",
    "message": "The request contains invalid data.",
    "request_id": "019...",
    "details": { "title": "must not be empty" }
  }
}
```

`details` is present only when safe structured detail is available. Common
statuses are `400`, `401`, `403`, `404`, `409`, `413`, `422`, `428`, `429`,
`502`, and `503`. Unknown JSON fields are rejected. Titles are limited to 500
Unicode scalar values, project names to 200, descriptions and notes to 20,000,
todo attachments to 20, and project participants to 100. Deployments may lower
these defensive limits. String limits apply to the raw input before
normalization, so surrounding whitespace counts. Required titles, project
names, and note bodies are trimmed for storage and must remain nonblank;
formatting-preserving descriptions and diary Markdown keep their whitespace.
User-authored text rejects U+0000 because PostgreSQL `text` cannot represent
it; callers receive a validation error rather than an internal failure. Public
actor and organization IDs are limited to 255 raw Unicode scalars, trimmed,
required to remain nonblank, and reject control characters.

Todo, todo-note, project, and project-task list endpoints use opaque keyset
cursors, newest first. `limit` defaults to 50 and accepts 1–100. Each list
response contains `items` and nullable `next_cursor`; clients must treat the
cursor as opaque and send it back unchanged.

## Authorization

Read operations are available to any active member of the selected
organization. Mutations use least privilege:

- Any active member may create a todo.
- A todo's assigner, assignee, or a todo manager may change its status.
- Only its assigner or a todo manager may change content, attachments, or
  assignee, or delete it.
- Only its assigner, assignee, or a todo manager may append a note.
- Only a Silicon may manage its own notification settings. A todo-specific
  subscription additionally requires it to be the active delegated todo's
  assigner.
- Only a Silicon may create a project; the creator is added as a participant.
- Current participating Silicons and project managers may mutate project data.
- An organization owner is a manager. Other IAM roles require the explicit
  `commit.todos.manage` or `commit.projects.manage` capability.

A patch containing multiple fields must satisfy every applicable rule. OBO
authentication never adds authority beyond the represented actor.

## Todos

### `GET /todos`

Lists organization-visible todos. Supported query fields are:

- `view`: `assigned_to_me` (default), `delegated_by_me`, or `all`;
- `status`: `completed`, `canceled`, `in_progress`, `blocked`, or `yet_to_do`;
- `assigned_to` and `assigned_by`: exact public IAM actor IDs;
- inclusive RFC 3339 `created_from` and `created_to` bounds; and
- `cursor` and `limit`.

Self-assigned work appears in `assigned_to_me`, never in
`delegated_by_me`. The response contains `items` and nullable `next_cursor`.

### `POST /todos`

Creates a todo. `title` and `assigned_to` are required. `description`, `status`,
and `attachments` are optional; status defaults to `yet_to_do`. Attachments are
unique canonical absolute HTTPS URLs from any image provider. They may contain
a query string, but cannot contain credentials, a fragment, surrounding
whitespace, control characters, or a non-default port, and are limited to 2,048
bytes. `assigned_by` always comes from the verified actor, and the assignee must
be an active Carbon or Silicon in the same organization.

### `GET /todos/{todo_id}`

Returns an organization-visible todo by UUID, including its public assignee ID,
assigner actor reference, status, canonical attachment URLs, and timestamps.

### `PATCH /todos/{todo_id}`

Replaces one or more of `title`, `description`, `assigned_to`, `status`, or the
complete `attachments` set. `description: null` clears the description. An empty
patch is rejected. A meaningful delegated-todo change creates an outbox event
for the assigning Silicon in the same transaction only when that Silicon has a
webhook and the effective notification rule selects the change.

### `DELETE /todos/{todo_id}`

Soft-deletes a todo and returns `204`. It disappears from all public reads
immediately; a repeat delete remains a no-op only for the original assigner or a
current todo manager. There is no restore endpoint. A worker redacts todo
content, notes, and attachment references after the configured retention period
(45 days by default). The deletion stores that deadline permanently, so a later
configuration change affects only future deletions. A still-live linked
idempotent response postpones redaction until the response expires.

## Todo notes

### `GET /todos/{todo_id}/notes`

Lists append-only notes newest first. Notes follow the parent todo's
organization visibility and are unavailable once it is deleted. The endpoint
accepts `cursor` and `limit` and returns `items` plus nullable `next_cursor`.

### `POST /todos/{todo_id}/notes`

Appends a non-empty `body`. The author is the represented actor. This operation
requires an idempotency key and emits the same delegated-todo notification class
as a todo change when applicable.

## Silicon notification settings

Notification configuration consists of one optional Hook destination and an
optional list-wide delegated-todo rule. The two are independent: a Silicon may
store a destination before subscribing, or retain a subscription while its
destination is disabled. No notification event is created unless both a
destination and an effective matching rule exist at mutation time.

A webhook must be the authenticated Silicon's canonical Hook public ingress:
`https://{host}/silicon/{silicon_id}/{endpoint_key}`. The endpoint key is
exactly six uppercase hexadecimal characters. The URL cannot contain
credentials, a query, a fragment, a non-default port, a localhost authority, or
a non-public literal address. The path's encoded Silicon segment must represent
the authenticated Silicon exactly. Commit stores no endpoint signing secret.

Rules use one of these scopes:

- `any_update` selects a meaningful todo patch, note append, or deletion;
- `status_updates` selects only an actual todo status transition; and
- `specific_statuses` selects only a transition into one of its non-empty,
  unique `statuses`.

The only selectable statuses are `completed`, `canceled`, `in_progress`,
`blocked`, and `yet_to_do`. `failed` is not a Commit todo state. Creation,
idempotent replay, and a no-op replacement do not produce an event.

### `GET /notification-settings`

Returns the authenticated Silicon's complete settings and a strong `ETag`.
When no settings have been persisted, the response is the virtual resource:

```json
{
  "webhook_url": null,
  "todo_list_subscription": null,
  "version": 0,
  "updated_at": null
}
```

Its ETag is `"0"`. Carbons receive `403`.

### `PUT /notification-settings`

Completely replaces the Silicon's settings. Both `webhook_url` and
`todo_list_subscription` are required in the JSON document and each may be
`null`. Send exactly one strong, quoted, canonical non-negative integer
`If-Match`; use `"0"` for the virtual resource. A successful change advances
the version once and returns its ETag. A stale request returns `409`, except an
exact stale retry whose desired settings already match returns the current
representation without another version advance.

For example, this enables list-wide status notifications:

```json
{
  "webhook_url": "https://hook.example/silicon/reviewer/A1B2C3",
  "todo_list_subscription": { "scope": "status_updates" }
}
```

Setting `todo_list_subscription` to `null` unsubscribes list-wide. Setting
`webhook_url` to `null` disables delivery.

### `GET /todos/{todo_id}/notification-subscription`

Returns the authenticated assigning Silicon's override resource and ETag for
one active delegated todo. A missing resource is represented as
`subscription: null`, version zero, `updated_at: null`, and ETag `"0"`. Carbons,
other Silicons, self-assigned todos, deleted todos, and unknown todos cannot be
used to access another destination's rule.

### `PUT /todos/{todo_id}/notification-subscription`

Completely replaces the override using the same `If-Match` and stale-retry
rules as Silicon-level settings. The JSON document must contain
`subscription`. A non-null rule is exclusive for this todo: if it does not
match a change, Commit does not fall back to the list-wide rule. Sending
`{"subscription": null}` retains the resource version as an unsubscribe
tombstone but removes the active override, so later mutations fall back to the
current list-wide rule.

Only todos whose `assigned_by` actor is a Silicon and whose `assigned_to` actor
differs are notification-eligible. Patch eligibility uses the resulting
assignment; notes and deletion use the locked current assignment. Personal
todo creation is deliberately excluded because the assigning Silicon already
knows it created the work. Personal todos have no subtodo resource in v1;
project tasks and their nested subtasks are separate, unassigned project work
and do not enter this notification flow.

## Projects

Project paths accept either the server UUID or the exact stable UID. A UID has
the form `{creation-slug}:{creator-public-id}:{utc-unix-milliseconds}`. A bare
slug is not a locator. When placing a UID in a request path, percent-encode it
as exactly one path segment; the decoded locator is limited to 2,048 UTF-8
bytes. Alternate UID spellings are rejected: its slug must be canonical and its
millisecond suffix must be the canonical signed decimal emitted by Commit.

### `GET /projects`

Lists organization-visible projects, optionally filtered by `status` and
participating `silicon_id`. Project states are `completed`, `blocked`,
`canceled`, `in_progress`, and `yet_to_start`.

### `POST /projects`

Creates a project from required `name` and a non-empty, unique `silicon_ids`
array. Every participant must be an active Silicon in the same organization.
Commit adds the creating Silicon if omitted and generates a UUIDv7, immutable
creation slug, and stable UID. The slug is generated once from letters and
numbers in the creation name, transliterated to lowercase hyphenated ASCII, and
bounded to 200 bytes. Symbols do not contribute names to the slug; a name with
no letters or numbers receives the stable `project` fallback. Later name
changes never alter the slug or UID.

### `GET /projects/{project_id}`

Returns the project, public participant IDs, creator, state, and timestamps.

### `PATCH /projects/{project_id}`

Replaces one or more of `name`, `status`, or the complete `silicon_ids` set.
The participant set must stay non-empty and must retain the project's creating
Silicon permanently; historical authorship is preserved when another
participant is removed. `completed` is rejected here—use the completion
operation so state and completion statement remain atomic.

## Project diary

### `GET /projects/{project_id}/diary`

Returns the complete Markdown diary and a strong `ETag` containing its positive
integer version. Every project starts with an empty diary at version 1.

### `PUT /projects/{project_id}/diary`

Replaces the complete Markdown document. Send the last observed ETag in
`If-Match` as one strong, quoted, positive integer (for example, `"3"`). Bare,
weak, wildcard, zero, and duplicate values are rejected. A successful write
increments the version once and returns the new ETag. A stale version returns
the standard error envelope with `409`; a missing precondition returns `428`.
The hard limit is 100,000 Unicode words.

## Project tasks

### `GET /projects/{project_id}/tasks`

Lists tasks and subtasks newest first. Clients reconstruct the hierarchy from
`parent_task_id`. The endpoint accepts `cursor` and `limit` and returns `items`
plus nullable `next_cursor`.

### `POST /projects/{project_id}/tasks`

Creates a task from required `title` and optional `description`, `status`, and
`parent_task_id`. Description defaults to an empty string and status to
`yet_to_do`. A parent must belong to the same project. The v1 model does not
assign project tasks to actors or link them to personal todos.

### `PATCH /projects/{project_id}/tasks/{task_id}`

Replaces one or more of `title`, `description`, or `status`. This endpoint is
not idempotency-keyed in the published v1 contract.

## Project entries

### `POST /projects/{project_id}/blockers`

Appends a blocker with required `title` and `description`; `status` is `open`
(default) or `resolved`.

### `POST /projects/{project_id}/updates`

Appends a milestone update with required `title` and `description`. It does not
implicitly change project status.

### `POST /projects/{project_id}/completion`

Atomically appends the project's one immutable completion statement and changes
the project to `completed`. Repeating the same idempotency key cannot create a
second statement; another completion attempt returns a conflict.

## Attachments

Todo attachment storage is provider-neutral. Creating or updating a todo only
validates and canonicalizes each HTTPS URL; it does not contact the provider.
Todo representations return those canonical URL references. Clients use
external-provider URLs directly when rendering; Commit does not upload, fetch,
or proxy attachment content.

## Durable notification flow

```text
eligible delegated todo change matches its effective subscription
  -> domain change, audit record, event, and routing snapshot commit together
  -> worker leases event
  -> internal Hook ingress receives the versioned, deduplicatable event
     and its snapshotted destination
  -> Hook delivers to the assigning Silicon's public endpoint
```

Delivery is at least once. Failures retry with capped exponential backoff and
eventually dead-letter without rolling back the already-committed todo change.
Each immutable event retains the originating `X-Request-ID`; the worker sends
it to Hook as `trace_id` on every retry and uses the event UUID as Hook's
`Idempotency-Key`. Payload version 2 also freezes the webhook URL, destination
settings version, effective list-or-todo source, scope, and subscription
version selected in the mutation transaction. Later destination changes or
unsubscription affect future mutations only; they do not redirect or cancel a
queued event.

## Deliberate v1 omissions

- Personal todos do not have due dates, priority, dependencies, or watchers.
- Project tasks do not have assignees or explicit ordering.
- Blockers, updates, and completion have no individual read/edit/delete routes.
- Notes are append-only and have no edit/delete routes.
- Activity and restore APIs are not exposed.
- Project and todo visibility is organization-wide rather than configurable.
- Uploading attachment bytes is outside Commit's scope.
