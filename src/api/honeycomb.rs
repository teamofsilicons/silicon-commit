//! Service-authenticated Honeycomb lifecycle, independent of test sessions.
use super::{AppState, auth::optional_header, extract::StrictJson, test_environments};
use crate::error::AppError;
use axum::{
    Json,
    extract::{Path, State},
    http::HeaderMap,
};
use secrecy::{ExposeSecret as _, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use subtle::ConstantTimeEq as _;
use uuid::Uuid;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Operation {
    operation_id: Uuid,
    environment_id: Uuid,
    org_id: String,
    app_id: String,
    environment_revision: i64,
    generation: i64,
    key_version: i64,
    action: String,
    testing_key: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    snapshot: Value,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    retired_apps: Vec<String>,
}
impl Operation {
    fn validate(&self, app: &str, path: &(String, Uuid, Uuid)) -> Result<(), AppError> {
        if self.org_id != path.0
            || self.environment_id != path.1
            || self.operation_id != path.2
            || self.app_id != app
            || self.org_id.is_empty()
            || self.org_id.len() > 128
            || self.environment_id.is_nil()
            || self.operation_id.is_nil()
            || self.environment_revision < 1
            || self.generation < 1
            || self.key_version < 1
            || self.testing_key.len() != 32
            || !self.testing_key.bytes().all(|b| b.is_ascii_alphanumeric())
            || !matches!(
                self.action.as_str(),
                "prepare"
                    | "import"
                    | "refresh-import"
                    | "rotate-key"
                    | "clean"
                    | "disable"
                    | "restore"
                    | "purge"
                    | "retire-applications"
            )
            || (self.action == "retire-applications"
                && (self.retired_apps.is_empty() || self.retired_apps.iter().any(String::is_empty)))
        {
            return Err(AppError::BadRequest {
                code: "invalid_honeycomb_operation".into(),
            });
        }
        Ok(())
    }
    fn receipt(&self, state: &str) -> Value {
        json!({"operation_id":self.operation_id,"environment_id":self.environment_id,"app_id":self.app_id,"environment_revision":self.environment_revision,"generation":self.generation,"key_version":self.key_version,"retired_apps":self.retired_apps,"state":state})
    }
}
fn authenticate(state: &AppState, headers: &HeaderMap) -> Result<(), AppError> {
    let expected = state
        .honeycomb_token
        .as_ref()
        .ok_or(AppError::Unauthenticated)?;
    let supplied = optional_header(headers, "authorization")?.ok_or(AppError::Unauthenticated)?;
    let supplied = supplied
        .strip_prefix("Bearer ")
        .ok_or(AppError::Unauthenticated)?;
    let actual: [u8; 32] = Sha256::digest(supplied.as_bytes()).into();
    let expected: [u8; 32] = Sha256::digest(expected.expose_secret().as_bytes()).into();
    if !bool::from(actual.ct_eq(&expected)) {
        return Err(AppError::Unauthenticated);
    }
    Ok(())
}
pub(crate) fn configured_token() -> Option<SecretString> {
    std::env::var("COMMIT_HONEYCOMB_SERVICE_TOKEN")
        .ok()
        .filter(|s| s.len() >= 32 && s.bytes().all(|b| b.is_ascii_graphic()))
        .map(SecretString::from)
}
pub(crate) async fn receipt(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((org, id, op)): Path<(String, Uuid, Uuid)>,
) -> Result<Json<Value>, AppError> {
    authenticate(&state, &headers)?;
    let result=sqlx::query_scalar("SELECT o.receipt FROM commit.honeycomb_operations o JOIN commit.honeycomb_environments e USING(environment_id) WHERE e.org_id=$1 AND e.environment_id=$2 AND o.operation_id=$3")
 .bind(org).bind(id).bind(op).fetch_optional(&state.pool).await?.ok_or(AppError::NotFound)?;
    Ok(Json(result))
}
pub(crate) async fn apply(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(path): Path<(String, Uuid, Uuid)>,
    StrictJson(op): StrictJson<Operation>,
) -> Result<Json<Value>, AppError> {
    authenticate(&state, &headers)?;
    let app = state
        .sessions
        .as_ref()
        .ok_or(AppError::ProviderUnavailable)?
        .app_id();
    op.validate(app, &path)?;
    Ok(Json(execute(&state.pool, &op).await?))
}
fn conflict(code: &'static str) -> AppError {
    AppError::Conflict { code: code.into() }
}
#[derive(sqlx::FromRow)]
struct Prior {
    org_id: String,
    app_id: String,
    environment_revision: i64,
    generation: i64,
    key_version: i64,
    state: String,
    operation_id: Uuid,
    root_key_digest: String,
}
async fn execute(pool: &sqlx::PgPool, op: &Operation) -> Result<Value, AppError> {
    let hash = hex::encode(Sha256::digest(
        serde_json::to_vec(op).map_err(|e| AppError::Internal(e.into()))?,
    ));
    let root_hash = test_environments::digest(&op.testing_key);
    let cipher =
        test_environments::encrypt_iam_key(&op.testing_key).ok_or(AppError::ProviderUnavailable)?;
    let mut tx = pool.begin().await?;
    // Match discovery's lock so an old IAM response cannot cross a lifecycle change.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,41987))")
        .bind(op.environment_id.to_string())
        .execute(&mut *tx)
        .await?;
    let prior:Option<Prior>=sqlx::query_as("SELECT org_id,app_id,environment_revision,generation,key_version,state,operation_id,root_key_digest FROM commit.honeycomb_environments WHERE environment_id=$1 FOR UPDATE").bind(op.environment_id).fetch_optional(&mut *tx).await?;
    if let Some(p) = &prior {
        if p.org_id != op.org_id || p.app_id != op.app_id {
            return Err(conflict("honeycomb_environment_owner_mismatch"));
        }
        let replay:Option<(String,Value)>=sqlx::query_as("SELECT request_hash,receipt FROM commit.honeycomb_operations WHERE environment_id=$1 AND operation_id=$2").bind(op.environment_id).bind(op.operation_id).fetch_optional(&mut *tx).await?;
        if let Some((stored, receipt)) = replay {
            if stored != hash {
                return Err(conflict("honeycomb_operation_reused"));
            }
            if receipt["state"] == "completed" {
                return Ok(receipt);
            }
            if p.operation_id != op.operation_id {
                return Err(conflict("honeycomb_operation_superseded"));
            }
        } else {
            let reimport =
                p.state == "retired" && matches!(op.action.as_str(), "prepare" | "import");
            if p.state == "purged"
                || op.environment_revision <= p.environment_revision
                || op.generation < p.generation
                || op.key_version < p.key_version
                || (op.generation != p.generation && op.action != "clean" && !reimport)
                || (op.action == "clean" && op.generation <= p.generation)
                || (op.key_version != p.key_version && op.action != "rotate-key" && !reimport)
                || (op.action == "rotate-key" && op.key_version <= p.key_version)
                || (op.key_version == p.key_version && root_hash != p.root_key_digest)
                || (p.state == "disabled"
                    && !matches!(
                        op.action.as_str(),
                        "restore" | "disable" | "clean" | "purge" | "rotate-key"
                    ))
                || (p.state == "retired" && !reimport && op.action != "purge")
                || matches!(p.state.as_str(), "pending" | "failed")
            {
                return Err(conflict("stale_honeycomb_operation"));
            }
        }
    } else if !matches!(op.action.as_str(), "prepare" | "import") {
        return Err(conflict("honeycomb_prepare_required"));
    }
    let retry = prior
        .as_ref()
        .is_some_and(|p| p.operation_id == op.operation_id);
    // Persist the barrier first: a crash leaves requests disabled and the exact operation retryable.
    sqlx::query("INSERT INTO commit.honeycomb_environments(environment_id,org_id,app_id,environment_revision,generation,key_version,state,operation_id,root_key_ciphertext,root_key_digest) VALUES($1,$2,$3,$4,$5,$6,'pending',$7,$8,$9) ON CONFLICT(environment_id) DO UPDATE SET environment_revision=$4,generation=$5,key_version=$6,state='pending',operation_id=$7,root_key_ciphertext=$8,root_key_digest=$9,updated_at=clock_timestamp()")
 .bind(op.environment_id).bind(&op.org_id).bind(&op.app_id).bind(op.environment_revision).bind(op.generation).bind(op.key_version).bind(op.operation_id).bind(cipher).bind(root_hash).execute(&mut *tx).await?;
    if !retry {
        sqlx::query(
            "UPDATE commit.honeycomb_environments SET resume_state=$2 WHERE environment_id=$1",
        )
        .bind(op.environment_id)
        .bind(if prior.as_ref().is_some_and(|p| p.state == "disabled") {
            "disabled"
        } else {
            "active"
        })
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE commit.honeycomb_environments SET last_activity_at=NULL,activity_reported_at=NULL,cleared_at=CASE WHEN $2 THEN clock_timestamp() ELSE cleared_at END,require_iam_clean=CASE WHEN $2 THEN true ELSE require_iam_clean END,iam_cleaned_before=CASE WHEN $2 THEN (SELECT iam_cleaned_at FROM commit.testing_environments WHERE environment_id=$1) ELSE iam_cleaned_before END WHERE environment_id=$1")
  .bind(op.environment_id).bind(op.action=="clean").execute(&mut *tx).await?;
        sqlx::query("UPDATE commit.testing_environments SET status='deleted',version=version+1,purge_after=NULL,updated_at=clock_timestamp() WHERE environment_id=$1").bind(op.environment_id).execute(&mut *tx).await?;
    }
    sqlx::query("INSERT INTO commit.honeycomb_operations(environment_id,operation_id,request_hash,receipt) VALUES($1,$2,$3,$4) ON CONFLICT(environment_id,operation_id) DO UPDATE SET receipt=EXCLUDED.receipt")
 .bind(op.environment_id).bind(op.operation_id).bind(hash).bind(op.receipt("pending")).execute(&mut *tx).await?;
    tx.commit().await?;
    let retires = op.action == "retire-applications" && op.retired_apps.contains(&op.app_id);
    let mut tx = pool.begin().await?;
    let finish = sqlx::query("SELECT commit.finish_honeycomb_operation($1,$2,$3,$4)")
        .bind(op.environment_id)
        .bind(op.operation_id)
        .bind(&op.action)
        .bind(retires)
        .execute(&mut *tx)
        .await;
    if finish.is_err() {
        tx.rollback().await?;
        sqlx::query("UPDATE commit.honeycomb_operations SET receipt=jsonb_set(receipt,'{state}','\"failed\"') WHERE environment_id=$1 AND operation_id=$2 AND receipt->>'state'='pending'").bind(op.environment_id).bind(op.operation_id).execute(pool).await?;
        return Ok(sqlx::query_scalar("SELECT receipt FROM commit.honeycomb_operations WHERE environment_id=$1 AND operation_id=$2").bind(op.environment_id).bind(op.operation_id).fetch_one(pool).await?);
    }
    tx.commit().await?;
    Ok(op.receipt("completed"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::DatabaseSettings, infrastructure::postgres, request_context};
    use std::{num::NonZeroU32, time::Duration};

    async fn pool() -> anyhow::Result<Option<sqlx::PgPool>> {
        let Ok(url) = std::env::var("COMMIT_TEST_DATABASE_URL") else {
            return Ok(None);
        };
        let settings = DatabaseSettings {
            url: SecretString::from(url),
            max_connections: NonZeroU32::new(5)
                .ok_or_else(|| anyhow::anyhow!("invalid fixed connection count"))?,
            min_connections: 0,
            acquire_timeout: Duration::from_secs(5),
            statement_timeout: Duration::from_secs(10),
        };
        let p = postgres::connect_migrator(&settings, "commit-honeycomb-tests").await?;
        let owner: String = sqlx::query_scalar("SELECT current_user::text")
            .fetch_one(&p)
            .await?;
        postgres::migrate(&p, &owner).await?;
        Ok(Some(p))
    }
    fn operation() -> Operation {
        Operation {
            operation_id: Uuid::new_v4(),
            environment_id: Uuid::new_v4(),
            org_id: "test-owner".into(),
            app_id: "tos>commit".into(),
            environment_revision: 1,
            generation: 1,
            key_version: 1,
            action: "prepare".into(),
            testing_key: "A".repeat(32),
            name: Some("Sandbox".into()),
            description: None,
            snapshot: json!({}),
            reason: "requested".into(),
            retired_apps: vec![],
        }
    }
    fn advance(op: &mut Operation, action: &str) {
        op.operation_id = Uuid::new_v4();
        op.environment_revision += 1;
        op.action = action.into();
    }
    async fn seed_environment(p: &sqlx::PgPool, id: Uuid) -> anyhow::Result<()> {
        sqlx::query("INSERT INTO commit.testing_environments(environment_id,organization_id,creator_principal_id,name,iam_test_key_digest,key_digest,iam_test_key_ciphertext,iam_environment_id) VALUES($1,$1,$1,$2,$3,$4,$5,$1)")
  .bind(id).bind(format!("sandbox-{id}")).bind("a".repeat(64)).bind(test_environments::digest(&id.to_string())).bind(test_environments::encrypt_iam_key("").ok_or_else(||anyhow::anyhow!("test encryption key is required"))?).execute(p).await?;
        Ok(())
    }
    #[test]
    fn operation_validation_rejects_wrong_routes_and_secrets() {
        let mut op = operation();
        assert!(
            op.validate(
                "tos>commit",
                &(op.org_id.clone(), op.environment_id, op.operation_id)
            )
            .is_ok()
        );
        assert!(
            op.validate(
                "other>app",
                &(op.org_id.clone(), op.environment_id, op.operation_id)
            )
            .is_err()
        );
        assert!(
            op.validate(
                "tos>commit",
                &(op.org_id.clone(), op.environment_id, Uuid::new_v4())
            )
            .is_err()
        );
        op.testing_key = "bad".into();
        assert!(
            op.validate(
                "tos>commit",
                &(op.org_id.clone(), op.environment_id, op.operation_id)
            )
            .is_err()
        );
        assert!(!op.receipt("completed").to_string().contains("testing_key"));
    }
    #[tokio::test]
    async fn lifecycle_replay_cleanup_fences_and_terminal_removal() -> anyhow::Result<()> {
        let Some(pool) = pool().await? else {
            return Ok(());
        };
        let mut op = operation();
        let id = op.environment_id;
        seed_environment(&pool, id).await?;
        let first = execute(&pool, &op).await?;
        assert_eq!(first["state"], "completed");
        assert_eq!(execute(&pool, &op).await?, first);
        let first_op = op.operation_id;
        op.reason = "altered".into();
        assert!(execute(&pool, &op).await.is_err());
        op.reason = "requested".into();
        let local_version: i64 = sqlx::query_scalar(
            "SELECT version FROM commit.testing_environments WHERE environment_id=$1",
        )
        .bind(id)
        .fetch_one(&pool)
        .await?;
        let org = Uuid::new_v4();
        let other = Uuid::new_v4();
        sqlx::query("INSERT INTO commit.testing_organizations VALUES($1,$2,$2)")
            .bind(id)
            .bind(org)
            .execute(&pool)
            .await?;
        sqlx::query("INSERT INTO commit.organization_projection(organization_id,org_id,environment_id) VALUES($1,$3,$2),($4,$5,NULL)").bind(org).bind(id).bind(format!("sandbox-{org}")).bind(other).bind(format!("prod-{other}")).execute(&pool).await?;
        sqlx::query("INSERT INTO commit.email_jobs(organization_id,kind,recipient,subject,body) VALUES($1,'bug_report','test@example.com','test','private'),($2,'bug_report','test@example.com','prod','retain')").bind(org).bind(other).execute(&pool).await?;
        advance(&mut op, "clean");
        op.generation += 1;
        assert_eq!(execute(&pool, &op).await?["state"], "completed");
        assert_eq!(execute(&pool, &op).await?["state"], "completed");
        let counts:(i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM commit.email_jobs WHERE organization_id=$1),(SELECT count(*) FROM commit.email_jobs WHERE organization_id=$2)").bind(org).bind(other).fetch_one(&pool).await?;
        assert_eq!(counts, (0, 1));
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM commit.testing_environments WHERE environment_id=$1"
            )
            .bind(id)
            .fetch_one(&pool)
            .await?,
            1
        );
        request_context::scope("stale-write".into(), async {
            request_context::set_testing_scope(Some(request_context::TestingScope {
                id,
                version: local_version,
            }));
            let mut tx = pool.begin().await?;
            assert!(postgres::testing::guard(&mut tx).await.is_err());
            Ok::<_, anyhow::Error>(())
        })
        .await?;
        advance(&mut op, "disable");
        assert_eq!(execute(&pool, &op).await?["state"], "completed");
        // Cleanup while disabled must retain disabled state even after a retry.
        advance(&mut op, "clean");
        op.generation += 1;
        execute(&pool, &op).await?;
        execute(&pool, &op).await?;
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT state FROM commit.honeycomb_environments WHERE environment_id=$1"
            )
            .bind(id)
            .fetch_one(&pool)
            .await?,
            "disabled"
        );
        advance(&mut op, "restore");
        execute(&pool, &op).await?;
        advance(&mut op, "rotate-key");
        op.key_version += 1;
        op.testing_key = "B".repeat(32);
        execute(&pool, &op).await?;
        let cipher: Vec<u8> = sqlx::query_scalar(
            "SELECT root_key_ciphertext FROM commit.honeycomb_environments WHERE environment_id=$1",
        )
        .bind(id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(
            test_environments::decrypt_iam_key(&cipher).as_deref(),
            Some(op.testing_key.as_str())
        );
        assert!(!cipher.windows(32).any(|w| w == op.testing_key.as_bytes()));
        advance(&mut op, "purge");
        execute(&pool, &op).await?;
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM commit.honeycomb_operations WHERE environment_id=$1 AND operation_id=$2").bind(id).bind(first_op).fetch_one(&pool).await?,1);
        advance(&mut op, "restore");
        assert!(execute(&pool, &op).await.is_err());
        sqlx::query("DELETE FROM commit.email_jobs WHERE organization_id=$1")
            .bind(other)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM commit.organization_projection WHERE organization_id=$1")
            .bind(other)
            .execute(&pool)
            .await?;
        Ok(())
    }
    #[tokio::test]
    async fn pending_operations_recover_and_activity_cannot_cross_clean() -> anyhow::Result<()> {
        let Some(pool) = pool().await? else {
            return Ok(());
        };
        let mut op = operation();
        let id = op.environment_id;
        seed_environment(&pool, id).await?;
        execute(&pool, &op).await?;
        let version: i64 = sqlx::query_scalar(
            "SELECT version FROM commit.testing_environments WHERE environment_id=$1",
        )
        .bind(id)
        .fetch_one(&pool)
        .await?;
        sqlx::query("SELECT commit.mark_honeycomb_activity($1,$2)")
            .bind(id)
            .bind(version)
            .execute(&pool)
            .await?;
        let activity:time::OffsetDateTime=sqlx::query_scalar("SELECT last_activity_at FROM commit.honeycomb_activity_outbox() WHERE environment_id=$1").bind(id).fetch_one(&pool).await?;
        // Simulate interruption after the durable barrier, before finishing.
        sqlx::query(
            "UPDATE commit.honeycomb_environments SET state='pending' WHERE environment_id=$1",
        )
        .bind(id)
        .execute(&pool)
        .await?;
        sqlx::query("UPDATE commit.honeycomb_operations SET receipt=jsonb_set(receipt,'{state}','\"failed\"') WHERE environment_id=$1 AND operation_id=$2").bind(id).bind(op.operation_id).execute(&pool).await?;
        assert_eq!(execute(&pool, &op).await?["state"], "completed");
        advance(&mut op, "clean");
        op.generation += 1;
        execute(&pool, &op).await?;
        sqlx::query("SELECT commit.ack_honeycomb_activity($1,1,1,$2)")
            .bind(id)
            .bind(activity)
            .execute(&pool)
            .await?;
        let stale:bool=sqlx::query_scalar("SELECT last_activity_at IS NULL AND activity_reported_at IS NULL FROM commit.honeycomb_environments WHERE environment_id=$1").bind(id).fetch_one(&pool).await?;
        assert!(stale);
        let receipt = op.receipt("completed");
        let mut wrong = op;
        advance(&mut wrong, "rotate-key");
        wrong.generation += 1;
        wrong.key_version += 1;
        assert!(execute(&pool, &wrong).await.is_err());
        assert_eq!(receipt["generation"], 2);
        Ok(())
    }
    #[tokio::test]
    async fn discovery_rejects_a_response_that_crosses_disable_and_restore() -> anyhow::Result<()> {
        use crate::{
            api::sessions::SessionService,
            config::{AuthenticationMode, IamSettings},
            infrastructure::clients::iam::TrustedHeaderIdentityProvider,
        };
        use std::sync::Arc;
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};
        let Some(pool) = pool().await? else {
            return Ok(());
        };
        let server = MockServer::start().await;
        let mut state = crate::api::tests::test_state_with_pool(
            Arc::new(TrustedHeaderIdentityProvider::default()),
            pool.clone(),
        )?;
        state.sessions = Some(Arc::new(SessionService::new(
            &IamSettings {
                mode: AuthenticationMode::Iam,
                base_url: server.uri().parse()?,
                app_id: Some("tos>commit".into()),
                app_secret: Some(SecretString::from(
                    "production-credential-must-stay-out-of-sandbox",
                )),
                audience: "tos>commit".into(),
                webhook_secret: None,
                webhook_key_version: 1,
            },
            Duration::from_secs(2),
        )?));
        state.honeycomb_token = Some(SecretString::from(
            "service-credential-long-enough-for-control-plane",
        ));
        let mut headers = HeaderMap::new();
        assert!(authenticate(&state, &headers).is_err());
        headers.insert(
            "authorization",
            "Bearer AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".parse()?,
        );
        assert!(authenticate(&state, &headers).is_err());
        headers.insert(
            "authorization",
            "Bearer service-credential-long-enough-for-control-plane".parse()?,
        );
        assert!(authenticate(&state, &headers).is_ok());
        headers.append("authorization", "Bearer duplicate".parse()?);
        assert!(authenticate(&state, &headers).is_err());
        let mut op = operation();
        execute(&pool, &op).await?;
        let id = op.environment_id;
        let secret = format!("ask_{}{}", id.simple(), "z".repeat(11));
        let body = json!({"environment_id":id,"application":{"app_id":"tos>commit","base_url":"https://backend.commit.teamofsilicons.com","app_scope":{"iam":[],"external":[]},"webhook_scope":[],"testing_idle_days":15},"environment":{"environment_id":id,"org_id":op.org_id,"name":"Lifecycle sandbox","version":1,"key_generation":1,"cleaned_at":null,"created_at":"2026-01-01T00:00:00Z","creator_type":"carbon","creator_id":"owner"},"webhook_key_digest":test_environments::digest(&op.testing_key)});
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&body))
            .mount(&server)
            .await;
        request_context::scope(
            "discover".into(),
            test_environments::discovery::discover(&state, &secret),
        )
        .await?;
        server.reset().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(&body)
                    .set_delay(Duration::from_millis(100)),
            )
            .mount(&server)
            .await;
        let copy = state.clone();
        let key = secret.clone();
        let inflight = tokio::spawn(async move {
            request_context::scope(
                "inflight".into(),
                test_environments::discovery::discover(&copy, &key),
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if !server
                    .received_requests()
                    .await
                    .unwrap_or_default()
                    .is_empty()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await?;
        advance(&mut op, "disable");
        execute(&pool, &op).await?;
        advance(&mut op, "restore");
        execute(&pool, &op).await?;
        assert!(inflight.await?.is_err());
        // A new live IAM request after restoration is valid.
        request_context::scope(
            "restored".into(),
            test_environments::discovery::discover(&state, &secret),
        )
        .await?;
        advance(&mut op, "clean");
        op.generation += 1;
        execute(&pool, &op).await?;
        // Even a freshly received context cannot resurrect a pre-clean IAM world.
        assert!(
            request_context::scope(
                "old-clean".into(),
                test_environments::discovery::discover(&state, &secret)
            )
            .await
            .is_err()
        );
        Ok(())
    }
    #[tokio::test]
    async fn failed_cleanup_rolls_back_content_and_exact_retry_recovers() -> anyhow::Result<()> {
        let Some(pool) = pool().await? else {
            return Ok(());
        };
        let mut op = operation();
        let id = op.environment_id;
        seed_environment(&pool, id).await?;
        execute(&pool, &op).await?;
        let retained = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO commit.telemetry_events(id,environment_id,event) VALUES($1,$2,'{}')",
        )
        .bind(retained)
        .bind(id)
        .execute(&pool)
        .await?;
        // Force failure on the last state update, after the cleanup has executed.
        let constraint = format!("failure_{}", id.simple());
        sqlx::query(sqlx::AssertSqlSafe(format!("ALTER TABLE commit.honeycomb_environments ADD CONSTRAINT {constraint} CHECK(environment_id<>'{id}'::uuid OR state<>'active') NOT VALID"))).execute(&pool).await?;
        advance(&mut op, "clean");
        op.generation += 1;
        assert_eq!(execute(&pool, &op).await?["state"], "failed");
        let row:(String,String,i64)=sqlx::query_as("SELECT h.state,e.status,(SELECT count(*) FROM commit.telemetry_events WHERE id=$2) FROM commit.honeycomb_environments h JOIN commit.testing_environments e USING(environment_id) WHERE environment_id=$1").bind(id).bind(retained).fetch_one(&pool).await?;
        assert_eq!(row, ("pending".into(), "deleted".into(), 1));
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "ALTER TABLE commit.honeycomb_environments DROP CONSTRAINT {constraint}"
        )))
        .execute(&pool)
        .await?;
        assert_eq!(execute(&pool, &op).await?["state"], "completed");
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM commit.telemetry_events WHERE id=$1"
            )
            .bind(retained)
            .fetch_one(&pool)
            .await?,
            0
        );
        Ok(())
    }
}
