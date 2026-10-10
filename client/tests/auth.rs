//! Silicon Accounts public-client sign-in against a stub of the Accounts token endpoints.

use std::time::Duration;

use serde_json::{Value, json};
use silicon_commit_client::ExposeSecret as _;
use silicon_commit_client::auth::{
    AccountKind, AccountsAuth, DeviceEvent, DeviceStatus, SignInRefusal, peek_claims,
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_string_contains, method, path},
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn tokens(access: &str, refresh: &str, kind: &str, id: &str) -> Value {
    let mut account = json!({"uuid":"zQo","membership_id":"commit:zQo","kind":kind,"id":id,
        "display_name":"Ada","pfp_url":"","version":3});
    if kind == "silicon" {
        account["custodian"] = json!({"uuid":"cUs","id":"c:custodian"});
    } else {
        account["email"] = json!("ada@example.test");
    }
    json!({"access_token":access,"token_type":"Bearer","expires_in":1800,"refresh_token":refresh,
        "refresh_token_expires_at":"2029-03-25T02:33:57.696Z","scope":"profile email",
        "membership_id":"commit:zQo","account":account})
}

fn oauth_error(code: &str, description: &str) -> ResponseTemplate {
    ResponseTemplate::new(400).set_body_json(json!({"error":code,"error_description":description}))
}

fn device_authorization(interval: u64) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({"device_code":"sad_device_secret","user_code":"WDJB-MJHT",
        "verification_uri":"http://accounts.test/device","verification_uri_complete":"http://accounts.test/device?code=WDJB-MJHT",
        "expires_in":600,"interval":interval}))
}

async fn mount_poll(server: &MockServer, response: ResponseTemplate) {
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .and(body_string_contains(
            "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code",
        ))
        .and(body_string_contains("device_code=sad_device_secret"))
        .and(body_string_contains("client_id=commit"))
        .respond_with(response)
        .up_to_n_times(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn device_sign_in_uses_commits_client_id_and_waits_for_approval() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/device/authorize"))
        .and(|r: &wiremock::Request| {
            let body: Value = serde_json::from_slice(&r.body).unwrap_or_default();
            body["client_id"] == "commit"
                && body["scope"] == "email"
                && body["client_label"] == "test laptop"
                && !r.headers.contains_key("authorization")
        })
        .respond_with(device_authorization(1))
        .expect(1)
        .mount(&server)
        .await;
    mount_poll(
        &server,
        oauth_error("authorization_pending", "Not approved yet."),
    )
    .await;
    mount_poll(
        &server,
        ResponseTemplate::new(200).set_body_json(tokens("eyJ.a.b", "sar_first", "carbon", "c:ada")),
    )
    .await;
    let auth = AccountsAuth::new(server.uri())?;
    let device = auth
        .start_device_sign_in(Some("email"), Some("test laptop"))
        .await?;
    assert_eq!(device.user_code, "WDJB-MJHT");
    assert!(!format!("{device:?}").contains("sad_device_secret"));
    let mut events = Vec::new();
    let sign_in = auth
        .wait_for_device_sign_in(&device, |event| events.push(format!("{event:?}")))
        .await?;
    assert_eq!(events, vec!["Pending".to_owned()]);
    assert_eq!(sign_in.access_token.expose_secret(), "eyJ.a.b");
    assert_eq!(
        sign_in
            .refresh_token
            .as_ref()
            .map(|t| t.expose_secret().to_owned())
            .as_deref(),
        Some("sar_first")
    );
    assert_eq!(
        (sign_in.account.uuid.as_str(), sign_in.account.id.as_str()),
        ("zQo", "c:ada")
    );
    assert_eq!(sign_in.account.kind, AccountKind::Carbon);
    assert_eq!(sign_in.account.email.as_deref(), Some("ada@example.test"));
    assert_eq!(sign_in.refresh_expires_at, Some(1_869_100_437));
    assert!(!format!("{sign_in:?}").contains("sar_first"));
    Ok(())
}

#[tokio::test]
async fn device_polls_map_every_answer_and_waiting_ends_on_denial_or_expiry() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/device/authorize"))
        .respond_with(device_authorization(1))
        .mount(&server)
        .await;
    let auth = AccountsAuth::new(server.uri())?;
    let device = auth.start_device_sign_in(None, None).await?;
    mount_poll(&server, oauth_error("slow_down", "Too fast.")).await;
    assert!(matches!(
        auth.poll_device_sign_in(&device).await?,
        DeviceStatus::SlowDown
    ));
    mount_poll(&server, oauth_error("authorization_pending", "Waiting.")).await;
    assert!(matches!(
        auth.poll_device_sign_in(&device).await?,
        DeviceStatus::Pending
    ));
    mount_poll(&server, oauth_error("access_denied", "Denied.")).await;
    let denied = auth
        .wait_for_device_sign_in(&device, |_| {})
        .await
        .err()
        .ok_or("signed in")?;
    assert_eq!(denied.code(), "access_denied");
    assert!(denied.to_string().contains("WDJB-MJHT"), "{denied}");
    mount_poll(&server, oauth_error("expired_token", "Expired.")).await;
    let expired = auth
        .wait_for_device_sign_in(&device, |_| {})
        .await
        .err()
        .ok_or("signed in")?;
    assert_eq!(expired.code(), "expired_token");
    mount_poll(
        &server,
        ResponseTemplate::new(503)
            .set_body_json(json!({"error":{"code":"unavailable","message":"Down."}})),
    )
    .await;
    mount_poll(
        &server,
        ResponseTemplate::new(200).set_body_json(tokens("eyJ.c.d", "sar_x", "carbon", "c:ada")),
    )
    .await;
    let mut transient = 0;
    auth.wait_for_device_sign_in(&device, |event| {
        if matches!(event, DeviceEvent::TransientError { .. }) {
            transient += 1;
        }
    })
    .await?;
    assert_eq!(transient, 1);
    Ok(())
}

#[tokio::test]
async fn slt_exchange_sends_the_client_id_alone_and_explains_refusals() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .and(body_string_contains(
            "grant_type=urn%3Asilicon%3Aparams%3Aoauth%3Agrant-type%3Aslt",
        ))
        .and(body_string_contains("slt=slt_good"))
        .and(body_string_contains("client_id=commit"))
        .and(|r: &wiremock::Request| {
            !r.headers.contains_key("authorization")
                && !String::from_utf8_lossy(&r.body).contains("client_secret")
        })
        .respond_with(ResponseTemplate::new(200).set_body_json(tokens(
            "eyJ.s.i",
            "sar_silicon",
            "silicon",
            "si:scout",
        )))
        .expect(1)
        .mount(&server)
        .await;
    let refusals = [
        (
            "slt_used",
            "The short-lived token was already used; each one works once. Get a new one.",
            SignInRefusal::AlreadyUsed,
        ),
        (
            "slt_late",
            "The short-lived token expired at 2026-10-10T00:00:00.000Z (they last 120 seconds); get a new one with `silicon-accounts login --app commit`.",
            SignInRefusal::Expired,
        ),
        (
            "slt_other",
            "The short-lived token was issued for the app 'remind', not for 'commit'; get one for 'commit' with `silicon-accounts login --app commit`.",
            SignInRefusal::WrongApp,
        ),
        (
            "slt_typo",
            "The short-lived token is not known: it is mistyped or was never issued.",
            SignInRefusal::Unknown,
        ),
    ];
    for (slt, description, _) in &refusals {
        Mock::given(method("POST"))
            .and(path("/v1/oauth/token"))
            .and(body_string_contains(format!("slt={slt}")))
            .respond_with(
                oauth_error("invalid_grant", description).insert_header("x-request-id", "req-slt"),
            )
            .mount(&server)
            .await;
    }
    let auth = AccountsAuth::new(server.uri())?;
    let sign_in = auth.exchange_slt("  slt_good\n").await?;
    assert_eq!(sign_in.account.kind, AccountKind::Silicon);
    assert_eq!(
        sign_in.account.custodian.as_ref().map(|c| c.id.as_str()),
        Some("c:custodian")
    );
    for (slt, description, refusal) in refusals {
        let error = auth.exchange_slt(slt).await.err().ok_or("accepted")?;
        assert!(error.is_sign_in_refused());
        assert_eq!(SignInRefusal::of(&error), Some(refusal));
        assert_eq!(error.request_id(), Some("req-slt"));
        let text = error.to_string();
        assert!(text.contains(description), "{text}");
    }
    let before = server.received_requests().await.unwrap_or_default().len();
    for bad in [
        "",
        "   ",
        "sar_refresh_token",
        "stk-0123456789",
        "oac_old_code",
    ] {
        let error = auth.exchange_slt(bad).await.err().ok_or("accepted")?;
        assert_eq!(error.code(), "invalid_input");
        if !bad.trim().is_empty() {
            assert!(
                !error.to_string().contains(bad),
                "the value is never echoed: {error}"
            );
        }
    }
    assert_eq!(
        server.received_requests().await.unwrap_or_default().len(),
        before,
        "malformed tokens are never sent"
    );
    Ok(())
}

#[tokio::test]
async fn refresh_rotates_with_the_client_id_and_reuse_is_a_refusal() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("refresh_token=sar_old"))
        .and(body_string_contains("client_id=commit"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(tokens("eyJ.n.ew", "sar_new", "carbon", "c:ada")),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .and(body_string_contains("refresh_token=sar_used"))
        .respond_with(oauth_error("invalid_grant", "The sign-in this refresh token belongs to was revoked at 2026-10-10T00:00:00.000Z (refresh_token_reuse); sign in again."))
        .mount(&server)
        .await;
    let auth = AccountsAuth::new(server.uri())?;
    let renewed = auth.refresh("sar_old").await?;
    assert_eq!(
        renewed
            .refresh_token
            .as_ref()
            .map(|t| t.expose_secret().to_owned())
            .as_deref(),
        Some("sar_new")
    );
    let reused = auth.refresh("sar_used").await.err().ok_or("accepted")?;
    assert!(reused.is_sign_in_refused());
    assert_eq!(SignInRefusal::of(&reused), Some(SignInRefusal::SignInEnded));
    Ok(())
}

#[tokio::test]
async fn revoke_posts_the_token_with_the_client_id() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/revoke"))
        .and(body_string_contains("token=sar_live"))
        .and(body_string_contains("token_type_hint=refresh_token"))
        .and(body_string_contains("client_id=commit"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"revoked":true})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/revoke"))
        .and(body_string_contains("token=sar_gone"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"revoked":false,"message":"Nothing was revoked: unknown token."}),
        ))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/revoke"))
        .and(body_string_contains("token=sar_down"))
        .respond_with(ResponseTemplate::new(503).set_body_string("upstream down"))
        .mount(&server)
        .await;
    let auth = AccountsAuth::new(server.uri())?;
    assert!(auth.revoke("sar_live").await?.revoked);
    let gone = auth.revoke("sar_gone").await?;
    assert!(!gone.revoked && gone.message.is_some_and(|m| m.contains("unknown")));
    let down = auth.revoke("sar_down").await.err().ok_or("accepted")?;
    assert!(down.is_transient(), "{down}");
    assert!(
        !down.to_string().contains("upstream down"),
        "foreign bodies are not echoed: {down}"
    );
    Ok(())
}

#[tokio::test]
async fn unreachable_accounts_errors_name_the_service_and_url_rules_apply() -> TestResult {
    let auth = AccountsAuth::new("http://127.0.0.1:9")?;
    let error = auth
        .exchange_slt("slt_value")
        .await
        .err()
        .ok_or("accepted")?;
    assert!(error.is_transient());
    assert!(
        error
            .to_string()
            .contains("Silicon Accounts at http://127.0.0.1:9"),
        "{error}"
    );
    assert!(!error.to_string().contains("slt_value"));
    assert!(
        AccountsAuth::new("http://accounts.example.com").is_err(),
        "plain http only for this machine"
    );
    assert!(
        AccountsAuth::builder("https://accounts.example.com")
            .app_id("Not An Id")
            .build()
            .is_err()
    );
    let custom = AccountsAuth::builder("https://accounts.example.com/")
        .app_id("commit-dev")
        .build()?;
    assert_eq!(custom.app_id(), "commit-dev");
    tokio::time::timeout(Duration::from_secs(1), async {}).await?;
    Ok(())
}

#[test]
fn claims_are_peeked_for_display_only() {
    use base64::Engine as _;
    let encode = |v: &Value| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v.to_string());
    let token = format!(
        "{}.{}.signature",
        encode(&json!({"alg":"EdDSA","kid":"dev-1"})),
        encode(
            &json!({"sub":"zQo","aud":"commit","exp":1_900_000_000,"kind":"silicon","id":"si:scout","iss":"http://localhost:9590"})
        )
    );
    let claims = peek_claims(&token).expect("claims");
    assert_eq!((claims.sub.as_str(), claims.exp), ("zQo", 1_900_000_000));
    assert_eq!(claims.kind, Some(AccountKind::Silicon));
    assert_eq!(claims.aud, vec!["commit".to_owned()]);
    assert!(peek_claims("sar_not_a_jwt").is_none());
    assert!(peek_claims("a.b").is_none());
}
