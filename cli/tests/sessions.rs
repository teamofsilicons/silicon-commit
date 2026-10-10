//! Saved sessions: single-flight refresh, replay after 401, ended and old sessions, logout,
//! profiles, and sessions that must not travel to another server.

mod support;

use serde_json::{Value, json};
use std::fs;
use support::{Home, json_output, jwt, now, oauth_error, run, session, stderr, tokens};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_string_contains, header, method, path},
};

async fn me_ok(api: &MockServer, token: &str, expected: u64) {
    Mock::given(method("GET"))
        .and(path("/api/v1/me"))
        .and(header("authorization", format!("Bearer {token}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"uuid":"zQo","id":"c:ada-renamed","kind":"carbon","display_name":"Ada L."}),
        ))
        .expect(expected)
        .mount(api)
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_expiring_session_is_refreshed_once_across_concurrent_commands() {
    let home = Home::new();
    let accounts = MockServer::start().await;
    let api = MockServer::start().await;
    home.write_session(
        None,
        &session(
            &accounts.uri(),
            &api.uri(),
            "eyJ.expired",
            "sar_old",
            now() + 30,
        ),
    );
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("refresh_token=sar_old"))
        .and(body_string_contains("client_id=commit"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(std::time::Duration::from_millis(300))
                .set_body_json(tokens("eyJ.renewed", "sar_new", "carbon", "c:ada")),
        )
        .expect(1)
        .mount(&accounts)
        .await;
    me_ok(&api, "eyJ.renewed", 2).await;
    let first = run({
        let mut c = home.command();
        c.args(["login", "status", "--json"]);
        c
    });
    let second = run({
        let mut c = home.command();
        c.args(["login", "status", "--json"]);
        c
    });
    let (first, second) = tokio::join!(first, second);
    for output in [first, second] {
        let status = json_output(output);
        assert_eq!(status["authenticated"], true);
        assert_eq!(status["verified"], true);
    }
    let saved = home.read_session(None);
    assert_eq!(saved["refresh_token"], "sar_new");
    assert_eq!(saved["access_token"], "eyJ.renewed");
    assert!(saved["expires_at"].as_i64().unwrap() > now() + 1700);
    assert!(saved["refresh_started_at"].is_null());
    assert_eq!(
        saved["account"]["id"], "c:ada-renamed",
        "the id the API reports is remembered"
    );
    assert_eq!(saved["account"]["display_name"], "Ada L.");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_access_token_is_refreshed_and_the_identical_write_is_replayed() {
    let home = Home::new();
    let accounts = MockServer::start().await;
    let api = MockServer::start().await;
    home.write_session(
        None,
        &session(
            &accounts.uri(),
            &api.uri(),
            "eyJ.old",
            "sar_old",
            now() + 1500,
        ),
    );
    let payload = json!({"title":"Original","assigned_to":"si:builder"});
    let input = home.0.join("todo.json");
    fs::write(&input, payload.to_string()).unwrap();
    let changed = input.clone();
    Mock::given(method("POST"))
        .and(path("/api/v1/todos"))
        .and(header("authorization", "Bearer eyJ.old"))
        .respond_with(move |_: &wiremock::Request| {
            fs::write(&changed, json!({"title":"Changed","assigned_to":"si:builder"}).to_string()).unwrap();
            ResponseTemplate::new(401).set_body_json(json!({"error":{"code":"session_ended","message":"Signed out after this token was issued."}}))
        })
        .expect(1)
        .mount(&api)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .and(body_string_contains("refresh_token=sar_old"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(tokens("eyJ.new", "sar_new", "carbon", "c:ada")),
        )
        .expect(1)
        .mount(&accounts)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/todos"))
        .and(header("authorization", "Bearer eyJ.new"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id":"created"})))
        .expect(1)
        .mount(&api)
        .await;
    let output = home
        .command()
        .args([
            "todos",
            "create",
            "--data",
            &format!("@{}", input.display()),
        ])
        .output()
        .unwrap();
    assert_eq!(json_output(output)["id"], "created");
    let writes: Vec<_> = api
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.url.path() == "/api/v1/todos")
        .collect();
    assert_eq!(writes.len(), 2);
    assert_eq!(writes[0].body, writes[1].body, "the same body is replayed");
    assert_eq!(
        serde_json::from_slice::<Value>(&writes[1].body).unwrap(),
        payload
    );
    assert_eq!(
        writes[0].headers.get("idempotency-key"),
        writes[1].headers.get("idempotency-key")
    );
    assert_eq!(home.read_session(None)["refresh_token"], "sar_new");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_uncertain_refresh_keeps_the_session_and_says_so() {
    let home = Home::new();
    let accounts = MockServer::start().await;
    let api = MockServer::start().await;
    home.write_session(
        None,
        &session(&accounts.uri(), &api.uri(), "eyJ.old", "sar_old", 1),
    );
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .respond_with(ResponseTemplate::new(503).set_body_json(
            json!({"error":{"code":"unavailable","message":"Silicon Accounts is restarting."}}),
        ))
        .expect(2)
        .mount(&accounts)
        .await;
    let failed = home.command().args(["todos", "list"]).output().unwrap();
    assert_eq!(failed.status.code(), Some(1));
    let text = stderr(&failed);
    assert!(
        text.contains("Could not refresh the Commit session") && text.contains("restarting"),
        "{text}"
    );
    let saved = home.read_session(None);
    assert_eq!(saved["access_token"], "eyJ.old");
    assert_eq!(saved["refresh_token"], "sar_old");
    assert!(
        saved["refresh_started_at"].is_i64(),
        "the in-flight marker stays: the answer was lost"
    );
    let status = json_output(
        home.command()
            .args(["login", "status", "--json"])
            .output()
            .unwrap(),
    );
    assert_eq!(status["authenticated"], true);
    assert_eq!(status["verified"], false);
    assert!(status["warning"].as_str().unwrap().contains("restarting"));
    assert!(api.received_requests().await.unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_refresh_token_ends_the_session_for_good() {
    let home = Home::new();
    let accounts = MockServer::start().await;
    let api = MockServer::start().await;
    home.write_session(
        None,
        &session(&accounts.uri(), &api.uri(), "eyJ.old", "sar_reused", 1),
    );
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .respond_with(oauth_error(
            "invalid_grant",
            "The sign-in this refresh token belongs to was revoked at 2026-10-10T00:00:00.000Z (refresh_token_reuse); sign in again.",
        ))
        .expect(1)
        .mount(&accounts)
        .await;
    let status = json_output(
        home.command()
            .args(["login", "status", "--json"])
            .output()
            .unwrap(),
    );
    assert_eq!(status["authenticated"], false);
    assert_eq!(status["reason"], "session_ended");
    assert!(
        status["message"]
            .as_str()
            .unwrap()
            .contains("refresh_token_reuse")
    );
    let saved = home.read_session(None);
    assert_eq!(saved["ended"]["reason"], "sign_in_ended");
    let listed = home.command().args(["todos", "list"]).output().unwrap();
    assert_eq!(listed.status.code(), Some(1));
    let text = stderr(&listed);
    assert!(
        text.contains("session_ended") && text.contains("commit login"),
        "{text}"
    );
    assert_eq!(
        accounts.received_requests().await.unwrap().len(),
        1,
        "an ended session is not refreshed again"
    );
    let logout = json_output(home.command().args(["logout", "--json"]).output().unwrap());
    assert_eq!(logout["signed_out"], true);
    assert_eq!(logout["reason"], "session_ended");
    assert!(!home.session_path(None).exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn logout_revokes_at_silicon_accounts_before_deleting_the_session() {
    let home = Home::new();
    let accounts = MockServer::start().await;
    let api = MockServer::start().await;
    home.write_session(
        None,
        &session(
            &accounts.uri(),
            &api.uri(),
            "eyJ.live",
            "sar_live",
            now() + 1500,
        ),
    );
    Mock::given(method("POST"))
        .and(path("/v1/oauth/revoke"))
        .and(body_string_contains("token=sar_live"))
        .respond_with(ResponseTemplate::new(503).set_body_string("down"))
        .up_to_n_times(1)
        .mount(&accounts)
        .await;
    let failed = home.command().args(["logout"]).output().unwrap();
    assert_eq!(failed.status.code(), Some(1));
    assert!(stderr(&failed).contains("--force"), "{}", stderr(&failed));
    assert!(home.session_path(None).exists(), "kept for a retry");
    Mock::given(method("POST"))
        .and(path("/v1/oauth/revoke"))
        .and(body_string_contains("token=sar_live"))
        .and(body_string_contains("token_type_hint=refresh_token"))
        .and(body_string_contains("client_id=commit"))
        .and(|r: &wiremock::Request| !r.headers.contains_key("authorization"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"revoked":true})))
        .expect(1)
        .mount(&accounts)
        .await;
    let done = json_output(
        home.command()
            .env("COMMIT_ACCESS_TOKEN", "eyJ.unrelated")
            .args(["logout", "--json"])
            .output()
            .unwrap(),
    );
    assert_eq!(
        done,
        json!({"signed_out":true,"id":"c:ada","uuid":"zQo","revoked":true})
    );
    assert!(!home.session_path(None).exists());
    let again = json_output(home.command().args(["logout", "--json"]).output().unwrap());
    assert_eq!(again, json!({"signed_out":false,"reason":"not_signed_in"}));
    // --force deletes locally when Silicon Accounts cannot be reached.
    home.write_session(
        None,
        &session(
            "http://127.0.0.1:9",
            &api.uri(),
            "eyJ.live",
            "sar_unreachable",
            now() + 1500,
        ),
    );
    let forced = home
        .command()
        .args(["logout", "--force", "--json"])
        .output()
        .unwrap();
    assert!(stderr(&forced).contains("warning"));
    let forced = json_output(forced);
    assert_eq!(forced["revoked"], false);
    assert!(!home.session_path(None).exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn old_and_damaged_session_files_ask_for_a_new_sign_in_without_network() {
    let home = Home::new();
    let server = MockServer::start().await;
    let legacy = json!({"access_token":"oat_saved","refresh_token":"ort_saved","api_url":server.uri(),
        "org_id":"tos","actor":{"type":"carbon","public_id":"c:person"},"expires_at":4102444800_u64});
    for (content, reason) in [
        (legacy.to_string(), "legacy_session"),
        (
            "{\"version\":2,\"oops\":true}".to_owned(),
            "unreadable_session",
        ),
        ("garbage".to_owned(), "unreadable_session"),
    ] {
        fs::create_dir_all(home.state()).unwrap();
        fs::write(home.session_path(None), &content).unwrap();
        let status = json_output(
            home.command()
                .args(["login", "status", "--json"])
                .output()
                .unwrap(),
        );
        assert_eq!(status["authenticated"], false);
        assert_eq!(status["reason"], reason);
        let listed = home
            .command()
            .env("COMMIT_API_URL", server.uri())
            .args(["todos", "list"])
            .output()
            .unwrap();
        assert_eq!(listed.status.code(), Some(1));
        let text = stderr(&listed);
        assert!(
            text.contains(reason) && text.contains("commit login"),
            "{text}"
        );
        assert!(!text.contains("oat_saved") && !text.contains("ort_saved"));
        assert_eq!(
            fs::read_to_string(home.session_path(None)).unwrap(),
            content,
            "status and commands never rewrite it"
        );
        let logout = json_output(home.command().args(["logout", "--json"]).output().unwrap());
        assert_eq!(logout["reason"], "unusable_session");
        assert!(!home.session_path(None).exists());
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn profiles_hold_independent_accounts() {
    let home = Home::new();
    let accounts = MockServer::start().await;
    let api = MockServer::start().await;
    home.write_session(
        Some("work"),
        &session(
            &accounts.uri(),
            &api.uri(),
            "eyJ.work",
            "sar_work",
            now() + 1500,
        ),
    );
    home.write_session(
        None,
        &session(
            &accounts.uri(),
            &api.uri(),
            "eyJ.personal",
            "sar_personal",
            now() + 1500,
        ),
    );
    for (profile, token) in [("work", "eyJ.work"), ("default", "eyJ.personal")] {
        Mock::given(method("GET"))
            .and(path("/api/v1/todos"))
            .and(header("authorization", format!("Bearer {token}").as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"profile":profile})))
            .expect(1)
            .mount(&api)
            .await;
    }
    assert_eq!(
        json_output(
            home.command()
                .args(["--profile", "work", "todos", "list"])
                .output()
                .unwrap()
        )["profile"],
        "work"
    );
    assert_eq!(
        json_output(home.command().args(["todos", "list"]).output().unwrap())["profile"],
        "default"
    );
    Mock::given(method("POST"))
        .and(path("/v1/oauth/revoke"))
        .and(body_string_contains("token=sar_work"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"revoked":true})))
        .expect(1)
        .mount(&accounts)
        .await;
    json_output(
        home.command()
            .env("COMMIT_PROFILE", "work")
            .args(["logout", "--json"])
            .output()
            .unwrap(),
    );
    assert!(!home.session_path(Some("work")).exists());
    assert!(home.session_path(None).exists());
    let invalid = home
        .command()
        .args(["--profile", "../work", "todos", "list"])
        .output()
        .unwrap();
    assert_eq!(invalid.status.code(), Some(2));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_saved_session_is_never_sent_to_another_server() {
    let home = Home::new();
    let accounts = MockServer::start().await;
    let api = MockServer::start().await;
    let other = MockServer::start().await;
    home.write_session(
        None,
        &session(
            &accounts.uri(),
            &api.uri(),
            "eyJ.saved",
            "sar_saved",
            now() + 1500,
        ),
    );
    let listed = home
        .command()
        .args(["--api-url", &other.uri(), "todos", "list"])
        .output()
        .unwrap();
    assert_eq!(listed.status.code(), Some(1));
    assert!(stderr(&listed).contains("signed_in_elsewhere"));
    let status = json_output(
        home.command()
            .env("ACCOUNTS_URL", other.uri())
            .args(["login", "status", "--json"])
            .output()
            .unwrap(),
    );
    assert_eq!(status["authenticated"], false);
    assert_eq!(status["reason"], "signed_in_elsewhere");
    assert_eq!(status["session_api_url"], api.uri());
    // The same API spelled with /api/v1 is the same server.
    Mock::given(method("GET"))
        .and(path("/api/v1/projects"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items":[]})))
        .expect(1)
        .mount(&api)
        .await;
    json_output(
        home.command()
            .args([
                "--api-url",
                &format!("{}/api/v1/", api.uri()),
                "projects",
                "list",
            ])
            .output()
            .unwrap(),
    );
    assert!(other.received_requests().await.unwrap().is_empty());
    assert!(accounts.received_requests().await.unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn offline_status_reads_only_the_file() {
    let home = Home::new();
    let accounts = MockServer::start().await;
    let api = MockServer::start().await;
    home.write_session(
        None,
        &session(&accounts.uri(), &api.uri(), "eyJ.old", "sar_old", 1),
    );
    let status = json_output(
        home.command()
            .args(["login", "status", "--json", "--offline"])
            .output()
            .unwrap(),
    );
    assert_eq!(status["authenticated"], true);
    assert_eq!(status["verified"], false);
    assert_eq!(status["uuid"], "zQo");
    assert_eq!(status["expires_at"], "1970-01-01T00:00:01Z");
    let text = home
        .command()
        .args(["login", "status", "--offline"])
        .output()
        .unwrap();
    assert_eq!(text.status.code(), Some(0));
    assert!(
        String::from_utf8_lossy(&text.stdout)
            .contains("Signed in to Commit as c:ada (Ada), a Carbon.")
    );
    assert!(accounts.received_requests().await.unwrap().is_empty());
    assert!(api.received_requests().await.unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_explicit_token_is_used_as_is_and_never_saved() {
    let home = Home::new();
    let api = MockServer::start().await;
    let token = jwt("zQo", "si:scout", "silicon", now() + 900);
    Mock::given(method("GET"))
        .and(path("/api/v1/todos"))
        .and(header("authorization", format!("Bearer {token}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items":[]})))
        .expect(1)
        .mount(&api)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/me"))
        .and(header("authorization", format!("Bearer {token}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"uuid":"zQo","id":"si:scout","kind":"silicon","display_name":"Scout"}),
        ))
        .expect(1)
        .mount(&api)
        .await;
    let base = |c: &mut std::process::Command| {
        c.env("COMMIT_API_URL", api.uri())
            .env("COMMIT_ACCESS_TOKEN", &token);
    };
    let mut list = home.command();
    base(&mut list);
    json_output(list.args(["todos", "list"]).output().unwrap());
    let mut status = home.command();
    base(&mut status);
    let status = json_output(status.args(["login", "status", "--json"]).output().unwrap());
    assert_eq!(status["verified"], true);
    assert_eq!(status["source"], "token");
    assert_eq!(status["display_name"], "Scout");
    let rejected = json_output(
        home.command()
            .args(["--token", "not-a-jwt", "login", "status", "--json"])
            .output()
            .unwrap(),
    );
    assert_eq!(rejected["reason"], "token_malformed");
    assert!(!home.state().exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_known_custodian_id_survives_an_answer_without_it() {
    let home = Home::new();
    let accounts = MockServer::start().await;
    let api = MockServer::start().await;
    let mut saved = session(
        &accounts.uri(),
        &api.uri(),
        "eyJ.silicon",
        "sar_silicon",
        now() + 1500,
    );
    saved["account"] = json!({"uuid":"C66","id":"si:scout","kind":"silicon","custodian":{"uuid":"0Nn","id":"c:ada"}});
    home.write_session(None, &saved);
    Mock::given(method("GET"))
        .and(path("/api/v1/me"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"uuid":"C66","id":"si:scout","kind":"silicon",
            "display_name":"Scout","custodian":{"type":"carbon","id":"","uuid":"0Nn"}})),
        )
        .expect(1)
        .mount(&api)
        .await;
    let status = json_output(
        home.command()
            .args(["login", "status", "--json"])
            .output()
            .unwrap(),
    );
    assert_eq!(status["custodian"], json!({"uuid":"0Nn","id":"c:ada"}));
    assert_eq!(
        home.read_session(None)["account"]["custodian"]["id"],
        "c:ada"
    );
}
