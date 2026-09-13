# Silicon Commit API

The API is served under `/api/v1/` (for local development: `http://127.0.0.1:8080/api/v1/`). Use `Authorization: Bearer <Silicon-IAm-access-token>` and `X-Org-ID` for organization-scoped operations. Mutations should include an `Idempotency-Key`; versioned updates use the response ETag in `If-Match`.

Core resources are `/todos`, `/projects`, notification settings, attachments, `/healthz`, and `/version`. The canonical schemas and status codes are in [`../openapi.yaml`](../openapi.yaml) and [`../API_DOCS.md`](../API_DOCS.md).

## Authentication and IAM webhooks

`POST /api/v1/auth/login` exchanges an IAM short-lived token (`slt`) for access and refresh tokens. `POST /api/v1/auth/refresh` rotates a refresh token, and `POST /api/v1/auth/logout` revokes a token family. Mutations require `Idempotency-Key`.


`GET /api/v1/iam` is public and returns `app_id` and `iam_url` from the deployment's IAM configuration. It never returns application secrets; an unavailable IAM session service returns 503.

`GET /api/v1/auth/status` accepts a bearer token, optional `X-Org-ID`, and optional `X-Testing-Environment-Key`. It verifies current authorization using the official IAM SDK. Success returns `authenticated: true`, `app_id`, `actor: {type, id}`, `org_id`, and `organizations`. An explicit organization must match the live grant. Without one, status checks all selected active organizations; `org_id` is null if multiple are available. Missing/rejected credentials or no active organization grant return 401. IAM/network failures retain their error status. The response contains neither tokens nor internal principal IDs, and test requests use the linked IAM environment. The client and CLI map 401 to a JSON `authenticated: false` result.

IAM deliveries arrive at `/webhook/`. Commit verifies `X-Silicon-IAM-Event-Id`, `X-Silicon-IAM-Timestamp`, `X-Silicon-IAM-Key-Version`, and `X-Silicon-IAM-Signature` over `timestamp.body` before parsing or storing anything. Duplicate event IDs are idempotent; reuse with different bytes is rejected.

## Collaboration and history

Projects are public within the organization by default. Carbon and Silicon creators can set `description`, URL `attachments`, `private`, `carbon_ids`, `silicon_ids`, IAM `tags` (names or IDs), and nested initial `tasks`. Tasks accept an optional `assigned_to`. Updating an assignment creates or updates its linked todo; `assigned_to:null` reopens unassigned work. Todos accept an optional `project_id`. Private project access is checked for reads, writes, history, todo listings and retries.

- `POST /projects/{project}/tasks/{task}/claim`: atomic claim, with Idempotency-Key.
- `DELETE /projects/{project}/tasks/{task}`: remove task, descendants and linked todos.
- `GET /projects/{project}/versions?before=VERSION&limit=50`: retained revision metadata.
- `GET /projects/{project}/versions/{version}`: one complete retained snapshot.
- `GET|PUT /email-settings`: current identity's organization email and event preferences.
- `POST /reports`: authenticated report `{message,pr?}` with Idempotency-Key.
- `GET /contracts`: live [contract governance](CONTRACTS.md).

## Testing environments

Send the imported application's `app_secret` as `X-Testing-App-Secret` or `X-Testing-Environment-Key`; do not send both. `GET /testing-context` validates it live with IAM and returns the environment's name and ID. Use the same header during login, refresh and every operation. A test SLT or existing test public ID can log in; production accepts only SLTs. [Testing instructions and legacy compatibility](TEST_ENVIRONMENTS.md) describe isolation and lifecycle.
