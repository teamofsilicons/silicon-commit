# Deployment verification — 2026-09-08

Public endpoint: https://backend.commit.teamofsilicons.com

## Passed

- Workspace tests: 120 backend unit tests and five client transport tests.
- Eight PostgreSQL integration tests run explicitly against a fresh, isolated
  local PostgreSQL instance using `COMMIT_TEST_DATABASE_URL`. These cover todos,
  projects, replay protection, authorization boundaries, retention, notification
  routing, immutable snapshots, migrations, and timestamp behavior.
- Clippy with all targets/features and warnings denied.
- Fresh production migrations and the full runtime-role SQL contract against
  the private RDS database, including rejection of unauthorized worker writes.
- API and worker running with separate unprivileged database roles and verified
  database TLS. Both processes started without application errors.
- HTTPS health and readiness: 200; public version endpoint: 200.
- Real IAM SLT → Commit login: 200. Authenticated todo, project, and test
  environment listings: 200. Refresh: 200. Logout: 204. Revoked-token read: 401.
- Anonymous todo read: 401. Malformed SLT rejected with 400. Unsigned webhook: 401.
- Created an IAM test environment and linked Commit test environment. Verified
  Commit key retrieval, rotation, old-key rejection, cleanup, deletion,
  deleted-key rejection, restoration, and cleanup after restoration.
  Both environments created for this check were retired afterward.
- Namecheap authoritative DNS points to `44.214.143.90`; HTTPS is valid.
- Database CloudFormation import drift detection: `IN_SYNC`.

## Not completed

- Live assignment and participant resolution: the new IAM application token is
  rejected by the administrative directory endpoints, while app-readable directory
  responses lack the internal UUIDs required by Commit.
- Product workflows inside the IAM sandbox: Commit has not implemented the new
  distinct sandbox application secret. Management operations were tested, not a
  complete sandbox todo/project workflow.
- IAM-generated notification delivery: the webhook awaits verified approval in IAM.

No claim is made that all requirements in UNDERSTANDING.md have passed live
end-to-end verification. The endpoint is deployed; these integration gaps remain.
