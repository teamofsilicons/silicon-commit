//! Organization-owned testing-environment lifecycle metadata.

use super::{AppState, auth::action, extract::StrictJson};
use crate::{
    application::ports::OrganizationRole,
    error::AppError,
    request_context::{self, IamTestingCredentials},
};
use axum::{
    Json,
    extract::{Path, State},
    http::HeaderMap,
};
use chacha20poly1305::{
    ChaCha20Poly1305, Nonce,
    aead::{Aead, KeyInit},
};
use secrecy::{ExposeSecret as _, SecretString};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use uuid::Uuid;

/// Resolves and pins the Commit test key to one active environment.
pub(crate) async fn resolve_context(
    pool: &sqlx::PgPool,
    headers: &HeaderMap,
) -> Result<(), AppError> {
    let mut values = headers.get_all("x-testing-environment-key").iter();
    let Some(raw) = values.next() else {
        return Ok(());
    };
    let key = raw.to_str().map_err(|_| AppError::Unauthenticated)?;
    if values.next().is_some() || key.len() != 32 || !key.bytes().all(|b| b.is_ascii_alphanumeric())
    {
        return Err(AppError::Unauthenticated);
    }
    let row = sqlx::query_as::<_, (Uuid, i64, Vec<u8>, Option<String>, Option<Vec<u8>>)>("UPDATE commit.testing_environments SET last_activity_at=clock_timestamp() WHERE key_digest=$1 AND status='active' RETURNING environment_id,version,iam_test_key_ciphertext,iam_app_id,iam_app_secret_ciphertext")
        .bind(digest(key)).fetch_optional(pool).await.map_err(internal)?.ok_or(AppError::Unauthenticated)?;
    let (Some(app_id), Some(app_secret_ciphertext)) = (row.3, row.4) else {
        return Err(AppError::Conflict {
            code: "testing_environment_iam_credentials_required".into(),
        });
    };
    let iam_key = decrypt_iam_key(&row.2).ok_or(AppError::ProviderUnavailable)?;
    let app_secret =
        decrypt_iam_key(&app_secret_ciphertext).ok_or(AppError::ProviderUnavailable)?;
    request_context::set_environment_key(Some(key.to_owned()));
    request_context::set_iam_testing_credentials(Some(IamTestingCredentials {
        environment_key: SecretString::from(iam_key),
        app_id,
        app_secret: SecretString::from(app_secret),
    }));
    request_context::set_testing_scope(Some(request_context::TestingScope {
        id: row.0,
        version: row.1,
    }));
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateInput {
    pub name: String,
    pub description: Option<String>,
    /// Root key for the isolated Silicon `IAm` testing environment.
    pub iam_test_key: SecretString,
    /// Canonical Commit application ID imported into that IAM environment.
    pub iam_app_id: String,
    /// The imported test application's secret, encrypted before storage.
    pub iam_app_secret: SecretString,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct IamCredentialsInput {
    pub iam_app_id: String,
    pub iam_app_secret: SecretString,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub(crate) struct Environment {
    pub environment_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub status: String,
    pub version: i64,
    #[serde(with = "time::serde::rfc3339::option")]
    pub deleted_at: Option<time::OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub purge_after: Option<time::OffsetDateTime>,
}

#[derive(Debug, Serialize)]
pub(crate) struct Created {
    #[serde(flatten)]
    pub environment: Environment,
    pub key: String,
}

pub(crate) async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    StrictJson(input): StrictJson<CreateInput>,
) -> Result<Json<Created>, AppError> {
    require_production_control_plane(&headers)?;
    let actor = state
        .authenticate(&headers, action::TEST_ENVIRONMENTS_CREATE, None)
        .await?;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM commit.testing_environments WHERE organization_id=$1 AND status='active'").bind(actor.organization_id.as_uuid()).fetch_one(&state.pool).await.map_err(internal)?;
    if count >= 100 {
        return Err(AppError::Conflict {
            code: "test_environment_limit".into(),
        });
    }
    if input.iam_test_key.expose_secret().len() != 32
        || !input
            .iam_test_key
            .expose_secret()
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric())
    {
        return Err(AppError::Validation {
            details: serde_json::json!({"fields":[{"field":"iam_test_key","message":"must be exactly 32 alphanumeric characters"}]}),
        });
    }
    let credentials = IamTestingCredentials {
        environment_key: input.iam_test_key,
        app_id: input.iam_app_id,
        app_secret: input.iam_app_secret,
    };
    verify_pair(&state, &credentials).await?;
    let key = generate_key();
    let commit_key_digest = digest(&key);
    let id = Uuid::now_v7();
    let iam_ciphertext =
        encrypt_iam_key(credentials.environment_key.expose_secret()).ok_or(AppError::Internal(
            anyhow::anyhow!("test environment encryption is not configured"),
        ))?;
    let app_secret_ciphertext =
        encrypt_iam_key(credentials.app_secret.expose_secret()).ok_or(AppError::Internal(
            anyhow::anyhow!("test environment encryption is not configured"),
        ))?;
    let key_ciphertext = encrypt_iam_key(&key).ok_or(AppError::Internal(anyhow::anyhow!(
        "test environment encryption is not configured"
    )))?;
    let environment = sqlx::query_as::<_, Environment>("INSERT INTO commit.testing_environments (environment_id,organization_id,creator_principal_id,name,description,iam_test_key_digest,iam_test_key_ciphertext,key_digest,key_ciphertext,iam_app_id,iam_app_secret_ciphertext) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11) RETURNING environment_id,name,description,status,version,deleted_at,purge_after")
        .bind(id).bind(actor.organization_id.as_uuid()).bind(actor.actor.principal_id.as_uuid()).bind(input.name.trim()).bind(input.description.as_deref().map(str::trim)).bind(digest(credentials.environment_key.expose_secret())).bind(iam_ciphertext).bind(commit_key_digest).bind(key_ciphertext).bind(&credentials.app_id).bind(app_secret_ciphertext).fetch_one(&state.pool).await.map_err(|e| if let sqlx::Error::Database(db)=&e && db.constraint().is_some_and(|c| c=="testing_environments_organization_id_name_key") { AppError::Conflict { code: "test_environment_name_taken".into() } } else { internal(e) })?;
    Ok(Json(Created { environment, key }))
}

/// Pairs or rotates only the imported IAM app credential of an owned environment.
pub(crate) async fn pair_iam_credentials(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    StrictJson(input): StrictJson<IamCredentialsInput>,
) -> Result<Json<Environment>, AppError> {
    require_production_control_plane(&headers)?;
    let actor = state
        .authenticate(
            &headers,
            action::TEST_ENVIRONMENTS_MANAGE,
            Some(id.to_string()),
        )
        .await?;
    if actor.organization_role != OrganizationRole::Owner
        && !actor
            .capabilities
            .contains(action::TEST_ENVIRONMENTS_MANAGE)
    {
        return Err(AppError::Forbidden);
    }
    let (version, iam_ciphertext) = sqlx::query_as::<_, (i64, Vec<u8>)>(
        "SELECT version,iam_test_key_ciphertext FROM commit.testing_environments WHERE environment_id=$1 AND organization_id=$2 AND status='active'",
    )
    .bind(id)
    .bind(actor.organization_id.as_uuid())
    .fetch_optional(&state.pool)
    .await
    .map_err(internal)?
    .ok_or(AppError::NotFound)?;
    let credentials = IamTestingCredentials {
        environment_key: SecretString::from(
            decrypt_iam_key(&iam_ciphertext).ok_or(AppError::ProviderUnavailable)?,
        ),
        app_id: input.iam_app_id,
        app_secret: input.iam_app_secret,
    };
    verify_pair(&state, &credentials).await?;
    let app_secret_ciphertext =
        encrypt_iam_key(credentials.app_secret.expose_secret()).ok_or(AppError::Internal(
            anyhow::anyhow!("test environment encryption is not configured"),
        ))?;
    let environment = sqlx::query_as::<_, Environment>(
        "UPDATE commit.testing_environments SET iam_app_id=$1,iam_app_secret_ciphertext=$2,version=version+1,updated_at=clock_timestamp() WHERE environment_id=$3 AND organization_id=$4 AND status='active' AND version=$5 RETURNING environment_id,name,description,status,version,deleted_at,purge_after",
    )
    .bind(&credentials.app_id)
    .bind(app_secret_ciphertext)
    .bind(id)
    .bind(actor.organization_id.as_uuid())
    .bind(version)
    .fetch_optional(&state.pool)
    .await
    .map_err(internal)?
    .ok_or(AppError::Conflict { code: "testing_environment_changed".into() })?;
    Ok(Json(environment))
}

fn require_production_control_plane(headers: &HeaderMap) -> Result<(), AppError> {
    if headers.contains_key("x-testing-environment-key")
        || request_context::testing_scope().is_some()
    {
        return Err(AppError::Forbidden);
    }
    Ok(())
}

async fn verify_pair(
    state: &AppState,
    credentials: &IamTestingCredentials,
) -> Result<(), AppError> {
    let service = state
        .sessions
        .as_ref()
        .ok_or(AppError::ProviderUnavailable)?;
    if credentials.app_id != service.app_id()
        || !credentials
            .app_id
            .split_once('>')
            .is_some_and(|(org, app)| {
                [org, app].into_iter().all(|part| {
                    !part.is_empty()
                        && part.bytes().all(|byte| {
                            byte.is_ascii_lowercase()
                                || byte.is_ascii_digit()
                                || matches!(byte, b'-' | b'_')
                        })
                })
            })
    {
        return Err(AppError::Validation {
            details: serde_json::json!({"fields":[{"field":"iam_app_id","message":"must match this deployment's canonical IAM application ID"}]}),
        });
    }
    let secret = credentials.app_secret.expose_secret();
    if !(32..=4096).contains(&secret.len()) || !secret.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err(AppError::Validation {
            details: serde_json::json!({"fields":[{"field":"iam_app_secret","message":"must contain 32-4096 visible ASCII characters"}]}),
        });
    }
    service.verify_testing_credentials(credentials).await
}

pub(crate) async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<Environment>>, AppError> {
    let actor = state
        .authenticate(&headers, action::TEST_ENVIRONMENTS_MANAGE, None)
        .await?;
    let environments = sqlx::query_as::<_, Environment>("SELECT environment_id,name,description,status,version,deleted_at,purge_after FROM commit.testing_environments WHERE organization_id=$1 ORDER BY created_at DESC").bind(actor.organization_id.as_uuid()).fetch_all(&state.pool).await.map_err(internal)?;
    Ok(Json(environments))
}

#[derive(Debug, Serialize)]
pub(crate) struct RetrievedKey {
    pub environment_id: Uuid,
    pub key: String,
}

/// Retrieves the current Commit test key for an environment.
///
/// Access is limited to organization operators authorized for environment
/// management. The key is decrypted only in memory and is never persisted in
/// plaintext.
pub(crate) async fn retrieve_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<RetrievedKey>, AppError> {
    let actor = state
        .authenticate(
            &headers,
            action::TEST_ENVIRONMENTS_MANAGE,
            Some(id.to_string()),
        )
        .await?;
    let ciphertext = sqlx::query_scalar::<_, Option<Vec<u8>>>(
        "SELECT key_ciphertext FROM commit.testing_environments WHERE environment_id=$1 AND organization_id=$2 AND status='active'",
    )
    .bind(id)
    .bind(actor.organization_id.as_uuid())
    .fetch_optional(&state.pool)
    .await
    .map_err(internal)?
    .flatten()
    .ok_or(AppError::NotFound)?;
    let key = decrypt_iam_key(&ciphertext).ok_or(AppError::ProviderUnavailable)?;
    Ok(Json(RetrievedKey {
        environment_id: id,
        key,
    }))
}

pub(crate) async fn rotate(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Created>, AppError> {
    let actor = state
        .authenticate(
            &headers,
            action::TEST_ENVIRONMENTS_MANAGE,
            Some(id.to_string()),
        )
        .await?;
    let key = generate_key();
    let key_ciphertext = encrypt_iam_key(&key).ok_or(AppError::Internal(anyhow::anyhow!(
        "test environment encryption is not configured"
    )))?;
    let result=sqlx::query_as::<_,Environment>("UPDATE commit.testing_environments SET key_digest=$1,key_ciphertext=$2,version=version+1,updated_at=clock_timestamp() WHERE environment_id=$3 AND organization_id=$4 AND status='active' RETURNING environment_id,name,description,status,version,deleted_at,purge_after").bind(digest(&key)).bind(key_ciphertext).bind(id).bind(actor.organization_id.as_uuid()).fetch_optional(&state.pool).await.map_err(internal)?;
    result
        .map(|environment| Json(Created { environment, key }))
        .ok_or(AppError::NotFound)
}

pub(crate) async fn delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Environment>, AppError> {
    let actor = state
        .authenticate(
            &headers,
            action::TEST_ENVIRONMENTS_MANAGE,
            Some(id.to_string()),
        )
        .await?;
    let environment=sqlx::query_as::<_,Environment>("UPDATE commit.testing_environments SET status='deleted',deleted_at=clock_timestamp(),purge_after=clock_timestamp()+interval '30 days',version=version+1,updated_at=clock_timestamp() WHERE environment_id=$1 AND organization_id=$2 AND status='active' RETURNING environment_id,name,description,status,version,deleted_at,purge_after").bind(id).bind(actor.organization_id.as_uuid()).fetch_optional(&state.pool).await.map_err(internal)?;
    environment.map(Json).ok_or(AppError::NotFound)
}

pub(crate) async fn restore(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Created>, AppError> {
    let actor = state
        .authenticate(
            &headers,
            action::TEST_ENVIRONMENTS_MANAGE,
            Some(id.to_string()),
        )
        .await?;
    let key = generate_key();
    let key_ciphertext = encrypt_iam_key(&key).ok_or(AppError::Internal(anyhow::anyhow!(
        "test environment encryption is not configured"
    )))?;
    let environment = sqlx::query_as::<_, Environment>("UPDATE commit.testing_environments SET status='active',key_ciphertext=$1,deleted_at=NULL,purge_after=NULL,key_digest=$2,version=version+1,updated_at=clock_timestamp(),last_activity_at=clock_timestamp() WHERE environment_id=$3 AND organization_id=$4 AND status='deleted' AND purge_after > clock_timestamp() RETURNING environment_id,name,description,status,version,deleted_at,purge_after")
        .bind(key_ciphertext).bind(digest(&key)).bind(id).bind(actor.organization_id.as_uuid()).fetch_optional(&state.pool).await.map_err(internal)?;
    environment
        .map(|environment| Json(Created { environment, key }))
        .ok_or(AppError::NotFound)
}

/// Removes all Commit data for the test organization's isolated data plane.
pub(crate) async fn clean(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Environment>, AppError> {
    resolve_context(&state.pool, &headers).await?;
    let scope = crate::request_context::testing_scope().ok_or(AppError::BadRequest {
        code: "testing_environment_required".into(),
    })?;
    if scope.id != id {
        return Err(AppError::NotFound);
    }
    let key = crate::request_context::current_environment_key().ok_or(AppError::Unauthenticated)?;
    let mut tx = state.pool.begin().await.map_err(internal)?;
    sqlx::query("SELECT commit.clean_testing_environment($1,$2)")
        .bind(id)
        .bind(digest(&key))
        .execute(&mut *tx)
        .await
        .map_err(internal)?;
    let environment = sqlx::query_as::<_,Environment>("SELECT environment_id,name,description,status,version,deleted_at,purge_after FROM commit.testing_environments WHERE environment_id=$1").bind(id).fetch_one(&mut *tx).await.map_err(internal)?;
    tx.commit().await.map_err(internal)?;
    Ok(Json(environment))
}

fn generate_key() -> String {
    hex::encode(rand::random::<[u8; 16]>())
}
fn digest(key: &str) -> String {
    hex::encode(Sha256::digest(key.as_bytes()))
}
fn crypto_key() -> Option<[u8; 32]> {
    let secret = std::env::var("COMMIT_IAM_APP_SECRET").ok()?;
    if secret.len() < 32 {
        return None;
    }
    Some(Sha256::digest(secret.as_bytes()).into())
}
fn encrypt_iam_key(key: &str) -> Option<Vec<u8>> {
    let cipher = ChaCha20Poly1305::new((&crypto_key()?).into());
    let mut nonce = [0u8; 12];
    rand::fill(&mut nonce);
    let mut out = nonce.to_vec();
    out.extend(
        cipher
            .encrypt(Nonce::from_slice(&nonce), key.as_bytes())
            .ok()?,
    );
    Some(out)
}
pub(crate) fn decrypt_iam_key(data: &[u8]) -> Option<String> {
    let (nonce, ciphertext) = data.split_at_checked(12)?;
    let cipher = ChaCha20Poly1305::new((&crypto_key()?).into());
    String::from_utf8(cipher.decrypt(Nonce::from_slice(nonce), ciphertext).ok()?).ok()
}
fn internal(error: impl Into<anyhow::Error>) -> AppError {
    AppError::Internal(error.into())
}

#[cfg(test)]
mod tests {
    use std::{num::NonZeroU32, sync::Arc, time::Duration};

    use anyhow::Context as _;
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use http::StatusCode;
    use serde_json::json;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_string_contains, header, method, path},
    };

    use crate::{
        api::sessions,
        config::{AuthenticationMode, DatabaseSettings, IamSettings},
        infrastructure::{clients::iam::TrustedHeaderIdentityProvider, postgres},
    };

    use super::*;

    const IAM_KEY: &str = "iamTestEnvironmentRootKey0000001";
    const TEST_SECRET: &str = "ask_imported_testing_application_secret_00000001";
    const ROTATED_SECRET: &str = "ask_imported_testing_application_secret_00000002";

    fn basic(secret: &str) -> String {
        format!("Basic {}", STANDARD.encode(format!("tos>commit:{secret}")))
    }

    fn pair_input(app_id: &str, secret: &str) -> StrictJson<IamCredentialsInput> {
        StrictJson(IamCredentialsInput {
            iam_app_id: app_id.to_owned(),
            iam_app_secret: SecretString::from(secret.to_owned()),
        })
    }

    fn control_headers(organization: Uuid, role: &str) -> anyhow::Result<HeaderMap> {
        let mut headers = HeaderMap::new();
        for (key, value) in [
            ("x-org-id", format!("pairing-{organization}")),
            ("x-test-organization-id", organization.to_string()),
            ("x-test-membership-id", Uuid::new_v4().to_string()),
            ("x-test-principal-id", Uuid::new_v4().to_string()),
            ("x-test-actor-type", "carbon".to_owned()),
            ("x-test-actor-id", "pairing-operator".to_owned()),
            ("x-test-org-role", role.to_owned()),
        ] {
            headers.insert(key, value.parse()?);
        }
        Ok(headers)
    }

    async fn scoped<T>(future: impl std::future::Future<Output = T>) -> T {
        request_context::scope(Uuid::new_v4().to_string(), future).await
    }

    #[test]
    fn pairing_inputs_require_complete_credentials_and_redact_secrets() -> anyhow::Result<()> {
        assert!(
            serde_json::from_value::<CreateInput>(json!({
                "name":"existing-contract", "iam_test_key":IAM_KEY
            }))
            .is_err()
        );
        for value in [
            json!({"iam_app_id":"tos>commit"}),
            json!({"iam_app_id":"tos>commit","iam_app_secret":TEST_SECRET,"iam_test_key":IAM_KEY}),
        ] {
            assert!(serde_json::from_value::<IamCredentialsInput>(value).is_err());
        }
        let input: CreateInput = serde_json::from_value(json!({
            "name":"paired", "iam_test_key":IAM_KEY,
            "iam_app_id":"tos>commit", "iam_app_secret":TEST_SECRET
        }))?;
        let debug = format!("{input:?}");
        assert!(!debug.contains(IAM_KEY));
        assert!(!debug.contains(TEST_SECRET));
        Ok(())
    }

    #[tokio::test]
    async fn postgres_pairing_is_owner_controlled_encrypted_and_used_by_every_session_handler()
    -> anyhow::Result<()> {
        assert_eq!(
            IAM_KEY.len(),
            32,
            "the IAM fixture root key must match the wire contract"
        );
        let Ok(database_url) = std::env::var("COMMIT_TEST_DATABASE_URL") else {
            eprintln!("skipping PostgreSQL pairing test: COMMIT_TEST_DATABASE_URL is not set");
            return Ok(());
        };
        let database = DatabaseSettings {
            url: SecretString::from(database_url),
            max_connections: NonZeroU32::new(4).context("fixed connection count")?,
            min_connections: 0,
            acquire_timeout: Duration::from_secs(5),
            statement_timeout: Duration::from_secs(30),
        };
        let migrator =
            postgres::connect_migrator(&database, "commit-pairing-test-migrator").await?;
        let owner: String = sqlx::query_scalar("SELECT current_user::text")
            .fetch_one(&migrator)
            .await?;
        postgres::migrate(&migrator, &owner).await?;
        migrator.close().await;
        let pool = postgres::connect(&database, "commit-pairing-test").await?;
        let encrypted_root = encrypt_iam_key(IAM_KEY)
            .context("set a test-only COMMIT_IAM_APP_SECRET for PostgreSQL pairing tests")?;
        let server = MockServer::start().await;
        let mut state = crate::api::tests::test_state_with_pool(
            Arc::new(TrustedHeaderIdentityProvider::default()),
            pool.clone(),
        )?;
        state.authentication_mode = AuthenticationMode::TrustedHeaders;
        state.sessions = Some(Arc::new(sessions::SessionService::new(
            &IamSettings {
                mode: AuthenticationMode::Iam,
                base_url: server.uri().parse()?,
                app_id: Some("tos>commit".to_owned()),
                app_secret: Some(SecretString::from(
                    "production-global-credential-must-never-reach-test-iam",
                )),
                audience: "tos>commit".to_owned(),
                webhook_secret: None,
                webhook_key_version: 1,
            },
            Duration::from_secs(2),
        )?));
        let organization = Uuid::new_v4();
        let environment = Uuid::new_v4();
        let commit_key = generate_key();
        let owner_headers = control_headers(organization, "owner")?;
        let mut test_headers = HeaderMap::new();
        test_headers.insert("x-testing-environment-key", commit_key.parse()?);
        test_headers.insert("idempotency-key", "pairing-regression-login".parse()?);
        sqlx::query("INSERT INTO commit.testing_environments (environment_id,organization_id,creator_principal_id,name,iam_test_key_digest,iam_test_key_ciphertext,key_digest) VALUES ($1,$2,$3,$4,$5,$6,$7)")
            .bind(environment).bind(organization).bind(Uuid::new_v4()).bind(format!("legacy-{environment}"))
            .bind(digest(IAM_KEY)).bind(&encrypted_root).bind(digest(&commit_key)).execute(&pool).await?;

        let legacy = scoped(sessions::login(
            State(state.clone()),
            test_headers.clone(),
            StrictJson(serde_json::from_value(json!({"slt":"slt_legacy"}))?),
        ))
        .await;
        assert!(
            matches!(legacy, Err(AppError::Conflict { code }) if code == "testing_environment_iam_credentials_required")
        );
        assert!(
            server
                .received_requests()
                .await
                .context("recorded IAM requests")?
                .is_empty()
        );

        let mut borrowed_test_owner = owner_headers.clone();
        borrowed_test_owner.insert("x-testing-environment-key", commit_key.parse()?);
        let forbidden = scoped(pair_iam_credentials(
            State(state.clone()),
            borrowed_test_owner,
            Path(environment),
            pair_input("tos>commit", TEST_SECRET),
        ))
        .await;
        assert!(matches!(forbidden, Err(AppError::Forbidden)));
        let wrong_owner = scoped(pair_iam_credentials(
            State(state.clone()),
            control_headers(Uuid::new_v4(), "owner")?,
            Path(environment),
            pair_input("tos>commit", TEST_SECRET),
        ))
        .await;
        assert!(matches!(wrong_owner, Err(AppError::NotFound)));
        let member = scoped(pair_iam_credentials(
            State(state.clone()),
            control_headers(organization, "member")?,
            Path(environment),
            pair_input("tos>commit", TEST_SECRET),
        ))
        .await;
        assert!(matches!(member, Err(AppError::Forbidden)));
        for (app_id, secret) in [
            ("another>app", TEST_SECRET),
            ("tos>commit", "too-short"),
            ("tos>commit", "ask_credential_contains_whitespace_here "),
        ] {
            let invalid = scoped(pair_iam_credentials(
                State(state.clone()),
                owner_headers.clone(),
                Path(environment),
                pair_input(app_id, secret),
            ))
            .await;
            assert!(matches!(invalid, Err(AppError::Validation { .. })));
        }
        assert!(
            server
                .received_requests()
                .await
                .context("recorded IAM requests")?
                .is_empty()
        );

        let wrong_secret = "ask_wrong_application_secret_not_owned_by_environment";
        Mock::given(method("POST")).and(path("/api/v1/oauth/introspect"))
            .and(header("authorization", basic(wrong_secret)))
            .and(header("x-testing-environment-key", IAM_KEY))
            .and(body_string_contains("token=commit-testing-credential-check"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({"error":{"code":"invalid_client","message":"invalid application credential","request_id":"pairing-test"}})))
            .expect(1).mount(&server).await;
        let invalid = scoped(pair_iam_credentials(
            State(state.clone()),
            owner_headers.clone(),
            Path(environment),
            pair_input("tos>commit", wrong_secret),
        ))
        .await;
        assert!(matches!(invalid, Err(AppError::Unauthenticated)));
        let untouched: (i64, Option<String>, Option<Vec<u8>>) = sqlx::query_as("SELECT version,iam_app_id,iam_app_secret_ciphertext FROM commit.testing_environments WHERE environment_id=$1")
            .bind(environment).fetch_one(&pool).await?;
        assert_eq!(untouched, (1, None, None));

        for (secret, count) in [(TEST_SECRET, 2), (ROTATED_SECRET, 1)] {
            Mock::given(method("POST"))
                .and(path("/api/v1/oauth/introspect"))
                .and(header("authorization", basic(secret)))
                .and(header("x-testing-environment-key", IAM_KEY))
                .and(body_string_contains(
                    "token=commit-testing-credential-check",
                ))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"active":false})))
                .expect(count)
                .mount(&server)
                .await;
        }
        let paired = scoped(pair_iam_credentials(
            State(state.clone()),
            owner_headers.clone(),
            Path(environment),
            pair_input("tos>commit", TEST_SECRET),
        ))
        .await?
        .0;
        assert_eq!(paired.version, 2);
        let response = serde_json::to_string(&paired)?;
        assert!(!response.contains(TEST_SECRET) && !response.contains(IAM_KEY));
        assert!(!response.contains("iam_app_secret"));
        let (stored_id, stored_secret, retained_root): (String, Vec<u8>, Vec<u8>) = sqlx::query_as("SELECT iam_app_id,iam_app_secret_ciphertext,iam_test_key_ciphertext FROM commit.testing_environments WHERE environment_id=$1")
            .bind(environment).fetch_one(&pool).await?;
        assert_eq!(stored_id, "tos>commit");
        assert_eq!(
            decrypt_iam_key(&stored_secret).as_deref(),
            Some(TEST_SECRET)
        );
        assert!(
            !stored_secret
                .windows(TEST_SECRET.len())
                .any(|window| window == TEST_SECRET.as_bytes())
        );
        assert_eq!(retained_root, encrypted_root);

        let principal = Uuid::new_v4();
        let token_response = json!({"access_token":"oat_paired","refresh_token":"ort_paired","token_type":"Bearer","expires_in":3600,"scope":"","actor":{"principal_id":principal,"type":"silicon","public_id":"smoke:paired-org"}});
        Mock::given(method("POST"))
            .and(path("/api/v1/app-auth/tokens"))
            .and(header("authorization", basic(TEST_SECRET)))
            .and(header("x-testing-environment-key", IAM_KEY))
            .and(body_string_contains("app_id=tos%3Ecommit"))
            .respond_with(ResponseTemplate::new(200).set_body_json(token_response))
            .expect(2)
            .mount(&server)
            .await;
        let snapshot = json!({"principal_id":principal,"actor_type":"silicon","public_id":"smoke:paired-org","organization_id":Uuid::new_v4(),"org_id":"paired-org","membership_id":Uuid::new_v4(),"membership_version":1,"authorization_epoch":1,"audience":"tos>commit","testing_environment_id":Uuid::new_v4(),"scopes":[],"org_role":"owner","tags":[]});
        Mock::given(method("POST"))
            .and(path("/api/v1/oauth/introspect"))
            .and(header("authorization", basic(TEST_SECRET)))
            .and(header("x-testing-environment-key", IAM_KEY))
            .and(body_string_contains("token=oat_paired"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"active":true,"authorizations":[snapshot]})),
            )
            .expect(2)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/oauth/revoke"))
            .and(header("authorization", basic(TEST_SECRET)))
            .and(header("x-testing-environment-key", IAM_KEY))
            .and(body_string_contains("token=ort_paired"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        let login = scoped(sessions::login(
            State(state.clone()),
            test_headers.clone(),
            StrictJson(serde_json::from_value(json!({"slt":"slt_paired"}))?),
        ))
        .await?
        .0;
        assert_eq!(login.access_token, "oat_paired");
        let mut authenticated_headers = test_headers.clone();
        authenticated_headers.insert("authorization", "Bearer oat_paired".parse()?);
        let status = scoped(sessions::status(
            State(state.clone()),
            authenticated_headers.clone(),
        ))
        .await?
        .0;
        assert_eq!(status["authenticated"], true);
        let organizations = scoped(sessions::organizations(
            State(state.clone()),
            authenticated_headers,
        ))
        .await?
        .0;
        assert_eq!(organizations.len(), 1);
        assert_eq!(organizations[0].as_str(), "paired-org");
        test_headers.insert("idempotency-key", "pairing-regression-refresh".parse()?);
        let refreshed = scoped(sessions::refresh(
            State(state.clone()),
            test_headers.clone(),
            StrictJson(serde_json::from_value(
                json!({"refresh_token":"ort_paired"}),
            )?),
        ))
        .await?
        .0;
        assert_eq!(refreshed.access_token, "oat_paired");
        test_headers.insert("idempotency-key", "pairing-regression-logout".parse()?);
        assert_eq!(
            scoped(sessions::logout(
                State(state.clone()),
                test_headers.clone(),
                StrictJson(serde_json::from_value(json!({"token":"ort_paired"}))?)
            ))
            .await?,
            StatusCode::NO_CONTENT
        );

        let created = scoped(create(
            State(state.clone()),
            owner_headers.clone(),
            StrictJson(CreateInput {
                name: format!("paired-{environment}"),
                description: None,
                iam_test_key: SecretString::from(IAM_KEY),
                iam_app_id: "tos>commit".to_owned(),
                iam_app_secret: SecretString::from(TEST_SECRET),
            }),
        ))
        .await?
        .0;
        let created_response = serde_json::to_string(&created)?;
        assert!(!created_response.contains(TEST_SECRET) && !created_response.contains(IAM_KEY));
        let created_secret: Vec<u8> = sqlx::query_scalar("SELECT iam_app_secret_ciphertext FROM commit.testing_environments WHERE environment_id=$1")
            .bind(created.environment.environment_id).fetch_one(&pool).await?;
        assert_eq!(
            decrypt_iam_key(&created_secret).as_deref(),
            Some(TEST_SECRET)
        );

        let mut manager_headers = control_headers(organization, "admin")?;
        manager_headers.insert(
            "x-test-capabilities",
            action::TEST_ENVIRONMENTS_MANAGE.parse()?,
        );
        let rotated = scoped(pair_iam_credentials(
            State(state.clone()),
            manager_headers,
            Path(environment),
            pair_input("tos>commit", ROTATED_SECRET),
        ))
        .await?
        .0;
        assert_eq!(rotated.version, 3);
        scoped(async {
            request_context::set_testing_scope(Some(request_context::TestingScope {
                id: environment,
                version: paired.version,
            }));
            let mut transaction = pool.begin().await?;
            assert!(matches!(
                postgres::testing::guard(&mut transaction).await,
                Err(AppError::Unauthenticated)
            ));
            transaction.rollback().await?;
            Ok::<_, anyhow::Error>(())
        })
        .await?;
        let forbidden_nested_control = scoped(async {
            request_context::set_testing_scope(Some(request_context::TestingScope {
                id: environment,
                version: rotated.version,
            }));
            pair_iam_credentials(
                State(state.clone()),
                owner_headers,
                Path(environment),
                pair_input("tos>commit", TEST_SECRET),
            )
            .await
        })
        .await;
        assert!(matches!(forbidden_nested_control, Err(AppError::Forbidden)));
        sqlx::query("DELETE FROM commit.testing_environments WHERE environment_id=ANY($1)")
            .bind(vec![environment, created.environment.environment_id])
            .execute(&pool)
            .await?;
        pool.close().await;
        Ok(())
    }
}
