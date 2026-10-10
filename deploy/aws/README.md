# Commit AWS deployment

Production API: https://backend.commit.teamofsilicons.com/api/v1/ (Silicon Accounts events arrive at
https://backend.commit.teamofsilicons.com/webhook/). Documentation: https://docs.commit.teamofsilicons.com.

This is a standalone EC2 deployment without a load balancer. Caddy terminates
HTTPS and forwards to the API on `127.0.0.1:8080`. API and worker run as separate
non-root containers. PostgreSQL runs privately in RDS with encrypted storage,
seven days of automated backups, deletion protection, and verified TLS. The web
app is a separate Vercel project (see [the cutover runbook](../../docs/migration/cutover.md)).

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

Build an ARM64 image with the source revision embedded: the **Build pinned ARM64
backend image** workflow (`backend-image.yml`, on a `release/**` branch) leaves it as
an artifact, or build and push it yourself:

```sh
aws ecr get-login-password --profile silicon-production --region us-east-1 |
  docker login --username AWS --password-stdin 234951665042.dkr.ecr.us-east-1.amazonaws.com

docker buildx build --platform linux/arm64 --push \
  --build-arg GIT_COMMIT_SHA="$(git rev-parse HEAD)" \
  -t "234951665042.dkr.ecr.us-east-1.amazonaws.com/silicon-commit:$(git rev-parse --short HEAD)" .
```

Copy the deployment files to the root-owned `/opt/commit` directory and run the
bootstrap there as root. `host.py` does both through Systems Manager (it uses the
`silicon-production` profile and prints the host's output):

```sh
python3 deploy/aws/host.py copy deploy/aws/bootstrap.py /opt/commit/bootstrap.py
python3 deploy/aws/host.py copy deploy/postgres_runtime_grants.sql /opt/commit/postgres_runtime_grants.sql
python3 deploy/aws/host.py copy tests/postgres_runtime_grants.sql /opt/commit/test_runtime_grants.sql
python3 deploy/aws/host.py run "python3 /opt/commit/bootstrap.py SECRET_ARN DATABASE_HOST IMAGE"
```

Pass the immutable ECR `repository@sha256:...` image reference as `IMAGE`.
The script retrieves secrets on the server, pulls images, prepares environment
files and grant scripts, and creates dedicated database roles if absent. It then
stops both old API and worker processes before migrating, applies and tests runtime
permissions, and starts their replacements. A failure while stopping services
restores only those that were previously running. Once migration starts, a failure
leaves old services stopped because earlier migrations may already have committed.
Do not run deployments concurrently. Runtime files
are root-readable only; temporary migration and administrator environment files
are removed even when database setup fails. The test role checks intentionally
exercise denied writes inside rolled-back transactions.

Before a release with new migrations, stop API and worker and take and verify a
recoverable database backup; bootstrap does not create backups. Migration 0033
(Silicon Accounts) adds account columns that older images cannot write, so after it
runs, recovery means the upgraded image or restoring the backup together with the
previous image.

Two cutover steps need the database, which accepts only this host: draining the
webhook and email queues before 0033, and linking the existing data to Silicon
Accounts accounts after it. `cutover.py` runs both on the host (`queues` with psql;
`plan`, `dry-run` and `apply` run `commit-migrate link-identities` in the deployed image):

```sh
python3 deploy/aws/host.py copy deploy/aws/cutover.py /opt/commit/cutover.py
python3 deploy/aws/host.py run "python3 /opt/commit/cutover.py SECRET_ARN DATABASE_HOST queues"
python3 deploy/aws/host.py run "python3 /opt/commit/cutover.py SECRET_ARN DATABASE_HOST IMAGE plan"
```

The [cutover runbook](../../docs/migration/cutover.md) has the whole sequence:
reviewing the mapping, the dry run, applying it and checking the result.

The deployment secret contains Commit's Silicon Accounts app secret
(`COMMIT_APP_SECRET`), the account webhook secret (`COMMIT_ACCOUNTS_WEBHOOK_SECRET`),
optionally `COMMIT_PROOF_ISSUERS`, `ACCOUNTS_URL` and `ACCOUNTS_API_URL`, and generated
database passwords. Bootstrap refuses to deploy without the two Accounts secrets and
copies no variables of the previous sign-in or packaging systems. Do not place its values in
CloudFormation, user data, SSM command arguments, or Git. Caddy certificate state
persists in Docker volumes on the EC2 disk. Keep that disk when maintaining the
host; an instance replacement obtains a new certificate after DNS is updated.

Verify `/healthz`, `/readyz`, `/api/v1/version`, `/api/v1/accounts`, a signed-in
`/api/v1/me`, an account webhook test delivery, worker logs, database grants, and DNS
after every rollout. An image rollback does not undo database migrations; review
schema compatibility first.

## Documentation

`npm ci --prefix docs-site && npm run build --prefix docs-site`, then
`python3 deploy/aws/deploy_docs.py` publishes `docs-site/dist` into the Caddy container
on the same host and keeps the previous copy as `/config/commit-docs-previous`.

## History

Earlier verification reports and the first deployment's integration gaps are in
[`docs/history/aws/`](../../docs/history/aws/).
