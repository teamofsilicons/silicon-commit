use clap::{Args, Parser, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use silicon_commit_client::{Client, Mutation};
use std::{fs, path::PathBuf};

#[derive(Parser)]
#[command(
    name = "commit",
    bin_name = "commit",
    version,
    about = "Silicon Commit work manager",
    after_help = "Quick start:\n  commit iam --json\n  commit login <slt>\n  commit login status --json\n  commit todos list\n\nSet COMMIT_API_URL, COMMIT_ACCESS_TOKEN, and COMMIT_ORG_ID for non-interactive use.\nState defaults to $SILICON_HOME/.commit or $HOME/.commit; override with commit config home LOCATION.\nWrites accept --data '<json>' or --data @FILE and support --if-match.\nUse --test KEY for a sandbox and --no-update to skip automatic updates.\nRun commit <command> --help for arguments and subcommands."
)]
struct Root {
    #[arg(
        long,
        env = "COMMIT_API_URL",
        help = "Commit API origin or /api/v1 URL"
    )]
    api_url: Option<String>,
    #[arg(long, env = "COMMIT_ACCESS_TOKEN", hide_env_values = true)]
    token: Option<String>,
    #[arg(long, env = "COMMIT_ORG_ID")]
    org_id: Option<String>,
    #[arg(
        long,
        global = true,
        env = "COMMIT_TEST_KEY",
        hide_env_values = true,
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
    /// Exchange an IAM short-lived token, or check the current login.
    Login(Login),
    /// Revoke the saved session and remove its local credentials.
    Logout(JsonOutput),
    /// Show public IAM application details, including the app_id for login.
    Iam(JsonOutput),
    /// Configure local session storage.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Check whether the API process is alive.
    Health,
    /// Check whether the API is ready to serve requests.
    Ready,
    /// Show backend build metadata.
    Version,
    /// Manage todos, notes, and notification subscriptions.
    Todos {
        #[command(subcommand)]
        command: TodoCommand,
    },
    /// Manage projects, diaries, tasks, and milestone entries.
    Projects {
        #[command(subcommand)]
        command: ProjectCommand,
    },
    /// Read notification settings, or replace them with --data JSON.
    Notifications {
        #[arg(long)]
        data: Option<String>,
    },
    /// Create and manage isolated test environments.
    TestEnvironments {
        #[command(subcommand)]
        command: TestCommand,
    },
}
#[derive(Subcommand)]
enum ConfigCommand {
    /// Set the home directory used for Commit's local state.
    #[command(name = "home", visible_alias = "set_home_dir")]
    Home { location: PathBuf },
}
#[derive(Args)]
#[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
struct Login {
    #[arg(
        required = true,
        help = "Single-use short-lived token issued by Silicon IAm"
    )]
    slt: Option<String>,
    #[arg(long, help = "Print tokens instead of saving them locally")]
    no_save: bool,
    #[command(subcommand)]
    command: Option<LoginCommand>,
}
#[derive(Subcommand)]
enum LoginCommand {
    /// Verify the current token and show the authenticated Carbon or Silicon.
    Status(JsonOutput),
}
#[derive(Args)]
struct JsonOutput {
    #[arg(
        long,
        help = "Print machine-readable JSON (also the default output format)"
    )]
    json: bool,
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
    /// List resources in the current organization.
    List(TodoList),
    /// Fetch a resource by its public ID.
    Get { id: String },
    /// Create a todo. Requires title and assigned_to in --data JSON or @FILE.
    #[command(
        after_help = "Required fields:\n  title: string\n  assigned_to: public IAM ID string (Carbon or Silicon from your team)\n\nOptional fields:\n  description: string or null\n  status: yet_to_do (default), in_progress, blocked, completed, canceled\n  attachments: array of HTTPS URL strings\n\nExamples:\n  commit todos create --data '{\"title\":\"Eat\",\"assigned_to\":\"alex\"}'\n  commit todos create --data '{\"title\":\"Eat\",\"assigned_to\":\"assistant:example-org\"}'\n\nUse assigned_to, not assignee_id or assignee. Unknown fields are rejected."
    )]
    Create(Data),
    /// Update a resource using --data JSON or @FILE.
    Update {
        id: String,
        #[command(flatten)]
        data: Data,
    },
    /// Delete the specified resource.
    Delete { id: String },
    /// List notes attached to a todo.
    Notes {
        id: String,
        #[command(flatten)]
        page: PageArgs,
    },
    /// Append a note to a todo using --data JSON.
    AddNote {
        id: String,
        #[command(flatten)]
        data: Data,
    },
    /// Read a todo notification subscription.
    Subscription { id: String },
    /// Replace a todo notification subscription using --data JSON.
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
    /// List resources in the current organization.
    List(ProjectList),
    /// Fetch a resource by its public ID.
    Get { id: String },
    /// Create a resource using --data JSON or @FILE.
    Create(Data),
    /// Update a resource using --data JSON or @FILE.
    Update {
        id: String,
        #[command(flatten)]
        data: Data,
    },
    /// Read the project diary.
    Diary { id: String },
    /// Replace the diary using --data JSON and --if-match VERSION.
    SetDiary {
        id: String,
        #[command(flatten)]
        data: Data,
    },
    /// List project tasks and subtasks.
    Tasks {
        id: String,
        #[command(flatten)]
        page: PageArgs,
    },
    /// List project blockers, updates, and completion entries.
    Entries {
        id: String,
        #[command(flatten)]
        page: PageArgs,
    },
    /// Create a project task using --data JSON.
    CreateTask {
        id: String,
        #[command(flatten)]
        data: Data,
    },
    /// Update a project task using --data JSON.
    UpdateTask {
        project: String,
        task: String,
        #[command(flatten)]
        data: Data,
    },
    /// Add a project blocker using --data JSON.
    Blocker {
        id: String,
        #[command(flatten)]
        data: Data,
    },
    /// Add a project milestone update using --data JSON.
    CreateUpdate {
        id: String,
        #[command(flatten)]
        data: Data,
    },
    /// Record project completion using --data JSON.
    Complete {
        id: String,
        #[command(flatten)]
        data: Data,
    },
}
#[derive(Subcommand)]
enum TestCommand {
    List,
    /// Create a resource using --data JSON or @FILE.
    Create(Data),
    /// Rotate the test environment access key.
    Rotate {
        id: String,
    },
    /// Retrieve the test environment access key.
    Key {
        id: String,
    },
    /// Restore a deleted test environment during its retention period.
    Restore {
        id: String,
    },
    /// Clear all data from a test environment.
    Clean {
        id: String,
    },
    /// Delete the specified resource.
    Delete {
        id: String,
    },
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
// Check the required creation shape without duplicating server-side business limits.
fn parse_todo_create(input: &str) -> Result<Value, Box<dyn std::error::Error>> {
    let value = parse_data(input)?;
    let object = value.as_object().ok_or(
        "todos create requires a JSON object with title and assigned_to; run commit todos create --help",
    )?;
    if object.contains_key("assignee_id") || object.contains_key("assignee") {
        return Err("todo not created: use assigned_to as a public IAM ID string, not assignee_id or assignee; example: {\"title\":\"Eat\",\"assigned_to\":\"alex\"}".into());
    }
    for field in ["title", "assigned_to"] {
        if !object.get(field).is_some_and(Value::is_string) {
            return Err(format!(
                "todo not created: {field} is required and must be a string; run commit todos create --help"
            ).into());
        }
    }
    Ok(value)
}

fn session_path() -> PathBuf {
    configured_home_dir()
        .unwrap_or_else(default_home_dir)
        .join(".commit/session.json")
}
fn default_home_dir() -> PathBuf {
    std::env::var_os("SILICON_HOME")
        .or_else(|| std::env::var_os("HOME"))
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
async fn logout(a: &Root) -> Result<(), Box<dyn std::error::Error>> {
    let directory = match fs::read_to_string(home_dir_config_path()) {
        Ok(path) if !path.trim().is_empty() => PathBuf::from(path.trim()),
        Ok(_) => return Err("the configured Commit home directory is empty".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => default_home_dir(),
        Err(error) => return Err(error.into()),
    };
    let path = directory.join(".commit/session.json");
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let saved: Session = serde_json::from_slice(&bytes)?;
    if saved.refresh_token.is_empty() {
        return Err("the saved session has no refresh token".into());
    }
    let mut client = Client::new(&saved.api_url)?;
    if let Some(api) = &a.api_url
        && Client::new(api)?.base_url() != client.base_url()
    {
        return Err("the selected API does not match the saved session".into());
    }
    client = client.with_mutation(match &a.idempotency_key {
        Some(key) => Mutation::with_key(key)?,
        None => Mutation::new(),
    });
    // ponytail: legacy sessions do not store test context; callers must reuse
    // their login selector until a future session format persists it.
    if let Some(key) = &a.test {
        client = client.with_test_key(key)?;
    }
    client.logout(&saved.refresh_token).await?;
    fs::remove_file(path)?;
    Ok(())
}
#[tokio::main]
async fn main() -> std::process::ExitCode {
    match run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Error: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let a = Root::parse();
    if let Command::Config {
        command: ConfigCommand::Home { location },
    } = &a.command
    {
        return set_home_dir(location.clone());
    }
    if matches!(a.command, Command::Logout(_)) {
        logout(&a).await?;
        println!("{}", serde_json::json!({"removed": true}));
        return Ok(());
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
        Command::Config { .. } | Command::Logout(_) => {
            unreachable!("local session commands return before API setup")
        }
        Command::Iam(_) => c.iam().await?,
        Command::Login(Login {
            command: Some(LoginCommand::Status(_)),
            ..
        }) => c.login_status().await?,
        Command::Login(x) => {
            let slt = x.slt.as_deref().ok_or("a short-lived token is required")?;
            let s = c.login_with_slt(slt).await?;
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
            TodoCommand::Create(d) => {
                let payload = parse_todo_create(&d.data)?;
                match c.create_todo(&payload).await {
                    Err(ref error @ silicon_commit_client::Error::Api { ref status, .. })
                        if status.as_u16() == 422 =>
                    {
                        return Err(format!("{error}; todo not created. Check the fields and values against commit todos create --help; required fields are title and assigned_to.").into());
                    }
                    result => result?,
                }
            }
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
            ProjectCommand::Entries { id, page } => {
                let limit = page.limit.to_string();
                let mut params = vec![("limit", limit.as_str())];
                if let Some(cursor) = page.cursor.as_deref() {
                    params.push(("cursor", cursor));
                }
                c.project_entries(&id, &params).await?
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
            .args(["install", "--locked", "--force", "silicon-commit-cli"])
            .stdout(std::process::Stdio::from(std::io::stderr()))
            .status()
        {
            Ok(status) if status.success() => eprintln!("Silicon Commit CLI updated"),
            Ok(_) => eprintln!("CLI update failed; continuing with the current version"),
            Err(_) => eprintln!("cargo was unavailable; continuing with the current version"),
        }
    }
}
