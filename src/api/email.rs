//! Email delivery preferences (one per account) and durable bug reports.
use super::{AppState, auth::action, extract::StrictJson};
use crate::{
    application::accounts::remember, error::AppError,
    infrastructure::postgres::accounts as account_store,
};
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

/// `GET /api/v1/email-settings`.
///
/// Without a saved preference, the address defaults to the email the Carbon
/// shared with Commit at sign-in (`shared_email`), when it shared one.
pub(crate) async fn get(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    let actor = state
        .authenticate(&headers, action::EMAIL_SETTINGS_READ, None)
        .await?;
    let shared_email = account_store::load(&state.pool, actor.uuid())
        .await?
        .and_then(|account| account.email);
    let row: Option<Value> = sqlx::query_scalar(
        "SELECT jsonb_build_object('email', email, 'enabled', enabled, 'project_completed', project_completed, 'project_updates', project_updates, 'task_completed', task_completed, 'task_assigned', task_assigned) FROM commit.email_preferences WHERE account = $1",
    )
    .bind(actor.uuid().as_str())
    .fetch_optional(&state.pool)
    .await?;
    let saved = row.is_some();
    let mut settings = row.unwrap_or_else(|| {
        json!({
            "email": shared_email.clone().unwrap_or_default(),
            "enabled": true,
            "project_completed": true,
            "project_updates": false,
            "task_completed": false,
            "task_assigned": false
        })
    });
    if let Value::Object(fields) = &mut settings {
        fields.insert("saved".to_owned(), json!(saved));
        fields.insert("shared_email".to_owned(), json!(shared_email));
        if shared_email.is_none() && !saved {
            fields.insert(
                "hint".to_owned(),
                json!("No email was shared with Commit. Set one here, or sign in to Commit again and share your email to use it by default."),
            );
        }
    }
    Ok(Json(settings))
}

/// `PUT /api/v1/email-settings`.
pub(crate) async fn put(
    State(state): State<AppState>,
    headers: HeaderMap,
    StrictJson(p): StrictJson<Preferences>,
) -> Result<Json<Value>, AppError> {
    let actor = state
        .authenticate(&headers, action::EMAIL_SETTINGS_UPDATE, None)
        .await?;
    if p.email.len() > 254
        || (!p.email.is_empty()
            && (!p.email.contains('@')
                || p.email
                    .chars()
                    .any(|c| c.is_whitespace() || matches!(c, ',' | ';' | '<' | '>'))))
    {
        return Err(AppError::Validation {
            details: json!({"email":"Use one email address, or an empty string to remove it."}),
        });
    }
    let mut tx = state.pool.begin().await?;
    remember(&mut tx, &actor, &[]).await?;
    sqlx::query("INSERT INTO commit.email_preferences(account,email,enabled,project_completed,project_updates,task_completed,task_assigned) VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT(account) DO UPDATE SET email=$2,enabled=$3,project_completed=$4,project_updates=$5,task_completed=$6,task_assigned=$7,updated_at=clock_timestamp()")
        .bind(actor.uuid().as_str())
        .bind(&p.email)
        .bind(p.enabled)
        .bind(p.project_completed)
        .bind(p.project_updates)
        .bind(p.task_completed)
        .bind(p.task_assigned)
        .execute(&mut *tx)
        .await?;
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

/// `POST /api/v1/reports`: emails a bug report to the Commit maintainers (10 an hour per account).
pub(crate) async fn report(
    State(state): State<AppState>,
    headers: HeaderMap,
    StrictJson(input): StrictJson<Report>,
) -> Result<Json<Value>, AppError> {
    let actor = state
        .authenticate(&headers, action::REPORTS_CREATE, None)
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
    remember(&mut tx, &actor, &[]).await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,726))")
        .bind(format!("report:{}", actor.uuid()))
        .execute(&mut *tx)
        .await?;
    let prior: Option<(uuid::Uuid, String)> = sqlx::query_as(
        "SELECT id, request_hash FROM commit.email_jobs WHERE account = $1 AND report_key = $2",
    )
    .bind(actor.uuid().as_str())
    .bind(&key)
    .fetch_optional(&mut *tx)
    .await?;
    let id = if let Some((id, stored)) = prior {
        if stored != hash {
            return Err(AppError::Conflict {
                code: "idempotency_key_reused".into(),
            });
        }
        id
    } else {
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM commit.email_jobs WHERE account = $1 AND kind = 'bug_report' AND created_at > clock_timestamp() - interval '1 hour'")
            .bind(actor.uuid().as_str())
            .fetch_one(&mut *tx)
            .await?;
        if count >= 10 {
            return Err(AppError::Conflict {
                code: "report_hourly_limit".into(),
            });
        }
        let reporter = if actor.actor.id.as_str().is_empty() {
            actor.uuid().to_string()
        } else {
            format!("{} ({})", actor.actor.id, actor.uuid())
        };
        let via = actor
            .via_app()
            .map(|app| format!(" via {app}"))
            .unwrap_or_default();
        let body = format!(
            "{}\n\nReporter: {reporter}{via}\nPR: {}",
            input.message,
            input.pr.as_deref().unwrap_or("not supplied")
        );
        sqlx::query_scalar("INSERT INTO commit.email_jobs(account,kind,recipient,subject,body,report_key,request_hash) VALUES($1,'bug_report','saketdev12@gmail.com,shubhastro2@gmails.com,bugs@teamofsilicons.com','Commit bug report',$2,$3,$4) RETURNING id")
            .bind(actor.uuid().as_str())
            .bind(body)
            .bind(key)
            .bind(hash)
            .fetch_one(&mut *tx)
            .await?
    };
    tx.commit().await?;
    Ok(Json(
        json!({"id":id,"queued":true,"repository":"https://github.com/teamofsilicons/silicon-commit"}),
    ))
}
