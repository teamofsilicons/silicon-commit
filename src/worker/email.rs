//! Bounded durable Postmark delivery; sandbox messages are simulated in SQL.
use secrecy::{ExposeSecret as _, SecretString};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::time::Duration;
/// Processes one queued message without logging addresses, content or secrets.
pub(super) async fn process_once(pool: &PgPool) -> anyhow::Result<usize> {
    let mut tx = pool.begin().await?;
    let job: Option<Value> = sqlx::query_scalar("SELECT commit.claim_email()")
        .fetch_optional(&mut *tx)
        .await?;
    let Some(job) = job else {
        return Ok(0);
    };
    if job["simulated"] == true {
        tx.commit().await?;
        return Ok(1);
    }
    let id: uuid::Uuid = job["id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("invalid email job"))?
        .parse()?;
    let token = std::env::var("COMMIT_POSTMARK_SERVER_TOKEN")
        .ok()
        .filter(|v| !v.is_empty())
        .map(SecretString::from);
    let delivered = if let Some(token) = token {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let response=http.post("https://api.postmarkapp.com/email").header("X-Postmark-Server-Token",token.expose_secret()).header("Accept","application/json").json(&json!({"From":"commit@teamofsilicons.com","To":job["recipient"],"Subject":job["subject"],"TextBody":job["body"],"MessageStream":"outbound","TrackOpens":false,"TrackLinks":"None","Metadata":{"commit_job_id":id.to_string()}})).send().await;
        match response {
            Ok(r) if r.status().is_success() => {
                r.json::<Value>().await.is_ok_and(|v| v["ErrorCode"] == 0)
            }
            _ => false,
        }
    } else {
        false
    };
    sqlx::query("UPDATE commit.email_jobs SET attempts=attempts+1,status=CASE WHEN $2 THEN 'delivered' WHEN attempts>=11 THEN 'failed' ELSE 'pending' END,next_attempt_at=clock_timestamp()+interval '5 minutes' WHERE id=$1").bind(id).bind(delivered).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(1)
}
