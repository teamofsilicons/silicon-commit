//! Organization-owned testing-environment lifecycle metadata.

use super::{AppState, auth::action, extract::StrictJson};
use crate::error::AppError;
use axum::{
    Json,
    extract::{Path, State},
    http::HeaderMap,
};
use chacha20poly1305::{
    ChaCha20Poly1305, Nonce,
    aead::{Aead, KeyInit},
};
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
    let row = sqlx::query_as::<_, (Uuid, i64, Vec<u8>)>("UPDATE commit.testing_environments SET last_activity_at=clock_timestamp() WHERE key_digest=$1 AND status='active' RETURNING environment_id,version,iam_test_key_ciphertext")
        .bind(digest(key)).fetch_optional(pool).await.map_err(internal)?.ok_or(AppError::Unauthenticated)?;
    let iam_key = decrypt_iam_key(&row.2).ok_or(AppError::ProviderUnavailable)?;
    crate::request_context::set_environment_key(Some(key.to_owned()));
    crate::request_context::set_iam_environment_key(Some(iam_key));
    crate::request_context::set_testing_scope(Some(crate::request_context::TestingScope {
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
    pub iam_test_key: String,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub(crate) struct Environment {
    pub environment_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub status: String,
    pub version: i64,
    pub deleted_at: Option<time::OffsetDateTime>,
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
    let actor = state
        .authenticate(&headers, action::TEST_ENVIRONMENTS_CREATE, None)
        .await?;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM commit.testing_environments WHERE organization_id=$1 AND status='active'").bind(actor.organization_id.as_uuid()).fetch_one(&state.pool).await.map_err(internal)?;
    if count >= 100 {
        return Err(AppError::Conflict {
            code: "test_environment_limit".into(),
        });
    }
    if input.iam_test_key.len() != 32
        || !input
            .iam_test_key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric())
    {
        return Err(AppError::Validation {
            details: serde_json::json!({"fields":[{"field":"iam_test_key","message":"must be exactly 32 alphanumeric characters"}]}),
        });
    }
    let key = generate_key();
    let commit_key_digest = digest(&key);
    let id = Uuid::now_v7();
    let iam_ciphertext = encrypt_iam_key(&input.iam_test_key).ok_or(AppError::Internal(
        anyhow::anyhow!("test environment encryption is not configured"),
    ))?;
    let key_ciphertext = encrypt_iam_key(&key).ok_or(AppError::Internal(anyhow::anyhow!(
        "test environment encryption is not configured"
    )))?;
    let environment = sqlx::query_as::<_, Environment>("INSERT INTO commit.testing_environments (environment_id,organization_id,creator_principal_id,name,description,iam_test_key_digest,iam_test_key_ciphertext,key_digest,key_ciphertext) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9) RETURNING environment_id,name,description,status,version,deleted_at,purge_after")
        .bind(id).bind(actor.organization_id.as_uuid()).bind(actor.actor.principal_id.as_uuid()).bind(input.name.trim()).bind(input.description.as_deref().map(str::trim)).bind(digest(&input.iam_test_key)).bind(iam_ciphertext).bind(commit_key_digest).bind(key_ciphertext).fetch_one(&state.pool).await.map_err(|e| if let sqlx::Error::Database(db)=&e && db.constraint().is_some_and(|c| c=="testing_environments_organization_id_name_key") { AppError::Conflict { code: "test_environment_name_taken".into() } } else { internal(e) })?;
    Ok(Json(Created { environment, key }))
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
