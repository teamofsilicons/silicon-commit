//! PostgreSQL persistence for versioned Silicon notification configuration.

use sqlx::{AssertSqlSafe, FromRow, PgConnection, PgPool};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{
    application::ports::{VerifiedActor, WebhookRoutingSnapshot},
    domain::{
        AccountUuid, Actor, ActorId, NotificationRule, NotificationScope, NotificationSettings,
        NotificationSubscriptionLevel, NotificationVersion, Todo, TodoId,
        TodoNotificationSubscription, TodoStatus, ValidatedNotificationSettingsUpdate,
        ValidatedTodoNotificationSubscriptionUpdate, WebhookUrl,
    },
    error::AppError,
    infrastructure::postgres::accounts::account_uuid,
};

/// Current assignment fields needed to authorize a per-todo subscription.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TodoNotificationTarget {
    /// The todo's owner: the Silicon whose subscription this is.
    pub(crate) assigned_by: Actor,
    pub(crate) assigned_to: AccountUuid,
}

/// Serializes notification configuration and delivery decisions for one Silicon.
///
/// Todo mutation callers must lock their todo row before acquiring this lock.
/// Per-todo configuration follows that same order; list-level configuration has
/// no todo dependency and acquires only this lock.
pub(crate) async fn lock_actor_notifications(
    connection: &mut PgConnection,
    silicon: &AccountUuid,
) -> Result<(), AppError> {
    sqlx::query(
        r"
        SELECT pg_advisory_xact_lock(
            hashtextextended(jsonb_build_array('commit.notifications', $1::text)::text, 0)
        )
        ",
    )
    .bind(silicon.as_str())
    .execute(connection)
    .await?;
    Ok(())
}

/// Selects and snapshots the effective rule for one already-locked delegated todo.
///
/// The caller must hold the todo row lock before entering this function. The
/// Silicon-scoped advisory lock serializes list settings with per-todo overrides,
/// so the returned endpoint and rule versions remain one atomic decision until
/// the caller commits its outbox insert.
pub(crate) async fn effective_routing_snapshot(
    connection: &mut PgConnection,
    todo: &Todo,
    resulting_status: Option<TodoStatus>,
) -> Result<Option<WebhookRoutingSnapshot>, AppError> {
    lock_actor_notifications(connection, &todo.assigned_by.uuid).await?;

    let row = sqlx::query_as::<_, EffectiveRoutingRow>(
        r"
        SELECT settings.webhook_url,
               settings.todo_list_scope,
               settings.todo_list_statuses,
               settings.version AS destination_version,
               subscription.scope AS todo_scope,
               subscription.statuses AS todo_statuses,
               subscription.version AS todo_version
          FROM commit.silicon_notification_settings AS settings
          LEFT JOIN commit.todo_notification_subscriptions AS subscription
            ON subscription.silicon_account = settings.silicon_account
           AND subscription.todo_id = $2
         WHERE settings.silicon_account = $1
        ",
    )
    .bind(todo.assigned_by.uuid.as_str())
    .bind(todo.id.into_uuid())
    .fetch_optional(connection)
    .await?;

    row.map(|row| row.into_snapshot(todo, resulting_status))
        .transpose()
        .map(Option::flatten)
}

/// Reads a Silicon's settings without locking.
pub(crate) async fn get_settings(
    pool: &PgPool,
    silicon: &Actor,
) -> Result<Option<NotificationSettings>, AppError> {
    let row = sqlx::query_as::<_, SettingsRow>(SETTINGS_SELECT)
        .bind(silicon.uuid.as_str())
        .fetch_optional(pool)
        .await?;
    row.map(|row| row.into_domain(&silicon.id)).transpose()
}

/// Reads and row-locks a Silicon's settings.
pub(crate) async fn lock_settings(
    connection: &mut PgConnection,
    silicon: &Actor,
) -> Result<Option<NotificationSettings>, AppError> {
    let row = sqlx::query_as::<_, SettingsRow>(AssertSqlSafe(format!(
        "{SETTINGS_SELECT} FOR UPDATE OF settings"
    )))
    .bind(silicon.uuid.as_str())
    .fetch_optional(connection)
    .await?;
    row.map(|row| row.into_domain(&silicon.id)).transpose()
}

/// Creates the first persisted Silicon-level settings resource at version one.
pub(crate) async fn insert_settings(
    connection: &mut PgConnection,
    silicon: &Actor,
    desired: &ValidatedNotificationSettingsUpdate,
) -> Result<NotificationSettings, AppError> {
    let (scope, statuses) = rule_columns(desired.todo_list_subscription.as_ref());
    let row = sqlx::query_as::<_, SettingsRow>(
        r"
        INSERT INTO commit.silicon_notification_settings (
            silicon_account, webhook_url, todo_list_scope, todo_list_statuses
        )
        VALUES ($1, $2, $3::commit.notification_scope, $4)
        RETURNING webhook_url, todo_list_scope, todo_list_statuses, version, updated_at
        ",
    )
    .bind(silicon.uuid.as_str())
    .bind(desired.webhook_url.as_ref().map(WebhookUrl::as_str))
    .bind(scope)
    .bind(statuses)
    .fetch_one(connection)
    .await?;
    row.into_domain(&silicon.id)
}

/// Replaces a locked Silicon-level resource and advances its version once.
pub(crate) async fn update_settings(
    connection: &mut PgConnection,
    silicon: &Actor,
    current_version: NotificationVersion,
    desired: &ValidatedNotificationSettingsUpdate,
) -> Result<Option<NotificationSettings>, AppError> {
    let (scope, statuses) = rule_columns(desired.todo_list_subscription.as_ref());
    let row = sqlx::query_as::<_, SettingsRow>(
        r"
        UPDATE commit.silicon_notification_settings
           SET webhook_url = $2,
               todo_list_scope = $3::commit.notification_scope,
               todo_list_statuses = $4
         WHERE silicon_account = $1
           AND version = $5
        RETURNING webhook_url, todo_list_scope, todo_list_statuses, version, updated_at
        ",
    )
    .bind(silicon.uuid.as_str())
    .bind(desired.webhook_url.as_ref().map(WebhookUrl::as_str))
    .bind(scope)
    .bind(statuses)
    .bind(current_version.get())
    .fetch_optional(connection)
    .await?;
    row.map(|row| row.into_domain(&silicon.id)).transpose()
}

/// Reads the active todo assignment needed by subscription authorization.
pub(crate) async fn get_todo_notification_target(
    pool: &PgPool,
    todo_id: TodoId,
) -> Result<Option<TodoNotificationTarget>, AppError> {
    let row = sqlx::query_as::<_, TodoTargetRow>(TODO_TARGET_SELECT)
        .bind(todo_id.into_uuid())
        .fetch_optional(pool)
        .await?;
    row.map(TodoTargetRow::into_domain).transpose()
}

/// Reads and row-locks the active todo before the Silicon notification lock.
pub(crate) async fn lock_todo_notification_target(
    connection: &mut PgConnection,
    todo_id: TodoId,
) -> Result<Option<TodoNotificationTarget>, AppError> {
    let row = sqlx::query_as::<_, TodoTargetRow>(AssertSqlSafe(format!(
        "{TODO_TARGET_SELECT} FOR UPDATE OF todo"
    )))
    .bind(todo_id.into_uuid())
    .fetch_optional(connection)
    .await?;
    row.map(TodoTargetRow::into_domain).transpose()
}

/// Reads one per-todo subscription resource without locking.
pub(crate) async fn get_todo_subscription(
    pool: &PgPool,
    silicon: &AccountUuid,
    todo_id: TodoId,
) -> Result<Option<TodoNotificationSubscription>, AppError> {
    let row = sqlx::query_as::<_, TodoSubscriptionRow>(TODO_SUBSCRIPTION_SELECT)
        .bind(silicon.as_str())
        .bind(todo_id.into_uuid())
        .fetch_optional(pool)
        .await?;
    row.map(TodoSubscriptionRow::into_domain).transpose()
}

/// Reads and row-locks one per-todo subscription resource.
pub(crate) async fn lock_todo_subscription(
    connection: &mut PgConnection,
    silicon: &AccountUuid,
    todo_id: TodoId,
) -> Result<Option<TodoNotificationSubscription>, AppError> {
    let row = sqlx::query_as::<_, TodoSubscriptionRow>(AssertSqlSafe(format!(
        "{TODO_SUBSCRIPTION_SELECT} FOR UPDATE OF subscription"
    )))
    .bind(silicon.as_str())
    .bind(todo_id.into_uuid())
    .fetch_optional(connection)
    .await?;
    row.map(TodoSubscriptionRow::into_domain).transpose()
}

/// Creates the first per-todo subscription resource at version one.
pub(crate) async fn insert_todo_subscription(
    connection: &mut PgConnection,
    silicon: &AccountUuid,
    todo_id: TodoId,
    desired: &ValidatedTodoNotificationSubscriptionUpdate,
) -> Result<TodoNotificationSubscription, AppError> {
    let (scope, statuses) = rule_columns(desired.subscription.as_ref());
    let row = sqlx::query_as::<_, TodoSubscriptionRow>(
        r"
        INSERT INTO commit.todo_notification_subscriptions (silicon_account, todo_id, scope, statuses)
        VALUES ($1, $2, $3::commit.notification_scope, $4)
        RETURNING todo_id, scope, statuses, version, updated_at
        ",
    )
    .bind(silicon.as_str())
    .bind(todo_id.into_uuid())
    .bind(scope)
    .bind(statuses)
    .fetch_one(connection)
    .await?;
    row.into_domain()
}

/// Replaces a locked per-todo subscription and advances its version once.
pub(crate) async fn update_todo_subscription(
    connection: &mut PgConnection,
    silicon: &AccountUuid,
    todo_id: TodoId,
    current_version: NotificationVersion,
    desired: &ValidatedTodoNotificationSubscriptionUpdate,
) -> Result<Option<TodoNotificationSubscription>, AppError> {
    let (scope, statuses) = rule_columns(desired.subscription.as_ref());
    let row = sqlx::query_as::<_, TodoSubscriptionRow>(
        r"
        UPDATE commit.todo_notification_subscriptions
           SET scope = $3::commit.notification_scope,
               statuses = $4
         WHERE silicon_account = $1
           AND todo_id = $2
           AND version = $5
        RETURNING todo_id, scope, statuses, version, updated_at
        ",
    )
    .bind(silicon.as_str())
    .bind(todo_id.into_uuid())
    .bind(scope)
    .bind(statuses)
    .bind(current_version.get())
    .fetch_optional(connection)
    .await?;
    row.map(TodoSubscriptionRow::into_domain).transpose()
}

/// Appends minimal non-secret notification configuration audit evidence.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn insert_audit_event(
    connection: &mut PgConnection,
    actor: &VerifiedActor,
    action: &'static str,
    resource_type: &'static str,
    resource_id: Uuid,
    request_id: &str,
    change_summary: &serde_json::Value,
    audit_retention: std::time::Duration,
) -> Result<(), AppError> {
    super::todos::insert_audit_event(
        connection,
        actor,
        action,
        resource_type,
        resource_id,
        request_id,
        change_summary,
        audit_retention,
    )
    .await
}

const SETTINGS_SELECT: &str = r"
    SELECT settings.webhook_url,
           settings.todo_list_scope,
           settings.todo_list_statuses,
           settings.version,
           settings.updated_at
      FROM commit.silicon_notification_settings AS settings
     WHERE settings.silicon_account = $1
";

const TODO_TARGET_SELECT: &str = r"
    SELECT owner.uuid AS assigned_by_uuid,
           owner.kind AS assigned_by_kind,
           owner.public_id AS assigned_by_id,
           todo.assigned_to_account
      FROM commit.todos AS todo
      JOIN commit.accounts AS owner ON owner.uuid = todo.assigned_by_account
     WHERE todo.id = $1
       AND todo.deleted_at IS NULL
";

const TODO_SUBSCRIPTION_SELECT: &str = r"
    SELECT subscription.todo_id,
           subscription.scope,
           subscription.statuses,
           subscription.version,
           subscription.updated_at
      FROM commit.todo_notification_subscriptions AS subscription
     WHERE subscription.silicon_account = $1
       AND subscription.todo_id = $2
";

#[derive(FromRow)]
struct SettingsRow {
    webhook_url: Option<String>,
    todo_list_scope: Option<NotificationScope>,
    todo_list_statuses: Vec<TodoStatus>,
    version: i64,
    updated_at: OffsetDateTime,
}

impl SettingsRow {
    fn into_domain(self, actor_id: &ActorId) -> Result<NotificationSettings, AppError> {
        let webhook_url = self
            .webhook_url
            .map(|value| WebhookUrl::from_persisted(value, actor_id))
            .transpose()
            .map_err(corrupt_persisted_data)?;
        let todo_list_subscription = persisted_rule(self.todo_list_scope, self.todo_list_statuses)?;
        let version = persisted_version(self.version)?;
        Ok(NotificationSettings {
            webhook_url,
            todo_list_subscription,
            version,
            updated_at: Some(self.updated_at),
        })
    }
}

#[derive(FromRow)]
struct TodoSubscriptionRow {
    todo_id: Uuid,
    scope: Option<NotificationScope>,
    statuses: Vec<TodoStatus>,
    version: i64,
    updated_at: OffsetDateTime,
}

impl TodoSubscriptionRow {
    fn into_domain(self) -> Result<TodoNotificationSubscription, AppError> {
        Ok(TodoNotificationSubscription {
            todo_id: TodoId::from_uuid(self.todo_id),
            subscription: persisted_rule(self.scope, self.statuses)?,
            version: persisted_version(self.version)?,
            updated_at: Some(self.updated_at),
        })
    }
}

#[derive(FromRow)]
struct TodoTargetRow {
    assigned_by_uuid: String,
    assigned_by_kind: crate::domain::ActorType,
    assigned_by_id: String,
    assigned_to_account: String,
}

#[derive(FromRow)]
struct EffectiveRoutingRow {
    webhook_url: Option<String>,
    todo_list_scope: Option<NotificationScope>,
    todo_list_statuses: Vec<TodoStatus>,
    destination_version: i64,
    todo_scope: Option<NotificationScope>,
    todo_statuses: Option<Vec<TodoStatus>>,
    todo_version: Option<i64>,
}

impl EffectiveRoutingRow {
    fn into_snapshot(
        self,
        todo: &Todo,
        resulting_status: Option<TodoStatus>,
    ) -> Result<Option<WebhookRoutingSnapshot>, AppError> {
        let Some(webhook_url) = self.webhook_url else {
            return Ok(None);
        };
        let webhook_url = WebhookUrl::from_persisted(webhook_url, &todo.assigned_by.id)
            .map_err(corrupt_persisted_data)?;
        let destination_version = persisted_version(self.destination_version)?;
        let list_rule = persisted_rule(self.todo_list_scope, self.todo_list_statuses)?;
        let todo_rule = persisted_rule(self.todo_scope, self.todo_statuses.unwrap_or_default())?;

        let (subscription_level, rule, subscription_version) = if let Some(rule) = todo_rule {
            let version = self.todo_version.ok_or_else(|| {
                corrupt_persisted_data("todo notification rule has no resource version")
            })?;
            (
                NotificationSubscriptionLevel::Todo,
                rule,
                persisted_version(version)?,
            )
        } else {
            let Some(rule) = list_rule else {
                return Ok(None);
            };
            (
                NotificationSubscriptionLevel::List,
                rule,
                destination_version,
            )
        };

        if !rule.matches_update(resulting_status) {
            return Ok(None);
        }

        WebhookRoutingSnapshot::new(
            webhook_url,
            destination_version,
            subscription_level,
            rule.scope,
            subscription_version,
        )
        .map(Some)
        .map_err(corrupt_persisted_data)
    }
}

impl TodoTargetRow {
    fn into_domain(self) -> Result<TodoNotificationTarget, AppError> {
        Ok(TodoNotificationTarget {
            assigned_by: Actor::new(
                account_uuid(self.assigned_by_uuid)?,
                self.assigned_by_kind,
                ActorId::from_persisted(self.assigned_by_id),
            ),
            assigned_to: account_uuid(self.assigned_to_account)?,
        })
    }
}

fn rule_columns(rule: Option<&NotificationRule>) -> (Option<NotificationScope>, Vec<TodoStatus>) {
    match rule {
        Some(rule) => (Some(rule.scope), rule.statuses().to_vec()),
        None => (None, Vec::new()),
    }
}

fn persisted_rule(
    scope: Option<NotificationScope>,
    statuses: Vec<TodoStatus>,
) -> Result<Option<NotificationRule>, AppError> {
    match scope {
        Some(scope) => NotificationRule::from_persisted(scope, statuses)
            .map(Some)
            .map_err(corrupt_persisted_data),
        None if statuses.is_empty() => Ok(None),
        None => Err(corrupt_persisted_data(
            "notification rule has statuses without a scope",
        )),
    }
}

fn persisted_version(value: i64) -> Result<NotificationVersion, AppError> {
    if value < 1 {
        return Err(corrupt_persisted_data(
            "persisted notification version is not positive",
        ));
    }
    NotificationVersion::new(value).map_err(corrupt_persisted_data)
}

fn corrupt_persisted_data(error: impl std::fmt::Display) -> AppError {
    AppError::Internal(anyhow::anyhow!(
        "invalid persisted notification data: {error}"
    ))
}

#[cfg(test)]
mod tests {
    use super::rule_columns;
    use crate::domain::{NotificationRule, NotificationScope, TodoStatus};

    #[test]
    fn absent_rules_map_to_a_null_scope_and_empty_status_array() {
        let (scope, statuses) = rule_columns(None);
        assert_eq!(scope, None);
        assert!(statuses.is_empty());
    }

    #[test]
    fn persisted_rules_round_trip_normalized_statuses() {
        let rule = NotificationRule::from_persisted(
            NotificationScope::SpecificStatuses,
            vec![TodoStatus::Blocked, TodoStatus::Completed],
        );
        assert!(rule.is_ok());
        let Some(rule) = rule.ok() else {
            return;
        };
        let (scope, statuses) = rule_columns(Some(&rule));
        assert_eq!(scope, Some(NotificationScope::SpecificStatuses));
        assert_eq!(statuses, [TodoStatus::Completed, TodoStatus::Blocked]);
    }
}
