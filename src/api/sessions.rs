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
    application::ports::InboundCredential,
    config::IamSettings,
    domain::PublicOrganizationId,
    error::AppError,
    infrastructure::clients::ClientBuildError,
    request_context::{self, IamTestingCredentials},
};

/// A server-side IAM application client. Its credentials never reach CLI users.
#[derive(Clone, Debug)]
pub struct SessionService {
    pub(crate) client: Client,
    app_id: String,
    iam_url: String,
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
            .telemetry(false)
            .build().map_err(|_| ClientBuildError::HttpClient)?;
        Ok(Self {
            client,
            app_id,
            iam_url: settings.base_url.to_string(),
        })
    }

    pub(crate) fn app_id(&self) -> &str {
        &self.app_id
    }

    pub(crate) fn testing_client(
        &self,
        credentials: &IamTestingCredentials,
    ) -> Result<Client, AppError> {
        if credentials.app_id != self.app_id {
            return Err(AppError::Unauthenticated);
        }
        if credentials.environment_key.expose_secret().is_empty() {
            return self
                .client
                .with_credential(Credential::application(
                    &credentials.app_id,
                    credentials.app_secret.expose_secret(),
                ))
                .with_testing_application(
                    &credentials.app_id,
                    credentials.app_secret.expose_secret(),
                )
                .map_err(map_error);
        }
        let environment =
            silicon_iam_client::EnvironmentKey::new(credentials.environment_key.expose_secret())
                .map_err(|_| AppError::Unauthenticated)?;
        Ok(self
            .client
            .with_credential(Credential::application(
                &credentials.app_id,
                credentials.app_secret.expose_secret(),
            ))
            .with_environment(environment))
    }

    fn request_client(&self) -> Result<Client, AppError> {
        if let Some(credentials) = request_context::current_iam_testing_credentials() {
            return self.testing_client(&credentials);
        }
        if request_context::current_environment_key().is_some()
            || request_context::testing_scope().is_some()
        {
            return Err(AppError::Unauthenticated);
        }
        Ok(self.client.clone())
    }

    /// Checks the paired application credential without issuing a user token.
    pub(crate) async fn verify_testing_credentials(
        &self,
        credentials: &IamTestingCredentials,
    ) -> Result<(), AppError> {
        self.testing_client(credentials)?
            .oauth()
            .introspect(
                &models::TokenIntrospectionRequest {
                    token: "commit-testing-credential-check".to_owned(),
                    token_type_hint: None,
                },
                None,
            )
            .await
            .map_err(|error| match error {
                silicon_iam_client::Error::Api(error) if error.code == "invalid_client" => {
                    AppError::Unauthenticated
                }
                error => map_error(error),
            })?;
        Ok(())
    }
}

/// Public application metadata; application secrets stay on the server.
pub(crate) async fn iam(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, AppError> {
    let service = state
        .sessions
        .as_ref()
        .ok_or(AppError::ProviderUnavailable)?;
    Ok(Json(serde_json::json!({
        "app_id": service.app_id,
        "iam_url": service.iam_url,
    })))
}

/// Verify a session using live IAM authorization, without returning tokens.
pub(crate) async fn status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, AppError> {
    let InboundCredential::Bearer(token) = super::auth::iam_credential(&headers)? else {
        return Err(AppError::Unauthenticated);
    };
    let service = state
        .sessions
        .as_ref()
        .ok_or(AppError::ProviderUnavailable)?;
    let org_id = super::auth::optional_header(&headers, "x-org-id")?
        .map(|org| {
            org.parse::<PublicOrganizationId>()
                .map_err(|_| AppError::BadRequest {
                    code: "invalid_x_org_id".into(),
                })
        })
        .transpose()?;
    super::test_environments::resolve_context(&state, &headers).await?;
    let client = service.request_client()?;
    verified_status(
        &client,
        &service.app_id,
        token.expose_secret(),
        org_id.as_ref(),
    )
    .await
    .map(Json)
}

fn snapshot_environment_matches(environment: Option<uuid::Uuid>) -> bool {
    match request_context::current_iam_testing_credentials() {
        None => environment.is_none(),
        Some(credentials) if credentials.environment_key.expose_secret().is_empty() => {
            environment == request_context::testing_scope().map(|s| s.id)
        }
        Some(_) => true, // Legacy root-selected IAM performs the environment binding.
    }
}

async fn verified_status(
    client: &Client,
    app_id: &str,
    token: &str,
    org_id: Option<&PublicOrganizationId>,
) -> Result<serde_json::Value, AppError> {
    let snapshots = if let Some(org) = org_id {
        client
            .oauth()
            .authorization(token, Some(org.as_str()))
            .await
            .map_err(map_error)?
            .map(|snapshot| vec![snapshot])
    } else {
        client
            .oauth()
            .authorizations(token)
            .await
            .map_err(map_error)?
    }
    .ok_or(AppError::Unauthenticated)?;
    let first = snapshots.first().ok_or(AppError::Unauthenticated)?;
    if snapshots.iter().any(|snapshot| {
        snapshot.audience != app_id
            || !snapshot_environment_matches(snapshot.testing_environment_id)
            || snapshot.public_id != first.public_id
            || snapshot.actor_type != first.actor_type
            || org_id.is_some_and(|org| snapshot.org_id != org.as_str())
    }) {
        return Err(AppError::Unauthenticated);
    }
    let actor = crate::domain::ActorRef::new(
        match first.actor_type.as_ref() {
            Some(models::ApplicationAuthorizationActorType::Carbon) => {
                crate::domain::ActorType::Carbon
            }
            Some(models::ApplicationAuthorizationActorType::Silicon) => {
                crate::domain::ActorType::Silicon
            }
            None => return Err(AppError::Forbidden),
            Some(models::ApplicationAuthorizationActorType::Other(_)) => {
                return Err(AppError::BadGateway);
            }
        },
        first
            .public_id
            .as_ref()
            .ok_or(AppError::Forbidden)?
            .parse()
            .map_err(|_| AppError::BadGateway)?,
    );
    let organizations = snapshots
        .iter()
        .map(|snapshot| {
            snapshot
                .org_id
                .parse::<PublicOrganizationId>()
                .map_err(|_| AppError::BadGateway)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(serde_json::json!({
        "authenticated": true,
        "app_id": app_id,
        "actor": actor,
        "org_id": org_id.map(PublicOrganizationId::as_str).or_else(|| {
            (organizations.len() == 1).then(|| organizations[0].as_str())
        }),
        "organizations": organizations,
    }))
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
    super::test_environments::resolve_context(&state, &headers).await?;
    let client = service.request_client()?;
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
        if snapshot.audience != app_id
            || !snapshot_environment_matches(snapshot.testing_environment_id)
        {
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
    super::test_environments::resolve_context(&state, &headers).await?;
    let client = service.request_client()?;
    // IAM issues 32-byte, unpadded base64url authorization codes. Public actor
    // IDs remain valid only after the request selected a verified testing plane.
    let issued_code = input
        .slt
        .expose_secret()
        .strip_prefix("oac_")
        .is_some_and(|code| {
            code.len() == 43
                && code
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        });
    if request_context::testing_scope().is_none() && !issued_code {
        return Err(AppError::Unauthenticated);
    }
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
    super::test_environments::resolve_context(&state, &headers).await?;
    let client = service.request_client()?;
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
    super::test_environments::resolve_context(&state, &headers).await?;
    let client = service.request_client()?;
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
        Error::Api(error) if error.code == "invalid_client" => AppError::ProviderUnavailable,
        Error::Api(error)
            if matches!(error.code.as_str(), "invalid_grant" | "refresh_token_reuse") =>
        {
            AppError::Unauthenticated
        }
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
    use axum::response::IntoResponse as _;
    use std::{sync::Arc, time::Duration};

    #[test]
    fn application_authentication_failure_is_not_user_session_expiry() {
        for (code, expected) in [
            ("invalid_client", 503),
            ("invalid_grant", 401),
            ("unauthenticated", 401),
        ] {
            let error = silicon_iam_client::ApiError {
                status: if code == "invalid_grant" { 400 } else { 401 },
                code: code.to_owned(),
                message: "redacted".to_owned(),
                details: None,
                request_id: None,
            };
            assert_eq!(
                super::map_error(error.into())
                    .into_response()
                    .status()
                    .as_u16(),
                expected
            );
        }
    }

    use super::selected_organizations;
    use axum::{Json, extract::State, http::HeaderMap};
    use secrecy::SecretString;
    use serde_json::json;
    use silicon_iam_client::{Client, Credential};
    use sqlx::postgres::PgPoolOptions;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{header, method, path},
    };

    fn login_state(server: &MockServer) -> anyhow::Result<super::AppState> {
        let pool = PgPoolOptions::new()
            .connect_lazy("postgresql://postgres:postgres@127.0.0.1:1/unused")?;
        let mut state = crate::api::tests::test_state_with_pool(
            Arc::new(crate::infrastructure::clients::iam::TrustedHeaderIdentityProvider::default()),
            pool,
        )?;
        state.sessions = Some(Arc::new(super::SessionService::new(
            &crate::config::IamSettings {
                mode: crate::config::AuthenticationMode::Iam,
                base_url: server.uri().parse()?,
                app_id: Some("commit".to_owned()),
                app_secret: Some(SecretString::from("test-application-secret")),
                audience: "commit".to_owned(),
                webhook_secret: None,
                webhook_key_version: 1,
            },
            Duration::from_secs(2),
        )?));
        Ok(state)
    }

    fn login_headers() -> anyhow::Result<HeaderMap> {
        let mut headers = HeaderMap::new();
        headers.insert("idempotency-key", "commit-login-regression-1".parse()?);
        Ok(headers)
    }

    #[tokio::test]
    async fn production_login_exchanges_current_iam_code_with_stable_retry_identity()
    -> anyhow::Result<()> {
        let server = MockServer::start().await;
        let code = format!("oac_{}_-", "a".repeat(41));
        let expected = code.clone();
        Mock::given(method("POST"))
            .and(path("/api/v1/app-auth/tokens"))
            .and(header("idempotency-key", "commit-login-regression-1"))
            .and(move |request: &wiremock::Request| {
                let form = url::form_urlencoded::parse(&request.body)
                    .into_owned()
                    .collect::<std::collections::HashMap<_, _>>();
                form.get("slt") == Some(&expected)
                    && form.get("app_id").is_some_and(|id| id == "commit")
                    && !form.contains_key("refresh_token")
                    && !request.headers.contains_key("x-testing-application")
                    && !request.headers.contains_key("x-testing-environment-key")
            })
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token":"oat_fixture", "refresh_token":"ort_fixture",
                "token_type":"Bearer", "expires_in":300, "scope":"self.identity.read"
            })))
            .expect(2)
            .mount(&server)
            .await;
        let state = login_state(&server)?;
        for _ in 0..2 {
            let Json(tokens) = crate::request_context::scope(
                "production-login".to_owned(),
                super::login(
                    State(state.clone()),
                    login_headers()?,
                    super::StrictJson(serde_json::from_value(json!({"slt":code}))?),
                ),
            )
            .await?;
            assert_eq!(tokens.access_token, "oat_fixture");
        }
        Ok(())
    }

    #[tokio::test]
    async fn production_login_rejects_actor_ids_and_non_authorization_credentials_locally()
    -> anyhow::Result<()> {
        let server = MockServer::start().await;
        let state = login_state(&server)?;
        for invalid in [
            "alice".to_owned(),
            "worker:tos".to_owned(),
            format!("slt_{}", "a".repeat(43)),
            format!("oat_{}", "a".repeat(43)),
            format!("ort_{}", "a".repeat(43)),
            format!("oac_{}", "a".repeat(42)),
            format!("oac_{}", "a".repeat(44)),
            format!("oac_{}=", "a".repeat(42)),
            format!("oac_{} ", "a".repeat(42)),
        ] {
            let result = crate::request_context::scope(
                "invalid-login".to_owned(),
                super::login(
                    State(state.clone()),
                    login_headers()?,
                    super::StrictJson(serde_json::from_value(json!({"slt":invalid}))?),
                ),
            )
            .await;
            assert!(matches!(
                result,
                Err(crate::error::AppError::Unauthenticated)
            ));
        }
        assert!(
            server
                .received_requests()
                .await
                .is_some_and(|requests| requests.is_empty())
        );
        Ok(())
    }

    #[tokio::test]
    async fn production_login_keeps_iam_rejection_authoritative() -> anyhow::Result<()> {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/app-auth/tokens"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "error":{"code":"invalid_grant","message":"The code is expired or spent."}
            })))
            .expect(1)
            .mount(&server)
            .await;
        let result = crate::request_context::scope(
            "rejected-login".to_owned(),
            super::login(
                State(login_state(&server)?),
                login_headers()?,
                super::StrictJson(serde_json::from_value(
                    json!({"slt":format!("oac_{}", "a".repeat(43))}),
                )?),
            ),
        )
        .await;
        assert!(matches!(
            result,
            Err(crate::error::AppError::Unauthenticated)
        ));
        Ok(())
    }

    fn snapshot(org: &str, audience: &str) -> serde_json::Value {
        json!({
            "principal_id": "11111111-1111-4111-8111-111111111111",
            "actor_type": "carbon", "public_id": "person",
            "organization_id": "22222222-2222-4222-8222-222222222222",
            "org_id": org,
            "membership_id": format!("person[{org}]"),
            "membership_version": 1, "authorization_epoch": 1,
            "audience": audience, "testing_environment_id": null,
            "scopes": [], "org_role": null, "tags": null
        })
    }

    #[tokio::test]
    async fn login_status_verifies_identity_and_organization()
    -> Result<(), Box<dyn std::error::Error>> {
        for kind in ["carbon", "silicon"] {
            for scoped in [false, true] {
                let server = MockServer::start().await;
                let mut grant = snapshot("tos", "commit");
                grant["actor_type"] = json!(kind);
                let response = if scoped {
                    json!({"active":true,"authorization":grant})
                } else {
                    json!({"active":true,"authorizations":[grant]})
                };
                Mock::given(method("POST"))
                    .and(path("/api/v1/oauth/introspect"))
                    .and(move |request: &wiremock::Request| {
                        request.headers.contains_key("x-org-id") == scoped
                    })
                    .respond_with(ResponseTemplate::new(200).set_body_json(response))
                    .expect(1)
                    .mount(&server)
                    .await;
                let client = Client::builder(&server.uri())?
                    .credential(Credential::application("commit", "test-secret"))
                    .auto_update(false)
                    .build()?;
                let org = "tos".parse()?;
                let output =
                    super::verified_status(&client, "commit", "oat_test", scoped.then_some(&org))
                        .await?;
                assert_eq!(output["authenticated"], true);
                assert_eq!(output["actor"], json!({"type":kind,"id":"person"}));
                assert_eq!(output["org_id"], "tos");
                assert!(!output.to_string().contains("principal_id"));
                assert!(!output.to_string().contains("oat_test"));
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn login_status_rejects_inactive_wrong_audience_and_inconsistent_grants()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut other_actor = snapshot("second", "commit");
        other_actor["public_id"] = json!("somebody-else");
        let cases = [
            (json!({"active":false}), None),
            (json!({"active":true,"authorizations":[]}), None),
            (
                json!({"active":true,"authorizations":[snapshot("tos", "other>app")]}),
                None,
            ),
            (
                json!({"active":true,"authorizations":[snapshot("tos", "commit"),other_actor]}),
                None,
            ),
            (
                json!({"active":true,"authorization":snapshot("wrong-org", "commit")}),
                Some("tos"),
            ),
            (json!({"active":true}), None),
        ];
        for (response, org) in cases {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/api/v1/oauth/introspect"))
                .respond_with(ResponseTemplate::new(200).set_body_json(response))
                .expect(1)
                .mount(&server)
                .await;
            let client = Client::builder(&server.uri())?
                .credential(Credential::application("commit", "test-secret"))
                .auto_update(false)
                .build()?;
            let org = org.map(str::parse).transpose()?;
            assert!(
                super::verified_status(&client, "commit", "oat_test", org.as_ref())
                    .await
                    .is_err()
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn selected_organizations_use_only_live_iam_grants()
    -> Result<(), Box<dyn std::error::Error>> {
        let responses = [
            (
                json!({"active": true, "authorizations": [snapshot("z-team", "commit"), snapshot("a-team", "commit")]}),
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
                .credential(Credential::application("commit", "test-secret"))
                .auto_update(false)
                .build()?;
            let result = selected_organizations(&client, "commit", "oat_test").await;
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
