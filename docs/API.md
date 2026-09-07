# Silicon Commit API

The API is served under `/api/v1/` (for local development: `http://127.0.0.1:8080/api/v1/`). Use `Authorization: Bearer <Silicon-IAm-access-token>` and `X-Org-ID` for organization-scoped operations. Mutations should include an `Idempotency-Key`; versioned updates use the response ETag in `If-Match`.

Core resources are `/todos`, `/projects`, notification settings, attachments, `/healthz`, and `/version`. The canonical schemas and status codes are in [`../openapi.yaml`](../openapi.yaml) and [`../API_DOCS.md`](../API_DOCS.md).

## Authentication and IAM webhooks

`POST /api/v1/auth/login` exchanges an IAM short-lived token (`slt`) for access and refresh tokens. `POST /api/v1/auth/refresh` rotates a refresh token, and `POST /api/v1/auth/logout` revokes a token family. Mutations require `Idempotency-Key`.

IAM deliveries arrive at `/webhook/`. Commit verifies `X-Silicon-IAM-Event-Id`, `X-Silicon-IAM-Timestamp`, `X-Silicon-IAM-Key-Version`, and `X-Silicon-IAM-Signature` over `timestamp.body` before parsing or storing anything. Duplicate event IDs are idempotent; reuse with different bytes is rejected.

## Testing environments

Testing environments are created under `/api/v1/test-environments` with an IAM testing key. The response contains a generated 32-character Commit key. Send that key as `X-Testing-Environment-Key` on every test request; it selects an isolated organization data plane and forwards the linked IAM test context. The same routes are available in the Rust client and CLI (`commit --test <key> ...`).

The lifecycle endpoints are `GET`/`POST /test-environments`, `GET /test-environments/{id}/key`, `POST /test-environments/{id}/rotate`, `POST /test-environments/{id}/restore`, `POST /test-environments/{id}/clean`, and `DELETE /test-environments/{id}`. Creation and management require the normal authenticated organization authority. Anyone holding the active Commit key may use the environment and clean its data. Environments allow at most 10 projects and 100 todos, expire after 15 days without activity, and remain recoverable for 30 days after deletion.
