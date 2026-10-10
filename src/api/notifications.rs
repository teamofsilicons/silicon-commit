//! Silicon notification-settings HTTP handlers (the Silicon itself or its custodian).

use axum::{
    Json,
    extract::State,
    http::{HeaderMap, HeaderValue, header},
    response::{IntoResponse, Response},
};
use serde::Serialize;

use crate::{
    domain::{
        ActorId, NotificationSettingsUpdate, NotificationVersion, TodoId,
        TodoNotificationSubscriptionUpdate,
    },
    error::AppError,
};

use super::{
    AppState,
    auth::action,
    extract::{NotificationIfMatch, StrictJson, StrictPath, StrictQuery},
    required_request_id,
};

/// Which Silicon's settings: the caller's own (omit it) or, for a custodian, one of its Silicons.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SiliconQuery {
    silicon: Option<String>,
}

impl SiliconQuery {
    fn selector(&self) -> Result<Option<ActorId>, AppError> {
        self.silicon
            .as_deref()
            .map(|value| {
                ActorId::new(value).map_err(|error| AppError::Validation {
                    details: serde_json::json!({ "silicon": error.to_string() }),
                })
            })
            .transpose()
    }
}

/// `GET /api/v1/notification-settings[?silicon=si:…]`.
pub(crate) async fn get_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    StrictQuery(query): StrictQuery<SiliconQuery>,
) -> Result<Response, AppError> {
    let actor = state
        .authenticate(&headers, action::NOTIFICATION_SETTINGS_READ, None)
        .await?;
    let silicon = query.selector()?;
    let settings = state
        .notifications
        .get_settings(&actor, silicon.as_ref())
        .await?;
    versioned_response(settings.version, settings)
}

/// `PUT /api/v1/notification-settings[?silicon=si:…]`.
pub(crate) async fn replace_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    StrictQuery(query): StrictQuery<SiliconQuery>,
    NotificationIfMatch(expected_version): NotificationIfMatch,
    StrictJson(input): StrictJson<NotificationSettingsUpdate>,
) -> Result<Response, AppError> {
    let actor = state
        .authenticate(&headers, action::NOTIFICATION_SETTINGS_UPDATE, None)
        .await?;
    let silicon = query.selector()?;
    let request_id = required_request_id()?;
    let settings = state
        .notifications
        .replace_settings(
            &actor,
            silicon.as_ref(),
            input,
            expected_version,
            &request_id,
        )
        .await?;
    versioned_response(settings.version, settings)
}

/// `GET /api/v1/todos/{todo_id}/notification-subscription`.
pub(crate) async fn get_todo_subscription(
    State(state): State<AppState>,
    headers: HeaderMap,
    StrictPath(todo_id): StrictPath<TodoId>,
) -> Result<Response, AppError> {
    let resource = todo_id.to_string();
    let actor = state
        .authenticate(&headers, action::TODO_SUBSCRIPTION_READ, Some(resource))
        .await?;
    let subscription = state
        .notifications
        .get_todo_subscription(&actor, todo_id)
        .await?;
    versioned_response(subscription.version, subscription)
}

/// `PUT /api/v1/todos/{todo_id}/notification-subscription`.
pub(crate) async fn replace_todo_subscription(
    State(state): State<AppState>,
    headers: HeaderMap,
    StrictPath(todo_id): StrictPath<TodoId>,
    NotificationIfMatch(expected_version): NotificationIfMatch,
    StrictJson(input): StrictJson<TodoNotificationSubscriptionUpdate>,
) -> Result<Response, AppError> {
    let resource = todo_id.to_string();
    let actor = state
        .authenticate(&headers, action::TODO_SUBSCRIPTION_UPDATE, Some(resource))
        .await?;
    let request_id = required_request_id()?;
    let subscription = state
        .notifications
        .replace_todo_subscription(&actor, todo_id, input, expected_version, &request_id)
        .await?;
    versioned_response(subscription.version, subscription)
}

fn versioned_response<T>(version: NotificationVersion, value: T) -> Result<Response, AppError>
where
    T: Serialize,
{
    let etag = HeaderValue::from_str(&format!("\"{}\"", version.get())).map_err(|_| {
        AppError::Internal(anyhow::anyhow!(
            "notification version could not be represented as an ETag"
        ))
    })?;
    let mut response = Json(value).into_response();
    response.headers_mut().insert(header::ETAG, etag);
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::versioned_response;
    use crate::domain::{NotificationSettings, NotificationVersion, TodoNotificationSubscription};

    #[test]
    fn virtual_notification_resources_emit_the_zero_etag() {
        let version = NotificationVersion::new(0);
        assert!(version.is_ok());
        let Some(version) = version.ok() else {
            return;
        };
        let response = versioned_response(version, NotificationSettings::empty());
        assert!(response.is_ok());
        assert_eq!(
            response.ok().and_then(|response| {
                response
                    .headers()
                    .get(http::header::ETAG)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned)
            }),
            Some("\"0\"".to_owned())
        );
    }

    #[test]
    fn versioned_todo_resource_type_remains_serializable() {
        fn assert_serializable<T: serde::Serialize>() {}
        assert_serializable::<TodoNotificationSubscription>();
    }
}
