//! Helpers shared by the CLI integration tests: a throwaway home, stub token responses and
//! session files. Every test runs the real `commit` binary with a cleared environment.
#![allow(dead_code)]

use serde_json::{Value, json};
use std::{
    fs,
    io::Write as _,
    path::PathBuf,
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};
use wiremock::ResponseTemplate;

pub struct Home(pub PathBuf);

impl Home {
    pub fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "commit-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    /// The binary with an empty environment except HOME and PATH.
    pub fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_commit"));
        command
            .env_clear()
            .env("HOME", &self.0)
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .stdin(Stdio::null());
        command
    }

    pub fn state(&self) -> PathBuf {
        self.0.join(".commit")
    }

    pub fn session_path(&self, profile: Option<&str>) -> PathBuf {
        match profile {
            None => self.state().join("session.json"),
            Some(name) => self
                .state()
                .join("profiles")
                .join(name)
                .join("session.json"),
        }
    }

    pub fn write_session(&self, profile: Option<&str>, value: &Value) -> PathBuf {
        let path = self.session_path(profile);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
        path
    }

    pub fn read_session(&self, profile: Option<&str>) -> Value {
        serde_json::from_slice(&fs::read(self.session_path(profile)).unwrap()).unwrap()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// A well-formed (unsigned) access token; the stubs never verify signatures.
pub fn jwt(sub: &str, id: &str, kind: &str, exp: i64) -> String {
    use base64::Engine as _;
    let encode = |v: Value| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v.to_string());
    format!(
        "{}.{}.c2lnbmF0dXJl",
        encode(json!({"alg":"EdDSA","typ":"JWT","kid":"test"})),
        encode(
            json!({"sub":sub,"aud":"commit","exp":exp,"iat":exp - 1800,"kind":kind,"id":id,"iss":"http://accounts.test"})
        )
    )
}

/// A Silicon Accounts token response for Commit.
pub fn tokens(access: &str, refresh: &str, kind: &str, id: &str) -> Value {
    let mut account = json!({"uuid":"zQo","membership_id":"commit:zQo","kind":kind,"id":id,
        "display_name":"Ada","pfp_url":"","version":2});
    if kind == "silicon" {
        account["custodian"] = json!({"uuid":"cUs","id":"c:custodian"});
    }
    json!({"access_token":access,"token_type":"Bearer","expires_in":1800,"refresh_token":refresh,
        "refresh_token_expires_at":"2029-03-25T02:33:57.696Z","scope":"profile",
        "membership_id":"commit:zQo","account":account})
}

pub fn oauth_error(code: &str, description: &str) -> ResponseTemplate {
    ResponseTemplate::new(400).set_body_json(json!({"error":code,"error_description":description}))
}

/// A saved session as the CLI writes it.
pub fn session(accounts: &str, api: &str, access: &str, refresh: &str, expires_at: i64) -> Value {
    json!({
        "version": 2,
        "app_id": "commit",
        "accounts_url": accounts,
        "api_url": api,
        "access_token": access,
        "refresh_token": refresh,
        "expires_at": expires_at,
        "refresh_expires_at": 1_869_100_437,
        "scope": "profile",
        "account": {"uuid":"zQo","id":"c:ada","kind":"carbon","display_name":"Ada"},
        "method": "device",
        "signed_in_at": 1_760_000_000
    })
}

/// Asserts success and parses stdout.
pub fn json_output(output: Output) -> Value {
    assert!(
        output.status.success(),
        "exit {:?}\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout is not JSON ({e}): {}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

pub fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Runs a command with `input` on stdin.
pub fn with_stdin(mut command: Command, input: &str) -> Output {
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

/// Runs a blocking command off the async runtime.
pub async fn run(mut command: Command) -> Output {
    tokio::task::spawn_blocking(move || command.output().unwrap())
        .await
        .unwrap()
}
