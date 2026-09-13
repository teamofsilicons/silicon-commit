# Test in an isolated sandbox

Create an environment and import Commit's application in [Silicon IAM](https://docs.iam.teamofsilicons.com/api/testing-environments/). Use the imported application's `app_secret` to select it. Commit discovers its environment ID and name from IAM automatically. You do not enter or pair the administrative root key.

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

Manage cleaning, deletion, restore and secret rotation in IAM's administrative environment controls. Commit validates IAM context live on every selected request. A newer IAM clean timestamp atomically clears that sandbox before accepting new work; stale lifecycle versions are rejected. Discovered environments use the regular workflow without the old demo capacity or idle-expiry limits.

IAM webhooks are verified over the complete raw body. Testing envelopes route only to their matching environment; retries deduplicate and event-ID collisions with different bodies are rejected. Live IAM verification remains authoritative when events arrive out of order. Secret fields are redacted before storing verified event payloads.

Database identities, projects, todos, snapshots, audit records, pending jobs and telemetry are isolated. Notification and report emails and outgoing work webhooks are simulated. Test telemetry stays in local sandbox storage and is never exported into the production Space Station table.

## Legacy compatibility

Existing manually paired environments and 32-character Commit keys still work through `/test-environments`. These are labeled legacy administrative controls in the UI. Creation accepts the IAM root key and imported app credential; management requires production authority. Old environments retain their 10-project/100-todo caps and historical retention policy. The legacy clean endpoint refuses a discovered `ask_` secret: an application selector is not an administrative root credential. New integrations should use automatic app-secret selection above.
