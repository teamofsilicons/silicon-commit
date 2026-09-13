//! Exports production diagnostics to the dedicated Commit Space Station table.
use sqlx::PgPool;
use std::time::Duration;
/// Flushes a bounded batch; test rows never enter the production Space Station table.
pub(super) async fn process_once(pool: &PgPool) -> anyhow::Result<usize> {
    if std::env::var("COMMIT_TELEMETRY").is_ok_and(|s| matches!(s.as_str(), "off" | "false" | "0"))
    {
        return Ok(0);
    }
    let Ok(key) = std::env::var("COMMIT_TELEMETRY_TABLE_KEY") else {
        return Ok(0);
    };
    if !key.starts_with("table-committelemetry-") {
        return Err(anyhow::anyhow!(
            "configure the dedicated committelemetry table key"
        ));
    }
    let mut tx = pool.begin().await?;
    let rows:Vec<(uuid::Uuid,serde_json::Value)>=sqlx::query_as("SELECT id,event FROM commit.telemetry_events WHERE environment_id IS NULL AND exported_at IS NULL ORDER BY created_at LIMIT 100 FOR UPDATE SKIP LOCKED").fetch_all(&mut *tx).await?;
    if rows.is_empty() {
        return Ok(0);
    }
    let ids = rows.iter().map(|(id, _)| *id).collect::<Vec<_>>();
    let sent = tokio::task::spawn_blocking(move || {
        let home = std::env::var("COMMIT_TELEMETRY_HOME")
            .unwrap_or_else(|_| "/tmp/commit-telemetry".into());
        let Ok(client) = space_station::SpaceClient::builder(&key)
            .home(home)
            .url("https://backend.spacestation.teamofsilicons.com")
            .flush_timeout(Duration::from_secs(3))
            .on_error(|_| {})
            .build()
        else {
            return false;
        };
        for (id, mut event) in rows {
            event["event_id"] = serde_json::json!(id);
            client.record(event);
        }
        client.flush()
    })
    .await?;
    if sent {
        sqlx::query(
            "UPDATE commit.telemetry_events SET exported_at=clock_timestamp() WHERE id=ANY($1)",
        )
        .bind(&ids)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(if sent { ids.len() } else { 0 })
}
