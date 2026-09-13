# Silicon IAm integration

Commit authenticates through the configured IAm application (`COMMIT_IAM_APP_ID`, `COMMIT_IAM_APP_SECRET`, and `COMMIT_IAM_BASE_URL`). Store application secrets only in the deployment secret store or an ignored local `.env` file. IAM membership and capability responses are authoritative for organization access. Configure the Commit webhook callback in IAm using the deployment endpoint and the separately provisioned signing secret.

`GET /api/v1/iam` is public and returns `app_id` and `iam_url` from the deployment's IAM configuration. It never returns application secrets; an unavailable IAM session service returns 503.

`GET /api/v1/auth/status` accepts a bearer token, optional `X-Org-ID`, and optional `X-Testing-Environment-Key`. It verifies current authorization using the official IAM SDK. Success returns `authenticated: true`, `app_id`, `actor: {type, id}`, `org_id`, and `organizations`. An explicit organization must match the live grant. Without one, status checks all selected active organizations; `org_id` is null if multiple are available. Missing/rejected credentials or no active organization grant return 401. IAM/network failures retain their error status. The response contains neither tokens nor internal principal IDs, and test requests use the linked IAM environment. The client and CLI map 401 to a JSON `authenticated: false` result.
