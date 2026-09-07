//! Application login using IAM's short-lived-token exchange.

use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
};
use secrecy::{ExposeSecret as _, SecretString};
use serde::Deserialize;
use silicon_iam_client::{Client, Credential, IdempotencyKey, Mutation, models};

use super::{AppState, extract::StrictJson};
use crate::{config::IamSettings, error::AppError, infrastructure::clients::ClientBuildError};

/// A server-side IAM application client. Its credentials never reach CLI users.
#[derive(Clone, Debug)]
pub struct SessionService {
    pub(crate) client: Client,
    app_id: String,
}

impl SessionService {
    /// Builds the official SDK with the deployment's application credentials.
    pub fn new(
        settings: &IamSettings,
        timeout: std::time::Duration,
    ) -> Result<Self, ClientBuildError> {
        let app_id = settings
            .app_id
            .as_ref()
            .ok_or(ClientBuildError::InvalidCredential)?
            .clone();
        let secret = settings
            .app_secret
            .as_ref()
            .ok_or(ClientBuildError::InvalidCredential)?;
        let mut origin = settings.base_url.clone();
        if origin.path().trim_end_matches('/') == "/api/v1" {
            origin.set_path("/");
        }
        let client = Client::builder(origin.as_str())
            .map_err(|_| ClientBuildError::InvalidEndpoint)?
            .credential(Credential::application(&app_id, secret.expose_secret()))
            .timeout(timeout)
            // Backend dependencies are upgraded and tested at build time.
            .auto_update(false)
            .build().map_err(|_| ClientBuildError::HttpClient)?;
        Ok(Self { client, app_id })
    }
}

/// Only IAM's single-use SLT can begin a login.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Login {
    slt: SecretString,
}

/// A refresh token continues an existing application session.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Refresh {
    refresh_token: SecretString,
}

/// Revoking a refresh token revokes its complete session family.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Logout {
    token: SecretString,
}

pub(crate) async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    StrictJson(input): StrictJson<Login>,
) -> Result<Json<models::OAuthTokenResponse>, AppError> {
    let service = state
        .sessions
        .as_ref()
        .ok_or(AppError::ProviderUnavailable)?;
    let mutation = mutation(&headers)?;
    super::test_environments::resolve_context(&state.pool, &headers).await?;
    let client = match crate::request_context::current_iam_environment_key() {
        Some(key) => service.client.with_environment(
            silicon_iam_client::EnvironmentKey::new(key).map_err(|_| AppError::Unauthenticated)?,
        ),
        None => service.client.clone(),
    };
    client
        .oauth()
        .login(&service.app_id, input.slt.expose_secret(), &mutation)
        .await
        .map(Json)
        .map_err(map_error)
}

pub(crate) async fn refresh(
    State(state): State<AppState>,
    headers: HeaderMap,
    StrictJson(input): StrictJson<Refresh>,
) -> Result<Json<models::OAuthTokenResponse>, AppError> {
    let service = state
        .sessions
        .as_ref()
        .ok_or(AppError::ProviderUnavailable)?;
    let mutation = mutation(&headers)?;
    super::test_environments::resolve_context(&state.pool, &headers).await?;
    let client = match crate::request_context::current_iam_environment_key() {
        Some(key) => service.client.with_environment(
            silicon_iam_client::EnvironmentKey::new(key).map_err(|_| AppError::Unauthenticated)?,
        ),
        None => service.client.clone(),
    };
    client
        .oauth()
        .refresh(
            &service.app_id,
            input.refresh_token.expose_secret(),
            &mutation,
        )
        .await
        .map(Json)
        .map_err(map_error)
}

pub(crate) async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
    StrictJson(input): StrictJson<Logout>,
) -> Result<StatusCode, AppError> {
    let service = state
        .sessions
        .as_ref()
        .ok_or(AppError::ProviderUnavailable)?;
    let mutation = mutation(&headers)?;
    super::test_environments::resolve_context(&state.pool, &headers).await?;
    let client = match crate::request_context::current_iam_environment_key() {
        Some(key) => service.client.with_environment(
            silicon_iam_client::EnvironmentKey::new(key).map_err(|_| AppError::Unauthenticated)?,
        ),
        None => service.client.clone(),
    };
    client
        .oauth()
        .revoke(
            &models::OAuthRevocationRequest {
                token: input.token.expose_secret().to_owned(),
                token_type_hint: None,
            },
            &mutation,
        )
        .await
        .map_err(map_error)?;
    Ok(StatusCode::NO_CONTENT)
}

fn mutation(headers: &HeaderMap) -> Result<Mutation, AppError> {
    let mut values = headers.get_all("idempotency-key").iter();
    let key = values
        .next()
        .and_then(|v| v.to_str().ok())
        .ok_or(AppError::PreconditionRequired)?;
    if values.next().is_some() {
        return Err(AppError::BadRequest {
            code: "duplicate_idempotency_key".into(),
        });
    }
    IdempotencyKey::parse(key)
        .map(Mutation::with_key)
        .map_err(|_| AppError::BadRequest {
            code: "invalid_idempotency_key".into(),
        })
}

pub(crate) fn map_error(error: silicon_iam_client::Error) -> AppError {
    use silicon_iam_client::Error;
    match error {
        Error::Api(error) => match error.status {
            400 | 422 => AppError::BadRequest {
                code: "iam_rejected_input".into(),
            },
            401 => AppError::Unauthenticated,
            403 => AppError::Forbidden,
            404 => AppError::NotFound,
            409 => AppError::Conflict {
                code: "iam_conflict".into(),
            },
            _ => AppError::ProviderUnavailable,
        },
        Error::RateLimited { retry_after, .. } => AppError::RateLimited {
            retry_after_seconds: retry_after.as_secs().max(1),
        },
        Error::Decode(_) | Error::ResponseTooLarge { .. } | Error::ApiVersionUnsupported { .. } => {
            AppError::BadGateway
        }
        _ => AppError::ProviderUnavailable,
    }
}
