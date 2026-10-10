//! `POST /webhook/`: Silicon Accounts app-webhook deliveries.
//!
//! The signature is verified over the raw body bytes (HMAC-SHA256 with the
//! `whsec_…` secret, 5-minute timestamp tolerance), the event is deduplicated
//! on `event_id`, applied, and answered with 2xx. Bad signatures answer 401,
//! bodies that are not Accounts events 400; event types Commit does not use
//! are recorded and answered with 2xx.

use axum::{
    Json,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use secrecy::ExposeSecret as _;
use serde_json::json;
use sha2::{Digest as _, Sha256};
use silicon_accounts_client::{
    DEFAULT_WEBHOOK_TOLERANCE, SIGNATURE_HEADER, TIMESTAMP_HEADER, WebhookError, WebhookPayload,
    verify_and_parse_webhook,
};

use super::AppState;
use crate::{application::accounts::AccountEvent, error::AppError};

pub(crate) async fn receive(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let Some(secret) = state.webhook_secret.as_ref() else {
        return Err(AppError::ProviderUnavailable);
    };
    let timestamp = header_text(&headers, TIMESTAMP_HEADER);
    let signature = header_text(&headers, SIGNATURE_HEADER);
    let event = match verify_and_parse_webhook(
        secret.expose_secret(),
        &timestamp,
        &signature,
        &body,
        DEFAULT_WEBHOOK_TOLERANCE,
    ) {
        Ok(event) => event,
        Err(WebhookError::InvalidBody(reason)) => {
            return Err(AppError::Invalid {
                code: "invalid_webhook_body".into(),
                message: format!("The body is not a Silicon Accounts event: {reason}"),
            });
        }
        Err(error) => {
            tracing::warn!(%error, "refused an Accounts webhook delivery");
            return Err(AppError::Authentication {
                code: "invalid_webhook_signature".into(),
                message: error.to_string(),
            });
        }
    };
    let account_event = AccountEvent::from(&event.payload);
    let digest = hex::encode(Sha256::digest(&body));
    let outcome = state
        .accounts
        .apply_webhook(
            &event.event_id,
            &event.event_type,
            event.occurred_at,
            &account_event,
            &digest,
        )
        .await?;
    Ok((
        StatusCode::OK,
        Json(json!({
            "event_id": event.event_id,
            "applied": outcome.applied,
            "outcome": outcome.outcome,
        })),
    )
        .into_response())
}

fn header_text(headers: &HeaderMap, name: &str) -> String {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned()
}

impl From<&WebhookPayload> for AccountEvent {
    fn from(payload: &WebhookPayload) -> Self {
        match payload {
            WebhookPayload::AccountIdChanged(change) => Self::IdChanged {
                uuid: change.uuid.clone(),
                new_id: change.new_id.clone(),
            },
            WebhookPayload::AccountUpdated(update) => match &update.account {
                Some(account) => Self::Updated {
                    uuid: update.uuid.clone(),
                    version: account.version,
                    account: serde_json::to_value(account).unwrap_or_default(),
                },
                None => Self::Unknown,
            },
            WebhookPayload::AccountDeleted(gone) => Self::Deleted {
                uuid: gone.uuid.clone(),
            },
            WebhookPayload::MembershipSignedOut(signed_out) => Self::SignedOut {
                uuid: signed_out.uuid.clone(),
                reason: signed_out.reason.clone(),
            },
            WebhookPayload::MembershipAccessRemoved(removed) => Self::AccessRemoved {
                uuid: removed.uuid.clone(),
            },
            WebhookPayload::CustodianChanged(change) => Self::CustodianChanged {
                silicon: change.uuid.clone(),
                to: change
                    .to
                    .as_ref()
                    .map(|to| (to.uuid.clone(), to.id.clone())),
            },
            WebhookPayload::Ping => Self::Ping,
            _ => Self::Unknown,
        }
    }
}
