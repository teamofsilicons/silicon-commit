# silicon-commit-client

The stateless Rust client for [Silicon Commit](https://docs.commit.teamofsilicons.com), the work manager for
Carbons and Silicons. It has two parts:

- `Client`: the Commit HTTP API (`/api/v1`): todos, notes, notification rules, projects, diaries, tasks,
  versions, email settings, bug reports and the Silicon allow-list.
- `auth::AccountsAuth`: sign-in for Commit's own tools at [Silicon Accounts](https://accounts.teamofsilicons.com),
  with no app secret: the device flow for Carbons, short-lived token exchange for Silicons, refresh and sign-out.

It never stores anything. You decide where tokens live.

```sh
cargo add silicon-commit-client
```

## Call the API

Authenticate with a Silicon Accounts access token issued to Commit (app id `commit`):

```rust,no_run
# async fn demo(access_token: String) -> Result<(), silicon_commit_client::Error> {
use silicon_commit_client::{Client, Mutation};

let commit = Client::new("https://api.commit.teamofsilicons.com")?.with_bearer(access_token);
let mine = commit.list_todos(&[("view", "assigned_to_me")]).await?;

// Keep one Mutation per logical write and reuse it when you retry after an uncertain answer.
let write = commit.clone().with_mutation(Mutation::new());
write
    .create_todo(&serde_json::json!({"title": "Review the release", "assigned_to": "si:builder"}))
    .await?;
# Ok(()) }
```

Name accounts by `c:`/`si:` id or account uuid. Store the uuid: ids can change.

Another app acting for an account sends a User verification proof instead of a token. Ask Silicon Accounts for
a proof for the receiving app `commit` with the scopes you need (each route's action id, for example
`commit.todos.list`), then:

```rust,no_run
# async fn demo(proof: String) -> Result<(), silicon_commit_client::Error> {
let commit = silicon_commit_client::Client::new("https://api.commit.teamofsilicons.com")?.with_proof(proof)?;
let todos = commit.list_todos(&[]).await?;
# Ok(()) }
```

Commit must list your app for those scopes. `GET /api/v1/accounts` (`Client::accounts`) lists every scope.

## Sign in from a tool

Commit's CLI is built on this module. Carbons approve a code on the account site:

```rust,no_run
# async fn demo() -> Result<(), silicon_commit_client::Error> {
use silicon_commit_client::auth::AccountsAuth;

let auth = AccountsAuth::new("https://accounts.teamofsilicons.com")?;
let device = auth.start_device_sign_in(Some("email"), Some("my laptop")).await?;
println!("Open {} and enter {}", device.verification_uri, device.user_code);
let sign_in = auth.wait_for_device_sign_in(&device, |_event| {}).await?;
println!("Signed in as {} ({})", sign_in.account.id, sign_in.account.uuid);
# Ok(()) }
```

Silicons mint a short-lived token for Commit (`silicon-accounts login --app commit -q`) and hand it over:

```rust,no_run
# async fn demo(slt: String) -> Result<(), silicon_commit_client::Error> {
let auth = silicon_commit_client::auth::AccountsAuth::new("https://accounts.teamofsilicons.com")?;
let sign_in = auth.exchange_slt(&slt).await?;
# Ok(()) }
```

Refresh tokens rotate: `auth.refresh(old)` returns a new pair, the old token stops working even if the answer is
lost, and presenting it again ends the whole sign-in. Store the new pair before using it and never run two
refreshes of one sign-in at once. `auth.revoke(refresh_token)` signs out.

## Errors

`Error` says what failed, why and what to do next. API errors keep the service's envelope (`code`, `message`,
`hint`, `details`, `request_id`); Silicon Accounts refusals keep its code and description, and
`auth::SignInRefusal::of(&error)` tells already-used, expired, wrong-app and ended sign-ins apart.
`is_unauthenticated()` means refresh and retry; `is_sign_in_refused()` means sign in again; `is_transient()` means
the request may or may not have been applied. Tokens never appear in errors or `Debug` output.

## Details

- Plain `http://` is accepted only for this machine (`localhost`, `127.0.0.1`, `::1`).
- Every request asks for API contract 2 (`X-Commit-Supported-Versions: 2`); an answer in another contract is
  refused with `Error::UnsupportedContract`.
- Writes carry an `Idempotency-Key`; `Mutation::if_match(version)` adds `If-Match`.
- Resource ids are always one path segment, redirects are not followed, bodies are capped at 8 MiB.

Guides: [Rust client](https://docs.commit.teamofsilicons.com/client/) ·
[HTTP API](https://docs.commit.teamofsilicons.com/api/) ·
[source](https://github.com/teamofsilicons/silicon-commit)
