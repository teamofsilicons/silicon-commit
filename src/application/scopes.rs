//! Commit's action ids.
//!
//! Every API route performs exactly one action. The ids are also the scopes
//! another app requests in a User verification proof when it acts for an
//! account at Commit (`Authorization: Proof sap_…`); `COMMIT_PROOF_ISSUERS`
//! says which apps may use which scope. They keep the ids of the IAM era so
//! existing callers' scope names stay valid.

/// Read the represented Silicon's notification settings.
pub const NOTIFICATION_SETTINGS_READ: &str = "commit.notification_settings.read";
/// Replace the represented Silicon's notification settings.
pub const NOTIFICATION_SETTINGS_UPDATE: &str = "commit.notification_settings.update";
/// List todos.
pub const TODOS_LIST: &str = "commit.todos.list";
/// Create a todo.
pub const TODOS_CREATE: &str = "commit.todos.create";
/// Read one todo.
pub const TODOS_READ: &str = "commit.todos.read";
/// Update one todo.
pub const TODOS_UPDATE: &str = "commit.todos.update";
/// Delete one todo.
pub const TODOS_DELETE: &str = "commit.todos.delete";
/// List todo notes.
pub const TODO_NOTES_LIST: &str = "commit.todo_notes.list";
/// Append a todo note.
pub const TODO_NOTES_CREATE: &str = "commit.todo_notes.create";
/// Read a todo-specific notification subscription.
pub const TODO_SUBSCRIPTION_READ: &str = "commit.todo_subscription.read";
/// Replace a todo-specific notification subscription.
pub const TODO_SUBSCRIPTION_UPDATE: &str = "commit.todo_subscription.update";
/// List projects.
pub const PROJECTS_LIST: &str = "commit.projects.list";
/// Create a project.
pub const PROJECTS_CREATE: &str = "commit.projects.create";
/// Read one project, its entries and its history.
pub const PROJECTS_READ: &str = "commit.projects.read";
/// Update one project.
pub const PROJECTS_UPDATE: &str = "commit.projects.update";
/// Read a diary.
pub const DIARY_READ: &str = "commit.project_diary.read";
/// Replace a diary.
pub const DIARY_UPDATE: &str = "commit.project_diary.update";
/// List project tasks.
pub const PROJECT_TASKS_LIST: &str = "commit.project_tasks.list";
/// Create a project task.
pub const PROJECT_TASKS_CREATE: &str = "commit.project_tasks.create";
/// Update a project task.
pub const PROJECT_TASKS_UPDATE: &str = "commit.project_tasks.update";
/// Claim an unassigned project task.
pub const PROJECT_TASKS_CLAIM: &str = "commit.project_tasks.claim";
/// Delete a project task and its subtasks.
pub const PROJECT_TASKS_DELETE: &str = "commit.project_tasks.delete";
/// Add a blocker.
pub const PROJECT_BLOCKERS_CREATE: &str = "commit.project_blockers.create";
/// Add a milestone update.
pub const PROJECT_UPDATES_CREATE: &str = "commit.project_updates.create";
/// Complete a project.
pub const PROJECT_COMPLETION_CREATE: &str = "commit.project_completion.create";
/// Read the caller's email delivery settings.
pub const EMAIL_SETTINGS_READ: &str = "commit.email_settings.read";
/// Replace the caller's email delivery settings.
pub const EMAIL_SETTINGS_UPDATE: &str = "commit.email_settings.update";
/// File a bug report.
pub const REPORTS_CREATE: &str = "commit.reports.create";
/// Read who the caller is in Commit (`GET /me`).
pub const ME_READ: &str = "commit.me.read";
/// Read a Silicon's allow-list.
pub const ALLOWLIST_READ: &str = "commit.allowlist.read";
/// Change a Silicon's allow-list.
pub const ALLOWLIST_UPDATE: &str = "commit.allowlist.update";

/// Every scope Commit honours, for configuration validation and documentation.
pub const ALL: &[&str] = &[
    NOTIFICATION_SETTINGS_READ,
    NOTIFICATION_SETTINGS_UPDATE,
    TODOS_LIST,
    TODOS_CREATE,
    TODOS_READ,
    TODOS_UPDATE,
    TODOS_DELETE,
    TODO_NOTES_LIST,
    TODO_NOTES_CREATE,
    TODO_SUBSCRIPTION_READ,
    TODO_SUBSCRIPTION_UPDATE,
    PROJECTS_LIST,
    PROJECTS_CREATE,
    PROJECTS_READ,
    PROJECTS_UPDATE,
    DIARY_READ,
    DIARY_UPDATE,
    PROJECT_TASKS_LIST,
    PROJECT_TASKS_CREATE,
    PROJECT_TASKS_UPDATE,
    PROJECT_TASKS_CLAIM,
    PROJECT_TASKS_DELETE,
    PROJECT_BLOCKERS_CREATE,
    PROJECT_UPDATES_CREATE,
    PROJECT_COMPLETION_CREATE,
    EMAIL_SETTINGS_READ,
    EMAIL_SETTINGS_UPDATE,
    REPORTS_CREATE,
    ME_READ,
    ALLOWLIST_READ,
    ALLOWLIST_UPDATE,
];

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::ALL;

    #[test]
    fn scopes_are_unique_and_namespaced() {
        let unique = ALL.iter().collect::<HashSet<_>>();
        assert_eq!(unique.len(), ALL.len());
        assert!(ALL.iter().all(|scope| scope.starts_with("commit.")));
    }
}
