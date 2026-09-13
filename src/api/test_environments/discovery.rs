//! Live application-secret validation and atomic local sandbox materialization.
use super::*;
use silicon_iam_client::models::ApplicationTestingContext;

type ExistingEnvironment = (
    String,
    i64,
    Option<i64>,
    Option<time::OffsetDateTime>,
    String,
);

pub(super) async fn discover(state: &AppState, secret: &str) -> Result<(), AppError> {
    if secret.len() != 47
        || !secret.starts_with("ask_")
        || !secret[4..]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    {
        return Err(AppError::Unauthenticated);
    }
    let sessions = state
        .sessions
        .as_ref()
        .ok_or(AppError::ProviderUnavailable)?;
    let credentials = IamTestingCredentials {
        environment_key: SecretString::from(String::new()),
        app_id: sessions.app_id().to_owned(),
        app_secret: SecretString::from(secret.to_owned()),
    };
    let current = sessions
        .testing_client(&credentials)?
        .applications()
        .testing_context()
        .await
        .map_err(super::super::sessions::map_error)?;
    let meta = validate_metadata(&current, sessions.app_id())?;
    let id = current.environment_id;
    let mut tx = state.pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,41987))")
        .bind(id.to_string())
        .execute(&mut *tx)
        .await?;
    let prior:Option<ExistingEnvironment>=sqlx::query_as("SELECT status,version,iam_control_version,iam_cleaned_at,key_digest FROM commit.testing_environments WHERE environment_id=$1 FOR UPDATE").bind(id).fetch_optional(&mut *tx).await?;
    if let Some((status, version, control, cleaned, _)) = &prior {
        if status != "active" || control.is_some_and(|v| v > meta.version) {
            return Err(AppError::Unauthenticated);
        }
        if *cleaned != meta.cleaned_at {
            sqlx::query("SELECT commit.reset_discovered_testing_environment($1,$2)")
                .bind(id)
                .bind(version)
                .execute(&mut *tx)
                .await?;
        }
    }
    let cipher = encrypt_iam_key(secret).ok_or(AppError::ProviderUnavailable)?;
    let empty = encrypt_iam_key("").ok_or(AppError::ProviderUnavailable)?;
    let hash = digest(secret);
    let key_digest = current
        .webhook_key_digest
        .as_ref()
        .ok_or(AppError::BadGateway)?;
    if key_digest.len() != 64 || !key_digest.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(AppError::BadGateway);
    }
    let changed = prior
        .as_ref()
        .is_none_or(|(_, _, control, cleaned, old_hash)| {
            *control != Some(meta.version) || *cleaned != meta.cleaned_at || *old_hash != hash
        });
    let version:i64=sqlx::query_scalar("INSERT INTO commit.testing_environments(environment_id,organization_id,creator_principal_id,name,description,iam_test_key_digest,iam_test_key_ciphertext,key_digest,key_ciphertext,iam_app_id,iam_app_secret_ciphertext,iam_environment_id,iam_control_version,iam_cleaned_at,iam_owner_org_id,iam_creator_id) VALUES($1,$1,$1,$2,$3,$4,$5,$6,$7,$8,$7,$1,$9,$10,$11,$12) ON CONFLICT(environment_id) DO UPDATE SET name=EXCLUDED.name,description=EXCLUDED.description,iam_test_key_digest=EXCLUDED.iam_test_key_digest,key_digest=EXCLUDED.key_digest,key_ciphertext=EXCLUDED.key_ciphertext,iam_app_secret_ciphertext=EXCLUDED.iam_app_secret_ciphertext,iam_control_version=EXCLUDED.iam_control_version,iam_cleaned_at=EXCLUDED.iam_cleaned_at,version=commit.testing_environments.version+CASE WHEN $13 THEN 1 ELSE 0 END,last_activity_at=clock_timestamp(),updated_at=clock_timestamp() RETURNING version")
        .bind(id).bind(&meta.name).bind(&meta.description).bind(key_digest).bind(empty).bind(&hash).bind(cipher).bind(sessions.app_id()).bind(meta.version).bind(meta.cleaned_at).bind(&meta.org_id).bind(&meta.creator_id).bind(changed).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    request_context::set_environment_key(Some(secret.to_owned()));
    request_context::set_iam_testing_credentials(Some(credentials));
    request_context::set_testing_scope(Some(request_context::TestingScope { id, version }));
    Ok(())
}

fn validate_metadata<'a>(
    current: &'a ApplicationTestingContext,
    app_id: &str,
) -> Result<&'a silicon_iam_client::models::TestingEnvironmentMetadata, AppError> {
    let meta = current.environment.as_ref().ok_or(AppError::BadGateway)?;
    if current.application.app_id != app_id
        || current.environment_id.is_nil()
        || meta.environment_id != current.environment_id
        || meta.version < 1
        || meta.name.trim().is_empty()
        || !matches!(meta.creator_type.as_str(), "carbon" | "silicon")
        || meta.creator_id.is_empty()
    {
        return Err(AppError::Unauthenticated);
    }
    Ok(meta)
}

/// Identifies the selected world without exposing its credential or root key.
pub(crate) async fn selected(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, AppError> {
    resolve_context(&state, &headers).await?;
    let scope = request_context::testing_scope().ok_or(AppError::BadRequest {
        code: "testing_environment_required".into(),
    })?;
    let (name,): (String,)=sqlx::query_as("SELECT name FROM commit.testing_environments WHERE environment_id=$1 AND version=$2 AND status='active'")
        .bind(scope.id).bind(scope.version).fetch_optional(&state.pool).await?.ok_or(AppError::Unauthenticated)?;
    Ok(Json(
        serde_json::json!({"testing":true,"environment_id":scope.id,"name":name}),
    ))
}
