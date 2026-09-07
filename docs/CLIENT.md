# Rust client

`silicon-commit-client` is stateless and uses the public API. Construct it with `Client::new`, then add a bearer token with `with_bearer`, organization context with `with_org_id`, and a test-environment key with `with_test_key`. Resource methods cover health/readiness, sessions, todos, notes, notification subscriptions, projects, diary/tasks/blockers/updates/completion, attachments, and test-environment lifecycle. Writes automatically carry an idempotency key; use `with_mutation` to reuse one on retry or add an ETag precondition.

```rust
let commit = Client::new("https://backend.commit.teamofsilicons.com/api/v1/")?
    .with_bearer(access_token);
let todos = commit.list_todos(&[]).await?;
```

The client uses `silicon-iam-client` for IAM login/session flows and keeps credentials redacted in debug output. Configure automatic dependency updates explicitly at the application level; backend integrations disable updates during server requests.
