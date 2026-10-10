# Deployment

Prepare release artifacts before the maintenance window. Stop both the API and worker and verify a recoverable
database backup before applying migrations. Migration 0033 (Silicon Accounts) re-keys every row to accounts; older
backends cannot write the migrated schema, so after it begins keep old processes stopped until the database is
restored or the upgraded backend is running.

Run `commit-migrate` once with `COMMIT_MIGRATOR_DATABASE_URL` and `COMMIT_SCHEMA_OWNER` set to the schema owner. Run
the API and worker with separate runtime roles. Grant them the application privileges after every schema migration:

```sh
psql "$ADMIN_DATABASE_URL" -v ON_ERROR_STOP=1 \
  -v database_name=silicon_commit -v schema_owner=commit_migrator \
  -v api_role=commit_api -v worker_role=commit_worker \
  -f deploy/postgres_runtime_grants.sql
```

Use the reviewed role template; do not grant blanket table deletion or routine execution.
`tests/postgres_runtime_grants.sql` checks the result.

The API needs `COMMIT_APP_SECRET`, `COMMIT_ACCOUNTS_WEBHOOK_SECRET` and, for apps that act for accounts,
`COMMIT_PROOF_ISSUERS` ([Silicon Accounts integration](ACCOUNTS.md)). Health and readiness endpoints are `/healthz` and
`/readyz`; the product API is mounted at `/api/v1/`, and Silicon Accounts events arrive at `/webhook/`.

The first deployment of the Silicon Accounts build follows the [cutover runbook](migration/cutover.md): after the
migration, link the existing data with `commit-migrate link-identities`.

The current standalone AWS deployment and release procedure are documented in
[`deploy/aws/README.md`](../deploy/aws/README.md).
