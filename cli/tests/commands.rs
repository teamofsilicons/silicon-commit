use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_json, header, method, path},
};

struct Home(PathBuf);
impl Home {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "commit-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_commit"));
        for name in [
            "COMMIT_API_URL",
            "COMMIT_ACCESS_TOKEN",
            "COMMIT_ORG_ID",
            "COMMIT_TEST_KEY",
            "SILICON_HOME",
        ] {
            command.env_remove(name);
        }
        command.env("HOME", &self.0).arg("--no-update");
        command
    }
}
impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn json_output(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn help_and_login_grammar_work_without_network_or_state() {
    let home = Home::new();
    for args in [
        vec!["--help"],
        vec!["-h"],
        vec!["login", "--help"],
        vec!["login", "status", "--help"],
        vec!["logout", "--help"],
        vec!["iam", "--help"],
    ] {
        let output = home.command().args(args).output().unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("Usage:"));
    }
    assert!(
        !home
            .command()
            .arg("login")
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(
        !home
            .command()
            .args(["login", "status", "--no-save"])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(
        json_output(
            home.command()
                .args(["login", "status", "--json"])
                .output()
                .unwrap()
        )["authenticated"],
        false
    );
    assert!(!home.0.join(".commit").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn logout_revokes_the_saved_session_before_removing_only_its_credentials() {
    for (status, test_key) in [
        (204, None),
        (503, None),
        (204, Some("abcdefghijklmnopqrstuvwxyz123456")),
    ] {
        let home = Home::new();
        let silicon = Home::new();
        let configured = Home::new();
        let server = MockServer::start().await;
        let other_server = MockServer::start().await;
        let mut command = home.command();
        command
            .env("SILICON_HOME", &silicon.0)
            .args(["config", "home"])
            .arg(&configured.0);
        assert!(command.output().unwrap().status.success());
        let pointer = silicon.0.join(".commit/home_dir");
        let original_pointer = fs::read(&pointer).unwrap();
        let state = configured.0.join(".commit");
        fs::create_dir_all(&state).unwrap();
        fs::write(state.join("test-key"), b"unrelated configuration").unwrap();
        let session = state.join("session.json");
        let saved = serde_json::to_vec(&json!({
            "access_token": "oat_saved", "refresh_token": "ort_saved",
            "api_url": server.uri(), "org_id": "tos"
        }))
        .unwrap();
        fs::write(&session, &saved).unwrap();
        let run = |api: &str| {
            let mut command = home.command();
            command
                .env("SILICON_HOME", &silicon.0)
                .env("COMMIT_API_URL", api)
                .env("COMMIT_ACCESS_TOKEN", "oat_unrelated")
                .env("COMMIT_ORG_ID", "unrelated")
                .args(["logout", "--json"]);
            if let Some(key) = test_key {
                command.env("COMMIT_TEST_KEY", key);
            }
            command.output().unwrap()
        };
        assert!(!run(&other_server.uri()).status.success());
        assert_eq!(fs::read(&session).unwrap(), saved);
        assert!(other_server.received_requests().await.unwrap().is_empty());
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/logout"))
            .and(body_json(json!({"token":"ort_saved"})))
            .and(move |r: &wiremock::Request| {
                !r.headers.contains_key("authorization")
                    && !r.headers.contains_key("x-org-id")
                    && r.headers.contains_key("idempotency-key")
                    && r.headers
                        .get("x-testing-environment-key")
                        .and_then(|v| v.to_str().ok())
                        == test_key
            })
            .respond_with(ResponseTemplate::new(status))
            .expect(1)
            .mount(&server)
            .await;
        let output = run(&format!("{}/api/v1/", server.uri()));
        if status == 204 {
            assert_eq!(json_output(output)["removed"], true);
            assert!(!session.exists());
            // An absent session succeeds without validating or contacting any API.
            assert_eq!(json_output(run("invalid URL"))["removed"], true);
        } else {
            assert!(!output.status.success());
            assert_eq!(fs::read(&session).unwrap(), saved);
        }
        assert_eq!(fs::read(&pointer).unwrap(), original_pointer);
        assert_eq!(
            fs::read(state.join("test-key")).unwrap(),
            b"unrelated configuration"
        );
        assert!(!home.0.join(".commit").exists());
        assert!(!silicon.0.join(".commit/session.json").exists());
        fs::write(&session, b"invalid session").unwrap();
        assert!(!run(&server.uri()).status.success());
        assert_eq!(fs::read(&session).unwrap(), b"invalid session");
        fs::write(silicon.0.join(".commit/session.json"), &saved).unwrap();
        fs::write(&pointer, b"").unwrap();
        assert!(!run(&server.uri()).status.success());
        assert_eq!(
            fs::read(silicon.0.join(".commit/session.json")).unwrap(),
            saved
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn silicon_home_persists_login_and_explicit_configuration_overrides_it() {
    let home = Home::new();
    let silicon = Home::new();
    let configured = Home::new();
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/api/v1/auth/login"))
        .and(body_json(json!({"slt":"slt_once"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token":"oat_secret", "refresh_token":"ort_secret", "token_type":"Bearer",
            "expires_in":3600, "scope":"profile.read", "actor":{"type":"carbon","id":"person"}, "org_id":"tos"
        }))).expect(3).mount(&server).await;
    let login = || {
        let mut command = home.command();
        command.env("SILICON_HOME", &silicon.0).args([
            "--api-url",
            &server.uri(),
            "login",
            "slt_once",
        ]);
        command
    };
    json_output(login().output().unwrap());
    assert!(silicon.0.join(".commit/session.json").exists());
    assert!(!home.0.join(".commit").exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(silicon.0.join(".commit/session.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    let output = home
        .command()
        .env("SILICON_HOME", &silicon.0)
        .args(["config", "home"])
        .arg(&configured.0)
        .output()
        .unwrap();
    assert!(output.status.success());
    json_output(login().output().unwrap());
    assert!(configured.0.join(".commit/session.json").exists());
    assert!(silicon.0.join(".commit/home_dir").exists());
    json_output(
        home.command()
            .args(["--api-url", &server.uri(), "login", "slt_once"])
            .output()
            .unwrap(),
    );
    assert!(home.0.join(".commit/session.json").exists());
    assert!(
        !home
            .command()
            .args(["config", "home"])
            .arg(home.0.join("missing"))
            .output()
            .unwrap()
            .status
            .success()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn discovery_and_status_are_clean_json_and_use_current_credentials() {
    let home = Home::new();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/iam"))
        .and(|r: &wiremock::Request| {
            !r.headers.contains_key("authorization")
                && !r.headers.contains_key("x-testing-environment-key")
                && !r.headers.contains_key("x-org-id")
        })
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                json!({"app_id":"tos>commit","iam_url":"https://iam.example/api/v1/"}),
            ),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/auth/status"))
        .and(header("authorization", "Bearer oat_current"))
        .and(header("x-org-id", "tos"))
        .and(header(
            "x-testing-environment-key",
            "abcdefghijklmnopqrstuvwxyz123456",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"authenticated":true,"actor":{"type":"silicon","id":"agent"},"org_id":"tos"}),
        ))
        .expect(1)
        .mount(&server)
        .await;
    let run = |args: &[&str]| {
        json_output(
            home.command()
                .env("COMMIT_API_URL", server.uri())
                .env("COMMIT_ACCESS_TOKEN", "oat_current")
                .env("COMMIT_ORG_ID", "tos")
                .env("COMMIT_TEST_KEY", "abcdefghijklmnopqrstuvwxyz123456")
                .args(args)
                .output()
                .unwrap(),
        )
    };
    assert_eq!(run(&["iam", "--json"])["app_id"], "tos>commit");
    let status = run(&["login", "status", "--json"]);
    assert_eq!(status["authenticated"], true);
    assert_eq!(status["actor"]["type"], "silicon");
    assert!(!status.to_string().contains("oat_current"));
}

#[test]
fn todo_help_explains_assignment_without_exposing_environment_credentials() {
    let home = Home::new();
    for args in [vec!["--help"], vec!["todos", "create", "--help"]] {
        let output = home
            .command()
            .env("COMMIT_ACCESS_TOKEN", "oat_private_help_token")
            .env("COMMIT_TEST_KEY", "abcdefghijklmnopqrstuvwxyz123456")
            .args(&args)
            .output()
            .unwrap();
        assert!(output.status.success());
        let help = String::from_utf8(output.stdout).unwrap();
        assert!(!help.contains("oat_private_help_token"));
        assert!(!help.contains("abcdefghijklmnopqrstuvwxyz123456"));
        if args.len() > 1 {
            assert!(help.contains("assigned_to"));
            assert!(help.contains("assistant:example-org"));
            assert!(help.contains("alex"));
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_todo_assignments_explain_the_fix_without_sending_a_request() {
    let home = Home::new();
    let server = MockServer::start().await;
    for (payload, expected) in [
        (
            json!({"title":"Eat","assignee_id":"head-of-sales:final-test"}),
            "use assigned_to",
        ),
        (
            json!({"title":"Eat","assignee":{"id":"head-of-sales:final-test","type":"silicon"}}),
            "use assigned_to",
        ),
        (
            json!({"title":"Eat","assigned_to":"saket","assignee_id":"saket"}),
            "use assigned_to",
        ),
        (
            json!({"title":"Eat"}),
            "assigned_to is required and must be a string",
        ),
        (
            json!({"title":"Eat","assigned_to":{"id":"saket"}}),
            "assigned_to is required and must be a string",
        ),
        (
            json!({"assigned_to":"saket"}),
            "title is required and must be a string",
        ),
        (json!([]), "requires a JSON object"),
    ] {
        // Exercise both inline JSON and @FILE, the two supported input paths.
        let file = home.0.join("todo.json");
        fs::write(&file, payload.to_string()).unwrap();
        for data in [payload.to_string(), format!("@{}", file.display())] {
            let output = home
                .command()
                .args([
                    "--api-url",
                    &server.uri(),
                    "todos",
                    "create",
                    "--data",
                    &data,
                ])
                .output()
                .unwrap();
            assert!(!output.status.success());
            assert!(output.stdout.is_empty());
            let error = String::from_utf8(output.stderr).unwrap();
            assert!(error.contains(expected), "{error}");
        }
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn todo_creation_preserves_payload_and_server_validation_correlation() {
    let home = Home::new();
    let server = MockServer::start().await;
    for (actor, status) in [("saket", 201), ("head-of-sales:final-test", 422)] {
        let payload = json!({"title":"Eat","assigned_to":actor,
            "description":null,"status":"yet_to_do","attachments":[]});
        let mut response = ResponseTemplate::new(status);
        if status == 201 {
            response = response.set_body_json(json!({"id":"created-todo","assigned_to":actor}));
        } else {
            response = response
                .insert_header("x-request-id", "todo-validation-request")
                .set_body_json(
                    json!({"error":{"code":"validation_failed","details":"private-body-value"}}),
                );
        }
        Mock::given(method("POST"))
            .and(path("/api/v1/todos"))
            .and(body_json(&payload))
            .and(header("authorization", "Bearer oat_test"))
            .respond_with(response)
            .expect(1)
            .mount(&server)
            .await;
        let output = home
            .command()
            .args([
                "--api-url",
                &server.uri(),
                "--token",
                "oat_test",
                "--org-id",
                "final-test",
                "todos",
                "create",
                "--data",
                &payload.to_string(),
            ])
            .output()
            .unwrap();
        if status == 201 {
            assert_eq!(json_output(output)["id"], "created-todo");
        } else {
            assert!(!output.status.success());
            assert!(output.stdout.is_empty());
            let error = String::from_utf8(output.stderr).unwrap();
            for part in [
                "422",
                "validation_failed",
                "todo-validation-request",
                "commit todos create --help",
            ] {
                assert!(error.contains(part), "{error}");
            }
            assert!(!error.contains("private-body-value"));
            assert!(!error.contains("oat_test"));
        }
    }
}
