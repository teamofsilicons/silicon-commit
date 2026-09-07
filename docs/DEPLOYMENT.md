# Deployment

Run `commit-migrate` once with `COMMIT_MIGRATOR_DATABASE_URL` and
`COMMIT_SCHEMA_OWNER` set to the schema owner. Run the API and worker with a
separate runtime role. Grant that role the application privileges after every
schema migration:

```sql
GRANT USAGE ON SCHEMA commit TO silicon_commit_runtime;
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA commit TO silicon_commit_runtime;
GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA commit TO silicon_commit_runtime;
GRANT EXECUTE ON FUNCTION commit.run_retention_pass(integer) TO silicon_commit_runtime;
GRANT EXECUTE ON FUNCTION commit.clean_testing_environment(uuid,text) TO silicon_commit_runtime;
GRANT EXECUTE ON FUNCTION commit.purge_testing_environments(integer) TO silicon_commit_runtime;
ALTER DEFAULT PRIVILEGES IN SCHEMA commit GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO silicon_commit_runtime;
ALTER DEFAULT PRIVILEGES IN SCHEMA commit GRANT USAGE, SELECT ON SEQUENCES TO silicon_commit_runtime;
```

Set `COMMIT_IAM_APP_SECRET` in the API secret store. Test-environment IAm root
keys are encrypted with a key derived from that secret; changing the
application secret requires re-encrypting existing test-environment keys
before starting the API.

The API must have `COMMIT_IAM_DIRECTORY_TOKEN` configured in IAM mode. Health
and readiness endpoints are `/healthz` and `/readyz`; the product API is
mounted at `/api/v1/`, and IAM webhooks arrive at `/webhook/`.
