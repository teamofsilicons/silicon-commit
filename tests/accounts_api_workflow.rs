//! Real HTTP routing and PostgreSQL workflows authenticated by Silicon Accounts.
//!
//! A local Silicon Accounts double serves the JWKS, userinfo, account lookups, token
//! introspection and proof verification; access tokens are `EdDSA` JWTs signed here.

mod common;

use std::{sync::Arc, time::Duration};

use anyhow::{Context as _, ensure};
use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Method, Request, StatusCode},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signer as _, SigningKey};
use secrecy::SecretString;
use serde_json::{Value, json};
use silicon_accounts_client::sign_webhook;
use silicon_commit::{
    api::{self, AppState},
    application::{
        accounts::AccountService, notifications::NotificationSettingsService,
        ports::IdentityProvider, projects::ProjectService, todos::TodoService,
    },
    config::{AccountsSettings, ProofIssuers, ServerSettings},
    domain::DomainLimits,
    infrastructure::clients::accounts::AccountsIdentity,
};
use sqlx::PgPool;
use time::OffsetDateTime;
use tower::ServiceExt as _;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_string_contains, header, method, path},
};

use common::{AUDIT_RETENTION, IDEMPOTENCY_TTL, TOMBSTONE_RETENTION, new_uuid, test_pool};

const ISSUER: &str = "https://accounts.test";
const APP_SECRET: &str = "commit-test-app-secret";
const WEBHOOK_SECRET: &str = "whsec_commit_test_secret";

/// One account as the Silicon Accounts double knows it.
#[derive(Clone)]
struct Account {
    uuid: String,
    id: String,
    kind: &'static str,
    custodian: Option<(String, String)>,
    email: Option<String>,
}

impl Account {
    fn carbon(label: &str) -> Self {
        Self {
            uuid: new_uuid(),
            id: format!("c:{label}-{}", new_uuid()[..6].to_ascii_lowercase()),
            kind: "carbon",
            custodian: None,
            email: Some(format!("{label}@example.test")),
        }
    }

    fn silicon(label: &str, custodian: &Self) -> Self {
        Self {
            uuid: new_uuid(),
            id: format!("si:{label}-{}", new_uuid()[..6].to_ascii_lowercase()),
            kind: "silicon",
            custodian: Some((custodian.uuid.clone(), custodian.id.clone())),
            email: None,
        }
    }

    fn custodian_json(&self) -> Value {
        self.custodian.as_ref().map_or(
            Value::Null,
            |(uuid, id)| json!({"uuid": uuid, "id": id, "kind": "carbon"}),
        )
    }

    /// `GET /v1/accounts/{uuid}` and `/v1/accounts/by-id/{id}`.
    fn summary(&self) -> Value {
        json!({
            "uuid": self.uuid, "kind": self.kind, "id": self.id, "display_name": self.id,
            "pfp_url": "", "status": "active", "custodian": self.custodian_json(),
        })
    }

    /// `GET /v1/userinfo` (the account as Commit may see it).
    fn app_view(&self) -> Value {
        let mut view = json!({
            "uuid": self.uuid, "membership_id": format!("commit:{}", self.uuid), "kind": self.kind,
            "id": self.id, "display_name": self.id, "pfp_url": "", "version": 1,
            "custodian": self.custodian_json(),
        });
        if let (Some(email), Value::Object(fields)) = (&self.email, &mut view) {
            fields.insert("email".to_owned(), json!(email));
            fields.insert("email_verified".to_owned(), json!(true));
        }
        view
    }
}

fn signing_key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn jwk(kid: &str, key: &SigningKey) -> Value {
    json!({
        "kty": "OKP", "crv": "Ed25519", "kid": kid, "use": "sig", "alg": "EdDSA",
        "x": URL_SAFE_NO_PAD.encode(key.verifying_key().to_bytes()),
    })
}

/// Signs a compact `EdDSA` JWT exactly like Silicon Accounts does.
fn mint(key: &SigningKey, kid: &str, claims: &Value) -> String {
    let header =
        URL_SAFE_NO_PAD.encode(json!({"alg": "EdDSA", "typ": "JWT", "kid": kid}).to_string());
    let payload = URL_SAFE_NO_PAD.encode(claims.to_string());
    let input = format!("{header}.{payload}");
    let signature = URL_SAFE_NO_PAD.encode(key.sign(input.as_bytes()).to_bytes());
    format!("{input}.{signature}")
}

fn now() -> i64 {
    OffsetDateTime::now_utc().unix_timestamp()
}

fn claims(account: &Account) -> Value {
    json!({
        "iss": ISSUER, "sub": account.uuid, "aud": "commit", "exp": now() + 600, "iat": now() - 5,
        "kind": account.kind, "id": account.id, "mid": format!("commit:{}", account.uuid),
        "fid": format!("fam-{}", account.uuid), "scope": "profile email",
    })
}

/// The Silicon Accounts double and the keys it publishes.
struct Accounts {
    server: MockServer,
    key: SigningKey,
}

impl Accounts {
    async fn start(accounts: &[&Account]) -> Self {
        let server = MockServer::start().await;
        let key = signing_key(7);
        Mock::given(method("GET"))
            .and(path("/.well-known/jwks.json"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"keys": [jwk("k1", &key)]})),
            )
            .mount(&server)
            .await;
        for account in accounts {
            Mock::given(method("GET"))
                .and(path(format!("/v1/accounts/{}", account.uuid)))
                .and(header("authorization", basic()))
                .respond_with(ResponseTemplate::new(200).set_body_json(account.summary()))
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path(format!("/v1/accounts/by-id/{}", account.id)))
                .and(header("authorization", basic()))
                .respond_with(ResponseTemplate::new(200).set_body_json(account.summary()))
                .mount(&server)
                .await;
        }
        // Unknown accounts.
        Mock::given(method("GET"))
            .and(wiremock::matchers::path_regex("^/v1/accounts/.*"))
            .respond_with(ResponseTemplate::new(404).set_body_json(json!({
                "error": {"code": "account_not_found", "message": "No account has this id."}
            })))
            .with_priority(10)
            .mount(&server)
            .await;
        Self { server, key }
    }

    fn token(&self, account: &Account) -> String {
        mint(&self.key, "k1", &claims(account))
    }

    fn token_with(&self, account: &Account, change: impl FnOnce(&mut Value)) -> String {
        let mut claims = claims(account);
        change(&mut claims);
        mint(&self.key, "k1", &claims)
    }

    /// `GET /v1/userinfo` answers for this token with the account (and its shared email).
    async fn userinfo(&self, token: &str, account: &Account) {
        Mock::given(method("GET"))
            .and(path("/v1/userinfo"))
            .and(header("authorization", format!("Bearer {token}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(account.app_view()))
            .with_priority(1)
            .mount(&self.server)
            .await;
    }

    /// `POST /v1/oauth/introspect` says whether this token is active.
    async fn introspection(&self, token: &str, active: bool) {
        Mock::given(method("POST"))
            .and(path("/v1/oauth/introspect"))
            .and(header("authorization", basic()))
            .and(body_string_contains(token))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"active": active})))
            .mount(&self.server)
            .await;
    }

    /// `POST /v1/proofs/verify` answers `answer` for this proof token, at most `calls` times.
    async fn proof(&self, token: &str, answer: Value, calls: u64) {
        Mock::given(method("POST"))
            .and(path("/v1/proofs/verify"))
            .and(header("authorization", basic()))
            .and(body_string_contains(token))
            .respond_with(ResponseTemplate::new(200).set_body_json(answer))
            .expect(0..=calls)
            .mount(&self.server)
            .await;
    }
}

fn basic() -> String {
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("commit:{APP_SECRET}"))
    )
}

fn valid_proof(issuer: &str, receiver: &str, user: &Account, scopes: &[&str]) -> Value {
    json!({
        "valid": true, "proof_id": format!("prf_{}", new_uuid()), "kind": "user_verification",
        "expires_at": (OffsetDateTime::now_utc() + time::Duration::minutes(5))
            .format(&time::format_description::well_known::Rfc3339).unwrap_or_default(),
        "issuing_app": {"app_id": issuer, "name": issuer},
        "receiving_app": {"app_id": receiver, "name": receiver},
        "user": {"uuid": user.uuid, "id": user.id, "kind": user.kind},
        "scopes": scopes,
    })
}

fn router(
    pool: &PgPool,
    accounts: &Accounts,
    issuers: &str,
    webhook: bool,
) -> anyhow::Result<Router> {
    let settings = AccountsSettings {
        issuer: ISSUER.to_owned(),
        api_url: accounts.server.uri().parse()?,
        app_id: "commit".to_owned(),
        app_secret: Some(SecretString::from(APP_SECRET)),
        webhook_secret: webhook.then(|| SecretString::from(WEBHOOK_SECRET)),
        proof_issuers: ProofIssuers::parse(issuers, silicon_commit::application::scopes::ALL)
            .map_err(anyhow::Error::msg)?,
    };
    let identity: Arc<dyn IdentityProvider> = Arc::new(AccountsIdentity::new(
        &settings,
        pool.clone(),
        Duration::from_secs(2),
        Duration::from_secs(5),
    )?);
    let limits = DomainLimits::default();
    let state = AppState::new(
        pool.clone(),
        Arc::clone(&identity),
        Arc::new(TodoService::new(
            pool.clone(),
            Arc::clone(&identity),
            limits,
            IDEMPOTENCY_TTL,
            AUDIT_RETENTION,
            TOMBSTONE_RETENTION,
        )),
        Arc::new(ProjectService::new(
            pool.clone(),
            Arc::clone(&identity),
            limits,
            IDEMPOTENCY_TTL,
            AUDIT_RETENTION,
        )),
        Arc::new(NotificationSettingsService::new(
            pool.clone(),
            Arc::clone(&identity),
            AUDIT_RETENTION,
        )),
        Arc::new(AccountService::new(
            pool.clone(),
            Arc::clone(&identity),
            TOMBSTONE_RETENTION,
            AUDIT_RETENTION,
        )),
        "http://127.0.0.1:4141/api/v1".parse()?,
    )
    .with_webhook_secret(settings.webhook_secret.clone())
    .with_accounts("commit", ISSUER);
    let server = ServerSettings {
        bind_addr: "127.0.0.1:0".parse()?,
        public_base_url: "http://127.0.0.1:4141/api/v1".parse()?,
        cors_allowed_origins: vec![],
        request_timeout: Duration::from_secs(10),
        max_body_bytes: 1_048_576,
        concurrency_limit: 8,
        shutdown_timeout: Duration::from_secs(1),
    };
    Ok(api::router(state, &server)?)
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Value,
}

impl Reply {
    fn code(&self) -> &str {
        self.body["error"]["code"].as_str().unwrap_or_default()
    }
}

/// One request; `credential` is the whole Authorization value.
async fn call(
    app: &Router,
    method: Method,
    uri: &str,
    credential: Option<&str>,
    body: Option<Value>,
    extra: &[(&str, &str)],
) -> anyhow::Result<Reply> {
    let mut builder = Request::builder()
        .method(method.clone())
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(credential) = credential {
        builder = builder.header("authorization", credential);
    }
    if matches!(method, Method::POST | Method::PATCH | Method::PUT)
        && !uri.contains("/notification")
    {
        builder = builder.header("idempotency-key", format!("key-{}", new_uuid()));
    }
    for (name, value) in extra {
        builder = builder.header(*name, *value);
    }
    let body = body.map_or_else(Body::empty, |value| Body::from(value.to_string()));
    let response = app.clone().oneshot(builder.body(body)?).await?;
    let (parts, body) = response.into_parts();
    let bytes = axum::body::to_bytes(body, 1_048_576).await?;
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .with_context(|| String::from_utf8_lossy(&bytes).into_owned())?
    };
    Ok(Reply {
        status: parts.status,
        headers: parts.headers,
        body,
    })
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

async fn webhook(
    app: &Router,
    body: &Value,
    timestamp: i64,
    secret: &str,
) -> anyhow::Result<Reply> {
    let raw = body.to_string();
    let signature = sign_webhook(secret, timestamp, raw.as_bytes());
    let request = Request::builder()
        .method(Method::POST)
        .uri("/webhook/")
        .header("content-type", "application/json")
        .header("x-accounts-timestamp", timestamp.to_string())
        .header("x-accounts-signature", signature)
        .body(Body::from(raw))?;
    let response = app.clone().oneshot(request).await?;
    let (parts, body) = response.into_parts();
    let bytes = axum::body::to_bytes(body, 1_048_576).await?;
    Ok(Reply {
        status: parts.status,
        headers: parts.headers,
        body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    })
}

fn event(kind: &str, data: Value) -> Value {
    json!({
        "event_id": format!("evt_{}", new_uuid()), "type": kind, "app_id": "commit",
        "occurred_at": OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339).unwrap_or_default(),
        "data": data,
    })
}

#[tokio::test]
async fn access_tokens_are_verified_locally_and_refused_precisely() -> anyhow::Result<()> {
    let Some(pool) = test_pool().await? else {
        return Ok(());
    };
    let ada = Account::carbon("ada");
    let accounts = Accounts::start(&[&ada]).await;
    let app = router(&pool, &accounts, "", true)?;
    let token = accounts.token(&ada);
    accounts.userinfo(&token, &ada).await;

    let me = call(
        &app,
        Method::GET,
        "/api/v1/me",
        Some(&bearer(&token)),
        None,
        &[],
    )
    .await?;
    ensure!(me.status == StatusCode::OK, "{:?} {}", me.status, me.body);
    ensure!(me.body["uuid"] == ada.uuid.as_str() && me.body["id"] == ada.id.as_str());
    ensure!(me.body["kind"] == "carbon" && me.body["email"] == "ada@example.test");
    ensure!(
        me.headers
            .get("x-commit-api-version")
            .is_some_and(|value| value == "2")
    );

    let unsigned = call(&app, Method::GET, "/api/v1/me", None, None, &[]).await?;
    ensure!(unsigned.status == StatusCode::UNAUTHORIZED && unsigned.code() == "unauthenticated");
    let tampered = {
        let mut parts = token.split('.').map(str::to_owned).collect::<Vec<_>>();
        let mut forged = claims(&ada);
        forged["sub"] = json!(new_uuid());
        parts[1] = URL_SAFE_NO_PAD.encode(forged.to_string());
        parts.join(".")
    };
    let rotated = mint(&signing_key(9), "k2", &claims(&ada));
    for (credential, expected) in [
        (
            accounts.token_with(&ada, |c| c["aud"] = json!("remind")),
            "token_wrong_audience",
        ),
        (
            accounts.token_with(&ada, |c| c["iss"] = json!("https://accounts.example")),
            "token_wrong_issuer",
        ),
        (
            accounts.token_with(&ada, |c| c["exp"] = json!(now() - 3_600)),
            "token_expired",
        ),
        (rotated, "token_unknown_key"),
        (tampered, "token_bad_signature"),
        ("not-a-jwt".to_owned(), "token_malformed"),
    ] {
        let reply = call(
            &app,
            Method::GET,
            "/api/v1/me",
            Some(&bearer(&credential)),
            None,
            &[],
        )
        .await?;
        ensure!(
            reply.status == StatusCode::UNAUTHORIZED && reply.code() == expected,
            "expected {expected}, got {:?} {}",
            reply.status,
            reply.body
        );
        ensure!(reply.headers.get("www-authenticate").is_some());
    }

    // IAM-era headers and contract 1 are refused with a precise answer.
    let retired = call(
        &app,
        Method::GET,
        "/api/v1/me",
        Some(&bearer(&token)),
        None,
        &[("x-org-id", "tos")],
    )
    .await?;
    ensure!(retired.status == StatusCode::BAD_REQUEST && retired.code() == "retired_header");
    let old_contract = call(
        &app,
        Method::GET,
        "/api/v1/me",
        Some(&bearer(&token)),
        None,
        &[("x-commit-api-version", "1")],
    )
    .await?;
    ensure!(
        old_contract.status == StatusCode::NOT_ACCEPTABLE
            && old_contract.code() == "unsupported_contract"
    );
    let basic = call(
        &app,
        Method::GET,
        "/api/v1/me",
        Some("Basic YTpi"),
        None,
        &[],
    )
    .await?;
    ensure!(basic.code() == "unsupported_authorization_scheme");

    // Public sign-in metadata needs no credential.
    let metadata = call(&app, Method::GET, "/api/v1/accounts", None, None, &[]).await?;
    ensure!(metadata.status == StatusCode::OK);
    ensure!(metadata.body["app_id"] == "commit" && metadata.body["accounts_url"] == ISSUER);
    Ok(())
}

#[tokio::test]
async fn accounts_webhooks_are_verified_deduplicated_and_end_sessions() -> anyhow::Result<()> {
    let Some(pool) = test_pool().await? else {
        return Ok(());
    };
    let ada = Account::carbon("hooked");
    let accounts = Accounts::start(&[&ada]).await;
    let app = router(&pool, &accounts, "", true)?;
    let early = accounts.token_with(&ada, |c| c["iat"] = json!(now() - 120));
    accounts.userinfo(&early, &ada).await;
    let first = call(
        &app,
        Method::GET,
        "/api/v1/me",
        Some(&bearer(&early)),
        None,
        &[],
    )
    .await?;
    ensure!(first.status == StatusCode::OK, "{}", first.body);

    // Signature and timestamp are checked over the raw body.
    let renamed = event(
        "account.id_changed",
        json!({"uuid": ada.uuid, "membership_id": format!("commit:{}", ada.uuid), "kind": "carbon",
               "old_id": ada.id, "new_id": "c:hooked-renamed"}),
    );
    let forged = webhook(&app, &renamed, now(), "whsec_someone_else").await?;
    ensure!(
        forged.status == StatusCode::UNAUTHORIZED && forged.code() == "invalid_webhook_signature"
    );
    let stale = webhook(&app, &renamed, now() - 600, WEBHOOK_SECRET).await?;
    ensure!(stale.status == StatusCode::UNAUTHORIZED, "{}", stale.body);
    let garbage = webhook(&app, &json!({"hello": "world"}), now(), WEBHOOK_SECRET).await?;
    ensure!(garbage.status == StatusCode::BAD_REQUEST && garbage.code() == "invalid_webhook_body");

    let applied = webhook(&app, &renamed, now(), WEBHOOK_SECRET).await?;
    ensure!(
        applied.status == StatusCode::OK && applied.body["applied"] == true,
        "{}",
        applied.body
    );
    let replayed = webhook(&app, &renamed, now(), WEBHOOK_SECRET).await?;
    ensure!(replayed.status == StatusCode::OK && replayed.body["applied"] == false);
    let me = call(
        &app,
        Method::GET,
        "/api/v1/me",
        Some(&bearer(&early)),
        None,
        &[],
    )
    .await?;
    ensure!(me.body["id"] == "c:hooked-renamed", "{}", me.body);

    // Unknown event types are recorded and acknowledged.
    let future = webhook(
        &app,
        &event("account.something_new", json!({"uuid": ada.uuid})),
        now(),
        WEBHOOK_SECRET,
    )
    .await?;
    ensure!(future.status == StatusCode::OK && future.body["applied"] == true);

    // Signing out everywhere refuses tokens issued before the event; later sign-ins work.
    let signed_out = webhook(
        &app,
        &event(
            "membership.signed_out",
            json!({"uuid": ada.uuid, "reason": "signed_out_everywhere"}),
        ),
        now(),
        WEBHOOK_SECRET,
    )
    .await?;
    ensure!(signed_out.status == StatusCode::OK, "{}", signed_out.body);
    let refused = call(
        &app,
        Method::GET,
        "/api/v1/me",
        Some(&bearer(&early)),
        None,
        &[],
    )
    .await?;
    ensure!(
        refused.status == StatusCode::UNAUTHORIZED && refused.code() == "session_ended",
        "{}",
        refused.body
    );
    let later = accounts.token_with(&ada, |c| c["iat"] = json!(now() + 2));
    let fresh = call(
        &app,
        Method::GET,
        "/api/v1/me",
        Some(&bearer(&later)),
        None,
        &[],
    )
    .await?;
    ensure!(fresh.status == StatusCode::OK, "{}", fresh.body);

    // A deleted account can never act again.
    let deleted = webhook(
        &app,
        &event("account.deleted", json!({"uuid": ada.uuid})),
        now(),
        WEBHOOK_SECRET,
    )
    .await?;
    ensure!(deleted.status == StatusCode::OK, "{}", deleted.body);
    let after = accounts.token_with(&ada, |c| c["iat"] = json!(now() + 60));
    let gone = call(
        &app,
        Method::GET,
        "/api/v1/me",
        Some(&bearer(&after)),
        None,
        &[],
    )
    .await?;
    ensure!(
        gone.status == StatusCode::UNAUTHORIZED && gone.code() == "account_deleted",
        "{}",
        gone.body
    );

    // Without the secret configured, deliveries are refused rather than trusted.
    let unconfigured = router(&pool, &accounts, "", false)?;
    let refused = webhook(
        &unconfigured,
        &event("ping", json!({})),
        now(),
        WEBHOOK_SECRET,
    )
    .await?;
    ensure!(refused.status == StatusCode::SERVICE_UNAVAILABLE);
    Ok(())
}

#[tokio::test]
async fn proofs_act_for_an_account_only_with_scope_issuer_and_receiver() -> anyhow::Result<()> {
    let Some(pool) = test_pool().await? else {
        return Ok(());
    };
    let ada = Account::carbon("proven");
    let accounts = Accounts::start(&[&ada]).await;
    let app = router(
        &pool,
        &accounts,
        "commit.todos.list=interface,commit.todos.create=interface",
        true,
    )?;
    let scopes = ["commit.todos.list", "commit.todos.create"];
    // Verified once per process (two routers below), then reused for the proof's short cache window.
    accounts
        .proof(
            "sap_interface_ok",
            valid_proof("interface", "commit", &ada, &scopes),
            2,
        )
        .await;
    accounts
        .proof("sap_from_dm", valid_proof("dm", "commit", &ada, &scopes), 1)
        .await;
    accounts
        .proof(
            "sap_for_remind",
            valid_proof("interface", "remind", &ada, &scopes),
            1,
        )
        .await;
    accounts
        .proof(
            "sap_unknown",
            json!({"valid": false, "expires_at": null}),
            1,
        )
        .await;
    let mut app_proof = valid_proof("interface", "commit", &ada, &scopes);
    app_proof["kind"] = json!("app_verification");
    app_proof["user"] = Value::Null;
    accounts.proof("sap_app_only", app_proof, 1).await;

    let listed = call(
        &app,
        Method::GET,
        "/api/v1/todos",
        Some("Proof sap_interface_ok"),
        None,
        &[],
    )
    .await?;
    ensure!(
        listed.status == StatusCode::OK,
        "{:?} {}",
        listed.status,
        listed.body
    );
    let created = call(
        &app,
        Method::POST,
        "/api/v1/todos",
        Some("Proof sap_interface_ok"),
        Some(json!({"title": "Book the venue", "assigned_to": ada.id})),
        &[],
    )
    .await?;
    ensure!(
        created.status == StatusCode::CREATED,
        "{:?} {}",
        created.status,
        created.body
    );
    ensure!(created.body["assigned_by"]["uuid"] == ada.uuid.as_str());
    let todo_id: uuid::Uuid = created.body["id"].as_str().context("todo id")?.parse()?;
    // The acting app is recorded with the work it did for the account.
    let via: Option<String> = sqlx::query_scalar(
        "SELECT changes->>'via_app' FROM commit.todo_activity WHERE todo_id = $1 AND activity_type = 'created'",
    )
    .bind(todo_id)
    .fetch_one(&pool)
    .await?;
    ensure!(via.as_deref() == Some("interface"));
    // The IAM-era alias performs the same action with the same credential.
    let alias = call(
        &app,
        Method::GET,
        "/api/v1/obo/todos/list",
        Some("Proof sap_interface_ok"),
        None,
        &[],
    )
    .await?;
    ensure!(alias.status == StatusCode::OK, "{}", alias.body);

    for (credential, uri, status, code) in [
        (
            "Proof sap_interface_ok",
            "/api/v1/projects",
            StatusCode::FORBIDDEN,
            "proof_scope_missing",
        ),
        (
            "Proof sap_from_dm",
            "/api/v1/todos",
            StatusCode::FORBIDDEN,
            "proof_issuer_not_allowed",
        ),
        (
            "Proof sap_for_remind",
            "/api/v1/todos",
            StatusCode::UNAUTHORIZED,
            "proof_wrong_receiver",
        ),
        (
            "Proof sap_unknown",
            "/api/v1/todos",
            StatusCode::UNAUTHORIZED,
            "proof_invalid",
        ),
        (
            "Proof sap_app_only",
            "/api/v1/todos",
            StatusCode::UNAUTHORIZED,
            "proof_without_account",
        ),
        (
            "Proof sapr_refresh",
            "/api/v1/todos",
            StatusCode::UNAUTHORIZED,
            "proof_malformed",
        ),
    ] {
        let reply = call(&app, Method::GET, uri, Some(credential), None, &[]).await?;
        ensure!(
            reply.status == status && reply.code() == code,
            "{credential} {uri}: expected {code}, got {:?} {}",
            reply.status,
            reply.body
        );
    }

    // With no COMMIT_PROOF_ISSUERS, no app may act for anyone.
    let closed = router(&pool, &accounts, "", true)?;
    let refused = call(
        &closed,
        Method::GET,
        "/api/v1/todos",
        Some("Proof sap_interface_ok"),
        None,
        &[],
    )
    .await?;
    ensure!(
        refused.status == StatusCode::FORBIDDEN && refused.code() == "proof_issuer_not_allowed"
    );
    Ok(())
}

#[tokio::test]
async fn every_route_family_applies_the_circle_and_the_custodian_rule() -> anyhow::Result<()> {
    let Some(pool) = test_pool().await? else {
        return Ok(());
    };
    let ada = Account::carbon("routes-ada");
    let scout = Account::silicon("routes-scout", &ada);
    let bea = Account::carbon("routes-bea");
    let helper = Account::silicon("routes-helper", &bea);
    let cy = Account::carbon("routes-cy");
    let accounts = Accounts::start(&[&ada, &scout, &bea, &helper, &cy]).await;
    let app = router(&pool, &accounts, "", true)?;
    let tokens = [&ada, &scout, &bea, &helper, &cy].map(|account| accounts.token(account));
    let [ada_token, scout_token, bea_token, helper_token, cy_token] = tokens.clone();
    for (token, account) in tokens.iter().zip([&ada, &scout, &bea, &helper, &cy]) {
        accounts.userinfo(token, account).await;
        accounts.introspection(token, true).await;
        // Commit learns each account (and so each custodian link) on first sight.
        let me = call(
            &app,
            Method::GET,
            "/api/v1/me",
            Some(&bearer(token)),
            None,
            &[],
        )
        .await?;
        ensure!(me.status == StatusCode::OK, "{}", me.body);
    }
    let me = call(
        &app,
        Method::GET,
        "/api/v1/me",
        Some(&bearer(&ada_token)),
        None,
        &[],
    )
    .await?;
    ensure!(
        me.body["silicons"][0]["uuid"] == scout.uuid.as_str(),
        "{}",
        me.body
    );

    // Names that are no account (malformed, or unknown to Accounts) are precise 422s.
    for unknown in ["me", "c:nobody-at-all", "zz"] {
        let reply = call(
            &app,
            Method::POST,
            "/api/v1/todos",
            Some(&bearer(&ada_token)),
            Some(json!({"title": "Nobody", "assigned_to": unknown})),
            &[],
        )
        .await?;
        ensure!(
            reply.status == StatusCode::UNPROCESSABLE_ENTITY
                && reply.body["error"]["details"]["assigned_to"].is_string(),
            "{unknown}: {:?} {}",
            reply.status,
            reply.body
        );
    }

    // Todos: a Silicon outside the circle takes work only from accounts it allowed.
    let todo = json!({"title": "Translate the brief", "assigned_to": helper.id});
    let closed = call(
        &app,
        Method::POST,
        "/api/v1/todos",
        Some(&bearer(&scout_token)),
        Some(todo.clone()),
        &[],
    )
    .await?;
    ensure!(
        closed.status == StatusCode::FORBIDDEN && closed.code() == "silicon_not_reachable",
        "{}",
        closed.body
    );
    let allowlist = format!("/api/v1/silicons/{}/allowed-accounts", helper.id);
    let not_custodian = call(
        &app,
        Method::PUT,
        &format!("{allowlist}/{}", scout.id),
        Some(&bearer(&ada_token)),
        None,
        &[],
    )
    .await?;
    ensure!(
        not_custodian.status == StatusCode::FORBIDDEN && not_custodian.code() == "not_custodian"
    );
    let allowed = call(
        &app,
        Method::PUT,
        &format!("{allowlist}/{}", scout.id),
        Some(&bearer(&bea_token)),
        None,
        &[],
    )
    .await?;
    ensure!(allowed.status == StatusCode::OK, "{}", allowed.body);
    ensure!(allowed.body["allowed"][0]["account"]["uuid"] == scout.uuid.as_str());
    let listed = call(
        &app,
        Method::GET,
        &allowlist,
        Some(&bearer(&helper_token)),
        None,
        &[],
    )
    .await?;
    ensure!(
        listed.body["allowed"]
            .as_array()
            .is_some_and(|items| items.len() == 1)
    );
    let created = call(
        &app,
        Method::POST,
        "/api/v1/todos",
        Some(&bearer(&scout_token)),
        Some(todo),
        &[],
    )
    .await?;
    ensure!(created.status == StatusCode::CREATED, "{}", created.body);
    let todo_path = format!(
        "/api/v1/todos/{}",
        created.body["id"].as_str().context("id")?
    );
    for (token, expected) in [
        (&ada_token, StatusCode::OK),    // the owner's custodian
        (&bea_token, StatusCode::OK),    // the assignee's custodian
        (&helper_token, StatusCode::OK), // the assignee
        (&cy_token, StatusCode::NOT_FOUND),
    ] {
        let reply = call(
            &app,
            Method::GET,
            &todo_path,
            Some(&bearer(token)),
            None,
            &[],
        )
        .await?;
        ensure!(
            reply.status == expected,
            "{todo_path}: {:?} {}",
            reply.status,
            reply.body
        );
    }
    // The custodian changes its Silicon's work as itself.
    let renamed = call(
        &app,
        Method::PATCH,
        &todo_path,
        Some(&bearer(&ada_token)),
        Some(json!({"title": "Translate the whole brief"})),
        &[],
    )
    .await?;
    ensure!(renamed.status == StatusCode::OK, "{}", renamed.body);
    let outsider_patch = call(
        &app,
        Method::PATCH,
        &todo_path,
        Some(&bearer(&bea_token)),
        Some(json!({"title": "Not yours"})),
        &[],
    )
    .await?;
    ensure!(
        outsider_patch.status == StatusCode::FORBIDDEN,
        "{}",
        outsider_patch.body
    );

    // Projects: public means "the owner's circle"; private means members only.
    let project = call(
        &app,
        Method::POST,
        "/api/v1/projects",
        Some(&bearer(&scout_token)),
        Some(json!({"name": "Routes launch", "silicon_ids": [scout.id]})),
        &[],
    )
    .await?;
    ensure!(project.status == StatusCode::CREATED, "{}", project.body);
    let project_path = format!(
        "/api/v1/projects/{}",
        project.body["id"].as_str().context("id")?
    );
    let circle_read = call(
        &app,
        Method::GET,
        &project_path,
        Some(&bearer(&ada_token)),
        None,
        &[],
    )
    .await?;
    ensure!(circle_read.status == StatusCode::OK);
    let stranger_read = call(
        &app,
        Method::GET,
        &project_path,
        Some(&bearer(&cy_token)),
        None,
        &[],
    )
    .await?;
    ensure!(stranger_read.status == StatusCode::NOT_FOUND);
    // Changing visibility is checked online: a sign-in Accounts revoked cannot do it.
    let revoked = accounts.token_with(&scout, |c| c["jti"] = json!("revoked"));
    accounts.introspection(&revoked, false).await;
    let refused = call(
        &app,
        Method::PATCH,
        &project_path,
        Some(&bearer(&revoked)),
        Some(json!({"private": true})),
        &[],
    )
    .await?;
    ensure!(
        refused.status == StatusCode::UNAUTHORIZED && refused.code() == "token_revoked",
        "{}",
        refused.body
    );
    let private = call(
        &app,
        Method::PATCH,
        &project_path,
        Some(&bearer(&scout_token)),
        Some(json!({"private": true})),
        &[],
    )
    .await?;
    ensure!(private.status == StatusCode::OK, "{}", private.body);
    // ada still reads it as the custodian of a member Silicon.
    let custodian_read = call(
        &app,
        Method::GET,
        &project_path,
        Some(&bearer(&ada_token)),
        None,
        &[],
    )
    .await?;
    ensure!(custodian_read.status == StatusCode::OK);

    // Notification settings: the Silicon, or its custodian with ?silicon=.
    let settings_path = format!("/api/v1/notification-settings?silicon={}", scout.id);
    let custodian_settings = call(
        &app,
        Method::GET,
        &settings_path,
        Some(&bearer(&ada_token)),
        None,
        &[],
    )
    .await?;
    ensure!(
        custodian_settings.status == StatusCode::OK,
        "{}",
        custodian_settings.body
    );
    let stranger_settings = call(
        &app,
        Method::GET,
        &settings_path,
        Some(&bearer(&bea_token)),
        None,
        &[],
    )
    .await?;
    ensure!(
        stranger_settings.status == StatusCode::FORBIDDEN
            && stranger_settings.code() == "not_custodian"
    );
    let carbon_settings = call(
        &app,
        Method::GET,
        "/api/v1/notification-settings",
        Some(&bearer(&ada_token)),
        None,
        &[],
    )
    .await?;
    ensure!(
        carbon_settings.status == StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        carbon_settings.body
    );

    // Email: one preference per account, defaulting to the email shared with Commit.
    let email = call(
        &app,
        Method::GET,
        "/api/v1/email-settings",
        Some(&bearer(&ada_token)),
        None,
        &[],
    )
    .await?;
    ensure!(
        email.body["email"] == "routes-ada@example.test" && email.body["saved"] == false,
        "{}",
        email.body
    );
    let silicon_email = call(
        &app,
        Method::GET,
        "/api/v1/email-settings",
        Some(&bearer(&scout_token)),
        None,
        &[],
    )
    .await?;
    ensure!(
        silicon_email.body["email"] == "" && silicon_email.body["hint"].is_string(),
        "{}",
        silicon_email.body
    );
    Ok(())
}
