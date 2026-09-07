//! Direct Silicon webhook publication adapter.

use std::time::Duration;

use async_trait::async_trait;
use http::{HeaderValue, StatusCode, header};
use serde::Serialize;
use serde_json::Value;
use url::Url;
use uuid::Uuid;

use crate::application::ports::{WebhookEvent, WebhookPublishError, WebhookPublisher};

use super::{BoundedBodyError, ClientBuildError, http_client, read_bounded, retry_after};

/// Redirect-free client for direct webhook delivery.
#[derive(Clone, Debug)]
pub struct WebhookClient {
    client: reqwest::Client,
    max_response_bytes: usize,
}

impl WebhookClient {
    /// Builds a direct webhook publisher.
    ///
    /// Destination URLs are taken from each immutable outbox routing snapshot.
    /// No intermediary service URL or credential is required.
    pub fn new(
        connect_timeout: Duration,
        request_timeout: Duration,
        max_response_bytes: usize,
    ) -> Result<Self, ClientBuildError> {
        if max_response_bytes == 0 {
            return Err(ClientBuildError::InvalidEndpoint);
        }
        Ok(Self {
            client: http_client(connect_timeout, request_timeout)?,
            max_response_bytes,
        })
    }
}

#[async_trait]
impl WebhookPublisher for WebhookClient {
    async fn publish(&self, event: &WebhookEvent) -> Result<(), WebhookPublishError> {
        validate_event(event)?;
        let publish_url = event
            .routing_snapshot
            .as_ref()
            .map(|snapshot| snapshot.webhook_url().as_str())
            .ok_or(WebhookPublishError::Unavailable)
            .and_then(|url| Url::parse(url).map_err(|_| WebhookPublishError::Rejected))?;
        let body = InternalWebhookEvent::from(event);
        let serialized = serde_json::to_vec(&body).map_err(|_| WebhookPublishError::Rejected)?;
        let response = self
            .client
            .post(publish_url)
            .header(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            )
            .header("idempotency-key", event.event_id.hyphenated().to_string())
            .body(serialized)
            .send()
            .await
            .map_err(|_| WebhookPublishError::Unavailable)?;

        let status = response.status();
        let headers = response.headers().clone();
        let _response_body = read_bounded(response, self.max_response_bytes)
            .await
            .map_err(|error| match error {
                BoundedBodyError::Transport => WebhookPublishError::Unavailable,
                BoundedBodyError::TooLarge => WebhookPublishError::InvalidResponse,
            })?;

        if status.is_success() {
            return Ok(());
        }

        match status {
            StatusCode::BAD_REQUEST
            | StatusCode::UNAUTHORIZED
            | StatusCode::FORBIDDEN
            | StatusCode::NOT_FOUND
            | StatusCode::PAYLOAD_TOO_LARGE
            | StatusCode::UNPROCESSABLE_ENTITY => Err(WebhookPublishError::Rejected),
            StatusCode::TOO_MANY_REQUESTS => Err(WebhookPublishError::RateLimited {
                retry_after: retry_after(&headers),
            }),
            _ => Err(WebhookPublishError::Unavailable),
        }
    }
}

#[derive(Serialize)]
struct InternalWebhookEvent<'a> {
    event_id: Uuid,
    org_id: &'a str,
    silicon_id: &'a str,
    #[serde(rename = "type")]
    event_type: &'a str,
    source: &'static str,
    payload_version: u16,
    #[serde(with = "time::serde::rfc3339")]
    occurred_at: time::OffsetDateTime,
    #[serde(skip_serializing_if = "Option::is_none")]
    trace_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    webhook_url: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    destination_version: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    subscription_level: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    subscription_scope: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    subscription_version: Option<i64>,
    payload: &'a Value,
}

impl<'a> From<&'a WebhookEvent> for InternalWebhookEvent<'a> {
    fn from(event: &'a WebhookEvent) -> Self {
        let routing = event.routing_snapshot.as_ref();
        Self {
            event_id: event.event_id,
            org_id: event.org_id.as_str(),
            silicon_id: event.silicon_id.as_str(),
            event_type: &event.event_type,
            source: "silicon-commit",
            payload_version: event.payload_version,
            occurred_at: event.occurred_at,
            trace_id: event.trace_id.as_deref(),
            webhook_url: routing.map(|snapshot| snapshot.webhook_url().as_str()),
            destination_version: routing.map(|snapshot| snapshot.destination_version().get()),
            subscription_level: routing.map(|snapshot| snapshot.subscription_level().as_str()),
            subscription_scope: routing.map(|snapshot| snapshot.subscription_scope().as_str()),
            subscription_version: routing.map(|snapshot| snapshot.subscription_version().get()),
            payload: &event.payload,
        }
    }
}

fn validate_event(event: &WebhookEvent) -> Result<(), WebhookPublishError> {
    let valid_type = event
        .event_type
        .strip_prefix("todo.")
        .is_some_and(|suffix| {
            !suffix.is_empty()
                && event.event_type.len() <= 128
                && suffix
                    .bytes()
                    .next()
                    .is_some_and(|byte| byte.is_ascii_lowercase())
                && suffix.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'.' | b'-' | b'_')
                })
        });
    let valid_trace = event.trace_id.as_deref().is_none_or(|trace_id| {
        (1..=128).contains(&trace_id.len()) && !trace_id.chars().any(char::is_control)
    });
    let valid_routing = matches!(
        (event.payload_version, event.routing_snapshot.as_ref()),
        (1, None) | (2.., Some(_))
    );
    if event.event_id.is_nil()
        || event.payload_version == 0
        || !event.payload.is_object()
        || !valid_type
        || !valid_trace
        || !valid_routing
    {
        return Err(WebhookPublishError::Rejected);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use time::OffsetDateTime;
    use uuid::Uuid;

    use super::{InternalWebhookEvent, validate_event};
    use crate::{
        application::ports::{WebhookEvent, WebhookRoutingSnapshot},
        domain::{
            ActorId, NotificationScope, NotificationSubscriptionLevel, NotificationVersion,
            PublicOrganizationId, WebhookUrl,
        },
    };

    #[test]
    fn rejects_non_object_payloads_before_network_io() {
        let org_id = PublicOrganizationId::new("tos");
        let silicon_id = ActorId::new("silicon-one");
        assert!(org_id.is_ok());
        assert!(silicon_id.is_ok());
        let (Ok(org_id), Ok(silicon_id)) = (org_id, silicon_id) else {
            return;
        };
        let event = WebhookEvent {
            event_id: Uuid::now_v7(),
            org_id,
            silicon_id,
            event_type: "todo.updated".to_owned(),
            payload_version: 1,
            occurred_at: OffsetDateTime::now_utc(),
            trace_id: None,
            routing_snapshot: None,
            payload: json!(["not", "an", "object"]),
        };
        assert!(validate_event(&event).is_err());
    }

    #[test]
    fn serializes_the_snapshotted_destination_for_webhook_dispatch() {
        let org_id = PublicOrganizationId::new("tos");
        let silicon_id = ActorId::new("silicon-one");
        let (Ok(org_id), Ok(silicon_id)) = (org_id, silicon_id) else {
            return;
        };
        let webhook_url = WebhookUrl::new(
            "https://hook.example.com/silicon/silicon-one/A1B2C3",
            &silicon_id,
        );
        let (Ok(webhook_url), Ok(destination_version), Ok(subscription_version)) = (
            webhook_url,
            NotificationVersion::new(3),
            NotificationVersion::new(5),
        ) else {
            return;
        };
        let routing_snapshot = WebhookRoutingSnapshot::new(
            webhook_url,
            destination_version,
            NotificationSubscriptionLevel::Todo,
            NotificationScope::StatusUpdates,
            subscription_version,
        );
        let Ok(routing_snapshot) = routing_snapshot else {
            return;
        };
        let event_id = Uuid::now_v7();
        let event = WebhookEvent {
            event_id,
            org_id,
            silicon_id,
            event_type: "todo.status_changed".to_owned(),
            payload_version: 2,
            occurred_at: OffsetDateTime::UNIX_EPOCH,
            trace_id: Some("trace-1".to_owned()),
            routing_snapshot: Some(routing_snapshot),
            payload: json!({ "status": "completed" }),
        };

        assert!(validate_event(&event).is_ok());
        let serialized = serde_json::to_value(InternalWebhookEvent::from(&event));
        assert!(serialized.is_ok());
        let Some(serialized) = serialized.ok() else {
            return;
        };
        assert_eq!(serialized["event_id"], event_id.to_string());
        assert_eq!(serialized["trace_id"], "trace-1");
        assert_eq!(
            serialized["webhook_url"],
            "https://hook.example.com/silicon/silicon-one/A1B2C3"
        );
        assert_eq!(serialized["destination_version"], 3);
        assert_eq!(serialized["subscription_level"], "todo");
        assert_eq!(serialized["subscription_scope"], "status_updates");
        assert_eq!(serialized["subscription_version"], 5);
    }

    #[test]
    fn rejects_new_payloads_without_a_routing_snapshot() {
        let org_id = PublicOrganizationId::new("tos");
        let silicon_id = ActorId::new("silicon-one");
        let (Ok(org_id), Ok(silicon_id)) = (org_id, silicon_id) else {
            return;
        };
        let event = WebhookEvent {
            event_id: Uuid::now_v7(),
            org_id,
            silicon_id,
            event_type: "todo.updated".to_owned(),
            payload_version: 2,
            occurred_at: OffsetDateTime::UNIX_EPOCH,
            trace_id: None,
            routing_snapshot: None,
            payload: json!({}),
        };

        assert!(validate_event(&event).is_err());
    }
}
