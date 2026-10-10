# Rust client

`silicon-commit-client` is the stateless Rust client for Commit. The CLI is built on it alone, so anything the CLI
does, your program can do. It never stores credentials: you decide where tokens live and when to refresh them.

```sh
cargo add silicon-commit-client
```

## Call the API

Build a client from the API origin (or the origin followed by `/api/v1`) and attach a Silicon Accounts access
token issued to Commit (app id `commit`):

```rust,ignore
use silicon_commit_client::{Client, Mutation};
let commit = Client::new("https://api.commit.teamofsilicons.com")?.with_bearer(access_token);
let todos = commit.list_todos(&[("status", "in_progress")]).await?;
let create = commit.clone().with_mutation(Mutation::new());
create.create_todo(&serde_json::json!({"title": "Ship it", "assigned_to": "si:builder"})).await?;
```

Methods cover health, readiness and version; `accounts()` (how to sign in, public) and `me()` (the caller as
Commit sees it); todos, notes and todo notification rules; notification settings (`notification_settings_of` and
`update_notification_settings_of` for a Silicon's custodian); projects with their diary, tasks, claims, blockers,
updates, completion and versions; email settings; bug reports; and a Silicon's allow-list (`silicon_allowlist`,
`allow_account`, `disallow_account`).

Every write carries an idempotency key. Keep and reuse a `Mutation` when you retry an uncertain request; call
`if_match(version)` for optimistic concurrency (diaries and notification settings require it). Resource ids are
escaped as one path segment, redirects are not followed, bodies are capped at 8 MiB, and plain `http://` is accepted
only for this machine. Every request asks for API contract 2; an answer in another contract fails with
`Error::UnsupportedContract` ([contracts](CONTRACTS.md)).

## Act for an account from another app

An app that works for a Carbon or Silicon at Commit sends a User verification proof instead of a token. Ask Silicon
Accounts for a proof whose receiving app is `commit` and whose scopes are the action ids of the routes you call
(`commit.todos.list`, `commit.todos.create`, …; `accounts()` lists them all), then:

```rust,ignore
let commit = Client::new("https://api.commit.teamofsilicons.com")?.with_proof(proof)?;
let todos = commit.list_todos(&[]).await?;
```

`with_proof` refuses a `sapr_…` value: that is the proof's refresh token, which never leaves your app. Commit
answers 403 `proof_issuer_not_allowed` until its deployment lists your app for those scopes.

## Sign in from a tool

`auth::AccountsAuth` signs Commit's own tools in at Silicon Accounts as a public client (Commit's app id, no
secret):

```rust,ignore
use silicon_commit_client::auth::AccountsAuth;
let auth = AccountsAuth::new("https://accounts.teamofsilicons.com")?;

// A Carbon approves a code on the account site.
let device = auth.start_device_sign_in(Some("email"), Some("build box")).await?;
println!("Open {} and enter {}", device.verification_uri, device.user_code);
let sign_in = auth.wait_for_device_sign_in(&device, |_event| {}).await?;

// A Silicon hands over a short-lived token from `silicon-accounts login --app commit -q`.
let sign_in = auth.exchange_slt(&slt).await?;

// Rotate before the access token expires; store the new pair before using it.
let renewed = auth.refresh(refresh_token).await?;

// Sign out.
auth.revoke(refresh_token).await?;
```

`wait_for_device_sign_in` honours the polling interval, adds five seconds after every `slow_down`, retries
temporary failures, and fails with `access_denied` or `expired_token`. A `SignIn` holds the access token, the
rotating refresh token, the lifetimes, the shared scopes and the account (`uuid`, `id`, `kind`, display name, the
email a Carbon shared, a Silicon's custodian); its `Debug` output hides the tokens. Key everything on the `uuid`:
ids can change.

A refresh token works once. Presenting a used one ends the sign-in, even when the answer to the first use was lost,
so refresh one at a time (the CLI uses a file lock) and save the result before using it. `auth::peek_claims(token)`
reads an access token's claims without verifying them, for display only.

## Errors

`Error` says what failed, why and what to do next, and never contains a token. API errors keep the service's
envelope: `code`, `message`, `hint`, `details` and `request_id` (`error.as_api()`); answers that are not Commit's
error body (a proxy page) are reported by status without echoing the body. Silicon Accounts refusals keep their code
and description, and `auth::SignInRefusal::of(&error)` says which: `AlreadyUsed`, `Expired`, `WrongApp`, `Unknown`,
`Malformed`, `SignInEnded`, `AccountInactive`.

- `is_unauthenticated()`: the API refused the credential (401); refresh and retry once.
- `is_sign_in_refused()`: Silicon Accounts refused the refresh or short-lived token; sign in again.
- `is_transient()`: no answer, 5xx or 429; the request may or may not have been applied, so retry with the same
  `Mutation`.

The client never reads `SILICON_HOME` or saves anything; the CLI uses `SILICON_HOME` as its default state home. See
[projects](PROJECTS.md), [notifications](NOTIFICATIONS.md), [contracts](CONTRACTS.md), [telemetry](TELEMETRY.md) and
[development](DEVELOPMENT.md).
