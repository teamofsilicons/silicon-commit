# Silicon IAm integration

Commit authenticates through the configured IAm application (`COMMIT_IAM_APP_ID`, `COMMIT_IAM_APP_SECRET`, and `COMMIT_IAM_BASE_URL`). Store application secrets only in the deployment secret store or an ignored local `.env` file. IAM membership and capability responses are authoritative for organization access. Configure the Commit webhook callback in IAm using the deployment endpoint and the separately provisioned signing secret.

`GET /api/v1/iam` is public and returns `app_id` and `iam_url` from the deployment's IAM configuration. It never returns application secrets; an unavailable IAM session service returns 503.

## Login and current IAM response contracts

`POST /api/v1/auth/login` accepts `{ "slt": "oac_…" }` with an `Idempotency-Key`. Production authorization codes use IAM's `oac_` prefix followed by 43 unpadded base64url characters. Commit forwards the original code and request key to the official IAM client; IAM checks issuance, application binding, expiry and single use. Retry an uncertain exchange with the same code and key. Access tokens, refresh tokens, actor IDs and the obsolete `slt_` prefix cannot start a production login. An existing Carbon/Silicon public ID remains available only after selecting and validating an isolated IAM testing context.

IAM's application-scoped organization response deliberately omits `status`. Commit accepts that projection without inventing an active status, checks its non-nil internal ID and exact organization handle, and still requires live IAM authentication and active member records. If a response includes a legacy `status`, it must be `active`; disabled, unknown or malformed values are rejected. Membership status and role are not optional authority.

## Required read grants

For the complete Carbon/Silicon assignment and project workflow, request and approve the following IAM scopes for `tos>commit`, then obtain fresh user consent. Keep the existing `self.identity.read` and `self.profile.read` grants. These are read disclosures, not Commit management permissions or IAM administration access.

| Additional scope | Why Commit needs it |
| --- | --- |
| `self.membership.read` | Discloses the caller's current organization role. An undisclosed role is rejected; ownership must never be inferred. |
| `self.tags.read` | Discloses the caller's tags for tag-based access to private projects. |
| `self.organizations.read` | Allows selected organization metadata reads and the organization binding in member records. |
| `directory.carbons.read` | Resolves Carbon assignees and project participants in the selected organization. |
| `directory.silicons.read` | Resolves Silicon assignees and project participants in the selected organization. |
| `directory.memberships.read` | Allows the active-member filter and discloses the current status and role needed to validate assignment targets. |

Production login and task authorization are separate: fixing the SLT format does not grant a missing role or directory permission. IAM's application approvals, current consent and active memberships remain authoritative on each operation. This repository change does not edit the deployed application's grants or any user's consent. Existing sessions must reauthorize after a scope change; use a freshly imported test application/world to verify the updated contract rather than assuming an older test import acquired new grants.

The scoped organization decoder correction is needed alongside these grants: adding permissions cannot make an intentionally redacted field appear. See [IAM's scope catalog](https://github.com/teamofsilicons/silicon-iam/blob/fe2a08d9a39802ce738100016d8db5fa126ac275/docs/IAM_SCOPES.md) and [testing environments](TEST_ENVIRONMENTS.md).

`GET /api/v1/auth/status` accepts a bearer token, optional `X-Org-ID`, and optional `X-Testing-Environment-Key`. It verifies current authorization using the official IAM SDK. Success returns `authenticated: true`, `app_id`, `actor: {type, id}`, `org_id`, and `organizations`. An explicit organization must match the live grant. Without one, status checks all selected active organizations; `org_id` is null if multiple are available. Missing/rejected credentials or no active organization grant return 401. IAM/network failures retain their error status. The response contains neither tokens nor internal principal IDs, and test requests use the linked IAM environment. The client and CLI map 401 to a JSON `authenticated: false` result.
