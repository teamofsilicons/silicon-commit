# Commit cutover to Silicon Accounts (operator runbook)

What has to happen, in order, when the Silicon Accounts build of the Commit service goes to production. Nothing
here has been run against production by the migration; every step is for a Carbon (or a Silicon they authorise).

## Before the deploy

1. **App credentials.** Commit's app (`commit`) exists at Silicon Apps / Silicon Accounts. Get its app secret and set
   the sign-in setup (the web's redirect URI, scopes `profile email`) and the app webhook:
   - URL `https://backend.commit.teamofsilicons.com/webhook/`
   - events `account.id_changed`, `account.updated`, `account.deleted`, `membership.signed_out`,
     `membership.access_removed`, `silicon.custodian_changed` (and `ping`)
   - keep the generated `whsec_…` secret.
2. **Deployment secret** (AWS Secrets Manager entry read by `deploy/aws/bootstrap.py`): add `COMMIT_APP_SECRET`,
   `COMMIT_ACCOUNTS_WEBHOOK_SECRET` and `COMMIT_PROOF_ISSUERS` (for example `*=<Silicon Interface app id>`). Optional:
   `ACCOUNTS_URL` / `ACCOUNTS_API_URL` (default `https://accounts.teamofsilicons.com`). The `COMMIT_IAM_*`,
   `COMMIT_HONEYCOMB_*` and testing-environment entries can stay until rollback is no longer needed; they are not
   copied any more. The bootstrap refuses to deploy without the two new secrets.
3. **Silicon Interface switches at the same time.** Commit stops honouring `X-IAM-OBO-Access-Token` / `X-App-ID` the
   moment the new image starts. Interface must call Commit with `Authorization: Proof sap_…` (a User verification
   proof for receiving app `commit`, scopes = the action ids it uses, e.g. `commit.todos.list`). The
   `/api/v1/obo/*` paths keep working for one release with the new credential. Until Interface ships this, its Commit
   features fail with 400 `retired_header`.
4. **Clients.** IAM-era CLI and web builds stop working (contract 1 answers 406). Release the Accounts builds of the
   CLI and web together with the service (later migration stages).
5. **Backup.** Take the usual pre-migration database snapshot. Migration 0033 only adds and re-points columns, but
   the old binaries cannot write the new schema (the account columns are NOT NULL), so rolling back means restoring
   the snapshot and the previous image.

## Deploy

6. Run the normal deploy (`deploy/aws/bootstrap.py`): it drains both services, runs `commit-migrate` (applies 0033),
   applies `deploy/postgres_runtime_grants.sql`, runs the grants contract, then starts the new API and worker.
   0033 refuses to run while two former organisations share a project UID and prints the query that lists them
   (production has one organisation, so this should not trigger).
7. Check `/readyz`, then `GET /api/v1/accounts` (public metadata) and sign in once.

## Link the IAM-era data

Right after the migration every IAM-era row belongs to an unlinked placeholder account, so it is visible to nobody.

8. `commit-migrate link-identities --plan > mapping.csv` (migrator environment plus `COMMIT_APP_SECRET`, so it can
   suggest the account Silicon Accounts knows under each IAM-era id). Check every line: `iam_principal_id,
   accounts_uuid,org_id`. Leave a uuid empty to keep that principal unlinked.
9. `commit-migrate link-identities --file mapping.csv --dry-run` and read the JSON report: links changed, rows
   re-pointed per column, rows kept on placeholders (an account can own one email preference, one settings row…),
   unmatched lines, principals left unlinked, and `public_projects_needing_shares` (formerly organisation-wide
   projects whose former readers are outside the owner's circle now).
10. `commit-migrate link-identities --file mapping.csv` to apply. It is idempotent; running it again with a corrected
    file re-points again without touching work changed after the cutover.
11. Share the projects the report lists with the people who should keep seeing them (add them as members), or leave
    them private to the owner's circle.

## Verify

12. As each linked account: `GET /api/v1/me`, list todos and projects; a Silicon's custodian sees its Silicon's work.
13. Send a test webhook from Silicon Accounts (`POST /v1/apps/commit/webhook/test`) and check the API logs
    (`applied Silicon Accounts webhook event`, outcome `ping`).
14. Interface: one action through a proof (for example list todos) succeeds; the API log records the issuing app.
