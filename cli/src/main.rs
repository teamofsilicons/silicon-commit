//! `commit`: the Silicon Commit CLI. Built only on the `silicon-commit-client` crate.

mod api;
mod docs;
mod login;
mod output;
mod session;
mod state;

use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

pub use output::CliError;

const ROOT_HELP: &str = "\
Sign in (once per profile; the session is saved and refreshed for you):
  commit login                        Carbon: approve a code on the account site
  silicon-accounts login --app commit -q | commit login --slt-stdin
                                      Silicon: exchange a short-lived token
  commit login status --json          who is signed in ({\"authenticated\":false} if nobody)
  commit accounts --json              app id, Silicon Accounts URL and API URL

Everyday work:
  commit todos list --view assigned_to_me
  commit todos create --data '{\"title\":\"Review the release\",\"assigned_to\":\"si:builder\"}'
  commit todos update TODO --data '{\"status\":\"completed\"}'
  commit projects create --data '{\"name\":\"Release\",\"description\":\"Ship version two\"}'
  commit projects tasks PROJECT

Accounts are named by c:/si: id (or account uuid). A Silicon takes work only from its
custodian, the custodian's other Silicons, and accounts it allowed (commit silicons --help).

Output is JSON on stdout. Errors go to stderr with the HTTP status, a stable code, the
request ID and a hint, and exit 1. Writes take --data '<json>' or --data @FILE; reuse
--idempotency-key when you retry the same write.

State lives in $SILICON_HOME/.commit (or ~/.commit); change it with `commit config home`.
Guides: commit docs start, commit <command> --help, https://docs.commit.teamofsilicons.com
Source: https://github.com/teamofsilicons/silicon-commit
Rust client: https://crates.io/crates/silicon-commit-client
Report a bug: commit report \"what happened\" [--pr <link to your fix>]";

#[derive(Parser, Clone)]
#[command(
    name = "commit",
    bin_name = "commit",
    version,
    about = "Silicon Commit: todos and projects for Carbons and Silicons",
    long_about = "Silicon Commit keeps todos and collaborative projects for Carbons and Silicons. \
Everything the website does, this CLI does: sign in, assign and track todos, run projects \
with tasks, a diary, blockers and updates, and choose how you are notified.",
    after_help = ROOT_HELP,
    max_term_width = 100
)]
pub struct Root {
    /// Commit API origin [default: the saved session's, else https://api.commit.teamofsilicons.com]
    #[arg(long, global = true, env = "COMMIT_API_URL", value_name = "URL")]
    pub api_url: Option<String>,
    /// Silicon Accounts origin used to sign in [default: the saved session's, else https://accounts.teamofsilicons.com]
    #[arg(long, global = true, env = "ACCOUNTS_URL", value_name = "URL")]
    pub accounts_url: Option<String>,
    /// Use this Silicon Accounts access token (issued to commit) instead of the saved session; never saved or refreshed
    #[arg(
        long,
        global = true,
        env = "COMMIT_ACCESS_TOKEN",
        hide_env_values = true,
        value_name = "TOKEN"
    )]
    pub token: Option<String>,
    /// Saved session to use; each profile holds one signed-in account
    #[arg(long, global = true, env = "COMMIT_PROFILE", default_value = "default", value_parser = state::profile_name, value_name = "NAME")]
    pub profile: String,
    /// Reuse this key when retrying the same write, so it is applied once
    #[arg(long, global = true, value_name = "KEY")]
    pub idempotency_key: Option<String>,
    /// Apply an update only if the resource is still at this version (diaries, notification settings)
    #[arg(long, global = true, value_name = "VERSION")]
    pub if_match: Option<i64>,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Clone)]
pub enum Command {
    /// Sign in to Commit: Carbons approve a code, Silicons exchange a short-lived token
    #[command(after_help = login::LOGIN_HELP)]
    Login(LoginArgs),
    /// End this profile's sign-in at Silicon Accounts and delete its saved session
    #[command(
        after_help = "Examples:\n  commit logout\n  commit --profile work logout --json\n  commit logout --force      # Silicon Accounts unreachable: delete the local session anyway"
    )]
    Logout(LogoutArgs),
    /// Show how to sign in to Commit: app id, Silicon Accounts URL, API URL (works signed out)
    #[command(
        after_help = "Prints the same object with or without --json and always exits 0, so tools can\ndiscover Commit before anyone signs in:\n  commit accounts --json\n  {\"app_id\":\"commit\",\"accounts_url\":\"https://accounts.teamofsilicons.com\",\"api_url\":\"https://api.commit.teamofsilicons.com\",\"version\":\"…\",…}"
    )]
    Accounts(JsonFlag),
    /// Deprecated name of `accounts`; kept one release for older Silicon runtimes
    #[command(hide = true)]
    Iam(JsonFlag),
    /// Show the signed-in account as Commit sees it (custodian, Silicons, shared email)
    Me,
    /// Create, read, update and delete todos, their notes and notification rules
    #[command(subcommand_required = true, arg_required_else_help = true, after_help = api::TODOS_HELP)]
    Todos {
        #[command(subcommand)]
        command: TodoCommand,
    },
    /// Run projects together: tasks and subtasks, diary, blockers, updates, versions
    #[command(subcommand_required = true, arg_required_else_help = true, after_help = api::PROJECTS_HELP)]
    Projects {
        #[command(subcommand)]
        command: ProjectCommand,
    },
    /// Read or replace a Silicon's webhook notification settings (yours, or as its custodian)
    #[command(after_help = api::NOTIFICATIONS_HELP)]
    Notifications {
        /// Replace the settings with this JSON object (or @FILE); without it, read them
        #[arg(long, value_name = "JSON|@FILE")]
        data: Option<String>,
        /// The Silicon whose settings to use (you are its custodian); default: your own
        #[arg(long, value_name = "SI_ID")]
        silicon: Option<String>,
    },
    /// Read or replace your email notification preferences
    #[command(after_help = api::EMAIL_HELP)]
    Email {
        /// Replace the preferences with this JSON object (or @FILE); without it, read them
        #[arg(long, value_name = "JSON|@FILE")]
        data: Option<String>,
    },
    /// Choose who besides its custodian and the custodian's other Silicons may assign work to a Silicon
    #[command(subcommand_required = true, arg_required_else_help = true, after_help = api::SILICONS_HELP)]
    Silicons {
        #[command(subcommand)]
        command: SiliconCommand,
    },
    /// Send a bug report to Commit's maintainers, optionally with a link to your fix
    #[command(
        after_help = "Describe what you did, what you expected and what happened; never include tokens.\nIf sending fails, a private copy is saved under the state directory.\n\nExamples:\n  commit report 'todos list returns 500 after creating a project; request ID 7f…'\n  commit report 'Wrong status after claim' --pr https://github.com/teamofsilicons/silicon-commit/pull/123\n  commit report 'Draft details' --save-only\n\nCommit is open source: reproduce, patch and open a pull request at\nhttps://github.com/teamofsilicons/silicon-commit, then attach it with --pr."
    )]
    Report {
        /// What happened, how to reproduce it, and what you expected
        message: String,
        /// Link to a pull request in teamofsilicons/silicon-commit that fixes it
        #[arg(long, value_name = "URL")]
        pr: Option<String>,
        /// Only save a local draft; contact nobody
        #[arg(long)]
        save_only: bool,
    },
    /// Local settings: state directory and telemetry
    #[command(subcommand_required = true, arg_required_else_help = true)]
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Read the bundled guides offline (start, projects, notifications, cli, client, api, …)
    #[command(after_help = docs::DOCS_HELP)]
    Docs {
        /// Guide to print
        #[arg(default_value = "start")]
        topic: String,
    },
    /// Check that the Commit API process is alive (no sign-in needed)
    Health,
    /// Check that the Commit API is ready to serve requests (no sign-in needed)
    Ready,
    /// Show the Commit API's build metadata (no sign-in needed)
    Version,
}

#[derive(Args, Clone)]
#[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
pub struct LoginArgs {
    /// A short-lived token as a positional argument (same as --slt)
    #[arg(value_name = "SLT", conflicts_with_all = ["slt", "slt_stdin"])]
    pub positional_slt: Option<String>,
    /// Sign in with a short-lived token from `silicon-accounts login --app commit -q`
    #[arg(long, value_name = "SLT", conflicts_with = "slt_stdin")]
    pub slt: Option<String>,
    /// Read the short-lived token from standard input (keeps it out of process lists)
    #[arg(long)]
    pub slt_stdin: bool,
    /// Extra account details to share with Commit, space-separated (e.g. "email"); device sign-in only
    #[arg(long, value_name = "SCOPES")]
    pub scope: Option<String>,
    /// Open the approval page in a browser (device sign-in)
    #[arg(long)]
    pub open: bool,
    /// Print the tokens instead of saving them (treat the output as a secret)
    #[arg(long)]
    pub no_save: bool,
    /// Print progress and the result as JSON
    #[arg(long)]
    pub json: bool,
    #[command(subcommand)]
    pub command: Option<LoginCommand>,
}

#[derive(Subcommand, Clone)]
pub enum LoginCommand {
    /// Show who is signed in on this profile (refreshes and checks the session unless --offline)
    #[command(after_help = login::STATUS_HELP)]
    Status(StatusArgs),
}

#[derive(Args, Clone)]
pub struct StatusArgs {
    /// Print JSON; always exits 0 ({"authenticated":false} when signed out)
    #[arg(long)]
    pub json: bool,
    /// Read only the saved session: no refresh, no network
    #[arg(long)]
    pub offline: bool,
}

#[derive(Args, Clone)]
pub struct LogoutArgs {
    /// Print JSON
    #[arg(long)]
    pub json: bool,
    /// Delete the saved session even if Silicon Accounts cannot be reached to end the sign-in
    #[arg(long)]
    pub force: bool,
}

#[derive(Args, Clone)]
pub struct JsonFlag {
    /// Print JSON (the output is the same object either way)
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Clone)]
pub struct Data {
    /// The request body: a JSON object, or @path/to/file.json
    #[arg(long, value_name = "JSON|@FILE")]
    pub data: String,
}

#[derive(Args, Clone)]
pub struct Page {
    /// Items per page (1-100)
    #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u16).range(1..=100))]
    pub limit: u16,
    /// Continue after this cursor (from the previous page's next_cursor)
    #[arg(long)]
    pub cursor: Option<String>,
}

#[derive(Args, Clone)]
pub struct TodoList {
    /// assigned_to_me (default), delegated_by_me, or all (everything you can see)
    #[arg(long, value_name = "VIEW")]
    pub view: Option<String>,
    /// yet_to_do, in_progress, blocked, completed or canceled
    #[arg(long)]
    pub status: Option<String>,
    /// Only todos assigned to this account (c:/si: id or uuid)
    #[arg(long, value_name = "ACCOUNT")]
    pub assigned_to: Option<String>,
    /// Only todos created by this account (c:/si: id or uuid)
    #[arg(long, value_name = "ACCOUNT")]
    pub assigned_by: Option<String>,
    /// Created at or after this RFC 3339 time
    #[arg(long, value_name = "TIME")]
    pub created_from: Option<String>,
    /// Created at or before this RFC 3339 time
    #[arg(long, value_name = "TIME")]
    pub created_to: Option<String>,
    #[command(flatten)]
    pub page: Page,
}

#[derive(Subcommand, Clone)]
pub enum TodoCommand {
    /// List todos you can see (assigned to you by default)
    List(TodoList),
    /// Read one todo
    Get { id: String },
    /// Create a todo for yourself or someone else (title and assigned_to required)
    #[command(after_help = api::TODO_CREATE_HELP)]
    Create(Data),
    /// Change a todo: title, description, status, attachments, assigned_to
    #[command(
        after_help = "Examples:\n  commit todos update TODO --data '{\"status\":\"in_progress\"}'\n  commit todos update TODO --data '{\"assigned_to\":\"c:alice\",\"description\":null}'\n\nThe todo's creator (or its custodian) changes everything; the assignee (or its\ncustodian) may change the status."
    )]
    Update {
        id: String,
        #[command(flatten)]
        data: Data,
    },
    /// Delete a todo you created
    Delete { id: String },
    /// List a todo's notes, oldest first
    Notes {
        id: String,
        #[command(flatten)]
        page: Page,
    },
    /// Add a note to a todo: --data '{"body":"…"}'
    AddNote {
        id: String,
        #[command(flatten)]
        data: Data,
    },
    /// Read the webhook rule for one todo you delegated (Silicons)
    Subscription { id: String },
    /// Replace the webhook rule for one todo you delegated (Silicons), with --if-match VERSION
    #[command(
        after_help = "Examples:\n  commit --if-match 0 todos set-subscription TODO --data '{\"subscription\":{\"scope\":\"specific_statuses\",\"statuses\":[\"completed\",\"blocked\"]}}'\n  commit --if-match 1 todos set-subscription TODO --data '{\"subscription\":null}'   # back to the list-wide rule\n\nScopes: any_update, status_updates, specific_statuses (with statuses). Read the current\nrule and its version first with `commit todos subscription TODO`; see `commit docs notifications`."
    )]
    SetSubscription {
        id: String,
        #[command(flatten)]
        data: Data,
    },
}

#[derive(Args, Clone)]
pub struct ProjectList {
    /// yet_to_start, in_progress, blocked, canceled or completed
    #[arg(long)]
    pub status: Option<String>,
    /// Only projects this Silicon is a member of (si: id or uuid)
    #[arg(long, value_name = "SI_ID")]
    pub silicon_id: Option<String>,
    #[command(flatten)]
    pub page: Page,
}

#[derive(Subcommand, Clone)]
pub enum ProjectCommand {
    /// List projects you can see
    List(ProjectList),
    /// Read one project (by id or UID)
    Get { id: String },
    /// Create a project, optionally private, with members and initial tasks
    #[command(after_help = api::PROJECT_CREATE_HELP)]
    Create(Data),
    /// Change a project: name, description, attachments, private, members
    #[command(
        after_help = "Examples:\n  commit projects update PROJECT --data '{\"private\":true,\"carbon_ids\":[\"c:alice\"],\"silicon_ids\":[\"si:builder\"]}'\n  commit projects update PROJECT --data '{\"description\":\"Ship version two by Friday\"}'"
    )]
    Update {
        id: String,
        #[command(flatten)]
        data: Data,
    },
    /// Read the project diary (Markdown, up to 100,000 words)
    Diary { id: String },
    /// Replace the diary: --data '{"markdown":"…"}' with --if-match VERSION
    #[command(
        after_help = "Example:\n  commit --if-match 3 projects set-diary PROJECT --data '{\"markdown\":\"# Plan\\n\\nFirst steps\"}'\n\nRead the diary first: its `version` is what --if-match needs."
    )]
    SetDiary {
        id: String,
        #[command(flatten)]
        data: Data,
    },
    /// List the project's tasks and subtasks
    Tasks {
        id: String,
        #[command(flatten)]
        page: Page,
    },
    /// Add a task: --data '{"title":"…","assigned_to":"si:…","parent_task_id":"…"}'
    CreateTask {
        id: String,
        #[command(flatten)]
        data: Data,
    },
    /// Change a task; assigning it creates or updates the linked todo
    UpdateTask {
        project: String,
        task: String,
        #[command(flatten)]
        data: Data,
    },
    /// Take an unassigned task (atomic; the first claim wins)
    Claim { project: String, task: String },
    /// Remove a task, its subtasks and their linked todos
    DeleteTask { project: String, task: String },
    /// List blockers, updates and the completion entry
    Entries {
        id: String,
        #[command(flatten)]
        page: Page,
    },
    /// Record a blocker: --data '{"title":"…","description":"…","status":"open"}'
    Blocker {
        id: String,
        #[command(flatten)]
        data: Data,
    },
    /// Record a milestone update: --data '{"title":"…","description":"…"}'
    CreateUpdate {
        id: String,
        #[command(flatten)]
        data: Data,
    },
    /// Complete the project for good: --data '{"title":"…","description":"…"}'
    Complete {
        id: String,
        #[command(flatten)]
        data: Data,
    },
    /// List retained versions, newest first (--before to page back)
    Versions {
        id: String,
        /// Only versions older than this one
        #[arg(long)]
        before: Option<i64>,
    },
    /// Read one retained version in full
    Version { id: String, version: i64 },
}

#[derive(Subcommand, Clone)]
pub enum SiliconCommand {
    /// List the accounts a Silicon accepts work from besides its custodian's Silicons
    AllowedAccounts {
        /// The Silicon (si: id or uuid)
        silicon: String,
    },
    /// Let an account assign todos to the Silicon and invite it to projects
    Allow {
        /// The Silicon (si: id or uuid)
        silicon: String,
        /// The account to allow (c:/si: id or uuid)
        account: String,
    },
    /// Take an account off the Silicon's list
    Disallow {
        /// The Silicon (si: id or uuid)
        silicon: String,
        /// The account to remove (c:/si: id or uuid)
        account: String,
    },
}

#[derive(Subcommand, Clone)]
pub enum ConfigCommand {
    /// Keep Commit's state in LOCATION/.commit instead of $SILICON_HOME or ~
    #[command(
        name = "home",
        visible_alias = "set_home_dir",
        after_help = "LOCATION must be an existing directory. The choice is remembered in\n$SILICON_HOME/.commit/home_dir (or ~/.commit/home_dir)."
    )]
    Home { location: PathBuf },
    /// Show the local configuration (no secrets)
    Show,
    /// Turn diagnostics on or off for every later command (COMMIT_TELEMETRY overrides it)
    Telemetry {
        #[arg(value_parser = ["on", "off"])]
        value: String,
    },
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let root = match Root::try_parse() {
        Ok(root) => root,
        Err(error) => {
            let code = error.exit_code();
            let _ = error.print();
            return std::process::ExitCode::from(u8::try_from(code).unwrap_or(2));
        }
    };
    state::set_profile(root.profile.clone());
    let json_errors = match &root.command {
        Command::Login(args) => match &args.command {
            Some(LoginCommand::Status(status)) => status.json,
            None => args.json,
        },
        Command::Logout(args) => args.json,
        _ => false,
    };
    match run(root).await {
        Ok(code) => std::process::ExitCode::from(code),
        Err(error) => {
            error.print(json_errors);
            std::process::ExitCode::from(error.exit)
        }
    }
}

/// Runs a command; returns the exit code on success (0, or 1 for "signed out" status text).
async fn run(root: Root) -> Result<u8, CliError> {
    match &root.command {
        Command::Login(args) => match &args.command {
            Some(LoginCommand::Status(status)) => login::status(&root, status).await,
            None => login::login(&root, args).await.map(|()| 0),
        },
        Command::Logout(args) => login::logout(args).await.map(|()| 0),
        Command::Accounts(flag) | Command::Iam(flag) => {
            login::accounts(&root, flag.json);
            Ok(0)
        }
        Command::Docs { topic } => docs::print(topic).map(|()| 0),
        Command::Config { command } => config(command).map(|()| 0),
        Command::Report {
            message,
            pr,
            save_only: true,
        } => docs::save_report_draft(message, pr.as_deref()).map(|()| 0),
        _ => api::run(root).await.map(|()| 0),
    }
}

fn config(command: &ConfigCommand) -> Result<(), CliError> {
    match command {
        ConfigCommand::Home { location } => {
            let home = state::set_home(location)?;
            println!("Commit home directory set to {}", home.display());
        }
        ConfigCommand::Telemetry { value } => {
            state::private_write(&state::directory().join("telemetry"), value.as_bytes())?;
            println!("Telemetry {value}");
        }
        ConfigCommand::Show => {
            let saved = match session::load() {
                Ok(session::Loaded::Session(s)) => {
                    Some((s.api_url.clone(), s.accounts_url.clone()))
                }
                _ => None,
            };
            output::print_json(&serde_json::json!({
                "home": state::home(),
                "state_dir": state::directory(),
                "profile": state::profile(),
                "session_file": session::path(),
                "signed_in": saved.is_some(),
                "api_url": saved.as_ref().map(|s| s.0.clone()),
                "accounts_url": saved.as_ref().map(|s| s.1.clone()),
                "telemetry": if state::telemetry_enabled() { "on" } else { "off" },
                "docs": docs::DOCS_URL,
                "repository": docs::REPOSITORY_URL,
            }));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Root, clap::Error> {
        Root::try_parse_from(std::iter::once("commit").chain(args.iter().copied()))
    }

    #[test]
    fn login_takes_a_token_three_ways_or_starts_the_device_flow() {
        let Command::Login(login) = parse(&["login"]).unwrap().command else {
            panic!()
        };
        assert!(
            login.positional_slt.is_none()
                && login.slt.is_none()
                && !login.slt_stdin
                && login.command.is_none()
        );
        let Command::Login(login) = parse(&["login", "slt_abc"]).unwrap().command else {
            panic!()
        };
        assert_eq!(login.positional_slt.as_deref(), Some("slt_abc"));
        let Command::Login(login) = parse(&["login", "--slt", "slt_abc", "--json"])
            .unwrap()
            .command
        else {
            panic!()
        };
        assert_eq!(login.slt.as_deref(), Some("slt_abc"));
        let Command::Login(login) = parse(&["login", "status", "--json", "--offline"])
            .unwrap()
            .command
        else {
            panic!()
        };
        assert!(matches!(
            login.command,
            Some(LoginCommand::Status(StatusArgs {
                json: true,
                offline: true
            }))
        ));
        for conflict in [
            vec!["login", "--slt", "slt_a", "--slt-stdin"],
            vec!["login", "slt_a", "--slt", "slt_b"],
            vec!["login", "slt_a", "--slt-stdin"],
            vec!["login", "status", "--no-save"],
        ] {
            assert!(parse(&conflict).is_err(), "{conflict:?}");
        }
    }

    #[test]
    fn global_options_work_after_the_subcommand_and_profiles_are_validated() {
        let root = parse(&[
            "todos",
            "list",
            "--api-url",
            "http://127.0.0.1:4141",
            "--profile",
            "work",
            "--limit",
            "5",
        ])
        .unwrap();
        assert_eq!(root.api_url.as_deref(), Some("http://127.0.0.1:4141"));
        assert_eq!(root.profile, "work");
        assert!(parse(&["--profile", "Work", "me"]).is_err());
        assert!(parse(&["--profile", "../x", "me"]).is_err());
        assert!(parse(&["todos", "list", "--limit", "0"]).is_err());
        assert!(parse(&["todos", "list", "--limit", "101"]).is_err());
        assert!(parse(&["iam", "--json"]).is_ok(), "hidden alias");
        assert!(parse(&["--org-id", "tos", "me"]).is_err());
    }

    #[test]
    fn the_command_tree_is_consistent() {
        use clap::CommandFactory as _;
        Root::command().debug_assert();
    }
}
