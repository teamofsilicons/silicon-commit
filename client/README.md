# Silicon Commit Rust client

A stateless client for the Silicon Commit work manager. Add it with
`cargo add silicon-commit-client`. Callers provide credentials and own persistence.

```rust,no_run
# async fn example() -> Result<(), silicon_commit_client::Error> {
use silicon_commit_client::Client;
let client = Client::new("https://backend.commit.teamofsilicons.com")?;
let iam = client.iam().await?;
let session = client.login_with_slt("slt_from_iam").await?;
let client = client.with_bearer(session.access_token);
let status = client.login_status().await?;
# Ok(())
# }
```

Methods cover sessions, todos, projects, notes, notifications, and test environments.
See the [client guide](https://github.com/teamofsilicons/silicon-commit/blob/main/docs/CLIENT.md)
and [API contract](https://github.com/teamofsilicons/silicon-commit/blob/main/openapi.yaml).
