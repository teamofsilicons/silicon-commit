# Rust client

`silicon-commit-client` is the stateless Rust client for the Commit HTTP API. It never writes credentials or session state; callers own token persistence and refresh policy. Build a client from a backend origin or `/api/v1/` URL, then attach the IAM access token, organization ID, and (for sandbox requests) the 32 character Commit test key:

```rust
use silicon_commit_client::{Client, Mutation};
let commit = Client::new("https://commit.teamofsilicons.com/api/v1/")?
    .with_bearer(access_token)
    .with_org_id("my-org");
let todos = commit.list_todos(&[("status", "in_progress")]).await?;
let create = commit.with_mutation(Mutation::new());
create.create_todo(&serde_json::json!({"title":"Ship it", "assigned_to":"actor"})).await?;
```

`login_with_slt` exchanges a Silicon IAm short-lived token at Commit and returns access and refresh tokens. `refresh_session` rotates a refresh token and `logout` revokes its family. Save these values only in the caller's secure store. A `SessionTokens` debug representation redacts token contents.

Every mutating method carries an idempotency key. Keep and reuse a `Mutation` when retrying an uncertain request; call `if_match(version)` for optimistic concurrency. Resource identifiers are escaped as one URL segment, redirects are disabled, and response bodies are capped at 8 MiB. `Error` preserves HTTP status, stable API error code, and request ID without retaining response bodies or secrets.

Methods cover health/readiness/version, sessions, todos and notes, list and todo subscriptions, notification settings, projects (diary, tasks, blockers, updates, completion), and test-environment lifecycle (create/list/key/rotate/clean/restore/delete). To target a sandbox, use `with_test_key` on the same client; all resource methods then operate against that isolated environment.

`latest_release` provides the crates.io version check used by the CLI. It performs a bounded, redirect-free request and does not mutate the running process. Applications may use it to schedule dependency updates while keeping their own update policy.
