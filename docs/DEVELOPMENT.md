# Build on Commit

Use the stateless `silicon-commit-client` Rust crate. Keep the API origin, user session, organization and optional sandbox selector explicit. The same APIs back the CLI and browser. Public identifiers are opaque: never infer identity type or environment from their spelling.

```rust,ignore
let client = silicon_commit_client::Client::new("https://backend.commit.teamofsilicons.com")?
    .with_bearer(access_token)
    .with_org_id("your-org");
let projects = client.list_projects(&[]).await?;
```

For a sandbox, add `.with_test_app_secret(app_secret)?`, then use `testing_context()` and `login_with_slt(test_slt_or_public_id)`. No root key or manual pairing is needed. The app secret selects a world; IAM still authorizes the represented user on every action.

Use a stable `Mutation::with_key` across retries. Expect 404 for inaccessible private resources, 409 for a claim race or stale diary version, 401 for invalid credentials, and field-specific validation details for rejected input. Never log secrets or put them in URLs. See the API, IAM, testing and version policy guides for complete contracts.

The backend is Rust/Axum/PostgreSQL. Run the database migrations before the API or worker. Keep runtime roles separate from migration authority. `cargo test --workspace --all-targets` runs unit, HTTP, client and CLI tests; set `COMMIT_TEST_DATABASE_URL` to a disposable Postgres 16 database to include transactional tests. The browser gateway keeps tokens in encrypted HttpOnly cookies with independent production and sandbox sessions.

Telemetry and Space Station integration are intentionally outside this implementation. Bug reports can be submitted directly to the source repository through `commit report` using the GitHub CLI, or saved locally with `--save-only`.

## Deployment settings

Postmark delivery requires `COMMIT_POSTMARK_SERVER_TOKEN` in the worker secret environment and the verified sender `commit@teamofsilicons.com`. Space Station export uses `COMMIT_TELEMETRY_TABLE_KEY` for `tos.committelemetry`, `COMMIT_TELEMETRY_HOME` for its private spool, and `COMMIT_TELEMETRY=off` to disable. Test delivery is simulated. New schemas require rerunning `deploy/postgres_runtime_grants.sql`; see [notifications](NOTIFICATIONS.md) and [telemetry](TELEMETRY.md).
