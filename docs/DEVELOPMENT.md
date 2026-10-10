# Build on Commit

Use the stateless `silicon-commit-client` Rust crate. Keep the API origin and the account's Silicon Accounts access
token explicit. The same APIs back the CLI and browser. Store accounts by their permanent uuid; `c:`/`si:` ids are
display data and can change.

```rust,ignore
let client = silicon_commit_client::Client::new("https://api.commit.teamofsilicons.com")?
    .with_bearer(access_token);
let projects = client.list_projects(&[]).await?;
```

Another app that acts for an account sends `Authorization: Proof sap_…` instead (a User verification proof for
`commit` with the scopes of the actions it performs, `Client::with_proof`); the deployment must allow that app in
`COMMIT_PROOF_ISSUERS`. A tool that signs Carbons and Silicons in by itself, like the CLI, uses
`silicon_commit_client::auth` (device flow, short-lived tokens, refresh, sign-out; see the
[Rust client](CLIENT.md#sign-in-from-a-tool)).

Use a stable `Mutation::with_key` across retries. Expect 404 for resources you cannot see, 403 with a precise code
when you can see but not change something (or a Silicon has not allowed you), 409 for a claim race or stale diary
version, 401 for invalid credentials, and field-specific validation details for rejected input. Never log secrets or
put them in URLs. See the [API](API.md), [Silicon Accounts integration](ACCOUNTS.md) and [version policy](CONTRACTS.md)
guides for complete contracts.

## Run it locally

The backend is Rust/Axum/PostgreSQL. Run the database migrations before the API or worker, and keep runtime roles
separate from migration authority. Point Commit at Silicon Accounts with `ACCOUNTS_URL` (a local stack may use
`http://localhost:…`), `COMMIT_APP_SECRET` and `COMMIT_ACCOUNTS_WEBHOOK_SECRET`; `.env.example` lists every setting.

To run Commit against a local Silicon Accounts stack (the Silicon Accounts testkit), point
`COMMIT_TEST_STACK` at the stack's JSON file and run `scripts/dev-accounts.sh`: it migrates a local database, starts
the API on `127.0.0.1:4141` and the worker, registers Commit's webhook at the stack and proves a delivery
(`scripts/dev-accounts-stop.sh` stops it and puts the webhook back). `scripts/e2e-accounts.sh` then signs real
Carbons and Silicons in and runs the end-to-end scenarios (API, CLI, device flow, sharing, webhooks, proofs, the
packaged CLI, restarts); [`tests/e2e/README.md`](../tests/e2e/README.md) lists what it needs.

`cargo test --workspace --all-targets` runs unit, HTTP, client and CLI tests. Set `COMMIT_TEST_DATABASE_URL` to a
disposable PostgreSQL 16 database (the role needs CREATEDB for the migration tests) to include the transactional
tests; the HTTP tests sign their own EdDSA tokens and serve a local Silicon Accounts double, so no Accounts stack is
needed.

To try an installable CLI package, build the CLI and pack it the way a release does:
`scripts/package-apps.sh VERSION TARGET target/release/commit` writes a Silicon Apps archive to `dist/apps/` after
checking the three commands every Silicon app answers; [releases](RELEASES.md) explains the whole path.

Diagnostics use the dedicated Space Station table through the worker; clients can opt out per request. `commit report`
submits a report through the Rust client and Commit API for Postmark delivery. Attach a source PR with `--pr`, or keep
a local draft with `--save-only`.

## Deployment settings

Postmark delivery requires `COMMIT_POSTMARK_SERVER_TOKEN` in the worker secret environment and the verified sender
`commit@teamofsilicons.com`. Space Station export uses `COMMIT_TELEMETRY_TABLE_KEY` for `tos.committelemetry`,
`COMMIT_TELEMETRY_HOME` for its private spool, and `COMMIT_TELEMETRY=off` to disable. New schemas require rerunning
`deploy/postgres_runtime_grants.sql`; see [notifications](NOTIFICATIONS.md), [telemetry](TELEMETRY.md) and
[deployment](DEPLOYMENT.md).
