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
use crate::{
    application::ports::InboundCredential, config::IamSettings, domain::PublicOrganizationId,
    error::AppError, infrastructure::clients::ClientBuildError,
};

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

/// Lists only the current session's selected, active IAM organizations.
pub(crate) async fn organizations(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<PublicOrganizationId>>, AppError> {
    let InboundCredential::Bearer(token) = super::auth::iam_credential(&headers)? else {
        return Err(AppError::Unauthenticated);
    };
    let service = state
        .sessions
        .as_ref()
        .ok_or(AppError::ProviderUnavailable)?;
    super::test_environments::resolve_context(&state.pool, &headers).await?;
    let client = match crate::request_context::current_iam_environment_key() {
        Some(key) => service.client.with_environment(
            silicon_iam_client::EnvironmentKey::new(key).map_err(|_| AppError::Unauthenticated)?,
        ),
        None => service.client.clone(),
    };
    selected_organizations(&client, &service.app_id, token.expose_secret())
        .await
        .map(Json)
}

async fn selected_organizations(
    client: &Client,
    app_id: &str,
    token: &str,
) -> Result<Vec<PublicOrganizationId>, AppError> {
    let snapshots = client
        .oauth()
        .authorizations(token)
        .await
        .map_err(map_error)?
        .ok_or(AppError::Unauthenticated)?;
    let mut organizations = Vec::with_capacity(snapshots.len());
    for snapshot in snapshots {
        if snapshot.audience != app_id {
            return Err(AppError::Unauthenticated);
        }
        organizations.push(
            snapshot
                .org_id
                .parse::<PublicOrganizationId>()
                .map_err(|_| AppError::BadGateway)?,
        );
    }
    organizations.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    organizations.dedup();
    Ok(organizations)
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

#[cfg(test)]
mod tests {
    use super::selected_organizations;
    use serde_json::json;
    use silicon_iam_client::{Client, Credential};
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    fn snapshot(org: &str, audience: &str) -> serde_json::Value {
        json!({
            "principal_id": "11111111-1111-4111-8111-111111111111",
            "actor_type": "carbon", "public_id": "person",
            "organization_id": "22222222-2222-4222-8222-222222222222",
            "org_id": org,
            "membership_id": "33333333-3333-4333-8333-333333333333",
            "membership_version": 1, "authorization_epoch": 1,
            "audience": audience, "testing_environment_id": null,
            "scopes": [], "org_role": null, "tags": null
        })
    }

    #[tokio::test]
    async fn selected_organizations_use_only_live_iam_grants()
    -> Result<(), Box<dyn std::error::Error>> {
        let responses = [
            (
                json!({"active": true, "authorizations": [snapshot("z-team", "tos>commit"), snapshot("a-team", "tos>commit")]}),
                Some(vec!["a-team", "z-team"]),
            ),
            (json!({"active": true, "authorizations": []}), Some(vec![])),
            (json!({"active": false}), None),
            (
                json!({"active": true, "authorizations": [snapshot("a-team", "other>app")]}),
                None,
            ),
            (json!({"active": true}), None),
        ];
        for (response, expected) in responses {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/api/v1/oauth/introspect"))
                .and(|request: &wiremock::Request| !request.headers.contains_key("x-org-id"))
                .respond_with(ResponseTemplate::new(200).set_body_json(response))
                .expect(1)
                .mount(&server)
                .await;
            let client = Client::builder(&server.uri())?
                .credential(Credential::application("tos>commit", "test-secret"))
                .auto_update(false)
                .build()?;
            let result = selected_organizations(&client, "tos>commit", "oat_test").await;
            if let Some(expected) = expected {
                let actual = result?;
                assert_eq!(
                    actual
                        .iter()
                        .map(crate::domain::PublicOrganizationId::as_str)
                        .collect::<Vec<_>>(),
                    expected
                );
            } else {
                assert!(result.is_err());
            }
        }
        Ok(())
    }
}
