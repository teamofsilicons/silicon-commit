# Commit AWS deployment

Production API: https://backend.commit.teamofsilicons.com/api/v1/

This is a standalone EC2 deployment without a load balancer. Caddy terminates
HTTPS and forwards to the API on `127.0.0.1:8080`. API and worker run as separate
non-root containers. PostgreSQL runs privately in RDS with encrypted storage,
seven days of automated backups, deletion protection, and verified TLS.

## Resources

AWS account `234951665042`, profile `silicon-production`, region `us-east-1`:

- `silicon-commit-edge`: `edge.json`; EC2 `t4g.small`, instance
  `i-0bd8d9688a5a5d252`, public IPv4 `44.214.143.90`.
- `silicon-commit-database`: `database.json`; RDS `db.t4g.micro`, database
  instance `silicon-commit-production`.
- ECR repository `silicon-commit`.
- Secrets Manager secret `silicon-commit/production`.
- Namecheap A record `backend.commit` with TTL 300.

The account's Elastic IP quota was exhausted, so the instance uses an automatic
public address. An OS reboot retains it; stopping and starting the EC2 instance
or replacing it can change it. Update the existing Namecheap record in that case.
Only TCP 80 and 443 are allowed inbound. Administration uses AWS Systems Manager.

The first CloudFormation attempt was rolled back because of the Elastic IP quota.
Its protected database was retained and imported into `silicon-commit-database`;
drift detection confirmed `IN_SYNC`. The replaced host and obsolete security
group were removed. The database and edge templates now represent the two live
stacks independently.

## Release procedure

Build and push an ARM64 image with the source revision embedded:

```sh
aws ecr get-login-password --profile silicon-production --region us-east-1 |
  docker login --username AWS --password-stdin 234951665042.dkr.ecr.us-east-1.amazonaws.com

docker buildx build --platform linux/arm64 --push \
  --build-arg GIT_COMMIT_SHA="$(git rev-parse HEAD)" \
  -t "234951665042.dkr.ecr.us-east-1.amazonaws.com/silicon-commit:$(git rev-parse --short HEAD)" .
```

Use SSM to copy these files to the root-owned `/opt/commit` directory:

- `deploy/aws/bootstrap.py` → `bootstrap.py`
- `deploy/postgres_runtime_grants.sql` → `postgres_runtime_grants.sql`
- `tests/postgres_runtime_grants.sql` → `test_runtime_grants.sql`

Run `python3 /opt/commit/bootstrap.py SECRET_ARN DATABASE_HOST IMAGE_DIGEST` as
root through SSM. Pass the immutable ECR `repository@sha256:...` image reference.
The script retrieves secrets on the server, creates dedicated database roles if
absent, migrates, applies and tests runtime permissions, and replaces the API,
worker, and Caddy containers. Do not run deployments concurrently. Runtime files
are root-readable only; temporary migration and administrator environment files
are removed even when database setup fails. The test role checks intentionally
exercise denied writes inside rolled-back transactions.

The deployment secret contains the confirmed IAM application and webhook secrets,
the IAM backend URL, and generated database passwords. Do not place its values in
CloudFormation, user data, SSM command arguments, or Git. Caddy certificate state
persists in Docker volumes on the EC2 disk. Keep that disk when maintaining the
host; an instance replacement obtains a new certificate after DNS is updated.

Verify `/healthz`, `/readyz`, `/api/v1/version`, IAM login/refresh/revocation,
worker logs, database grants, and DNS after every rollout. An image rollback does
not undo database migrations; review schema compatibility first.

## Historical integration gaps (September 8)

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
3. IAM's `tos>commit` webhook is `pending_review`, with pending URL
   `https://backend.commit.teamofsilicons.com/webhook/`. An eligible IAM operator
   must perform verified step-up approval. The existing signing secret is deployed.

The following release adds current IAM directory usage and automatic sandbox discovery. See [the September 13 release verification](verification-2026-09-13.md) for the deployed behavior, checks, and remaining verification boundaries. The September 8 report remains historical evidence.
