mod daemon;
mod runtime;
use clap::{Args, Parser, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use silicon_commit_client::{Client, Mutation};
use std::{fs, path::PathBuf};

#[derive(Parser, Clone)]
#[command(
    name = "commit",
    bin_name = "commit",
    version,
    about = "Silicon Commit work manager",
    after_help = "Quick start:\n  commit iam --json\n  commit login <slt>\n  commit login status --json\n  commit todos list\n\nSet COMMIT_API_URL, COMMIT_ACCESS_TOKEN, and COMMIT_ORG_ID for non-interactive use.\nState defaults to $SILICON_HOME/.commit or $HOME/.commit; override with commit config home LOCATION.\nWrites accept --data '<json>' or --data @FILE and support --if-match.\nUse --test APP_SECRET for a sandbox. Install and update with honeycomb install 'tos>commit'.\nRun commit <command> --help for arguments and subcommands."
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
        help = "IAM test app_secret; selects a sandbox without an IAM root key"
    )]
    test: Option<String>,
    #[arg(long, global = true, help = "Reuse a key when retrying the same write")]
    idempotency_key: Option<String>,
    #[arg(long, global = true, help = "Expected resource version for an update")]
    if_match: Option<i64>,
    #[arg(
        long,
        global = true,
        help = "Compatibility flag; Honeycomb manages CLI updates"
    )]
    no_update: bool,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand, Clone)]
enum Command {
    /// Browse bundled usage and development guides without network access.
    Docs {
        #[arg(default_value = "start")]
        topic: String,
    },
    /// Select an IAM sandbox, inspect it, or return to the production session.
    Testing {
        #[command(subcommand)]
        command: runtime::TestingCommand,
    },
    /// Inspect or remove the legacy updater; Honeycomb manages updates.
    Daemon {
        #[command(subcommand)]
        command: daemon::DaemonCommand,
    },
    /// Submit a GitHub bug report, optionally with a patch PR.
    Report {
        message: String,
        #[arg(long)]
        pr: Option<String>,
        #[arg(long)]
        save_only: bool,
    },
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
    /// Configure the email used for this organization and subscribed event kinds.
    Email {
        #[arg(long)]
        data: Option<String>,
    },
    Notifications {
        #[arg(long)]
        data: Option<String>,
    },
    /// Inspect legacy environments; create shared sandboxes through Honeycomb.
    TestEnvironments {
        #[command(subcommand)]
        command: TestCommand,
    },
}
#[derive(Subcommand, Clone)]
enum ConfigCommand {
    /// Set the home directory used for Commit's local state.
    #[command(name = "home", visible_alias = "set_home_dir")]
    Home { location: PathBuf },
    /// Inspect non-secret local configuration.
    Show,
    /// Disable legacy self-updates; configure managed updates in Honeycomb.
    Updates {
        #[arg(value_parser=["on","off"])]
        value: String,
    },
    /// Enable or disable diagnostics for all subsequent CLI requests.
    Telemetry {
        #[arg(value_parser=["on","off"])]
        value: String,
    },
}
#[derive(Args, Clone)]
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
#[derive(Subcommand, Clone)]
enum LoginCommand {
    /// Verify the current token and show the authenticated Carbon or Silicon.
    Status(JsonOutput),
}
#[derive(Args, Clone)]
struct JsonOutput {
    #[arg(
        long,
        help = "Print machine-readable JSON (also the default output format)"
    )]
    json: bool,
}
#[derive(Args, Default, Clone)]
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

#[derive(Subcommand, Clone)]
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
#[derive(Args, Default, Clone)]
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

#[derive(Subcommand, Clone)]
enum ProjectCommand {
    /// Atomically take an unassigned project task or subtask.
    Claim { project: String, task: String },
    /// Remove a task subtree and its linked todos.
    DeleteTask { project: String, task: String },
    /// List the latest project revisions; use --before to page backward.
    Versions {
        id: String,
        #[arg(long)]
        before: Option<i64>,
    },
    /// Read a complete retained project snapshot.
    Version { id: String, version: i64 },
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
#[derive(Subcommand, Clone)]
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
#[derive(Args, Clone)]
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
    #[serde(default)]
    test_key: Option<String>,
    #[serde(default)]
    expires_at: u64,
    #[serde(default)]
    refresh_started_at: Option<u64>,
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

async fn session_lock() -> Result<fs::File, Box<dyn std::error::Error>> {
    let path = session_path();
    Ok(
        tokio::task::spawn_blocking(move || -> std::io::Result<fs::File> {
            let directory = path.parent().expect("session path has a parent");
            fs::create_dir_all(directory)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
            }
            let mut options = fs::OpenOptions::new();
            options.read(true).write(true).create(true).truncate(false);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt as _;
                options.mode(0o600);
            }
            let file = options.open(path.with_extension("lock"))?;
            file.lock()?;
            Ok(file)
        })
        .await??,
    )
}

async fn refresh_saved_session(
    api: &str,
    rejected_access: Option<&str>,
) -> Result<Option<Session>, Box<dyn std::error::Error>> {
    let _lock = session_lock().await?;
    // Re-read after acquiring the process lock: another command may already
    // have rotated the single-use refresh token.
    let Some(mut session) = load_session() else {
        return Ok(None);
    };
    let mut client = Client::new(api)?;
    if client.base_url() != Client::new(&session.api_url)?.base_url() {
        return Err("the selected API does not match the saved session; sign in for this API or supply an explicit token".into());
    }
    if session.test_key.as_deref() != runtime::selected_key().as_deref() {
        return Err("saved session environment mismatch; sign in again".into());
    }
    let force = rejected_access.is_some_and(|token| token == session.access_token);
    if (!force
        && session.expires_at > now().saturating_add(60)
        && session.refresh_started_at.is_none())
        || session.refresh_token.is_empty()
    {
        return Ok(Some(session));
    }
    if let Some(key) = &session.test_key {
        client = client.with_test_key(key)?;
    }
    use sha2::{Digest as _, Sha256};
    // Retrying after a lost response must replay the same rotation, not mark
    // this refresh family compromised by consuming its old token twice.
    for _ in 0..2 {
        let started_at = *session.refresh_started_at.get_or_insert_with(now);
        save_session(&session)?;
        let key = format!(
            "commit-refresh-{:x}",
            Sha256::digest(session.refresh_token.as_bytes())
        );
        let tokens = client
            .clone()
            .with_mutation(Mutation::with_key(key)?)
            .refresh_session(&session.refresh_token)
            .await?;
        session.access_token = tokens.access_token;
        session.refresh_token = tokens.refresh_token;
        session.org_id = tokens.org_id.or(session.org_id);
        session.expires_at = started_at.saturating_add(tokens.expires_in.max(0) as u64);
        session.refresh_started_at = None;
        save_session(&session)?;
        if session.expires_at > now().saturating_add(60) {
            return Ok(Some(session));
        }
    }
    Err("refreshed access token has no usable lifetime; retry the command".into())
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
        .join(".commit")
        .join(runtime::session_file())
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
    runtime::private_write(&session_path(), &serde_json::to_vec_pretty(s)?)
}
async fn remember_organization(token: &str, org: &str) -> Result<(), Box<dyn std::error::Error>> {
    let _lock = session_lock().await?;
    if let Some(mut session) = load_session()
        && session.access_token == token
    {
        session.org_id = Some(org.to_owned());
        save_session(&session)?;
    }
    Ok(())
}

fn organization_from_status(status: &Value) -> Result<String, Box<dyn std::error::Error>> {
    if status["authenticated"] != true {
        return Err(
            "Your Commit session expired or was revoked. Run commit login <slt> to sign in again."
                .into(),
        );
    }
    if let Some(org) = status["org_id"].as_str().filter(|s| !s.is_empty()) {
        return Ok(org.to_owned());
    }
    let organizations = status["organizations"]
        .as_array()
        .map(|items| items.iter().filter_map(Value::as_str).collect::<Vec<_>>())
        .unwrap_or_default();
    match organizations.as_slice() {
        [org] => Ok((*org).to_owned()),
        [] => Err("Your session has no accessible organization. Check IAM organization access and sign in again.".into()),
        _ => Err(format!("Choose an organization with --org-id. Available organizations: {}", organizations.join(", ")).into()),
    }
}
async fn logout(a: &Root) -> Result<(), Box<dyn std::error::Error>> {
    let directory = match fs::read_to_string(home_dir_config_path()) {
        Ok(path) if !path.trim().is_empty() => PathBuf::from(path.trim()),
        Ok(_) => return Err("the configured Commit home directory is empty".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => default_home_dir(),
        Err(error) => return Err(error.into()),
    };
    let path = directory.join(".commit").join(runtime::session_file());
    if !path.exists() {
        return Ok(());
    }
    let _lock = session_lock().await?;
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
    if saved.test_key.as_deref() != runtime::selected_key().as_deref() {
        return Err("saved session belongs to a different environment; sign in again".into());
    }
    if let Some(key) = &saved.test_key {
        client = client.with_test_key(key)?;
    }
    client.logout(&saved.refresh_token).await?;
    fs::remove_file(path)?;
    Ok(())
}
#[tokio::main]
async fn main() -> std::process::ExitCode {
    runtime::initialize();
    let result = match Root::try_parse() {
        Ok(mut args) => {
            args.test = runtime::selected_key();
            let report = match &args.command {
                Command::Report {
                    message,
                    pr,
                    save_only: false,
                } => Some((message.clone(), pr.clone())),
                _ => None,
            };
            let result = run_with_session_recovery(args).await;
            if result.is_err()
                && let Some((message, pr)) = report
            {
                match runtime::save_report(&message, pr.as_deref()) {
                    Ok(path) => eprintln!(
                        "Report could not be submitted. Saved locally: {}",
                        path.display()
                    ),
                    Err(error) => eprintln!("Could not save the report locally: {error}"),
                }
            }
            result
        }
        Err(error) => {
            let code = error.exit_code();
            let _ = error.print();
            runtime::footer();
            return std::process::ExitCode::from(u8::try_from(code).unwrap_or(2));
        }
    };
    if let Err(error) = &result {
        if matches!(error.downcast_ref::<silicon_commit_client::Error>(), Some(silicon_commit_client::Error::Api { status, .. }) if status.as_u16() == 401)
        {
            eprintln!(
                "Your Commit session expired or was revoked. Run commit login <slt> to sign in again."
            );
        }
        if matches!(error.downcast_ref::<silicon_commit_client::Error>(), Some(silicon_commit_client::Error::Api { code, .. }) if code == "honeycomb_manages_testing_lifecycle")
        {
            eprintln!(
                "Manage this environment in Honeycomb, then select Commit with commit testing use '<app_secret>'. See commit docs testing."
            );
        }
        eprintln!(
            "commit: {error}\nSee commit docs or commit <command> --help for usage and recovery steps."
        );
    }
    runtime::footer();
    if result.is_ok() {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::FAILURE
    }
}
fn inactive_access() -> Box<dyn std::error::Error> {
    silicon_commit_client::Error::Api {
        status: 401_u16.try_into().expect("valid HTTP status"),
        code: "inactive_access_token".into(),
        request_id: None,
    }
    .into()
}

fn access_rejected(error: &(dyn std::error::Error + 'static)) -> bool {
    error
        .downcast_ref::<silicon_commit_client::Error>()
        .is_some_and(silicon_commit_client::Error::is_unauthenticated)
}

// Freeze file-backed JSON before the first request. A retry must use the same
// payload even if the source file is replaced while refresh is in flight.
fn freeze_command_data(command: &mut Command) -> Result<(), Box<dyn std::error::Error>> {
    let input = match command {
        Command::Todos {
            command:
                TodoCommand::Create(data)
                | TodoCommand::Update { data, .. }
                | TodoCommand::AddNote { data, .. }
                | TodoCommand::SetSubscription { data, .. },
        } => Some(&mut data.data),
        Command::Projects {
            command:
                ProjectCommand::Create(data)
                | ProjectCommand::Update { data, .. }
                | ProjectCommand::SetDiary { data, .. }
                | ProjectCommand::CreateTask { data, .. }
                | ProjectCommand::UpdateTask { data, .. }
                | ProjectCommand::Blocker { data, .. }
                | ProjectCommand::CreateUpdate { data, .. }
                | ProjectCommand::Complete { data, .. },
        } => Some(&mut data.data),
        Command::TestEnvironments {
            command: TestCommand::Create(data),
        } => Some(&mut data.data),
        Command::Email { data } | Command::Notifications { data } => data.as_mut(),
        _ => None,
    };
    if let Some(input) = input {
        *input = serde_json::to_string(&parse_data(input)?)?;
    }
    Ok(())
}

async fn run_with_session_recovery(mut args: Root) -> Result<(), Box<dyn std::error::Error>> {
    freeze_command_data(&mut args.command)?;
    args.idempotency_key
        .get_or_insert_with(Client::new_idempotency_key);
    let is_status = matches!(
        args.command,
        Command::Login(Login {
            command: Some(LoginCommand::Status(_)),
            ..
        })
    );
    let mut used_access = None;
    let first = run(args.clone(), &mut used_access, true).await;
    if let Err(error) = first {
        let Some((api, rejected)) = used_access.filter(|_| access_rejected(error.as_ref())) else {
            return Err(error);
        };
        // Reload under the same process lock used for proactive refresh. Adopt
        // another command's replacement instead of rotating a newer generation.
        match refresh_saved_session(&api, Some(&rejected)).await {
            Ok(Some(_)) => run(args, &mut None, false).await,
            Ok(None) => Err(error),
            Err(refresh_error) if is_status && access_rejected(refresh_error.as_ref()) => {
                println!(
                    "{}",
                    serde_json::json!({"authenticated":false,"actor":null,"org_id":null})
                );
                Ok(())
            }
            Err(refresh_error) => Err(refresh_error),
        }
    } else {
        first
    }
}

async fn run(
    a: Root,
    rejected_access: &mut Option<(String, String)>,
    retry_inactive_status: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let login_status = matches!(
        a.command,
        Command::Login(Login {
            command: Some(LoginCommand::Status(_)),
            ..
        })
    );
    if let Command::Todos {
        command: TodoCommand::Create(data),
    } = &a.command
    {
        parse_todo_create(&data.data)?;
    }
    match &a.command {
        Command::Config {
            command: ConfigCommand::Telemetry { value },
        } => {
            runtime::private_write(&runtime::directory().join("telemetry"), value.as_bytes())?;
            println!("Telemetry {value}");
            return Ok(());
        }

        Command::Docs { topic } => return runtime::docs(topic),
        Command::Report {
            message,
            pr,
            save_only: true,
        } => return runtime::report(message, pr.as_deref()),
        Command::Testing { command } => {
            return runtime::testing(command, a.api_url.as_deref()).await;
        }
        Command::Daemon { command } => return daemon::command(command).await,
        Command::Config {
            command: ConfigCommand::Show,
        } => {
            println!(
                "{}",
                serde_json::json!({"home":configured_home_dir().unwrap_or_else(default_home_dir),"auto_update":daemon::updates_enabled(),"update_manager":"honeycomb","docs":"https://docs.commit.teamofsilicons.com","repository":"https://github.com/teamofsilicons/silicon-commit"})
            );
            return Ok(());
        }
        Command::Config {
            command: ConfigCommand::Updates { value },
        } => return daemon::configure_updates(value == "on"),
        _ => {}
    }
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
    let mut saved = load_session();
    if saved
        .as_ref()
        .is_some_and(|s| s.test_key.as_deref() != runtime::selected_key().as_deref())
    {
        return Err("saved session environment mismatch; sign in again".into());
    }
    let api = a
        .api_url
        .clone()
        .or_else(|| saved.as_ref().map(|s| s.api_url.clone()))
        .unwrap_or_else(|| "https://backend.commit.teamofsilicons.com".to_owned());
    let uses_saved_session = a.token.is_none()
        && saved.is_some()
        && !matches!(
            a.command,
            Command::Login(Login { command: None, .. })
                | Command::Iam(_)
                | Command::Health
                | Command::Ready
                | Command::Version
                | Command::Report {
                    save_only: true,
                    ..
                }
        );
    if uses_saved_session {
        saved = match refresh_saved_session(&api, None).await {
            Ok(saved) => saved,
            Err(error)
                if matches!(
                    &a.command,
                    Command::Login(Login {
                        command: Some(LoginCommand::Status(_)),
                        ..
                    })
                ) && matches!(error.downcast_ref::<silicon_commit_client::Error>(), Some(silicon_commit_client::Error::Api { status, .. }) if status.as_u16() == 401) =>
            {
                None
            }
            Err(error) => return Err(error),
        };
    }
    let token = a.token.clone().or_else(|| {
        if uses_saved_session {
            saved.as_ref().map(|s| s.access_token.clone())
        } else {
            None
        }
    });
    if uses_saved_session {
        *rejected_access = token.clone().map(|token| (api.clone(), token));
    }
    let org = a.org_id.clone().or_else(|| {
        if uses_saved_session {
            saved.as_ref().and_then(|s| s.org_id.clone())
        } else {
            None
        }
    });
    let mut c = Client::new(&api)?
        .with_source("cli")?
        .with_telemetry(runtime::telemetry_enabled())
        .with_mutation({
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
    if let Some(t) = &token {
        c = c.with_bearer(t.clone());
    }
    if let Some(o) = &org {
        c = c.with_org_id(o.clone());
    }
    if let Some(k) = a.test {
        c = c.with_test_key(k)?;
    }
    let needs_organization = !matches!(
        &a.command,
        Command::Login(_)
            | Command::Iam(_)
            | Command::Health
            | Command::Ready
            | Command::Version
            | Command::Report {
                save_only: true,
                ..
            }
            | Command::TestEnvironments {
                command: TestCommand::Create(_)
                    | TestCommand::Rotate { .. }
                    | TestCommand::Restore { .. }
                    | TestCommand::Clean { .. }
                    | TestCommand::Delete { .. }
            }
    );
    if needs_organization && org.is_none() {
        let status = c.login_status().await?;
        if uses_saved_session && retry_inactive_status && status["authenticated"] == false {
            return Err(inactive_access());
        }
        let selected = organization_from_status(&status)?;
        if uses_saved_session && let Some(token) = &token {
            remember_organization(token, &selected).await?;
        }
        c = c.with_org_id(selected);
    }
    let output = match a.command {
        Command::Docs { .. }
        | Command::Testing { .. }
        | Command::Daemon { .. }
        | Command::Config { .. }
        | Command::Logout(_)
        | Command::Report {
            save_only: true, ..
        } => {
            unreachable!("local session commands return before API setup")
        }
        Command::Report {
            message,
            pr,
            save_only: false,
        } => {
            let result = c
                .report(&serde_json::json!({"message":message,"pr":pr}))
                .await?;
            if pr.is_none() {
                eprintln!(
                    "You can also attach a fix with --pr https://github.com/teamofsilicons/silicon-commit/pull/NUMBER"
                );
            }
            result
        }
        Command::Email { data: d } => {
            if let Some(d) = d {
                c.set_email_settings(&parse_data(&d)?).await?
            } else {
                c.email_settings().await?
            }
        }
        Command::Iam(_) => c.iam().await?,
        Command::Login(Login {
            command: Some(LoginCommand::Status(_)),
            ..
        }) => c.login_status().await?,
        Command::Login(x) => {
            let _lock = if x.no_save {
                None
            } else {
                Some(session_lock().await?)
            };
            let slt = x.slt.as_deref().ok_or("a short-lived token is required")?;
            let s = c.login_with_slt(slt).await?;
            if !x.no_save {
                save_session(&Session {
                    access_token: s.access_token.clone(),
                    refresh_token: s.refresh_token.clone(),
                    api_url: api,
                    org_id: s.org_id.clone(),
                    test_key: runtime::selected_key(),
                    expires_at: now().saturating_add(s.expires_in.max(0) as u64),
                    refresh_started_at: None,
                })?;
            }
            if runtime::selected_key().is_some() {
                runtime::remember_context(&c).await;
            }
            if x.no_save {
                serde_json::to_value(s)?
            } else {
                serde_json::json!({"authenticated":true,"actor":s.actor,"org_id":s.org_id})
            }
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
            ProjectCommand::Claim { project, task } => {
                c.claim_project_task(&project, &task).await?
            }
            ProjectCommand::DeleteTask { project, task } => {
                c.delete_project_task(&project, &task).await?
            }
            ProjectCommand::Versions { id, before } => {
                let value = before.map(|v| v.to_string());
                let query = value
                    .as_deref()
                    .map(|v| vec![("before", v)])
                    .unwrap_or_default();
                c.project_versions(&id, &query).await?
            }
            ProjectCommand::Version { id, version } => c.project_version(&id, version).await?,
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
    if uses_saved_session
        && token.is_some()
        && login_status
        && retry_inactive_status
        && output["authenticated"] == false
    {
        return Err(inactive_access());
    }
    println!("{}", serde_json::to_string_pretty(&output)?);

    Ok(())
}
