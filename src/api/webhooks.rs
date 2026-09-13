//! Exact-byte IAM signature verification and durable event deduplication.

use super::AppState;
use crate::{config::IamSettings, error::AppError, infrastructure::clients::ClientBuildError};
use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
};
use secrecy::ExposeSecret as _;
use sha2::{Digest as _, Sha256};
use silicon_iam_client::webhook::{WebhookSecret, WebhookSecretKeyring, WebhookVerifier};
use uuid::Uuid;

/// Constructs a verifier for the explicitly configured IAM key version.
pub(crate) fn verifier(
    settings: &IamSettings,
) -> Result<Option<WebhookVerifier>, ClientBuildError> {
    settings
        .webhook_secret
        .as_ref()
        .map(|secret| {
            let secret = WebhookSecret::new(secret.expose_secret())
                .map_err(|_| ClientBuildError::InvalidCredential)?;
            let keys = WebhookSecretKeyring::new(settings.webhook_key_version, secret)
                .map_err(|_| ClientBuildError::InvalidCredential)?;
            Ok(WebhookVerifier::new(keys))
        })
        .transpose()
}

pub(crate) async fn receive(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, AppError> {
    let verifier = state
        .webhook_verifier
        .as_ref()
        .ok_or(AppError::ProviderUnavailable)?;
    let delivery = verifier
        .verify(&headers, &body)
        .map_err(|_| AppError::Unauthenticated)?;
    // A testing envelope must be routed and verified against its IAM test key;
    // it may never fall through into the production inbox.
    let environments: Vec<Option<Uuid>> = if delivery.is_testing() {
        let envelope: serde_json::Value =
            serde_json::from_slice(&body).map_err(|_| AppError::BadGateway)?;
        let key = envelope
            .pointer("/test/testing_key")
            .and_then(serde_json::Value::as_str)
            .ok_or(AppError::BadGateway)?;
        let ids = sqlx::query_scalar::<_,Uuid>("SELECT environment_id FROM commit.testing_environments WHERE iam_test_key_digest=$1 AND status='active'")
            .bind(digest(key)).fetch_all(&state.pool).await.map_err(|e| AppError::Internal(e.into()))?;
        if ids.is_empty() {
            return Err(AppError::Unauthenticated);
        }
        ids.into_iter().map(Some).collect()
    } else {
        vec![None]
    };
    let event = delivery.event();
    let aggregate_id: Uuid = event
        .aggregate
        .get("id")
        .and_then(serde_json::Value::as_str)
        .and_then(|id| id.parse().ok())
        .ok_or(AppError::BadGateway)?;
    let aggregate_version = event
        .aggregate
        .get("version")
        .and_then(serde_json::Value::as_i64)
        .ok_or(AppError::BadGateway)?;
    let mut payload =
        serde_json::to_value(event).map_err(|error| AppError::Internal(error.into()))?;
    redact_secrets(&mut payload);
    let hash = hex::encode(Sha256::digest(&body));
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    for environment_id in environments {
        if let Some(id) = environment_id {
            let active: Option<Uuid> = sqlx::query_scalar("SELECT environment_id FROM commit.testing_environments WHERE environment_id=$1 AND status='active' FOR SHARE").bind(id).fetch_optional(&mut *tx).await.map_err(|e| AppError::Internal(e.into()))?;
            if active.is_none() {
                return Err(AppError::Unauthenticated);
            }
        }
        let inserted = sqlx::query("INSERT INTO commit.iam_webhook_events (event_id,event_type,organization_id,aggregate_id,aggregate_version,payload,payload_sha256,environment_id) VALUES ($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT (environment_id,event_id) DO NOTHING")
            .bind(event.event_id).bind(&event.event_type).bind(event.organization_id).bind(aggregate_id).bind(aggregate_version).bind(&payload).bind(&hash).bind(environment_id)
            .execute(&mut *tx).await.map_err(|e| AppError::Internal(e.into()))?;
        if inserted.rows_affected() == 0 {
            let original: String = sqlx::query_scalar("SELECT payload_sha256 FROM commit.iam_webhook_events WHERE event_id=$1 AND environment_id IS NOT DISTINCT FROM $2")
                .bind(event.event_id).bind(environment_id).fetch_one(&mut *tx).await.map_err(|e| AppError::Internal(e.into()))?;
            if original != hash {
                return Err(AppError::Conflict {
                    code: "webhook_event_id_reused".into(),
                });
            }
        }
    }
    tx.commit()
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    // Every product request introspects IAM live, so logout/removal becomes
    // effective even before this durable notification has arrived.
    Ok(StatusCode::NO_CONTENT)
}

fn digest(key: &str) -> String {
    hex::encode(Sha256::digest(key.as_bytes()))
}

fn redact_secrets(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(fields) => {
            for (key, value) in fields {
                let key = key.to_ascii_lowercase();
                if key.contains("secret")
                    || key.contains("token")
                    || matches!(
                        key.as_str(),
                        "testing_key" | "root_key" | "password" | "authorization"
                    )
                {
                    *value = serde_json::Value::String("[redacted]".into());
                } else {
                    redact_secrets(value);
                }
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                redact_secrets(value);
            }
        }
        _ => {}
    }
}
