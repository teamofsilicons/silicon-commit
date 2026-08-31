//! Authenticated internal Silicon Hook publication adapter.

use std::time::Duration;

use async_trait::async_trait;
use http::{HeaderValue, StatusCode, header};
use secrecy::{ExposeSecret as _, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;
use uuid::Uuid;

use crate::{
    application::ports::{HookEvent, HookPublishError, HookPublisher},
    config::HookSettings,
};

use super::{
    BoundedBodyError, ClientBuildError, exact_endpoint, http_client, is_json, read_bounded,
    retry_after,
};

/// Redirect-free client for Hook's single service-authenticated ingress.
#[derive(Clone, Debug)]
pub struct HookClient {
    client: reqwest::Client,
    publish_url: Option<Url>,
    authorization: Option<HeaderValue>,
    max_response_bytes: usize,
}

impl HookClient {
    /// Builds an internal Hook publisher.
    ///
    /// A development configuration may omit both URL and service credential;
    /// publication then fails as retryable rather than pretending delivery.
    ///
    /// # Errors
    ///
    /// Returns a redacted construction error for a partial configuration,
    /// unsafe URL, malformed credential, or HTTP client failure.
    pub fn new(
        settings: &HookSettings,
        connect_timeout: Duration,
        request_timeout: Duration,
        max_response_bytes: usize,
    ) -> Result<Self, ClientBuildError> {
        if max_response_bytes == 0 {
            return Err(ClientBuildError::InvalidEndpoint);
        }
        let (publish_url, authorization) = match (&settings.publish_url, &settings.service_token) {
            (Some(url), Some(token)) => (
                Some(exact_endpoint(url)?),
                Some(service_authorization(token)?),
            ),
            (None, None) => (None, None),
            _ => return Err(ClientBuildError::InvalidCredential),
        };
        Ok(Self {
            client: http_client(connect_timeout, request_timeout)?,
            publish_url,
            authorization,
            max_response_bytes,
        })
    }
}

#[async_trait]
impl HookPublisher for HookClient {
    async fn publish(&self, event: &HookEvent) -> Result<(), HookPublishError> {
        validate_event(event)?;
        let publish_url = self
            .publish_url
            .clone()
            .ok_or(HookPublishError::Unavailable)?;
        let authorization = self
            .authorization
            .clone()
            .ok_or(HookPublishError::Unavailable)?;
        let body = InternalHookEvent::from(event);
        let serialized = serde_json::to_vec(&body).map_err(|_| HookPublishError::Rejected)?;
        let response = self
            .client
            .post(publish_url)
            .header(header::AUTHORIZATION, authorization)
            .header(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            )
            .header("idempotency-key", event.event_id.hyphenated().to_string())
            .body(serialized)
            .send()
            .await
            .map_err(|_| HookPublishError::Unavailable)?;

        let status = response.status();
        let headers = response.headers().clone();
        let response_is_json = is_json(&headers);
        let response_body = read_bounded(response, self.max_response_bytes)
            .await
            .map_err(|error| match error {
                BoundedBodyError::Transport => HookPublishError::Unavailable,
                BoundedBodyError::TooLarge => HookPublishError::InvalidResponse,
            })?;

        if status == StatusCode::ACCEPTED {
            if response_body.is_empty() {
                return Ok(());
            }
            if !response_is_json {
                return Err(HookPublishError::InvalidResponse);
            }
            let acceptance: Acceptance = serde_json::from_slice(&response_body)
                .map_err(|_| HookPublishError::InvalidResponse)?;
            if acceptance.event_id != event.event_id || acceptance.status != "accepted" {
                return Err(HookPublishError::InvalidResponse);
            }
            return Ok(());
        }

        match status {
            StatusCode::BAD_REQUEST
            | StatusCode::PAYLOAD_TOO_LARGE
            | StatusCode::UNPROCESSABLE_ENTITY => Err(HookPublishError::Rejected),
            StatusCode::TOO_MANY_REQUESTS => Err(HookPublishError::RateLimited {
                retry_after: retry_after(&headers),
            }),
            _ => Err(HookPublishError::Unavailable),
        }
    }
}

#[derive(Debug, Serialize)]
struct InternalHookEvent<'a> {
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
    payload: &'a Value,
}

impl<'a> From<&'a HookEvent> for InternalHookEvent<'a> {
    fn from(event: &'a HookEvent) -> Self {
        Self {
            event_id: event.event_id,
            org_id: event.org_id.as_str(),
            silicon_id: event.silicon_id.as_str(),
            event_type: &event.event_type,
            source: "silicon-commit",
            payload_version: event.payload_version,
            occurred_at: event.occurred_at,
            trace_id: event.trace_id.as_deref(),
            payload: &event.payload,
        }
    }
}

#[derive(Debug, Deserialize)]
struct Acceptance {
    event_id: Uuid,
    status: String,
}

fn validate_event(event: &HookEvent) -> Result<(), HookPublishError> {
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
    if event.event_id.is_nil()
        || event.payload_version == 0
        || !event.payload.is_object()
        || !valid_type
        || !valid_trace
    {
        return Err(HookPublishError::Rejected);
    }
    Ok(())
}

fn service_authorization(token: &SecretString) -> Result<HeaderValue, ClientBuildError> {
    let token = token.expose_secret();
    if token.is_empty() || token.bytes().any(|byte| byte.is_ascii_whitespace()) {
        return Err(ClientBuildError::InvalidCredential);
    }
    let mut authorization = HeaderValue::from_str(&format!("Bearer {token}"))
        .map_err(|_| ClientBuildError::InvalidCredential)?;
    authorization.set_sensitive(true);
    Ok(authorization)
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use time::OffsetDateTime;
    use uuid::Uuid;

    use super::validate_event;
    use crate::{
        application::ports::HookEvent,
        domain::ids::{ActorId, PublicOrganizationId},
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
        let event = HookEvent {
            event_id: Uuid::now_v7(),
            org_id,
            silicon_id,
            event_type: "todo.updated".to_owned(),
            payload_version: 1,
            occurred_at: OffsetDateTime::now_utc(),
            trace_id: None,
            payload: json!(["not", "an", "object"]),
        };
        assert!(validate_event(&event).is_err());
    }
}
