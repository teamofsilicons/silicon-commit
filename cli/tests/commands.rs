//! API commands: request shapes, local validation, errors, reports, configuration.

mod support;

use serde_json::json;
use std::fs;
use support::{Home, json_output, now, session, stderr};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_json, header, method, path, query_param},
};

fn signed_in(home: &Home, api: &MockServer) {
    home.write_session(
        None,
        &session(
            "http://127.0.0.1:9",
            &api.uri(),
            "eyJ.saved",
            "sar_saved",
            now() + 1500,
        ),
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn commands_need_a_sign_in_but_health_checks_do_not() {
    let home = Home::new();
    let api = MockServer::start().await;
    for route in ["/healthz", "/readyz", "/api/v1/version"] {
        Mock::given(method("GET"))
            .and(path(route))
            .and(|r: &wiremock::Request| !r.headers.contains_key("authorization"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status":"ok"})))
            .expect(1)
            .mount(&api)
            .await;
    }
    for command in ["health", "ready", "version"] {
        json_output(
            home.command()
                .env("COMMIT_API_URL", api.uri())
                .arg(command)
                .output()
                .unwrap(),
        );
    }
    let output = home
        .command()
        .env("COMMIT_API_URL", api.uri())
        .args(["todos", "list"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let text = stderr(&output);
    for part in [
        "Not signed in",
        "not_signed_in",
        "commit login",
        "silicon-accounts login --app commit -q | commit login --slt-stdin",
    ] {
        assert!(text.contains(part), "{text}");
    }
    assert!(!home.state().exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_todos_are_explained_without_a_request() {
    let home = Home::new();
    let api = MockServer::start().await;
    signed_in(&home, &api);
    for (payload, expected) in [
        (
            json!({"title":"Eat","assignee_id":"si:builder"}),
            "use assigned_to",
        ),
        (
            json!({"title":"Eat","assignee":{"id":"si:builder"}}),
            "use assigned_to",
        ),
        (
            json!({"title":"Eat"}),
            "assigned_to is required and must be a string",
        ),
        (
            json!({"title":"Eat","assigned_to":{"id":"c:alice"}}),
            "assigned_to is required and must be a string",
        ),
        (
            json!({"assigned_to":"c:alice"}),
            "title is required and must be a string",
        ),
        (json!([]), "requires a JSON object"),
    ] {
        let file = home.0.join("todo.json");
        fs::write(&file, payload.to_string()).unwrap();
        for data in [payload.to_string(), format!("@{}", file.display())] {
            let output = home
                .command()
                .args(["todos", "create", "--data", &data])
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(1));
            assert!(output.stdout.is_empty());
            assert!(stderr(&output).contains(expected), "{}", stderr(&output));
        }
    }
    let broken = home
        .command()
        .args(["todos", "create", "--data", "{not json"])
        .output()
        .unwrap();
    assert!(stderr(&broken).contains("--data is not valid JSON"));
    assert!(api.received_requests().await.unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn server_validation_keeps_the_code_request_id_details_and_hint() {
    let home = Home::new();
    let api = MockServer::start().await;
    signed_in(&home, &api);
    let payload = json!({"title":"Eat","assigned_to":"si:elsewhere","description":null,"status":"yet_to_do","attachments":[]});
    Mock::given(method("POST"))
        .and(path("/api/v1/todos"))
        .and(body_json(&payload))
        .and(header("authorization", "Bearer eyJ.saved"))
        .respond_with(
            ResponseTemplate::new(422)
                .insert_header("x-request-id", "todo-validation-request")
                .set_body_json(json!({"error":{"code":"validation_failed","message":"The request contains invalid data.","details":{"assigned_to":"unknown account"}}})),
        )
        .expect(1)
        .mount(&api)
        .await;
    let output = home
        .command()
        .args(["todos", "create", "--data", &payload.to_string()])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let text = stderr(&output);
    for part in [
        "422",
        "validation_failed",
        "todo-validation-request",
        "unknown account",
        "commit todos create --help",
        "not created",
    ] {
        assert!(text.contains(part), "{text}");
    }
    assert!(!text.contains("eyJ.saved"));
    Mock::given(method("POST"))
        .and(path("/api/v1/todos"))
        .and(body_json(json!({"title":"Help","assigned_to":"si:outsider"})))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({"error":{"code":"silicon_not_reachable","message":"si:outsider does not take work from you."}})))
        .mount(&api)
        .await;
    let refused = home
        .command()
        .args([
            "todos",
            "create",
            "--data",
            "{\"title\":\"Help\",\"assigned_to\":\"si:outsider\"}",
        ])
        .output()
        .unwrap();
    let text = stderr(&refused);
    assert!(
        text.contains("silicon_not_reachable") && text.contains("commit silicons allow"),
        "{text}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn list_filters_custodian_selectors_and_the_allow_list_reach_the_api() {
    let home = Home::new();
    let api = MockServer::start().await;
    signed_in(&home, &api);
    Mock::given(method("GET"))
        .and(path("/api/v1/todos"))
        .and(query_param("view", "delegated_by_me"))
        .and(query_param("assigned_to", "si:builder"))
        .and(query_param("limit", "10"))
        .and(query_param("cursor", "next"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"items":[],"next_cursor":null})),
        )
        .expect(1)
        .mount(&api)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/notification-settings"))
        .and(query_param("silicon", "si:scout"))
        .and(header("if-match", "\"2\""))
        .and(body_json(json!({"webhook_url":"https://hooks.example/commit","todo_list_subscription":{"scope":"status_updates"}})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"version":3})))
        .expect(1)
        .mount(&api)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/silicons/si:scout/allowed-accounts/c:alice"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"silicon":{"id":"si:scout"},"allowed":[{"id":"c:alice"}]})),
        )
        .expect(1)
        .mount(&api)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/me"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"uuid":"zQo","id":"c:ada","silicons":[{"id":"si:scout"}]})),
        )
        .expect(1)
        .mount(&api)
        .await;
    json_output(
        home.command()
            .args([
                "todos",
                "list",
                "--view",
                "delegated_by_me",
                "--assigned-to",
                "si:builder",
                "--limit",
                "10",
                "--cursor",
                "next",
            ])
            .output()
            .unwrap(),
    );
    let settings = json_output(
        home.command()
            .args(["--if-match", "2", "notifications", "--silicon", "si:scout", "--data",
                   "{\"webhook_url\":\"https://hooks.example/commit\",\"todo_list_subscription\":{\"scope\":\"status_updates\"}}"])
            .output()
            .unwrap(),
    );
    assert_eq!(settings["version"], 3);
    assert_eq!(
        json_output(
            home.command()
                .args(["silicons", "allow", "si:scout", "c:alice"])
                .output()
                .unwrap()
        )["allowed"][0]["id"],
        "c:alice"
    );
    assert_eq!(
        json_output(home.command().arg("me").output().unwrap())["silicons"][0]["id"],
        "si:scout"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn carbons_without_a_shared_email_are_told_how_to_share_one() {
    let home = Home::new();
    let api = MockServer::start().await;
    signed_in(&home, &api);
    Mock::given(method("GET"))
        .and(path("/api/v1/email-settings"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                json!({"email":"","enabled":true,"saved":false,"shared_email":null}),
            ),
        )
        .mount(&api)
        .await;
    let output = home.command().arg("email").output().unwrap();
    assert!(
        stderr(&output).contains("commit login --scope email"),
        "{}",
        stderr(&output)
    );
    assert_eq!(json_output(output)["saved"], false);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_report_is_saved_locally_and_the_failure_is_kept() {
    let home = Home::new();
    let api = MockServer::start().await;
    signed_in(&home, &api);
    Mock::given(method("POST"))
        .and(path("/api/v1/reports"))
        .and(body_json(
            json!({"message":"Todos cannot be read.","pr":null}),
        ))
        .respond_with(ResponseTemplate::new(502).set_body_json(
            json!({"error":{"code":"invalid_provider_response","message":"Postmark refused."}}),
        ))
        .expect(1)
        .mount(&api)
        .await;
    let output = home
        .command()
        .args(["report", "Todos cannot be read."])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let text = stderr(&output);
    assert!(
        text.contains("Saved locally:") && text.contains("invalid_provider_response"),
        "{text}"
    );
    let reports: Vec<_> = fs::read_dir(home.state())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "md"))
        .collect();
    assert_eq!(reports.len(), 1);
    assert!(
        fs::read_to_string(&reports[0])
            .unwrap()
            .contains("Todos cannot be read.")
    );
}

#[test]
fn a_report_draft_needs_no_api_or_session() {
    let home = Home::new();
    fs::create_dir_all(home.state()).unwrap();
    fs::write(home.session_path(None), "invalid session").unwrap();
    let output = home
        .command()
        .env("COMMIT_API_URL", "invalid-url")
        .args(["report", "Offline report recovery", "--save-only"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(String::from_utf8_lossy(&output.stdout).contains("Saved bug report"));
    assert_eq!(
        fs::read_to_string(home.session_path(None)).unwrap(),
        "invalid session"
    );
    let bad_link = home
        .command()
        .args([
            "report",
            "x",
            "--pr",
            "https://example.com/pr",
            "--save-only",
        ])
        .output()
        .unwrap();
    assert!(stderr(&bad_link).contains("teamofsilicons/silicon-commit"));
}

#[test]
fn silicon_home_and_config_home_choose_where_state_lives() {
    let home = Home::new();
    let silicon = home.0.join("silicon-home");
    let chosen = home.0.join("chosen");
    fs::create_dir_all(&silicon).unwrap();
    fs::create_dir_all(&chosen).unwrap();
    let with_silicon = |args: &[&str]| {
        home.command()
            .env("SILICON_HOME", &silicon)
            .args(args)
            .output()
            .unwrap()
    };
    assert!(
        with_silicon(&["config", "telemetry", "off"])
            .status
            .success()
    );
    assert_eq!(
        fs::read_to_string(silicon.join(".commit/telemetry")).unwrap(),
        "off"
    );
    assert!(!home.state().exists(), "SILICON_HOME wins over HOME");
    let output = with_silicon(&["config", "home", chosen.to_str().unwrap()]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(silicon.join(".commit/home_dir").exists());
    let shown = json_output(with_silicon(&["config", "show"]));
    assert_eq!(
        shown["state_dir"],
        json!(fs::canonicalize(&chosen).unwrap().join(".commit"))
    );
    assert_eq!(
        shown["telemetry"], "on",
        "telemetry lives in the chosen home now"
    );
    assert_eq!(shown["signed_in"], false);
    let missing = with_silicon(&["config", "home", home.0.join("missing").to_str().unwrap()]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(stderr(&missing).contains("not a directory"));
}
