//! Local session selection and self-contained CLI documentation.
use super::*;
use sha2::{Digest as _, Sha256};
use std::sync::{OnceLock, RwLock};
static SELECTOR: OnceLock<RwLock<Option<String>>> = OnceLock::new();
#[derive(Subcommand)]
pub enum TestingCommand {
    /// Validate and select an imported IAM application's test secret.
    Use { app_secret: String },
    /// Resume production; retains sandbox and production sessions separately.
    Exit,
    /// Identify the selected sandbox without printing its secret.
    Status,
}
pub fn telemetry_enabled() -> bool {
    std::env::var("COMMIT_TELEMETRY")
        .ok()
        .or_else(|| fs::read_to_string(directory().join("telemetry")).ok())
        .is_none_or(|v| !matches!(v.trim(), "off" | "false" | "0"))
}
pub fn directory() -> PathBuf {
    configured_home_dir()
        .unwrap_or_else(default_home_dir)
        .join(".commit")
}
fn selection_path() -> PathBuf {
    directory().join("testing-selection")
}
pub fn initialize() {
    let args = std::env::args().collect::<Vec<_>>();
    let explicit = args
        .windows(2)
        .find(|w| w[0] == "--test")
        .map(|w| w[1].clone())
        .or_else(|| {
            args.iter()
                .find_map(|a| a.strip_prefix("--test=").map(str::to_owned))
        });
    let selected = explicit
        .or_else(|| std::env::var("COMMIT_TEST_KEY").ok())
        .or_else(|| fs::read_to_string(selection_path()).ok());
    let _ = SELECTOR.set(RwLock::new(selected));
}
pub fn selected_key() -> Option<String> {
    SELECTOR
        .get()
        .and_then(|s| s.read().ok().and_then(|v| v.clone()))
}
pub fn session_file() -> String {
    selected_key().map_or_else(
        || "session.json".to_owned(),
        |key| format!("test-{:x}.json", Sha256::digest(key.as_bytes())),
    )
}
fn metadata_path() -> PathBuf {
    directory().join(format!("{}.metadata", session_file()))
}
pub fn private_write(
    path: &std::path::Path,
    bytes: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write as _;
    let parent = path.parent().ok_or("state path has no parent")?;
    fs::create_dir_all(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    }
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(temporary, path)?;
    Ok(())
}
pub fn footer() {
    if selected_key().is_none() {
        return;
    }
    let metadata = fs::read(metadata_path())
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    let name = metadata
        .as_ref()
        .and_then(|m| m.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("selected sandbox (name not yet verified)");
    eprintln!("Testing environment: {name}. Use commit testing exit to return to production.");
}
pub async fn remember_context(client: &Client) {
    if let Ok(meta) = client.testing_context().await {
        let _ = private_write(
            &metadata_path(),
            &serde_json::to_vec(&meta).unwrap_or_default(),
        );
    }
}
pub async fn testing(
    command: &TestingCommand,
    api: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    match command {
        TestingCommand::Exit => {
            match fs::remove_file(selection_path()) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            };
            if let Some(s) = SELECTOR.get()
                && let Ok(mut s) = s.write()
            {
                *s = None;
            }
            println!(
                "Production selected. Its saved session is available; unset COMMIT_TEST_KEY or remove --test if set."
            );
        }
        TestingCommand::Use { app_secret } => {
            let client = Client::new(api.unwrap_or("https://backend.commit.teamofsilicons.com"))?
                .with_test_app_secret(app_secret)?;
            let meta = client.testing_context().await?;
            private_write(&selection_path(), app_secret.as_bytes())?;
            if let Some(s) = SELECTOR.get()
                && let Ok(mut s) = s.write()
            {
                *s = Some(app_secret.clone());
            }
            let path = directory().join(format!(
                "test-{:x}.json.metadata",
                Sha256::digest(app_secret.as_bytes())
            ));
            private_write(&path, &serde_json::to_vec(&meta)?)?;
            println!("{}", serde_json::to_string_pretty(&meta)?);
            eprintln!(
                "Selected test environment {}. Sign in with commit login <test-SLT-or-public-ID>.",
                meta["name"].as_str().unwrap_or("sandbox")
            );
        }
        TestingCommand::Status => {
            let Some(key) = selected_key() else {
                println!("{{\"testing\":false}}");
                return Ok(());
            };
            let c = Client::new(api.unwrap_or("https://backend.commit.teamofsilicons.com"))?
                .with_test_key(key)?;
            let meta = c.testing_context().await?;
            private_write(&metadata_path(), &serde_json::to_vec(&meta)?)?;
            println!("{}", serde_json::to_string_pretty(&meta)?);
        }
    }
    Ok(())
}
pub fn docs(topic: &str) -> Result<(), Box<dyn std::error::Error>> {
    let content =
        match topic {
            "start" | "usage" => include_str!("../docs/START.md"),
            "projects" => include_str!("../docs/PROJECTS.md"),
            "api" => include_str!("../docs/API.md"),
            "client" => include_str!("../docs/CLIENT.md"),
            "cli" => include_str!("../docs/CLI.md"),
            "testing" => include_str!("../docs/TEST_ENVIRONMENTS.md"),
            "development" => include_str!("../docs/DEVELOPMENT.md"),
            _ => return Err(
                "unknown guide; choose start, projects, api, client, cli, testing, or development"
                    .into(),
            ),
        };
    println!(
        "{content}\n\nDocs: https://docs.commit.teamofsilicons.com\nSource: https://github.com/teamofsilicons/silicon-commit\nRust client: https://crates.io/crates/silicon-commit-client\nExplore: commit docs <topic>; commit <command> --help"
    );
    Ok(())
}
pub fn report(
    message: &str,
    pr: Option<&str>,
    save_only: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if message.trim().is_empty() {
        return Err(
            "describe the bug, reproduction steps, expected result, and actual result".into(),
        );
    }
    if pr.is_some_and(|v| !v.starts_with("https://github.com/teamofsilicons/silicon-commit/pull/"))
    {
        return Err("--pr must link to a Silicon Commit GitHub pull request".into());
    }
    let body = format!(
        "{message}\n\nCLI version: {}\nPull request: {}\n",
        env!("CARGO_PKG_VERSION"),
        pr.unwrap_or("not supplied")
    );
    let path = directory().join(format!("report-{}.md", Client::new_idempotency_key()));
    private_write(&path, body.as_bytes())?;
    let _ = save_only;
    println!("Saved bug report: {}", path.display());
    if pr.is_none() {
        eprintln!(
            "You can also reproduce, patch, and open a PR at https://github.com/teamofsilicons/silicon-commit, then attach it with --pr."
        );
    }
    Ok(())
}
