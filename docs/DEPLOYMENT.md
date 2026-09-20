# Deployment

Prepare release artifacts before the maintenance window. Stop both the API and
worker and verify a recoverable database backup before applying migrations
0029–0030. The membership UUID-to-text migration is incompatible with older
backends; after migration begins, keep old processes stopped until the database
is restored or the upgraded backend is running.

Run `commit-migrate` once with `COMMIT_MIGRATOR_DATABASE_URL` and
`COMMIT_SCHEMA_OWNER` set to the schema owner. Run the API and worker with a
separate runtime role. Grant that role the application privileges after every
schema migration:

```sh
psql "$ADMIN_DATABASE_URL" -v ON_ERROR_STOP=1 \
  -v database_name=silicon_commit -v schema_owner=commit_migrator \
  -v api_role=commit_api -v worker_role=commit_worker \
  -f deploy/postgres_runtime_grants.sql
```

Use the reviewed role template; do not grant blanket table deletion or routine
execution. Configure the [Honeycomb participant](HONEYCOMB.md) for shared sandbox
lifecycle and activity. Test secrets are encrypted with
`COMMIT_TEST_ENVIRONMENT_ENCRYPTION_KEY`, falling back to the existing
`COMMIT_IAM_APP_SECRET` for compatibility. Preserve that key across deployment;
rotation requires re-encrypting retained secrets.

The API uses the authenticated user bearer for IAM directory reads. Health
and readiness endpoints are `/healthz` and `/readyz`; the product API is
mounted at `/api/v1/`, and IAM webhooks arrive at `/webhook/`.

The current standalone AWS deployment and release procedure are documented in
[`deploy/aws/README.md`](../deploy/aws/README.md), including known IAM integration
gaps and the checks performed against the live endpoint.
