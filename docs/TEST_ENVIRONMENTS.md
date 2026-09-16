# Test in an isolated sandbox

Create a shared environment and import Commit in [Honeycomb](https://honeycomb.teamofsilicons.com). Use the imported application's `app_secret` to select it. Commit discovers its environment ID and name from IAM automatically. You do not enter or pair the administrative root key.

```sh
commit testing use '<ask_...>'
commit login '<test-SLT-or-existing-Carbon/Silicon-public-ID>'
commit --org-id your-test-org projects create --data '{"name":"Try a release"}'
commit testing status
commit testing exit
# Or select a sandbox for just one command:
commit --test '<ask_...>' login status --json
```

In the website, choose **Test environment** on sign-in or Settings, enter the app secret, then sign in with a test SLT or an existing sandbox identity's public ID. A banner shows the name and current identity and lets you exit. Production and test cookies and CLI sessions are separate. Exiting resumes production or its sign-in screen.

## API and Rust

Send `X-Testing-App-Secret: ask_...` on every sandbox request. `X-Testing-Environment-Key` is a compatible alias. Do not send both headers. `GET /api/v1/testing-context` returns safe metadata. An invalid, revoked, mismatched or unavailable secret fails; it never selects production.

```rust
let sandbox = Client::new("https://backend.commit.teamofsilicons.com")?
    .with_test_app_secret(app_secret)?;
let metadata = sandbox.testing_context().await?;
let session = sandbox.login_with_slt(test_slt_or_public_id).await?;
let signed_in = sandbox.with_bearer(session.access_token).with_org_id("sandbox-org");
```

The app secret chooses the world. The signed-in user supplies authority. Normal project privacy, relationship rules, scopes and permissions apply; neither app secrets nor organization ownership bypass private projects. Public-ID login is available only in IAM testing. Production requires an SLT. Production tokens cannot access testing and test tokens cannot cross environments.

## Lifecycle and effects

Honeycomb owns preparation, key rotation, cleaning, disabling, restoration and permanent removal. Commit's authenticated participant endpoint works independently of user sessions. It records pending, completed or failed receipts for the exact operation, environment revision, cleaning generation and key version. Retried operations do not repeat completed work; reused IDs with different requests and stale revisions fail.

Cleaning blocks access, erases Commit's sandbox data and queued deliveries transactionally, and preserves the lifecycle link. Restoring permits access only after IAM confirms shared readiness; it never restores cleaned content. Old in-flight writes and IAM discovery responses cannot cross the cleanup barrier. Attachment URLs are references: Commit does not delete another application's files. Discovered environments have no demo capacity limits and are never independently retired. Commit reports generation-bound activity to Honeycomb for shared retention decisions.

See [participant deployment and contract](HONEYCOMB.md) for the internal integration.

IAM webhooks are verified over the complete raw body. Testing envelopes route only to their matching environment; retries deduplicate and event-ID collisions with different bodies are rejected. Live IAM verification remains authoritative when events arrive out of order. Secret fields are redacted before storing verified event payloads.

Database identities, projects, todos, snapshots, audit records, pending jobs and telemetry are isolated. Notification and report emails and outgoing work webhooks are simulated. Test telemetry stays in local sandbox storage and is never exported into the production Space Station table.

## Legacy compatibility

Existing manually paired environments and 32-character Commit keys retain their legacy management APIs and historical limits. New creation through Commit returns `honeycomb_manages_testing_lifecycle`; create new environments through Honeycomb. Discovered or Honeycomb-managed environments cannot be cleaned, disabled, restored or re-paired through these legacy APIs. App secrets select a sandbox and never grant administrative lifecycle authority.
