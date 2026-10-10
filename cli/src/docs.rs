//! Bundled guides (`commit docs`) and local bug-report drafts.

use std::path::PathBuf;

use crate::output::CliError;
use crate::state;

pub const DOCS_URL: &str = "https://docs.commit.teamofsilicons.com";
pub const REPOSITORY_URL: &str = "https://github.com/teamofsilicons/silicon-commit";
pub const CLIENT_URL: &str = "https://crates.io/crates/silicon-commit-client";

/// Topics, their aliases, and the guide each one prints.
const GUIDES: &[(&str, &[&str], &str)] = &[
    ("start", &["usage"], include_str!("../docs/START.md")),
    ("cli", &[], include_str!("../docs/CLI.md")),
    ("projects", &[], include_str!("../docs/PROJECTS.md")),
    (
        "notifications",
        &["email", "webhooks"],
        include_str!("../docs/NOTIFICATIONS.md"),
    ),
    ("client", &["rust"], include_str!("../docs/CLIENT.md")),
    ("api", &["http"], include_str!("../docs/API.md")),
    (
        "accounts",
        &["sign-in"],
        include_str!("../docs/ACCOUNTS.md"),
    ),
    (
        "contracts",
        &["versions"],
        include_str!("../docs/CONTRACTS.md"),
    ),
    (
        "development",
        &["build"],
        include_str!("../docs/DEVELOPMENT.md"),
    ),
    ("telemetry", &[], include_str!("../docs/TELEMETRY.md")),
    (
        "deployment",
        &["deploy"],
        include_str!("../docs/DEPLOYMENT.md"),
    ),
];

pub const DOCS_HELP: &str = "\
Topics:
  start          install, sign in, first todos and projects
  cli            every command, sessions, profiles, retries, errors
  projects       visibility, members, tasks, diary, versions
  notifications  Silicon webhooks, email preferences, bug reports
  client         the Rust client crate (silicon-commit-client)
  api            the HTTP API, credentials, sharing rules
  accounts       how Commit uses Silicon Accounts (for operators and integrators)
  contracts      API versions and compatibility
  development    building on Commit and running it locally
  telemetry      diagnostics and how to turn them off
  deployment     running the service

Examples:
  commit docs
  commit docs projects | less";

/// `commit docs TOPIC`.
pub fn print(topic: &str) -> Result<(), CliError> {
    let wanted = topic.trim().to_ascii_lowercase();
    let Some((_, _, content)) = GUIDES
        .iter()
        .find(|(name, aliases, _)| *name == wanted || aliases.contains(&wanted.as_str()))
    else {
        let names: Vec<&str> = GUIDES.iter().map(|(name, _, _)| *name).collect();
        return Err(CliError::new(
            "unknown_guide",
            format!("There is no guide called `{topic}`."),
        )
        .hint(format!("Choose one of: {}.", names.join(", "))));
    };
    println!(
        "{content}\n\nDocs: {DOCS_URL}\nSource: {REPOSITORY_URL}\nRust client: {CLIENT_URL}\nExplore: commit docs <topic>; commit <command> --help"
    );
    Ok(())
}

/// `commit report MESSAGE --save-only`.
pub fn save_report_draft(message: &str, pr: Option<&str>) -> Result<(), CliError> {
    let path = save_report(message, pr)?;
    println!("Saved bug report: {}", path.display());
    if pr.is_none() {
        eprintln!(
            "You can also reproduce, patch and open a pull request at {REPOSITORY_URL}, then attach it with --pr."
        );
    }
    Ok(())
}

/// Saves a private Markdown copy of a report under the state directory.
pub fn save_report(message: &str, pr: Option<&str>) -> Result<PathBuf, CliError> {
    if message.trim().is_empty() {
        return Err(CliError::new(
            "empty_report",
            "The report is empty: describe the bug, how to reproduce it, what you expected and what happened.",
        ));
    }
    if pr.is_some_and(|v| !v.starts_with("https://github.com/teamofsilicons/silicon-commit/pull/"))
    {
        return Err(CliError::new(
            "invalid_pr_link",
            "--pr must link to a pull request in teamofsilicons/silicon-commit (https://github.com/teamofsilicons/silicon-commit/pull/NUMBER).",
        ));
    }
    let body = format!(
        "{message}\n\nCLI version: {}\nPull request: {}\n",
        env!("CARGO_PKG_VERSION"),
        pr.unwrap_or("not supplied")
    );
    let path = state::directory().join(format!(
        "report-{}.md",
        silicon_commit_client::Client::new_idempotency_key()
    ));
    state::private_write(&path, body.as_bytes())?;
    Ok(path)
}
