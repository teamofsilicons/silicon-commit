//! Durable notification and retention workers.

use std::sync::Arc;

use tokio::{
    task::{JoinError, JoinSet},
    time::{MissedTickBehavior, interval, timeout},
};
use tracing::{error, info, warn};

use crate::{
    config::{RuntimeProfile, Settings},
    infrastructure::{clients::hook::HookClient, postgres},
    shutdown,
};

pub mod outbox;
pub mod retention;

/// Runs notification delivery and retention maintenance until shutdown.
///
/// # Errors
///
/// Returns an error when required dependencies cannot initialize. Individual
/// polling failures are logged and retried without terminating the worker.
pub async fn run(settings: Settings) -> anyhow::Result<()> {
    anyhow::ensure!(
        settings.runtime_profile == RuntimeProfile::Worker,
        "settings were not loaded for the worker process"
    );
    let pool = postgres::connect(&settings.database, "commit-worker").await?;
    let integrations = &settings.integrations;
    let hook = HookClient::new(
        &integrations.hook,
        integrations.connect_timeout,
        integrations.request_timeout,
        integrations.max_response_bytes,
    )?;
    let processor =
        outbox::OutboxProcessor::new(pool.clone(), Arc::new(hook), settings.worker.clone());
    let retention_policy = retention::RetentionPolicy::from(&settings.worker);
    let mut poll = interval(settings.worker.poll_interval);
    poll.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut maintenance = interval(settings.worker.maintenance_interval);
    maintenance.set_missed_tick_behavior(MissedTickBehavior::Delay);
    // Do not make every fresh replica run retention immediately.
    maintenance.tick().await;
    let shutdown = shutdown::signal();
    tokio::pin!(shutdown);
    let mut outbox_jobs = JoinSet::new();
    let mut retention_jobs = JoinSet::new();

    info!("Silicon Commit worker started");
    loop {
        tokio::select! {
            biased;
            () = &mut shutdown => {
                info!("shutdown requested; draining worker jobs");
                break;
            }
            _ = poll.tick(), if outbox_jobs.is_empty() => {
                let processor = processor.clone();
                outbox_jobs.spawn(async move { processor.process_once().await });
            }
            result = outbox_jobs.join_next(), if !outbox_jobs.is_empty() => {
                if let Some(result) = result {
                    log_outbox_result(result);
                }
            }
            _ = maintenance.tick(), if retention_jobs.is_empty() => {
                let maintenance_pool = pool.clone();
                retention_jobs.spawn(async move {
                    retention::run_cycle(&maintenance_pool, retention_policy).await
                });
            }
            result = retention_jobs.join_next(), if !retention_jobs.is_empty() => {
                if let Some(result) = result {
                    log_retention_result(result);
                }
            }
        }
    }

    if !drain_in_flight(
        &mut outbox_jobs,
        &mut retention_jobs,
        settings.server.shutdown_timeout,
    )
    .await
    {
        let outbox_job_count = outbox_jobs.len();
        let retention_job_count = retention_jobs.len();
        warn!(
            outbox_job_count,
            retention_job_count,
            "worker shutdown deadline exceeded; canceling jobs; leased outbox events will be recovered after lease expiration"
        );
        outbox_jobs.abort_all();
        retention_jobs.abort_all();
    }
    // Dropping a timed-out JoinSet requests cancellation without extending the
    // configured shutdown deadline. Any claimed outbox rows remain protected
    // by their finite leases and become eligible for recovery by another
    // worker after lease expiry.
    drop(outbox_jobs);
    drop(retention_jobs);
    pool.close().await;
    info!("Silicon Commit worker stopped");
    Ok(())
}

fn log_outbox_result(result: Result<anyhow::Result<usize>, JoinError>) {
    match result {
        Ok(Ok(count)) if count > 0 => info!(event_count = count, "processed outbox batch"),
        Ok(Ok(_)) => {}
        Ok(Err(error)) => error!(error = ?error, "outbox poll failed"),
        Err(error) => error!(error = ?error, "outbox poll task failed"),
    }
}

fn log_retention_result(
    result: Result<anyhow::Result<retention::RetentionCycleReport>, JoinError>,
) {
    match result {
        Ok(Ok(report)) if report.pass_budget_exhausted => {
            warn!(?report, "retention cycle reached its pass budget");
        }
        Ok(Ok(report)) => info!(?report, "retention cycle completed"),
        Ok(Err(error)) => error!(error = ?error, "retention cycle failed"),
        Err(error) => error!(error = ?error, "retention cycle task failed"),
    }
}

async fn drain_in_flight(
    outbox_jobs: &mut JoinSet<anyhow::Result<usize>>,
    retention_jobs: &mut JoinSet<anyhow::Result<retention::RetentionCycleReport>>,
    shutdown_timeout: std::time::Duration,
) -> bool {
    timeout(shutdown_timeout, async {
        while !outbox_jobs.is_empty() || !retention_jobs.is_empty() {
            tokio::select! {
                result = outbox_jobs.join_next(), if !outbox_jobs.is_empty() => {
                    if let Some(result) = result {
                        log_outbox_result(result);
                    }
                }
                result = retention_jobs.join_next(), if !retention_jobs.is_empty() => {
                    if let Some(result) = result {
                        log_retention_result(result);
                    }
                }
            }
        }
    })
    .await
    .is_ok()
}

#[cfg(test)]
mod tests {
    use std::{future::pending, time::Duration};

    use tokio::{sync::oneshot, task::JoinSet};

    use super::{drain_in_flight, retention};

    struct DropNotice(Option<oneshot::Sender<()>>);

    impl Drop for DropNotice {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }

    #[tokio::test]
    async fn shutdown_deadline_cancels_an_in_flight_outbox_batch() {
        let (started_sender, started_receiver) = oneshot::channel();
        let (dropped_sender, dropped_receiver) = oneshot::channel();
        let mut outbox_jobs = JoinSet::new();
        let mut retention_jobs: JoinSet<anyhow::Result<retention::RetentionCycleReport>> =
            JoinSet::new();

        outbox_jobs.spawn(async move {
            let _drop_notice = DropNotice(Some(dropped_sender));
            let _ = started_sender.send(());
            pending::<()>().await;
            Ok(0)
        });
        started_receiver
            .await
            .unwrap_or_else(|_| panic!("outbox test task stopped before it started"));

        assert!(!drain_in_flight(&mut outbox_jobs, &mut retention_jobs, Duration::ZERO,).await);

        outbox_jobs.abort_all();
        drop(outbox_jobs);
        dropped_receiver
            .await
            .unwrap_or_else(|_| panic!("outbox test task was not canceled"));
    }

    #[tokio::test]
    async fn graceful_shutdown_allows_an_in_flight_batch_to_finish() {
        let mut outbox_jobs = JoinSet::new();
        let mut retention_jobs: JoinSet<anyhow::Result<retention::RetentionCycleReport>> =
            JoinSet::new();
        outbox_jobs.spawn(async { Ok(1) });

        assert!(
            drain_in_flight(
                &mut outbox_jobs,
                &mut retention_jobs,
                Duration::from_secs(1),
            )
            .await
        );
        assert!(outbox_jobs.is_empty());
    }
}
