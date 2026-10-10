//! `commit login` (device flow and short-lived tokens) against a stub Silicon Accounts and
//! a stub Commit API.

mod support;

use serde_json::{Value, json};
use std::fs;
use support::{Home, json_output, oauth_error, stderr, tokens, with_stdin};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_string_contains, header, method, path},
};

async fn me_endpoint(api: &MockServer, token: &str) {
    Mock::given(method("GET"))
        .and(path("/api/v1/me"))
        .and(header("authorization", format!("Bearer {token}").as_str()))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                json!({"authenticated":true,"uuid":"zQo","id":"c:ada","kind":"carbon"}),
            ),
        )
        .mount(api)
        .await;
}

async fn device_authorize(accounts: &MockServer, scope: Option<&'static str>) {
    Mock::given(method("POST"))
        .and(path("/v1/device/authorize"))
        .and(move |r: &wiremock::Request| {
            let body: Value = serde_json::from_slice(&r.body).unwrap_or_default();
            body["client_id"] == "commit"
                && body.get("scope").and_then(Value::as_str) == scope
                && body["client_label"]
                    .as_str()
                    .is_some_and(|l| l.starts_with("Commit CLI"))
        })
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "device_code":"sad_device_secret","user_code":"MVHB-KQAW",
            "verification_uri":"http://accounts.test/device",
            "verification_uri_complete":"http://accounts.test/device?code=MVHB-KQAW",
            "expires_in":600,"interval":1})))
        .expect(1)
        .mount(accounts)
        .await;
}

async fn device_poll(accounts: &MockServer, response: ResponseTemplate) {
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .and(body_string_contains("device_code=sad_device_secret"))
        .and(body_string_contains("client_id=commit"))
        .respond_with(response)
        .up_to_n_times(1)
        .mount(accounts)
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn device_login_waits_through_pending_and_slow_down_then_saves_a_private_session() {
    let home = Home::new();
    let accounts = MockServer::start().await;
    let api = MockServer::start().await;
    device_authorize(&accounts, Some("email")).await;
    device_poll(&accounts, oauth_error("authorization_pending", "Not yet.")).await;
    device_poll(&accounts, oauth_error("slow_down", "Too fast.")).await;
    device_poll(
        &accounts,
        ResponseTemplate::new(200).set_body_json(tokens(
            "eyJ.device.access",
            "sar_device",
            "carbon",
            "c:ada",
        )),
    )
    .await;
    me_endpoint(&api, "eyJ.device.access").await;
    let output = home
        .command()
        .args([
            "--accounts-url",
            &accounts.uri(),
            "--api-url",
            &api.uri(),
            "login",
            "--scope",
            "email",
            "--json",
        ])
        .output()
        .unwrap();
    let errors = stderr(&output);
    let result = json_output(output);
    let progress: Vec<Value> = errors
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    assert_eq!(progress[0]["event"], "device_code", "{errors}");
    assert_eq!(progress[0]["user_code"], "MVHB-KQAW");
    assert_eq!(
        progress[0]["verification_uri_complete"],
        "http://accounts.test/device?code=MVHB-KQAW"
    );
    assert!(
        progress
            .iter()
            .any(|p| p["event"] == "slow_down" && p["interval"] == 6),
        "{errors}"
    );
    assert!(!errors.contains("sad_device_secret"));
    assert_eq!(result["authenticated"], true);
    assert_eq!(
        (
            result["uuid"].as_str(),
            result["id"].as_str(),
            result["kind"].as_str()
        ),
        (Some("zQo"), Some("c:ada"), Some("carbon"))
    );
    assert_eq!(result["method"], "device");
    assert_eq!(result["verified"], true);
    assert_eq!(result["refresh_expires_at"], "2029-03-25T02:33:57Z");
    let saved = home.read_session(None);
    assert_eq!(saved["version"], 2);
    assert_eq!(saved["refresh_token"], "sar_device");
    assert_eq!(saved["accounts_url"], accounts.uri());
    assert_eq!(saved["api_url"], api.uri());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = |p: &std::path::Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&home.session_path(None)), 0o600);
        assert_eq!(mode(&home.state()), 0o700);
    }
    let leftovers: Vec<_> = fs::read_dir(home.state())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert!(
        leftovers
            .iter()
            .all(|n| n == "session.json" || n == "session.lock"),
        "{leftovers:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn device_login_reports_denied_and_expired_codes_and_saves_nothing() {
    for (answer, code, words) in [
        ("access_denied", "access_denied", "denied"),
        ("expired_token", "expired_token", "expired"),
    ] {
        let home = Home::new();
        let accounts = MockServer::start().await;
        device_authorize(&accounts, None).await;
        device_poll(&accounts, oauth_error(answer, "No.")).await;
        let output = home
            .command()
            .env("ACCOUNTS_URL", accounts.uri())
            .env("COMMIT_API_URL", "http://127.0.0.1:9")
            .arg("login")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        let errors = stderr(&output);
        assert!(
            errors.contains("MVHB-KQAW")
                && errors.contains("To sign in to Commit, open http://accounts.test/device"),
            "{errors}"
        );
        assert!(errors.contains(code) && errors.contains(words), "{errors}");
        assert!(!home.session_path(None).exists());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn silicons_sign_in_with_a_short_lived_token_on_stdin_as_a_public_client() {
    let home = Home::new();
    let accounts = MockServer::start().await;
    let api = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .and(body_string_contains(
            "grant_type=urn%3Asilicon%3Aparams%3Aoauth%3Agrant-type%3Aslt",
        ))
        .and(body_string_contains("slt=slt_secret_value"))
        .and(body_string_contains("client_id=commit"))
        .and(|r: &wiremock::Request| !r.headers.contains_key("authorization"))
        .respond_with(ResponseTemplate::new(200).set_body_json(tokens(
            "eyJ.silicon.access",
            "sar_silicon",
            "silicon",
            "si:scout",
        )))
        .expect(3)
        .mount(&accounts)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/revoke"))
        .and(body_string_contains("token=sar_silicon"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"revoked":true})))
        .mount(&accounts)
        .await;
    me_endpoint(&api, "eyJ.silicon.access").await;
    let configure = |command: &mut std::process::Command| {
        command
            .env("ACCOUNTS_URL", accounts.uri())
            .env("COMMIT_API_URL", api.uri())
            .env("SILICON_HOME", home.0.join("silicon"));
    };
    let mut piped = home.command();
    configure(&mut piped);
    piped.args(["login", "--slt-stdin", "--json"]);
    let output = with_stdin(piped, "slt_secret_value\n");
    let both = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        stderr(&output)
    );
    assert!(
        !both.contains("slt_secret_value"),
        "the token is never echoed: {both}"
    );
    let result = json_output(output);
    assert_eq!(result["kind"], "silicon");
    assert_eq!(
        result["custodian"],
        json!({"uuid":"cUs","id":"c:custodian"})
    );
    assert_eq!(result["method"], "slt");
    let saved_at = home.0.join("silicon/.commit/session.json");
    assert!(saved_at.exists(), "SILICON_HOME holds the state");
    assert!(!home.state().exists());
    // --slt and the positional transition form work the same way.
    for args in [
        vec!["login", "--slt", "slt_secret_value"],
        vec!["login", "slt_secret_value"],
    ] {
        let mut command = home.command();
        configure(&mut command);
        let output = command.args(&args).output().unwrap();
        assert!(output.status.success(), "{args:?}: {}", stderr(&output));
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("Signed in to Commit as si:scout")
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn refused_short_lived_tokens_explain_why_and_how_to_mint_a_new_one() {
    let home = Home::new();
    let accounts = MockServer::start().await;
    let cases = [
        (
            "slt_used",
            "The short-lived token was already used; each one works once. Get a new one.",
            "already_used",
        ),
        (
            "slt_late",
            "The short-lived token expired at 2026-10-10T00:00:00.000Z (they last 120 seconds); get a new one with `silicon-accounts login --app commit`.",
            "expired",
        ),
        (
            "slt_remind",
            "The short-lived token was issued for the app 'remind', not for 'commit'; get one for 'commit' with `silicon-accounts login --app commit`.",
            "wrong_app",
        ),
    ];
    for (slt, description, _) in cases {
        Mock::given(method("POST"))
            .and(path("/v1/oauth/token"))
            .and(body_string_contains(format!("slt={slt}")))
            .respond_with(oauth_error("invalid_grant", description))
            .mount(&accounts)
            .await;
    }
    for (slt, description, reason) in cases {
        let mut command = home.command();
        command
            .env("ACCOUNTS_URL", accounts.uri())
            .args(["login", "--slt-stdin", "--json"]);
        let output = with_stdin(command, slt);
        assert_eq!(output.status.code(), Some(1));
        let line = stderr(&output);
        let error: Value = serde_json::from_str(line.trim()).unwrap_or_else(|_| panic!("{line}"));
        assert_eq!(error["error"]["code"], "invalid_grant");
        assert_eq!(error["error"]["reason"], reason);
        assert_eq!(error["error"]["message"], description);
        assert!(
            error["error"]["hint"]
                .as_str()
                .unwrap()
                .contains("silicon-accounts login --app commit -q | commit login --slt-stdin")
        );
        let text = home
            .command()
            .env("ACCOUNTS_URL", accounts.uri())
            .args(["login", "--slt", slt])
            .output()
            .unwrap();
        let text = stderr(&text);
        assert!(
            text.contains(description) && text.contains("hint:"),
            "{text}"
        );
    }
    assert!(!home.session_path(None).exists());
    let requests = accounts.received_requests().await.unwrap().len();
    for (args, input, expected) in [
        (vec!["login", "--slt-stdin"], Some(""), "empty"),
        (vec!["login", "--slt", "sar_refresh_value"], None, "slt_"),
        (vec!["login", "statuss"], None, "neither a subcommand"),
        (
            vec!["login", "--slt", "slt_x", "--scope", "email"],
            None,
            "device sign-in",
        ),
    ] {
        let mut command = home.command();
        command.env("ACCOUNTS_URL", accounts.uri()).args(&args);
        let output = match input {
            Some(text) => with_stdin(command, text),
            None => command.output().unwrap(),
        };
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        let text = stderr(&output);
        assert!(text.contains(expected), "{args:?}: {text}");
        assert!(!text.contains("sar_refresh_value"));
    }
    assert_eq!(
        accounts.received_requests().await.unwrap().len(),
        requests,
        "invalid input never reaches Silicon Accounts"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn signing_in_again_replaces_the_session_and_ends_the_previous_sign_in() {
    let home = Home::new();
    let old_accounts = MockServer::start().await;
    let accounts = MockServer::start().await;
    let api = MockServer::start().await;
    home.write_session(
        None,
        &support::session(
            &old_accounts.uri(),
            &api.uri(),
            "eyJ.old",
            "sar_previous",
            support::now() + 900,
        ),
    );
    Mock::given(method("POST"))
        .and(path("/v1/oauth/revoke"))
        .and(body_string_contains("token=sar_previous"))
        .and(body_string_contains("client_id=commit"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"revoked":true})))
        .expect(1)
        .mount(&old_accounts)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .and(body_string_contains("slt=slt_next"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(tokens("eyJ.next", "sar_next", "silicon", "si:scout")),
        )
        .expect(1)
        .mount(&accounts)
        .await;
    me_endpoint(&api, "eyJ.next").await;
    let output = home
        .command()
        .args([
            "--accounts-url",
            &accounts.uri(),
            "login",
            "--slt",
            "slt_next",
            "--json",
        ])
        .output()
        .unwrap();
    let result = json_output(output);
    assert_eq!(result["id"], "si:scout");
    let saved = home.read_session(None);
    assert_eq!(saved["refresh_token"], "sar_next");
    assert_eq!(saved["accounts_url"], accounts.uri());
    assert_eq!(
        saved["api_url"],
        api.uri(),
        "the API defaults to the saved session's"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn no_save_prints_the_tokens_once_and_writes_nothing() {
    let home = Home::new();
    let accounts = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(tokens(
            "eyJ.printed",
            "sar_printed",
            "silicon",
            "si:scout",
        )))
        .expect(1)
        .mount(&accounts)
        .await;
    let output = home
        .command()
        .args([
            "--accounts-url",
            &accounts.uri(),
            "login",
            "--slt",
            "slt_once",
            "--no-save",
        ])
        .output()
        .unwrap();
    assert!(stderr(&output).contains("secret"));
    let printed = json_output(output);
    assert_eq!(printed["access_token"], "eyJ.printed");
    assert_eq!(printed["refresh_token"], "sar_printed");
    assert!(!home.state().exists());
}
