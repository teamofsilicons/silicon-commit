//! Hook notification outbox delivery with bounded leases and retry state.

use std::{sync::Arc, time::Duration};

use sqlx::{FromRow, PgPool};
use time::OffsetDateTime;
use tokio::task::JoinSet;
use uuid::Uuid;

use crate::{
    application::ports::{HookEvent, HookPublishError, HookPublisher},
    config::WorkerSettings,
    domain::{ActorId, PublicOrganizationId},
};

/// Claims and delivers durable notification events for one worker replica.
#[derive(Clone)]
pub struct OutboxProcessor {
    pool: PgPool,
    publisher: Arc<dyn HookPublisher>,
    settings: WorkerSettings,
    worker_id: String,
}

impl OutboxProcessor {
    /// Creates a processor with a unique lease owner identifier.
    #[must_use]
    pub fn new(pool: PgPool, publisher: Arc<dyn HookPublisher>, settings: WorkerSettings) -> Self {
        Self {
            pool,
            publisher,
            settings,
            worker_id: format!("commit-worker-{}", Uuid::now_v7()),
        }
    }

    /// Claims and attempts one bounded event batch.
    ///
    /// # Errors
    ///
    /// Returns a database error when claiming or recording results fails.
    /// Provider failures are converted into retry/dead-letter state.
    pub async fn process_once(&self) -> anyhow::Result<usize> {
        self.recover_expired_or_exhausted().await?;
        let claims = self.claim().await?;
        let count = claims.len();
        let mut claims = claims.into_iter();
        let mut deliveries = JoinSet::new();
        let concurrency = self.settings.delivery_concurrency.get().min(count);
        for claim in claims.by_ref().take(concurrency) {
            self.spawn_delivery(&mut deliveries, claim);
        }

        while let Some(result) = deliveries.join_next().await {
            let (claim, delivery) = result?;
            match delivery {
                Ok(()) => self.mark_delivered(&claim).await?,
                Err(error) => self.mark_failed(&claim, error).await?,
            }
            if let Some(next_claim) = claims.next() {
                self.spawn_delivery(&mut deliveries, next_claim);
            }
        }
        Ok(count)
    }

    fn spawn_delivery(
        &self,
        deliveries: &mut JoinSet<(ClaimedEvent, Result<(), HookPublishError>)>,
        claim: ClaimedEvent,
    ) {
        let publisher = Arc::clone(&self.publisher);
        deliveries.spawn(async move {
            let result = publisher.publish(&claim.event).await;
            (claim, result)
        });
    }

    async fn recover_expired_or_exhausted(&self) -> Result<(), sqlx::Error> {
        let limit = i64::try_from(self.settings.batch_size.get()).unwrap_or(i64::MAX);
        let max_attempts = i32::from(self.settings.max_attempts.get());
        let dead_letter_retention =
            i64::try_from(self.settings.dead_letter_outbox_retention.as_secs()).unwrap_or(i64::MAX);
        sqlx::query(
            r#"
            WITH candidate AS (
                SELECT event.id
                FROM commit.outbox_events AS event
                WHERE (
                        event.status = 'in_flight'
                        AND event.lease_expires_at <= transaction_timestamp()
                    )
                   OR (
                        event.status = 'pending'
                        AND event.attempt_count >= $2
                    )
                ORDER BY COALESCE(event.lease_expires_at, event.available_at), event.id
                FOR UPDATE SKIP LOCKED
                LIMIT $1
            )
            UPDATE commit.outbox_events AS event
            SET status = CASE
                    WHEN event.attempt_count >= $2
                        THEN 'dead_letter'::commit.outbox_status
                    ELSE 'pending'::commit.outbox_status
                END,
                lease_owner = NULL,
                lease_expires_at = NULL,
                last_error_code = CASE
                    WHEN event.attempt_count >= $2
                        THEN 'delivery_attempts_exhausted'
                    ELSE event.last_error_code
                END,
                available_at = GREATEST(
                    event.available_at,
                    event.updated_at,
                    clock_timestamp()
                ),
                dead_lettered_at = CASE
                    WHEN event.attempt_count >= $2
                        THEN GREATEST(event.updated_at, clock_timestamp())
                    ELSE NULL
                END,
                purge_after = CASE
                    WHEN event.attempt_count >= $2
                        THEN GREATEST(event.updated_at, clock_timestamp())
                            + make_interval(secs => $3::double precision)
                    ELSE NULL
                END
            FROM candidate
            WHERE event.id = candidate.id
            "#,
        )
        .bind(limit)
        .bind(max_attempts)
        .bind(dead_letter_retention)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn claim(&self) -> Result<Vec<ClaimedEvent>, sqlx::Error> {
        // Never lease work that cannot begin immediately. Otherwise a large
        // claim could expire in memory while waiting behind the delivery
        // concurrency window and be delivered concurrently by another worker.
        let claim_limit = self
            .settings
            .batch_size
            .get()
            .min(self.settings.delivery_concurrency.get());
        let limit = i64::try_from(claim_limit).unwrap_or(i64::MAX);
        let lease_seconds =
            i64::try_from(self.settings.lease_duration.as_secs()).unwrap_or(i64::MAX);
        let max_attempts = i32::from(self.settings.max_attempts.get());
        let rows = sqlx::query_as::<_, ClaimedRow>(
            r#"
            WITH candidate AS (
                SELECT event.id
                FROM commit.outbox_events AS event
                WHERE event.status = 'pending'
                  AND event.available_at <= transaction_timestamp()
                  AND event.attempt_count < $4
                ORDER BY event.available_at, event.created_at, event.id
                FOR UPDATE SKIP LOCKED
                LIMIT $1
            ), claimed AS (
                UPDATE commit.outbox_events AS event
                SET status = 'in_flight',
                    attempt_count = event.attempt_count + 1,
                    lease_owner = $2,
                    lease_expires_at = GREATEST(
                            event.updated_at,
                            event.available_at,
                            clock_timestamp()
                        )
                        + make_interval(secs => $3::double precision)
                FROM candidate
                WHERE event.id = candidate.id
                RETURNING event.*
            )
            SELECT claimed.id,
                   organization.org_id,
                   recipient.actor_id AS silicon_id,
                   claimed.event_type,
                   claimed.payload_version,
                   claimed.payload,
                   claimed.created_at,
                   claimed.attempt_count
            FROM claimed
            JOIN commit.organization_projection AS organization
              ON organization.organization_id = claimed.organization_id
            JOIN commit.actor_projection AS recipient
              ON recipient.organization_id = claimed.organization_id
             AND recipient.principal_id = claimed.recipient_silicon_principal_id
            ORDER BY claimed.created_at, claimed.id
            "#,
        )
        .bind(limit)
        .bind(&self.worker_id)
        .bind(lease_seconds)
        .bind(max_attempts)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(ClaimedEvent::try_from).collect()
    }

    async fn mark_delivered(&self, claim: &ClaimedEvent) -> Result<(), sqlx::Error> {
        let retention_seconds =
            i64::try_from(self.settings.delivered_outbox_retention.as_secs()).unwrap_or(i64::MAX);
        sqlx::query(
            r#"
            UPDATE commit.outbox_events
            SET status = 'delivered',
                lease_owner = NULL,
                lease_expires_at = NULL,
                last_error_code = NULL,
                delivered_at = GREATEST(updated_at, clock_timestamp()),
                purge_after = GREATEST(updated_at, clock_timestamp())
                    + make_interval(secs => $3::double precision)
            WHERE id = $1
              AND status = 'in_flight'
              AND lease_owner = $2
            "#,
        )
        .bind(claim.event.event_id)
        .bind(&self.worker_id)
        .bind(retention_seconds)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn mark_failed(
        &self,
        claim: &ClaimedEvent,
        error: HookPublishError,
    ) -> Result<(), sqlx::Error> {
        let exhausted = usize::try_from(claim.attempt_count).map_or(true, |attempt| {
            attempt >= usize::from(self.settings.max_attempts.get())
        });
        if !error.is_retryable() || exhausted {
            let retention_seconds =
                i64::try_from(self.settings.dead_letter_outbox_retention.as_secs())
                    .unwrap_or(i64::MAX);
            sqlx::query(
                r#"
                UPDATE commit.outbox_events
                SET status = 'dead_letter',
                    lease_owner = NULL,
                    lease_expires_at = NULL,
                    last_error_code = $3,
                    dead_lettered_at = GREATEST(updated_at, clock_timestamp()),
                    purge_after = GREATEST(updated_at, clock_timestamp())
                        + make_interval(secs => $4::double precision)
                WHERE id = $1
                  AND status = 'in_flight'
                  AND lease_owner = $2
                "#,
            )
            .bind(claim.event.event_id)
            .bind(&self.worker_id)
            .bind(error.code())
            .bind(retention_seconds)
            .execute(&self.pool)
            .await?;
            return Ok(());
        }

        let retry_after = match error {
            HookPublishError::RateLimited {
                retry_after: Some(delay),
            } => delay.min(self.settings.max_retry_delay),
            _ => retry_delay(
                claim.event.event_id,
                claim.attempt_count,
                self.settings.max_retry_delay,
            ),
        };
        let retry_milliseconds = i64::try_from(retry_after.as_millis()).unwrap_or(i64::MAX);
        sqlx::query(
            r#"
            UPDATE commit.outbox_events
            SET status = 'pending',
                lease_owner = NULL,
                lease_expires_at = NULL,
                last_error_code = $3,
                purge_after = NULL,
                available_at = GREATEST(
                        updated_at,
                        available_at,
                        clock_timestamp()
                    )
                    + make_interval(secs => ($4::double precision / 1000.0))
            WHERE id = $1
              AND status = 'in_flight'
              AND lease_owner = $2
            "#,
        )
        .bind(claim.event.event_id)
        .bind(&self.worker_id)
        .bind(error.code())
        .bind(retry_milliseconds)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

#[derive(Debug, FromRow)]
struct ClaimedRow {
    id: Uuid,
    org_id: String,
    silicon_id: String,
    event_type: String,
    payload_version: i16,
    payload: serde_json::Value,
    created_at: OffsetDateTime,
    attempt_count: i32,
}

#[derive(Debug)]
struct ClaimedEvent {
    event: HookEvent,
    attempt_count: i32,
}

impl TryFrom<ClaimedRow> for ClaimedEvent {
    type Error = sqlx::Error;

    fn try_from(row: ClaimedRow) -> Result<Self, Self::Error> {
        let org_id = PublicOrganizationId::new(row.org_id)
            .map_err(|error| sqlx::Error::Decode(Box::new(error)))?;
        let silicon_id =
            ActorId::new(row.silicon_id).map_err(|error| sqlx::Error::Decode(Box::new(error)))?;
        let payload_version = u16::try_from(row.payload_version)
            .map_err(|error| sqlx::Error::Decode(Box::new(error)))?;
        let trace_id = row
            .payload
            .get("trace_id")
            .or_else(|| row.payload.get("request_id"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        Ok(Self {
            event: HookEvent {
                event_id: row.id,
                org_id,
                silicon_id,
                event_type: row.event_type,
                payload_version,
                occurred_at: row.created_at,
                trace_id,
                payload: row.payload,
            },
            attempt_count: row.attempt_count,
        })
    }
}

fn retry_delay(event_id: Uuid, attempt: i32, maximum: Duration) -> Duration {
    let exponent = u32::try_from(attempt.saturating_sub(1))
        .unwrap_or(31)
        .min(31);
    let cap_seconds = 2_u64.saturating_pow(exponent).min(maximum.as_secs().max(1));
    let mixed = event_id.as_u128() ^ u128::from(u32::try_from(attempt).unwrap_or_default());
    let jitter =
        u64::try_from(mixed % u128::from(cap_seconds.saturating_add(1))).unwrap_or(cap_seconds);
    Duration::from_secs(jitter.max(1))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use time::OffsetDateTime;
    use uuid::Uuid;

    use super::{ClaimedEvent, ClaimedRow, retry_delay};

    #[test]
    fn retry_delay_is_positive_and_bounded() {
        let maximum = Duration::from_secs(30);
        for attempt in 1..20 {
            let delay = retry_delay(Uuid::from_u128(42), attempt, maximum);
            assert!(delay >= Duration::from_secs(1));
            assert!(delay <= maximum);
        }
    }

    #[test]
    fn claimed_event_promotes_the_persisted_request_correlation() {
        let request_id = "01900000-0000-7000-8000-000000000001";
        let row = ClaimedRow {
            id: Uuid::now_v7(),
            org_id: "test-org".to_owned(),
            silicon_id: "silicon-one".to_owned(),
            event_type: "todo.updated".to_owned(),
            payload_version: 1,
            payload: serde_json::json!({ "request_id": request_id }),
            created_at: OffsetDateTime::UNIX_EPOCH,
            attempt_count: 1,
        };

        let claimed = ClaimedEvent::try_from(row);
        assert_eq!(
            claimed.ok().and_then(|claim| claim.event.trace_id),
            Some(request_id.to_owned())
        );
    }
}
