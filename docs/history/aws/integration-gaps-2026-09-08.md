# Commit AWS deployment: integration gaps (September 8, 2026)

Moved from `deploy/aws/README.md` unchanged when Commit moved to Silicon Accounts; kept as history.

The API uses `https://backend.iam.teamofsilicons.com/api/v1/` and
`POST /oauth/introspect`. Current authorization snapshots authenticate requests
without using IAM's administrative membership APIs. Snapshot bindings are checked
against the introspected subject, organization, membership, actor type, and audience.
Undisclosed or unknown roles never become elevated privileges, and OAuth scopes
are not treated as Commit management capabilities.

1. IAM application tokens still receive 403 from the existing organization and
   administrative member read endpoints. The app-readable `/directory/*` projection
   omits internal organization, membership, and principal UUIDs. Commit needs those
   identifiers to resolve assignees and project participants. Complete product
   mutation verification therefore remains blocked on the IAM directory contract.
2. IAM sandbox imports issue a separate test application secret. Commit currently
   stores only the IAM environment root key and uses its deployment application
   secret in sandbox requests. Supporting the new isolated application credentials
   needs an agreed provisioning contract and implementation; sandbox product login
   is not ready. Sandbox management and cleanup were verified independently.
3. IAM's `commit` webhook is `pending_review`, with pending URL
   `https://backend.commit.teamofsilicons.com/webhook/`. An eligible IAM operator
   must perform verified step-up approval. The existing signing secret is deployed.

The following release adds current IAM directory usage and automatic sandbox discovery. See [the September 13 release verification](verification-2026-09-13.md) for the deployed behavior, checks, and remaining verification boundaries. The September 8 report remains historical evidence.
