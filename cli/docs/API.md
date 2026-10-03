# Silicon Commit API

The API is served under `/api/v1/` (for local development: `http://127.0.0.1:8080/api/v1/`). Use `Authorization: Bearer <Silicon-IAm-access-token>` and `X-Org-ID` for organization-scoped operations. Mutations should include an `Idempotency-Key`; versioned updates use the response ETag in `If-Match`.

Core resources are `/todos`, `/projects`, notification settings, attachments, `/healthz`, and `/version`. The canonical schemas and status codes are in [`../openapi.yaml`](../openapi.yaml) and [`../API_DOCS.md`](../API_DOCS.md).

## Authentication and IAM webhooks

`POST /api/v1/auth/login` exchanges an IAM short-lived token (`slt`) for access and refresh tokens. `POST /api/v1/auth/refresh` rotates a refresh token, and `POST /api/v1/auth/logout` revokes a token family. Mutations require `Idempotency-Key`.


`GET /api/v1/iam` is public and returns `app_id` and `iam_url` from the deployment's IAM configuration. It never returns application secrets; an unavailable IAM session service returns 503.

`GET /api/v1/auth/status` accepts a bearer token, optional `X-Org-ID`, and optional `X-Testing-Environment-Key`. It verifies current authorization using the official IAM SDK. Success returns `authenticated: true`, `app_id`, `actor: {type, id}`, `org_id`, and `organizations`. An explicit organization must match the live grant. Without one, status checks all selected active organizations; `org_id` is null if multiple are available. Missing/rejected credentials or no active organization grant return 401. IAM/network failures retain their error status. The response contains neither tokens nor internal principal IDs, and test requests use the linked IAM environment. The client and CLI map 401 to a JSON `authenticated: false` result.

OBO requests use `X-App-ID` and `X-IAM-OBO-Access-Token`. Commit verifies the reusable token through IAM's `token-verifications` endpoint using the action ID selected by its handler and the HTTP method and server-matched route. For example, `GET /api/v1/obo/todos/{todo_id}/read` maps to `commit.todos.read`; the unique route template is registered in IAM while Commit enforces permissions on the actual requested todo. Each action has a distinct `/api/v1/obo/` route and retains its documented HTTP method; see the [endpoint catalog](https://github.com/teamofsilicons/silicon-commit/blob/release/iam5-obo-20261003/deploy/obo-endpoints.json). Ordinary REST routes retain their bearer-session contract and do not substitute an OBO alias during verification. The represented member, organization, recipient application and testing plane must match. The same valid token can authorize repeated approved operations; revoked or expired grants fail. Legacy `X-IAM-OBO-Access-Proof` credentials are rejected. The verified snapshot supports assignment to that represented member. Assignments or project participant changes requiring other directory members return 403 because an OBO grant does not provide directory-read credentials. Standard bearer sessions support assignments to other active organization members with the required IAM directory scopes.

IAM 5 release limitation: selecting a different downstream account does not automatically disclose that account’s identity, membership role or tags to Commit. Commit requires that verified authorization context for its resource permissions and rejects the operation when it is withheld. Same-account delegated operations with the required disclosures are supported. Do not broaden IAM permissions or substitute the initiating account to bypass this check.

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
