//! Silicon-owned notification settings and per-todo subscription workflows.

use std::{borrow::Cow, time::Duration};

use serde_json::json;
use sqlx::PgPool;

use crate::{
    application::ports::VerifiedActor,
    domain::{
        ExpectedNotificationVersion, NotificationSettings, NotificationSettingsUpdate,
        NotificationVersion, TodoId, TodoNotificationSubscription,
        TodoNotificationSubscriptionUpdate,
    },
    error::AppError,
    infrastructure::postgres::notifications as store,
};

/// Self-scoped notification configuration use cases for authenticated Silicons.
#[derive(Clone)]
pub struct NotificationSettingsService {
    pool: PgPool,
    audit_retention: Duration,
}

impl NotificationSettingsService {
    /// Creates the notification settings service.
    #[must_use]
    pub const fn new(pool: PgPool, audit_retention: Duration) -> Self {
        Self {
            pool,
            audit_retention,
        }
    }

    /// Reads the caller's Silicon-level settings, returning virtual version zero when absent.
    pub async fn get_settings(
        &self,
        actor: &VerifiedActor,
    ) -> Result<NotificationSettings, AppError> {
        authorize_silicon(actor)?;
        Ok(store::get_settings(&self.pool, actor)
            .await?
            .unwrap_or_else(NotificationSettings::empty))
    }

    /// Replaces the caller's complete Silicon-level settings under optimistic concurrency.
    pub async fn replace_settings(
        &self,
        actor: &VerifiedActor,
        request: NotificationSettingsUpdate,
        expected_version: ExpectedNotificationVersion,
        request_id: &str,
    ) -> Result<NotificationSettings, AppError> {
        authorize_silicon(actor)?;
        validate_request_id(request_id)?;
        let desired = request.validate(&actor.actor.id).map_err(AppError::from)?;

        let mut transaction = self.pool.begin().await?;
        crate::infrastructure::postgres::testing::guard(&mut transaction).await?;
        store::lock_actor_notifications(
            transaction.as_mut(),
            actor.organization_id,
            actor.actor.principal_id,
        )
        .await?;
        let current = store::lock_settings(transaction.as_mut(), actor).await?;

        let settings = if let Some(current) = current {
            match replacement_decision(
                expected_version,
                current.version,
                current.configuration_eq(&desired),
            ) {
                ReplacementDecision::ReturnCurrent => current,
                ReplacementDecision::Conflict => return Err(version_conflict()),
                ReplacementDecision::Replace => {
                    store::upsert_verified_actor(transaction.as_mut(), actor).await?;
                    let from_version = current.version;
                    let changed_fields = changed_settings_fields(&current, &desired);
                    let updated = store::update_settings(
                        transaction.as_mut(),
                        actor,
                        current.version,
                        &desired,
                    )
                    .await?
                    .ok_or_else(version_conflict)?;
                    store::insert_audit_event(
                        transaction.as_mut(),
                        actor,
                        "notification_settings.updated",
                        "notification_settings",
                        actor.actor.principal_id.into_uuid(),
                        request_id,
                        &json!({
                            "changed_fields": changed_fields,
                            "from_version": from_version.get(),
                            "to_version": updated.version.get(),
                            "webhook_configured": updated.webhook_url.is_some(),
                            "todo_list_subscribed": updated.todo_list_subscription.is_some(),
                        }),
                        self.audit_retention,
                    )
                    .await?;
                    updated
                }
            }
        } else {
            if expected_version.get() != 0 {
                return Err(version_conflict());
            }
            store::upsert_verified_actor(transaction.as_mut(), actor).await?;
            let inserted = store::insert_settings(transaction.as_mut(), actor, &desired).await?;
            store::insert_audit_event(
                transaction.as_mut(),
                actor,
                "notification_settings.created",
                "notification_settings",
                actor.actor.principal_id.into_uuid(),
                request_id,
                &json!({
                    "changed_fields": ["webhook_url", "todo_list_subscription"],
                    "from_version": 0,
                    "to_version": inserted.version.get(),
                    "webhook_configured": inserted.webhook_url.is_some(),
                    "todo_list_subscribed": inserted.todo_list_subscription.is_some(),
                }),
                self.audit_retention,
            )
            .await?;
            inserted
        };

        transaction.commit().await?;
        Ok(settings)
    }

    /// Reads one delegated todo's override, returning virtual version zero when absent.
    pub async fn get_todo_subscription(
        &self,
        actor: &VerifiedActor,
        todo_id: TodoId,
    ) -> Result<TodoNotificationSubscription, AppError> {
        let mut connection = self.pool.acquire().await?;
        super::todos::authorize_related_project(&mut connection, actor, todo_id, false).await?;
        drop(connection);
        authorize_silicon(actor)?;
        let target =
            store::get_todo_notification_target(&self.pool, actor.organization_id, todo_id)
                .await?
                .ok_or(AppError::NotFound)?;
        authorize_todo_target(actor, target)?;

        Ok(store::get_todo_subscription(&self.pool, actor, todo_id)
            .await?
            .unwrap_or_else(|| TodoNotificationSubscription::empty(todo_id)))
    }

    /// Replaces one delegated todo's override under optimistic concurrency.
    ///
    /// The todo is locked before the actor notification lock. Todo mutation
    /// producers use the same order, preventing a settings/delivery deadlock.
    pub async fn replace_todo_subscription(
        &self,
        actor: &VerifiedActor,
        todo_id: TodoId,
        request: TodoNotificationSubscriptionUpdate,
        expected_version: ExpectedNotificationVersion,
        request_id: &str,
    ) -> Result<TodoNotificationSubscription, AppError> {
        authorize_silicon(actor)?;
        validate_request_id(request_id)?;
        let desired = request.validate().map_err(AppError::from)?;

        let mut transaction = self.pool.begin().await?;
        crate::infrastructure::postgres::testing::guard(&mut transaction).await?;
        super::todos::authorize_related_project(&mut transaction, actor, todo_id, true).await?;
        let target = store::lock_todo_notification_target(
            transaction.as_mut(),
            actor.organization_id,
            todo_id,
        )
        .await?
        .ok_or(AppError::NotFound)?;
        authorize_todo_target(actor, target)?;
        store::lock_actor_notifications(
            transaction.as_mut(),
            actor.organization_id,
            actor.actor.principal_id,
        )
        .await?;
        let current = store::lock_todo_subscription(transaction.as_mut(), actor, todo_id).await?;

        let subscription = if let Some(current) = current {
            match replacement_decision(
                expected_version,
                current.version,
                current.configuration_eq(&desired),
            ) {
                ReplacementDecision::ReturnCurrent => current,
                ReplacementDecision::Conflict => return Err(version_conflict()),
                ReplacementDecision::Replace => {
                    store::upsert_verified_actor(transaction.as_mut(), actor).await?;
                    let from_version = current.version;
                    let updated = store::update_todo_subscription(
                        transaction.as_mut(),
                        actor,
                        todo_id,
                        current.version,
                        &desired,
                    )
                    .await?
                    .ok_or_else(version_conflict)?;
                    store::insert_audit_event(
                        transaction.as_mut(),
                        actor,
                        "todo_notification_subscription.updated",
                        "todo_notification_subscription",
                        todo_id.into_uuid(),
                        request_id,
                        &json!({
                            "changed_fields": ["subscription"],
                            "from_version": from_version.get(),
                            "to_version": updated.version.get(),
                            "subscribed": updated.subscription.is_some(),
                        }),
                        self.audit_retention,
                    )
                    .await?;
                    updated
                }
            }
        } else {
            if expected_version.get() != 0 {
                return Err(version_conflict());
            }
            store::upsert_verified_actor(transaction.as_mut(), actor).await?;
            let inserted =
                store::insert_todo_subscription(transaction.as_mut(), actor, todo_id, &desired)
                    .await?;
            store::insert_audit_event(
                transaction.as_mut(),
                actor,
                "todo_notification_subscription.created",
                "todo_notification_subscription",
                todo_id.into_uuid(),
                request_id,
                &json!({
                    "changed_fields": ["subscription"],
                    "from_version": 0,
                    "to_version": inserted.version.get(),
                    "subscribed": inserted.subscription.is_some(),
                }),
                self.audit_retention,
            )
            .await?;
            inserted
        };

        transaction.commit().await?;
        Ok(subscription)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReplacementDecision {
    ReturnCurrent,
    Replace,
    Conflict,
}

fn replacement_decision(
    expected: ExpectedNotificationVersion,
    current: NotificationVersion,
    desired_matches: bool,
) -> ReplacementDecision {
    if expected.matches(current) {
        if desired_matches {
            ReplacementDecision::ReturnCurrent
        } else {
            ReplacementDecision::Replace
        }
    } else if desired_matches
        && i64::try_from(expected.get()).is_ok_and(|expected| expected < current.get())
    {
        ReplacementDecision::ReturnCurrent
    } else {
        ReplacementDecision::Conflict
    }
}

fn changed_settings_fields(
    current: &NotificationSettings,
    desired: &crate::domain::ValidatedNotificationSettingsUpdate,
) -> Vec<&'static str> {
    let mut fields = Vec::with_capacity(2);
    if current.webhook_url != desired.webhook_url {
        fields.push("webhook_url");
    }
    if current.todo_list_subscription != desired.todo_list_subscription {
        fields.push("todo_list_subscription");
    }
    fields
}

fn authorize_silicon(actor: &VerifiedActor) -> Result<(), AppError> {
    if actor.actor.is_silicon() {
        Ok(())
    } else {
        Err(AppError::Forbidden)
    }
}

fn authorize_todo_target(
    actor: &VerifiedActor,
    target: store::TodoNotificationTarget,
) -> Result<(), AppError> {
    if target.assigned_by_principal_id == actor.actor.principal_id
        && target.assigned_to_principal_id != target.assigned_by_principal_id
    {
        Ok(())
    } else {
        Err(AppError::Forbidden)
    }
}

fn validate_request_id(request_id: &str) -> Result<(), AppError> {
    if request_id.is_empty()
        || request_id.len() > 255
        || request_id.trim() != request_id
        || request_id.chars().any(char::is_control)
    {
        return Err(AppError::BadRequest {
            code: Cow::Borrowed("invalid_request_id"),
        });
    }
    Ok(())
}

fn version_conflict() -> AppError {
    AppError::Conflict {
        code: Cow::Borrowed("notification_settings_version_conflict"),
    }
}

#[cfg(test)]
mod tests {
    use super::{ReplacementDecision, replacement_decision, validate_request_id};
    use crate::domain::{ExpectedNotificationVersion, NotificationVersion};

    fn versions(expected: u64, current: i64, desired_matches: bool) -> Option<ReplacementDecision> {
        Some(replacement_decision(
            ExpectedNotificationVersion::new(expected).ok()?,
            NotificationVersion::new(current).ok()?,
            desired_matches,
        ))
    }

    #[test]
    fn exact_current_version_replaces_only_changed_configuration() {
        assert_eq!(versions(3, 3, false), Some(ReplacementDecision::Replace));
        assert_eq!(
            versions(3, 3, true),
            Some(ReplacementDecision::ReturnCurrent)
        );
    }

    #[test]
    fn stale_exact_retry_is_idempotent_but_other_mismatches_conflict() {
        assert_eq!(
            versions(2, 3, true),
            Some(ReplacementDecision::ReturnCurrent)
        );
        assert_eq!(versions(2, 3, false), Some(ReplacementDecision::Conflict));
        assert_eq!(versions(4, 3, true), Some(ReplacementDecision::Conflict));
    }

    #[test]
    fn request_ids_match_the_durable_audit_contract() {
        assert!(validate_request_id("request-123").is_ok());
        assert!(validate_request_id("").is_err());
        assert!(validate_request_id(" leading").is_err());
        assert!(validate_request_id("line\nbreak").is_err());
    }
}
