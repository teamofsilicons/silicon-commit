//! Commands that call the Commit API: todos, projects, notifications, email, the Silicon
//! allow-list, `me`, reports and the public health/version checks.
//!
//! With a saved session, a request the API refuses with 401 is repeated once after a
//! refresh, with the same idempotency key and the same body (files given as @FILE are read
//! once, before the first attempt).

use std::fs;

use serde_json::Value;
use silicon_commit_client::auth::{AccountKind, peek_claims};
use silicon_commit_client::{Client, Error as ClientError, Mutation};

use crate::login::{elsewhere, targets};
use crate::output::{CliError, print_json};
use crate::session::{self, Loaded};
use crate::{Command, ProjectCommand, Root, SiliconCommand, TodoCommand, docs, state};

pub const TODOS_HELP: &str = "\
A todo belongs to whoever created it and is assigned to one account (yourself or anyone
you name by c:/si: id). The creator, the assignee, the Silicons each of them looks after
(and their custodians), and the members of a linked project can see it.

Examples:
  commit todos list                                  # assigned to me
  commit todos list --view delegated_by_me --status blocked
  commit todos create --data '{\"title\":\"Draft the notes\",\"assigned_to\":\"si:scribe\"}'
  commit todos update TODO --data '{\"status\":\"completed\"}'
  commit todos add-note TODO --data '{\"body\":\"Waiting on the review\"}'
  commit todos list --cursor NEXT_CURSOR             # next page";

pub const TODO_CREATE_HELP: &str = "\
Required fields:
  title         string
  assigned_to   the assignee: a c:/si: id (e.g. c:alice, si:builder) or an account uuid
Optional fields:
  description   string or null
  status        yet_to_do (default), in_progress, blocked, completed, canceled
  attachments   array of https URLs
  project_id    link the todo to a project you can change

Examples:
  commit todos create --data '{\"title\":\"Eat\",\"assigned_to\":\"c:alice\"}'
  commit todos create --data '{\"title\":\"Summarise the thread\",\"assigned_to\":\"si:scribe\",\"status\":\"in_progress\"}'
  commit todos create --data @todo.json

Use assigned_to, not assignee_id or assignee; unknown fields are rejected. A Silicon takes
todos only from its custodian, the custodian's other Silicons, and accounts it allowed
(`commit silicons allow`); others get 403 silicon_not_reachable.";

pub const PROJECTS_HELP: &str = "\
A project is visible to its owner, the owner's custodian and the Silicons the custodian
looks after (or the Carbon and the Silicons it looks after), its members, and the custodians
of member Silicons. `private: true` limits it to members and those custodians. Members
change it; tasks assigned to someone become their todos.

Examples:
  commit projects list --status in_progress
  commit projects create --data '{\"name\":\"Release\",\"description\":\"Ship version two\"}'
  commit projects create-task PROJECT --data '{\"title\":\"Build\",\"assigned_to\":\"si:builder\"}'
  commit projects claim PROJECT TASK
  commit projects diary PROJECT
  commit --if-match 3 projects set-diary PROJECT --data '{\"markdown\":\"# Plan\"}'
  commit projects versions PROJECT --before 42
See `commit docs projects` for the whole guide.";

pub const PROJECT_CREATE_HELP: &str = "\
Fields:
  name          string (required)
  description   string
  attachments   array of https URLs
  private       true limits the project to its members (default false)
  carbon_ids    members by c: id or uuid
  silicon_ids   members by si: id or uuid
  tasks         initial tasks: {title, description?, status?, assigned_to?, subtasks?}

Example:
  commit projects create --data '{\"name\":\"Private release\",\"private\":true,\"carbon_ids\":[\"c:alice\"],\"silicon_ids\":[\"si:builder\"],\"tasks\":[{\"title\":\"Build\",\"assigned_to\":\"si:builder\",\"subtasks\":[{\"title\":\"Verify\"}]}]}'";

pub const NOTIFICATIONS_HELP: &str = "\
Silicons get webhooks about the work they delegated: a list-wide rule plus per-todo rules
(`commit todos set-subscription`). A Silicon's custodian can manage them with --silicon.
Replacing needs --if-match with the current version.

Examples:
  commit notifications
  commit --if-match 0 notifications --data '{\"webhook_url\":\"https://hooks.example/commit\",\"todo_list_subscription\":{\"scope\":\"status_updates\"}}'
  commit notifications --silicon si:scout                # as its custodian
Scopes: any_update, status_updates, specific_statuses (with \"statuses\":[…]).";

pub const EMAIL_HELP: &str = "\
Carbons can get email about projects: completion (on by default), updates, completed
tasks, new assignments. Until you save an address, Commit uses the email you shared when
signing in (share one with `commit login --scope email`). Silicons use webhooks instead.

Examples:
  commit email
  commit email --data '{\"email\":\"you@example.com\",\"enabled\":true,\"project_completed\":true,\"project_updates\":false,\"task_completed\":true,\"task_assigned\":true}'
  commit email --data '{\"email\":\"you@example.com\",\"enabled\":false}'";

pub const SILICONS_HELP: &str = "\
A Silicon acts on what it receives, so it takes todos and project invitations only from
its custodian, the custodian's other Silicons, and the accounts on this list. The Silicon
or its custodian manages the list.

Examples:
  commit silicons allowed-accounts si:scout
  commit silicons allow si:scout c:alice
  commit silicons disallow si:scout c:alice";

/// A client for `api_url` with this process's telemetry choice and an optional bearer.
pub fn client(api_url: &str, token: Option<&str>) -> Result<Client, CliError> {
    let client = Client::new(api_url)?
        .with_source("cli")?
        .with_telemetry(state::telemetry_enabled());
    Ok(match token {
        Some(token) => client.with_bearer(token),
        None => client,
    })
}

fn parse_data(input: &str) -> Result<Value, CliError> {
    let text = match input.strip_prefix('@') {
        Some(path) => fs::read_to_string(path)
            .map_err(|e| CliError::io(format!("could not read --data file {path}"), &e))?,
        None => input.to_owned(),
    };
    serde_json::from_str(&text).map_err(|e| {
        CliError::new("invalid_json", format!("--data is not valid JSON: {e}"))
            .hint("Pass a JSON object inline (quote it for your shell) or as @path/to/file.json.")
    })
}

/// Checks the shape `todos create` needs without repeating the API's business rules.
fn check_todo_create(value: &Value) -> Result<(), CliError> {
    let help = "run `commit todos create --help` for the fields";
    let object = value.as_object().ok_or_else(|| {
        CliError::new(
            "invalid_todo",
            format!("todos create requires a JSON object with title and assigned_to; {help}"),
        )
    })?;
    if object.contains_key("assignee_id") || object.contains_key("assignee") {
        return Err(CliError::new(
            "invalid_todo",
            "todo not created: use assigned_to (a c:/si: id or account uuid), not assignee_id or assignee; example: {\"title\":\"Eat\",\"assigned_to\":\"c:alice\"}",
        ));
    }
    for field in ["title", "assigned_to"] {
        if !object.get(field).is_some_and(Value::is_string) {
            return Err(CliError::new(
                "invalid_todo",
                format!("todo not created: {field} is required and must be a string; {help}"),
            ));
        }
    }
    Ok(())
}

/// Reads every `--data` once (files included), so a repeated request sends the same body.
fn freeze_data(command: &mut Command) -> Result<(), CliError> {
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
        Command::Email { data } | Command::Notifications { data, .. } => data.as_mut(),
        _ => None,
    };
    if let Some(input) = input {
        *input = parse_data(input)?.to_string();
    }
    Ok(())
}

/// Runs an API command and prints its JSON result.
pub async fn run(mut root: Root) -> Result<(), CliError> {
    freeze_data(&mut root.command)?;
    if let Command::Todos {
        command: TodoCommand::Create(data),
    } = &root.command
    {
        check_todo_create(&parse_data(&data.data)?)?;
    }
    let key = root
        .idempotency_key
        .get_or_insert_with(Client::new_idempotency_key)
        .clone();
    let mut mutation = Mutation::with_key(key)?;
    if let Some(version) = root.if_match {
        mutation = mutation.if_match(version)?;
    }
    let mut result = call(&root, &mutation).await;
    if let (
        Err(error),
        Command::Todos {
            command: TodoCommand::Create(_),
        },
    ) = (&mut result, &root.command)
        && error.status == Some(422)
    {
        error.message = format!("{} The todo was not created.", error.message);
        error.hint = Some(
            "Check the fields named in the details against `commit todos create --help`; title and assigned_to (a c:/si: id or account uuid) are required.".into(),
        );
    }
    if let (Err(error), Command::Report { message, pr, .. }) = (&result, &root.command) {
        match docs::save_report(message, pr.as_deref()) {
            Ok(path) => eprintln!(
                "Report could not be submitted ({}). Saved locally: {}",
                error.code,
                path.display()
            ),
            Err(save) => eprintln!(
                "Report could not be submitted, and saving it locally failed too: {}",
                save.message
            ),
        }
    }
    let (value, kind) = result?;
    print_json(&value);
    match &root.command {
        Command::Report { pr: None, .. } => eprintln!(
            "You can also attach a fix: commit report \"…\" --pr https://github.com/teamofsilicons/silicon-commit/pull/NUMBER"
        ),
        Command::Email { data: None }
            if kind == Some(AccountKind::Carbon)
                && value["saved"] == false
                && value["shared_email"].is_null() =>
        {
            eprintln!(
                "No email is shared with Commit yet. Set one with `commit email --data '{{\"email\":\"you@example.com\"}}'`, or sign in again and share your email: `commit login --scope email`."
            );
        }
        _ => {}
    }
    Ok(())
}

/// Calls the API with the right credential; returns the result and the caller's kind.
async fn call(root: &Root, mutation: &Mutation) -> Result<(Value, Option<AccountKind>), CliError> {
    let saved = match session::load() {
        Ok(Loaded::Session(saved)) => Some(*saved),
        _ => None,
    };
    if matches!(
        root.command,
        Command::Health | Command::Ready | Command::Version
    ) {
        let api = targets(root, saved.as_ref()).api_url;
        return Ok((execute(&client(&api, None)?, &root.command).await?, None));
    }
    if let Some(token) = &root.token {
        let api = targets(root, saved.as_ref()).api_url;
        let kind = peek_claims(token).and_then(|c| c.kind);
        let client = client(&api, Some(token))?.with_mutation(mutation.clone());
        return Ok((execute(&client, &root.command).await?, kind));
    }
    let current = session::fresh(None, None).await?;
    if let Some(error) = elsewhere(root, &current) {
        return Err(error);
    }
    let kind = Some(current.account.kind);
    let first =
        client(&current.api_url, Some(&current.access_token))?.with_mutation(mutation.clone());
    match execute(&first, &root.command).await {
        Err(error) if error.is_unauthenticated() => {
            // Refresh once (another command may already have) and repeat the same request.
            let renewed =
                session::fresh(Some(&current.access_token), Some(&current.account.uuid)).await?;
            let again = client(&renewed.api_url, Some(&renewed.access_token))?
                .with_mutation(mutation.clone());
            Ok((execute(&again, &root.command).await?, kind))
        }
        other => Ok((other?, kind)),
    }
}

fn page<'a>(params: &mut Vec<(&'static str, &'a str)>, limit: &'a str, cursor: Option<&'a str>) {
    params.push(("limit", limit));
    if let Some(cursor) = cursor {
        params.push(("cursor", cursor));
    }
}

async fn execute(c: &Client, command: &Command) -> Result<Value, ClientError> {
    let data = |raw: &str| serde_json::from_str::<Value>(raw).map_err(ClientError::Decode);
    match command {
        Command::Health => c.health().await,
        Command::Ready => c.ready().await,
        Command::Version => c.version().await,
        Command::Me => c.me().await,
        Command::Email { data: Some(raw) } => c.set_email_settings(&data(raw)?).await,
        Command::Email { data: None } => c.email_settings().await,
        Command::Notifications {
            data: body,
            silicon,
        } => match (body, silicon) {
            (Some(raw), Some(silicon)) => {
                c.update_notification_settings_of(silicon, &data(raw)?)
                    .await
            }
            (Some(raw), None) => c.update_notification_settings(&data(raw)?).await,
            (None, Some(silicon)) => c.notification_settings_of(silicon).await,
            (None, None) => c.notification_settings().await,
        },
        Command::Silicons { command } => match command {
            SiliconCommand::AllowedAccounts { silicon } => c.silicon_allowlist(silicon).await,
            SiliconCommand::Allow { silicon, account } => c.allow_account(silicon, account).await,
            SiliconCommand::Disallow { silicon, account } => {
                c.disallow_account(silicon, account).await
            }
        },
        Command::Report { message, pr, .. } => {
            c.report(&serde_json::json!({ "message": message, "pr": pr }))
                .await
        }
        Command::Todos { command } => todos(c, command, &data).await,
        Command::Projects { command } => projects(c, command, &data).await,
        Command::Login(_)
        | Command::Logout(_)
        | Command::Accounts(_)
        | Command::Iam(_)
        | Command::Config { .. }
        | Command::Docs { .. } => Err(ClientError::Invalid(
            "this command does not call the Commit API".into(),
        )),
    }
}

async fn todos(
    c: &Client,
    command: &TodoCommand,
    data: &impl Fn(&str) -> Result<Value, ClientError>,
) -> Result<Value, ClientError> {
    match command {
        TodoCommand::List(q) => {
            let mut params = Vec::new();
            for (key, value) in [
                ("view", &q.view),
                ("status", &q.status),
                ("assigned_to", &q.assigned_to),
                ("assigned_by", &q.assigned_by),
                ("created_from", &q.created_from),
                ("created_to", &q.created_to),
            ] {
                if let Some(value) = value.as_deref() {
                    params.push((key, value));
                }
            }
            let limit = q.page.limit.to_string();
            page(&mut params, &limit, q.page.cursor.as_deref());
            c.list_todos(&params).await
        }
        TodoCommand::Get { id } => c.get_todo(id).await,
        TodoCommand::Create(d) => c.create_todo(&data(&d.data)?).await,
        TodoCommand::Update { id, data: d } => c.update_todo(id, &data(&d.data)?).await,
        TodoCommand::Delete { id } => c.delete_todo(id).await,
        TodoCommand::Notes { id, page: p } => {
            let mut params = Vec::new();
            let limit = p.limit.to_string();
            page(&mut params, &limit, p.cursor.as_deref());
            c.list_notes(id, &params).await
        }
        TodoCommand::AddNote { id, data: d } => c.add_note(id, &data(&d.data)?).await,
        TodoCommand::Subscription { id } => c.todo_subscription(id).await,
        TodoCommand::SetSubscription { id, data: d } => {
            c.replace_todo_subscription(id, &data(&d.data)?).await
        }
    }
}

async fn projects(
    c: &Client,
    command: &ProjectCommand,
    data: &impl Fn(&str) -> Result<Value, ClientError>,
) -> Result<Value, ClientError> {
    match command {
        ProjectCommand::List(q) => {
            let mut params = Vec::new();
            if let Some(status) = q.status.as_deref() {
                params.push(("status", status));
            }
            if let Some(silicon) = q.silicon_id.as_deref() {
                params.push(("silicon_id", silicon));
            }
            let limit = q.page.limit.to_string();
            page(&mut params, &limit, q.page.cursor.as_deref());
            c.list_projects(&params).await
        }
        ProjectCommand::Get { id } => c.get_project(id).await,
        ProjectCommand::Create(d) => c.create_project(&data(&d.data)?).await,
        ProjectCommand::Update { id, data: d } => c.update_project(id, &data(&d.data)?).await,
        ProjectCommand::Diary { id } => c.project_diary(id).await,
        ProjectCommand::SetDiary { id, data: d } => {
            c.replace_project_diary(id, &data(&d.data)?).await
        }
        ProjectCommand::Tasks { id, page: p } => {
            let mut params = Vec::new();
            let limit = p.limit.to_string();
            page(&mut params, &limit, p.cursor.as_deref());
            c.project_tasks(id, &params).await
        }
        ProjectCommand::CreateTask { id, data: d } => {
            c.create_project_task(id, &data(&d.data)?).await
        }
        ProjectCommand::UpdateTask {
            project,
            task,
            data: d,
        } => c.update_project_task(project, task, &data(&d.data)?).await,
        ProjectCommand::Claim { project, task } => c.claim_project_task(project, task).await,
        ProjectCommand::DeleteTask { project, task } => c.delete_project_task(project, task).await,
        ProjectCommand::Entries { id, page: p } => {
            let mut params = Vec::new();
            let limit = p.limit.to_string();
            page(&mut params, &limit, p.cursor.as_deref());
            c.project_entries(id, &params).await
        }
        ProjectCommand::Blocker { id, data: d } => {
            c.create_project_blocker(id, &data(&d.data)?).await
        }
        ProjectCommand::CreateUpdate { id, data: d } => {
            c.create_project_update(id, &data(&d.data)?).await
        }
        ProjectCommand::Complete { id, data: d } => c.complete_project(id, &data(&d.data)?).await,
        ProjectCommand::Versions { id, before } => {
            let before = before.map(|v| v.to_string());
            let query: Vec<(&str, &str)> = before
                .as_deref()
                .map(|v| vec![("before", v)])
                .unwrap_or_default();
            c.project_versions(id, &query).await
        }
        ProjectCommand::Version { id, version } => c.project_version(id, *version).await,
    }
}
