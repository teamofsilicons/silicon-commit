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
