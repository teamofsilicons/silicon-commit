//! Organization-specific delivery preferences and durable bug reports.
use super::{AppState, auth::action, extract::StrictJson};
use crate::{error::AppError, infrastructure::postgres, request_context};
use axum::{Json, extract::State, http::HeaderMap};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "Independent notification subscription switches mirror the wire contract"
)]
pub(crate) struct Preferences {
    email: String,
    #[serde(default = "yes")]
    enabled: bool,
    #[serde(default = "yes")]
    project_completed: bool,
    #[serde(default)]
    project_updates: bool,
    #[serde(default)]
    task_completed: bool,
    #[serde(default)]
    task_assigned: bool,
}
fn yes() -> bool {
    true
}
pub(crate) async fn get(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    let actor = state
        .authenticate(&headers, action::TODOS_LIST, None)
        .await?;
    let row:Option<Value>=sqlx::query_scalar("SELECT to_jsonb(e)-'organization_id'-'principal_id'-'updated_at' FROM commit.email_preferences e WHERE organization_id=$1 AND principal_id=$2").bind(actor.organization_id.into_uuid()).bind(actor.actor.principal_id.into_uuid()).fetch_optional(&state.pool).await?;
    Ok(Json(row.unwrap_or_else(||json!({"email":"","enabled":true,"project_completed":true,"project_updates":false,"task_completed":false,"task_assigned":false}))))
}
pub(crate) async fn put(
    State(state): State<AppState>,
    headers: HeaderMap,
    StrictJson(p): StrictJson<Preferences>,
) -> Result<Json<Value>, AppError> {
    let actor = state
        .authenticate(&headers, action::TODOS_LIST, None)
        .await?;
    if p.email.len() > 254
        || (!p.email.is_empty()
            && (!p.email.contains('@')
                || p.email
                    .chars()
                    .any(|c| c.is_whitespace() || matches!(c, ',' | ';' | '<' | '>'))))
    {
        return Err(AppError::Validation {
            details: json!({"email":"Use one organization email address, or an empty string to remove it."}),
        });
    }
    let mut tx = state.pool.begin().await?;
    postgres::testing::guard(&mut tx).await?;
    postgres::projects::upsert_verified_actor(&mut tx, &actor).await?;
    sqlx::query("INSERT INTO commit.email_preferences(organization_id,principal_id,email,enabled,project_completed,project_updates,task_completed,task_assigned) VALUES($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT(organization_id,principal_id) DO UPDATE SET email=$3,enabled=$4,project_completed=$5,project_updates=$6,task_completed=$7,task_assigned=$8,updated_at=clock_timestamp()")
 .bind(actor.organization_id.into_uuid()).bind(actor.actor.principal_id.into_uuid()).bind(&p.email).bind(p.enabled).bind(p.project_completed).bind(p.project_updates).bind(p.task_completed).bind(p.task_assigned).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(json!(p)))
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Report {
    message: String,
    #[serde(default)]
    pr: Option<String>,
}
pub(crate) async fn report(
    State(state): State<AppState>,
    headers: HeaderMap,
    StrictJson(input): StrictJson<Report>,
) -> Result<Json<Value>, AppError> {
    let actor = state
        .authenticate(&headers, action::TODOS_LIST, None)
        .await?;
    if input.message.trim().is_empty()
        || input.message.len() > 20000
        || input.pr.as_ref().is_some_and(|p| {
            !p.starts_with("https://github.com/teamofsilicons/silicon-commit/pull/")
                || p.len() > 200
        })
    {
        return Err(AppError::Validation {
            details: json!({"report":"Supply 1..20000 bytes of reproduction details and an optional Commit pull request URL."}),
        });
    }
    let key =
        super::auth::optional_header(&headers, "idempotency-key")?.ok_or(AppError::BadRequest {
            code: "idempotency_key_required".into(),
        })?;
    if key.len() > 255 || key.is_empty() {
        return Err(AppError::BadRequest {
            code: "invalid_idempotency_key".into(),
        });
    }
    let hash = hex::encode(Sha256::digest(
        serde_json::to_vec(&input).map_err(|e| AppError::Internal(e.into()))?,
    ));
    let mut tx = state.pool.begin().await?;
    postgres::testing::guard(&mut tx).await?;
    postgres::projects::upsert_verified_actor(&mut tx, &actor).await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,726))")
        .bind(format!(
            "{}:{}",
            actor.organization_id, actor.actor.principal_id
        ))
        .execute(&mut *tx)
        .await?;
    let prior:Option<(uuid::Uuid,String)>=sqlx::query_as("SELECT id,request_hash FROM commit.email_jobs WHERE organization_id=$1 AND principal_id=$2 AND report_key=$3").bind(actor.organization_id.into_uuid()).bind(actor.actor.principal_id.into_uuid()).bind(&key).fetch_optional(&mut *tx).await?;
    let id = if let Some((id, stored)) = prior {
        if stored != hash {
            return Err(AppError::Conflict {
                code: "idempotency_key_reused".into(),
            });
        }
        id
    } else {
        let count:i64=sqlx::query_scalar("SELECT count(*) FROM commit.email_jobs WHERE organization_id=$1 AND principal_id=$2 AND kind='bug_report' AND created_at>clock_timestamp()-interval '1 hour'").bind(actor.organization_id.into_uuid()).bind(actor.actor.principal_id.into_uuid()).fetch_one(&mut *tx).await?;
        if count >= 10 {
            return Err(AppError::Conflict {
                code: "report_hourly_limit".into(),
            });
        }
        let body = format!(
            "{}\n\nReporter: {} in {}\nPR: {}",
            input.message,
            actor.actor.id,
            actor.org_id,
            input.pr.as_deref().unwrap_or("not supplied")
        );
        sqlx::query_scalar("INSERT INTO commit.email_jobs(organization_id,principal_id,kind,recipient,subject,body,report_key,request_hash) VALUES($1,$2,'bug_report','saketdev12@gmail.com,shubhastro2@gmails.com,bugs@teamofsilicons.com','Commit bug report',$3,$4,$5) RETURNING id").bind(actor.organization_id.into_uuid()).bind(actor.actor.principal_id.into_uuid()).bind(body).bind(key).bind(hash).fetch_one(&mut *tx).await?
    };
    tx.commit().await?;
    Ok(Json(
        json!({"id":id,"queued":true,"testing":request_context::testing_scope().is_some(),"repository":"https://github.com/teamofsilicons/silicon-commit"}),
    ))
}
