# Silicon Commit API

The API is served under `/api/v1/` (for local development: `http://127.0.0.1:8080/api/v1/`). Use `Authorization: Bearer <Silicon-IAm-access-token>` and `X-Org-ID` for organization-scoped operations. Mutations should include an `Idempotency-Key`; versioned updates use the response ETag in `If-Match`.

Core resources are `/todos`, `/projects`, notification settings, attachments, `/healthz`, and `/version`. The canonical schemas and status codes are in [`../openapi.yaml`](../openapi.yaml) and [`../API_DOCS.md`](../API_DOCS.md).

## Authentication and IAM webhooks

`POST /api/v1/auth/login` exchanges an IAM short-lived token (`slt`) for access and refresh tokens. `POST /api/v1/auth/refresh` rotates a refresh token, and `POST /api/v1/auth/logout` revokes a token family. Mutations require `Idempotency-Key`.


`GET /api/v1/iam` is public and returns `app_id` and `iam_url` from the deployment's IAM configuration. It never returns application secrets; an unavailable IAM session service returns 503.

`GET /api/v1/auth/status` accepts a bearer token, optional `X-Org-ID`, and optional `X-Testing-Environment-Key`. It verifies current authorization using the official IAM SDK. Success returns `authenticated: true`, `app_id`, `actor: {type, id}`, `org_id`, and `organizations`. An explicit organization must match the live grant. Without one, status checks all selected active organizations; `org_id` is null if multiple are available. Missing/rejected credentials or no active organization grant return 401. IAM/network failures retain their error status. The response contains neither tokens nor internal principal IDs, and test requests use the linked IAM environment. The client and CLI map 401 to a JSON `authenticated: false` result.

IAM deliveries arrive at `/webhook/`. Commit verifies `X-Silicon-IAM-Event-Id`, `X-Silicon-IAM-Timestamp`, `X-Silicon-IAM-Key-Version`, and `X-Silicon-IAM-Signature` over `timestamp.body` before parsing or storing anything. Duplicate event IDs are idempotent; reuse with different bytes is rejected.

## Testing environments

Testing environments are created under `/api/v1/test-environments` with `iam_test_key`, the deployment's canonical `iam_app_id`, and the imported testing application's `iam_app_secret`. IAM imports issue a distinct application secret; Commit verifies that test credential before storing it encrypted. The response contains a generated 32-character Commit key. Send that key as `X-Testing-Environment-Key` on every test request; it selects the isolated data plane and its paired IAM application credential for login, refresh, logout, introspection, and OBO calls. Directory reads retain the user's bearer in the same paired IAM environment. Existing client/CLI creation accepts these fields through its JSON body (`commit test-environments create --data @private-create.json`).

The lifecycle endpoints are `GET`/`POST /test-environments`, `GET /test-environments/{id}/key`, `POST /test-environments/{id}/rotate`, `POST /test-environments/{id}/restore`, `POST /test-environments/{id}/clean`, and `DELETE /test-environments/{id}`. Creation and management require the normal authenticated organization authority. Anyone holding the active Commit key may use the environment and clean its data. Environments allow at most 10 projects and 100 todos, expire after 15 days without activity, and remain recoverable for 30 days after deletion.

`PUT /test-environments/{id}/iam-credentials` accepts `iam_app_id` and `iam_app_secret` to repair an existing unpaired environment or rotate its imported app secret. This API requires the production organization's owner or explicit `commit.test_environments.manage` capability and rejects `X-Testing-Environment-Key`. It retains the linked IAM root key, verifies the supplied credential without issuing a user token, returns metadata only, and advances the environment version. Existing unpaired rows return HTTP 409 `testing_environment_iam_credentials_required`; they never use the production application secret. See [Test environments](TEST_ENVIRONMENTS.md) for the upgrade procedure.
