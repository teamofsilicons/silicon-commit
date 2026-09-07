use clap::{Args, Parser, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use silicon_commit_client::{Client, Mutation};
use std::{fs, path::PathBuf};

#[derive(Parser)]
#[command(
    name = "commit",
    version,
    about = "Silicon Commit work manager",
    after_help = "Set COMMIT_API_URL, COMMIT_ACCESS_TOKEN, and COMMIT_ORG_ID for non-interactive use. Writes accept --data '<json>' and support --if-match."
)]
struct Root {
    #[arg(
        long,
        env = "COMMIT_API_URL",
        help = "Commit API origin or /api/v1 URL"
    )]
    api_url: Option<String>,
    #[arg(long, env = "COMMIT_ACCESS_TOKEN")]
    token: Option<String>,
    #[arg(long, env = "COMMIT_ORG_ID")]
    org_id: Option<String>,
    #[arg(
        long,
        global = true,
        env = "COMMIT_TEST_KEY",
        help = "32-character test-environment key"
    )]
    test: Option<String>,
    #[arg(long, global = true, help = "Reuse a key when retrying the same write")]
    idempotency_key: Option<String>,
    #[arg(long, global = true, help = "Expected resource version for an update")]
    if_match: Option<i64>,
    #[arg(
        long,
        global = true,
        help = "Disable the hourly crates.io update check"
    )]
    no_update: bool,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Login(Login),
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    Health,
    Ready,
    Version,
    Todos {
        #[command(subcommand)]
        command: TodoCommand,
    },
    Projects {
        #[command(subcommand)]
        command: ProjectCommand,
    },
    Notifications {
        #[arg(long)]
        data: Option<String>,
    },
    TestEnvironments {
        #[command(subcommand)]
        command: TestCommand,
    },
}
#[derive(Subcommand)]
enum ConfigCommand {
    /// Set the home directory used for Commit's local state.
    #[command(name = "set_home_dir")]
    SetHomeDir { location: PathBuf },
}
#[derive(Args)]
struct Login {
    slt: String,
    #[arg(long, help = "Print tokens instead of saving them locally")]
    no_save: bool,
}
#[derive(Args, Default)]
struct TodoList {
    #[arg(long, help = "assigned_to_me, delegated_by_me, or all")]
    view: Option<String>,
    #[arg(long)]
    status: Option<String>,
    #[arg(long)]
    assigned_to: Option<String>,
    #[arg(long)]
    assigned_by: Option<String>,
    #[arg(long, help = "RFC 3339 lower creation bound")]
    created_from: Option<String>,
    #[arg(long, help = "RFC 3339 upper creation bound")]
    created_to: Option<String>,
    #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u16).range(1..=100))]
    limit: u16,
    #[arg(long)]
    cursor: Option<String>,
}

#[derive(Args, Clone)]
struct PageArgs {
    #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u16).range(1..=100))]
    limit: u16,
    #[arg(long)]
    cursor: Option<String>,
}

#[derive(Subcommand)]
enum TodoCommand {
    List(TodoList),
    Get {
        id: String,
    },
    Create(Data),
    Update {
        id: String,
        #[command(flatten)]
        data: Data,
    },
    Delete {
        id: String,
    },
    Notes {
        id: String,
        #[command(flatten)]
        page: PageArgs,
    },
    AddNote {
        id: String,
        #[command(flatten)]
        data: Data,
    },
    Subscription {
        id: String,
    },
    SetSubscription {
        id: String,
        #[command(flatten)]
        data: Data,
    },
}
#[derive(Args, Default)]
struct ProjectList {
    #[arg(long)]
    status: Option<String>,
    #[arg(long)]
    silicon_id: Option<String>,
    #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u16).range(1..=100))]
    limit: u16,
    #[arg(long)]
    cursor: Option<String>,
}

#[derive(Subcommand)]
enum ProjectCommand {
    List(ProjectList),
    Get {
        id: String,
    },
    Create(Data),
    Update {
        id: String,
        #[command(flatten)]
        data: Data,
    },
    Diary {
        id: String,
    },
    SetDiary {
        id: String,
        #[command(flatten)]
        data: Data,
    },
    Tasks {
        id: String,
        #[command(flatten)]
        page: PageArgs,
    },
    CreateTask {
        id: String,
        #[command(flatten)]
        data: Data,
    },
    UpdateTask {
        project: String,
        task: String,
        #[command(flatten)]
        data: Data,
    },
    Blocker {
        id: String,
        #[command(flatten)]
        data: Data,
    },
    CreateUpdate {
        id: String,
        #[command(flatten)]
        data: Data,
    },
    Complete {
        id: String,
        #[command(flatten)]
        data: Data,
    },
}
#[derive(Subcommand)]
enum TestCommand {
    List,
    Create(Data),
    Rotate { id: String },
    Key { id: String },
    Restore { id: String },
    Clean { id: String },
    Delete { id: String },
}
#[derive(Args)]
struct Data {
    #[arg(long, help = "JSON object or @path/to/file")]
    data: String,
}
#[derive(Default, Serialize, Deserialize)]
struct Session {
    access_token: String,
    refresh_token: String,
    api_url: String,
    org_id: Option<String>,
}
fn parse_data(input: &str) -> Result<Value, Box<dyn std::error::Error>> {
    let text = input
        .strip_prefix('@')
        .map(fs::read_to_string)
        .transpose()?
        .unwrap_or_else(|| input.to_owned());
    Ok(serde_json::from_str(&text)?)
}
fn session_path() -> PathBuf {
    configured_home_dir()
        .unwrap_or_else(default_home_dir)
        .join(".commit/session.json")
}
fn default_home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}
fn home_dir_config_path() -> PathBuf {
    default_home_dir().join(".commit/home_dir")
}
fn configured_home_dir() -> Option<PathBuf> {
    let path = fs::read_to_string(home_dir_config_path()).ok()?;
    let path = PathBuf::from(path.trim());
    (!path.as_os_str().is_empty()).then_some(path)
}
fn set_home_dir(location: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let location = expand_home(location);
    if !location.is_dir() {
        return Err(format!("not a directory: {}", location.display()).into());
    }
    let location = fs::canonicalize(location)?;
    let config = home_dir_config_path();
    if let Some(parent) = config.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&config, location.to_string_lossy().as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&config, fs::Permissions::from_mode(0o600))?;
    }
    println!("Commit home directory set to {}", location.display());
    Ok(())
}
fn expand_home(location: PathBuf) -> PathBuf {
    let Some(text) = location.to_str().map(str::to_owned) else {
        return location;
    };
    if text == "~" {
        return default_home_dir();
    }
    text.strip_prefix("~/")
        .map_or(location, |rest| default_home_dir().join(rest))
}
fn load_session() -> Option<Session> {
    fs::read_to_string(session_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
}
fn save_session(s: &Session) -> Result<(), Box<dyn std::error::Error>> {
    let p = session_path();
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&p, serde_json::to_vec_pretty(s)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&p, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let a = Root::parse();
    if let Command::Config {
        command: ConfigCommand::SetHomeDir { location },
    } = &a.command
    {
        return set_home_dir(location.clone());
    }
    let saved = load_session();
    let api = a
        .api_url
        .clone()
        .or_else(|| saved.as_ref().map(|s| s.api_url.clone()))
        .unwrap_or_else(|| "http://127.0.0.1:8080".to_owned());
    let token = a
        .token
        .clone()
        .or_else(|| saved.as_ref().map(|s| s.access_token.clone()));
    let org = a
        .org_id
        .clone()
        .or_else(|| saved.as_ref().and_then(|s| s.org_id.clone()));
    let mut c = Client::new(&api)?.with_mutation({
        let mut m = if let Some(k) = a.idempotency_key {
            Mutation::with_key(k)?
        } else {
            Mutation::new()
        };
        if let Some(v) = a.if_match {
            m = m.if_match(v)?;
        }
        m
    });
    if let Some(t) = token {
        c = c.with_bearer(t);
    }
    if let Some(o) = org {
        c = c.with_org_id(o);
    }
    if let Some(k) = a.test {
        c = c.with_test_key(k)?;
    }
    let output = match a.command {
        Command::Config { .. } => unreachable!("config commands return before API setup"),
        Command::Login(x) => {
            let s = c.login_with_slt(&x.slt).await?;
            if !x.no_save {
                save_session(&Session {
                    access_token: s.access_token.clone(),
                    refresh_token: s.refresh_token.clone(),
                    api_url: api,
                    org_id: s.org_id.clone(),
                })?;
            }
            serde_json::to_value(s)?
        }
        Command::Health => c.health().await?,
        Command::Ready => c.ready().await?,
        Command::Version => c.version().await?,
        Command::Todos { command } => match command {
            TodoCommand::List(q) => {
                let mut params = Vec::new();
                if let Some(v) = q.view.as_deref() {
                    params.push(("view", v));
                }
                if let Some(v) = q.status.as_deref() {
                    params.push(("status", v));
                }
                if let Some(v) = q.assigned_to.as_deref() {
                    params.push(("assigned_to", v));
                }
                if let Some(v) = q.assigned_by.as_deref() {
                    params.push(("assigned_by", v));
                }
                if let Some(v) = q.created_from.as_deref() {
                    params.push(("created_from", v));
                }
                if let Some(v) = q.created_to.as_deref() {
                    params.push(("created_to", v));
                }
                let limit = q.limit.to_string();
                params.push(("limit", &limit));
                if let Some(v) = q.cursor.as_deref() {
                    params.push(("cursor", v));
                }
                c.list_todos(&params).await?
            }
            TodoCommand::Get { id } => c.get_todo(&id).await?,
            TodoCommand::Create(d) => c.create_todo(&parse_data(&d.data)?).await?,
            TodoCommand::Update { id, data } => {
                c.update_todo(&id, &parse_data(&data.data)?).await?
            }
            TodoCommand::Delete { id } => c.delete_todo(&id).await?,
            TodoCommand::Notes { id, page } => {
                let limit = page.limit.to_string();
                let mut params = vec![("limit", limit.as_str())];
                if let Some(cursor) = page.cursor.as_deref() {
                    params.push(("cursor", cursor));
                }
                c.list_notes(&id, &params).await?
            }
            TodoCommand::AddNote { id, data } => c.add_note(&id, &parse_data(&data.data)?).await?,
            TodoCommand::Subscription { id } => c.todo_subscription(&id).await?,
            TodoCommand::SetSubscription { id, data } => {
                c.replace_todo_subscription(&id, &parse_data(&data.data)?)
                    .await?
            }
        },
        Command::Projects { command } => match command {
            ProjectCommand::List(q) => {
                let mut params = Vec::new();
                if let Some(v) = q.status.as_deref() {
                    params.push(("status", v));
                }
                if let Some(v) = q.silicon_id.as_deref() {
                    params.push(("silicon_id", v));
                }
                let limit = q.limit.to_string();
                params.push(("limit", &limit));
                if let Some(v) = q.cursor.as_deref() {
                    params.push(("cursor", v));
                }
                c.list_projects(&params).await?
            }
            ProjectCommand::Get { id } => c.get_project(&id).await?,
            ProjectCommand::Create(d) => c.create_project(&parse_data(&d.data)?).await?,
            ProjectCommand::Update { id, data } => {
                c.update_project(&id, &parse_data(&data.data)?).await?
            }
            ProjectCommand::Diary { id } => c.project_diary(&id).await?,
            ProjectCommand::SetDiary { id, data } => {
                c.replace_project_diary(&id, &parse_data(&data.data)?)
                    .await?
            }
            ProjectCommand::Tasks { id, page } => {
                let limit = page.limit.to_string();
                let mut params = vec![("limit", limit.as_str())];
                if let Some(cursor) = page.cursor.as_deref() {
                    params.push(("cursor", cursor));
                }
                c.project_tasks(&id, &params).await?
            }
            ProjectCommand::CreateTask { id, data } => {
                c.create_project_task(&id, &parse_data(&data.data)?).await?
            }
            ProjectCommand::UpdateTask {
                project,
                task,
                data,
            } => {
                c.update_project_task(&project, &task, &parse_data(&data.data)?)
                    .await?
            }
            ProjectCommand::Blocker { id, data } => {
                c.create_project_blocker(&id, &parse_data(&data.data)?)
                    .await?
            }
            ProjectCommand::CreateUpdate { id, data } => {
                c.create_project_update(&id, &parse_data(&data.data)?)
                    .await?
            }
            ProjectCommand::Complete { id, data } => {
                c.complete_project(&id, &parse_data(&data.data)?).await?
            }
        },
        Command::Notifications { data: d } => {
            if let Some(d) = d {
                c.update_notification_settings(&parse_data(&d)?).await?
            } else {
                c.notification_settings().await?
            }
        }
        Command::TestEnvironments { command } => match command {
            TestCommand::List => c.list_test_environments().await?,
            TestCommand::Create(d) => c.create_test_environment(&parse_data(&d.data)?).await?,
            TestCommand::Rotate { id } => c.rotate_test_environment(&id).await?,
            TestCommand::Key { id } => c.retrieve_test_environment_key(&id).await?,
            TestCommand::Restore { id } => c.restore_test_environment(&id).await?,
            TestCommand::Clean { id } => c.clean_test_environment(&id).await?,
            TestCommand::Delete { id } => c.delete_test_environment(&id).await?,
        },
    };
    println!("{}", serde_json::to_string_pretty(&output)?);
    maybe_check_update(a.no_update).await;
    Ok(())
}

async fn maybe_check_update(disabled: bool) {
    if disabled {
        return;
    }
    let dir = configured_home_dir()
        .unwrap_or_else(default_home_dir)
        .join(".commit");
    let marker = dir.join("last-update-check");
    let now = std::time::SystemTime::now();
    if let Ok(meta) = fs::metadata(&marker)
        && let Ok(modified) = meta.modified()
        && now.duration_since(modified).unwrap_or_default() < std::time::Duration::from_secs(3600)
    {
        return;
    }
    let _ = fs::create_dir_all(&dir);
    let _ = fs::write(&marker, b"checked");
    if let Ok(release) = silicon_commit_client::latest_cli_release().await
        && release.version != env!("CARGO_PKG_VERSION")
    {
        eprintln!(
            "A newer silicon-commit release is available: {}; updating the CLI",
            release.version
        );
        // Run after the command has completed so an update cannot interrupt
        // the user's requested operation. A failed install is non-fatal: the
        // current binary remains usable and the next hourly check retries it.
        match std::process::Command::new("cargo")
            .args(["install", "--locked", "--force", "commit"])
            .status()
        {
            Ok(status) if status.success() => eprintln!("Silicon Commit CLI updated"),
            Ok(_) => eprintln!("CLI update failed; continuing with the current version"),
            Err(_) => eprintln!("cargo was unavailable; continuing with the current version"),
        }
    }
}
