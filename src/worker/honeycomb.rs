//! Durable, generation-bound activity reports for Honeycomb retention decisions.
use crate::api::test_environments::decrypt_iam_key;
use serde_json::json;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(sqlx::FromRow)]
struct Activity {
    environment_id: Uuid,
    app_id: String,
    generation: i64,
    key_version: i64,
    root_key_ciphertext: Vec<u8>,
    last_activity_at: OffsetDateTime,
}
pub(super) async fn process_once(pool: &PgPool) -> anyhow::Result<usize> {
    let Ok(origin) = std::env::var("COMMIT_HONEYCOMB_URL") else {
        return Ok(0);
    };
    let origin: url::Url = origin
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid Honeycomb origin"))?;
    anyhow::ensure!(
        origin.scheme() == "https"
            && origin.username().is_empty()
            && origin.password().is_none()
            && origin.query().is_none()
            && origin.fragment().is_none(),
        "Honeycomb requires an HTTPS origin without credentials or query parameters"
    );
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let rows: Vec<Activity> = sqlx::query_as("SELECT * FROM commit.honeycomb_activity_outbox()")
        .fetch_all(pool)
        .await?;
    let mut count = 0;
    for row in rows {
        let Some(key) = decrypt_iam_key(&row.root_key_ciphertext) else {
            continue;
        };
        let mut endpoint = origin.join("/api/v1/")?;
        endpoint
            .path_segments_mut()
            .map_err(|()| anyhow::anyhow!("invalid Honeycomb origin"))?
            .pop_if_empty()
            .extend([
                "environments",
                &row.environment_id.to_string(),
                "apps",
                &row.app_id,
                "activity",
            ]);
        let response = http
            .post(endpoint)
            .header("X-Testing-Environment-Key", key)
            .header(
                "Idempotency-Key",
                format!(
                    "commit:{}:{}:{}:{}",
                    row.environment_id,
                    row.generation,
                    row.key_version,
                    row.last_activity_at.unix_timestamp_nanos()
                ),
            )
            .json(&json!({"generation":row.generation,"key_version":row.key_version}))
            .send()
            .await;
        if response.is_ok_and(|r| r.status().is_success()) {
            sqlx::query("SELECT commit.ack_honeycomb_activity($1,$2,$3,$4)")
                .bind(row.environment_id)
                .bind(row.generation)
                .bind(row.key_version)
                .bind(row.last_activity_at)
                .execute(pool)
                .await?;
            count += 1;
        }
    }
    Ok(count)
}
