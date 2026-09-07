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
    #[arg(long, env = "COMMIT_API_URL", default_value = "http://127.0.0.1:8080")]
    api_url: String,
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
#[derive(Args)]
struct Login {
    slt: String,
    #[arg(long, help = "Print tokens instead of saving them locally")]
    no_save: bool,
}
#[derive(Subcommand)]
enum TodoCommand {
    List,
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
#[derive(Subcommand)]
enum ProjectCommand {
    List,
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
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".commit/session.json")
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
    let saved = load_session();
    let api = a.api_url.clone();
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
        Command::Login(x) => {
            let s = c.login_with_slt(&x.slt).await?;
            if !x.no_save {
                save_session(&Session {
                    access_token: s.access_token.clone(),
                    refresh_token: s.refresh_token.clone(),
                    api_url: api,
                    org_id: None,
                })?;
            }
            serde_json::to_value(s)?
        }
        Command::Health => c.health().await?,
        Command::Ready => c.ready().await?,
        Command::Version => c.version().await?,
        Command::Todos { command } => match command {
            TodoCommand::List => c.list_todos(&[]).await?,
            TodoCommand::Get { id } => c.get_todo(&id).await?,
            TodoCommand::Create(d) => c.create_todo(&parse_data(&d.data)?).await?,
            TodoCommand::Update { id, data } => {
                c.update_todo(&id, &parse_data(&data.data)?).await?
            }
            TodoCommand::Delete { id } => c.delete_todo(&id).await?,
            TodoCommand::Notes { id } => c.list_notes(&id, &[]).await?,
            TodoCommand::AddNote { id, data } => c.add_note(&id, &parse_data(&data.data)?).await?,
            TodoCommand::Subscription { id } => c.todo_subscription(&id).await?,
            TodoCommand::SetSubscription { id, data } => {
                c.replace_todo_subscription(&id, &parse_data(&data.data)?)
                    .await?
            }
        },
        Command::Projects { command } => match command {
            ProjectCommand::List => c.list_projects(&[]).await?,
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
            ProjectCommand::Tasks { id } => c.project_tasks(&id, &[]).await?,
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
    let Some(dir) = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .map(|p| p.join(".commit"))
    else {
        return;
    };
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
    if let Ok(release) = silicon_commit_client::latest_release().await
        && release.version != env!("CARGO_PKG_VERSION")
    {
        eprintln!(
            "A newer silicon-commit-client release is available: {}",
            release.version
        );
    }
}
