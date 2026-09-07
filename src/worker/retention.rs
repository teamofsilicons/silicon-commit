//! Bounded tombstone and operational-record retention.

use std::num::NonZeroUsize;

use sqlx::{FromRow, PgPool};

use crate::config::WorkerSettings;

/// Prevents one scheduled cycle from monopolizing the database indefinitely
/// when expired records arrive at least as fast as maintenance can remove them.
const MAX_PASSES_PER_CYCLE: usize = 128;

/// Per-statement work bound for one maintenance pass.
///
/// Todo content and audit deadlines are frozen on their records when those
/// records enter their lifecycle state. They are deliberately not supplied by
/// the worker at purge time.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetentionPolicy {
    batch_size: NonZeroUsize,
}

impl RetentionPolicy {
    /// Creates an explicit maintenance policy.
    #[must_use]
    pub const fn new(batch_size: NonZeroUsize) -> Self {
        Self { batch_size }
    }
}

impl From<&WorkerSettings> for RetentionPolicy {
    fn from(settings: &WorkerSettings) -> Self {
        Self::new(settings.maintenance_batch_size)
    }
}

/// Counts records affected by one maintenance pass.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RetentionReport {
    /// Deleted notes.
    pub notes_purged: u64,
    /// Deleted attachment references.
    pub attachments_purged: u64,
    /// Todo activity rows whose user-derived change details were cleared.
    pub activity_changes_redacted: u64,
    /// Todo activity metadata rows past the audit-retention window.
    pub activity_purged: u64,
    /// Redacted todo records.
    pub todos_redacted: u64,
    /// Expired idempotency responses.
    pub idempotency_purged: u64,
    /// Audit rows past their configured retention.
    pub audit_purged: u64,
    /// Delivered outbox rows past operational retention.
    pub delivered_outbox_purged: u64,
    /// Dead-lettered outbox rows past diagnostic retention.
    pub dead_letter_outbox_purged: u64,
    /// Test environments automatically retired after inactivity.
    pub testing_environments_expired: u64,
    /// Test-environment metadata permanently purged after its recovery TTL.
    pub testing_environments_purged: u64,
}

#[derive(Clone, Copy, Debug, Eq, FromRow, PartialEq)]
struct RetentionRow {
    notes_purged: i64,
    attachments_purged: i64,
    activity_changes_redacted: i64,
    activity_purged: i64,
    todos_redacted: i64,
    idempotency_purged: i64,
    audit_purged: i64,
    delivered_outbox_purged: i64,
    dead_letter_outbox_purged: i64,
}

impl TryFrom<RetentionRow> for RetentionReport {
    type Error = std::num::TryFromIntError;

    fn try_from(row: RetentionRow) -> Result<Self, Self::Error> {
        Ok(Self {
            notes_purged: u64::try_from(row.notes_purged)?,
            attachments_purged: u64::try_from(row.attachments_purged)?,
            activity_changes_redacted: u64::try_from(row.activity_changes_redacted)?,
            activity_purged: u64::try_from(row.activity_purged)?,
            todos_redacted: u64::try_from(row.todos_redacted)?,
            idempotency_purged: u64::try_from(row.idempotency_purged)?,
            audit_purged: u64::try_from(row.audit_purged)?,
            delivered_outbox_purged: u64::try_from(row.delivered_outbox_purged)?,
            dead_letter_outbox_purged: u64::try_from(row.dead_letter_outbox_purged)?,
            testing_environments_expired: 0,
            testing_environments_purged: 0,
        })
    }
}

impl RetentionReport {
    fn reached_batch_limit(self, batch_size: NonZeroUsize) -> bool {
        let limit = u64::try_from(batch_size.get()).unwrap_or(u64::MAX);
        [
            self.notes_purged,
            self.attachments_purged,
            self.activity_changes_redacted,
            self.activity_purged,
            self.todos_redacted,
            self.idempotency_purged,
            self.audit_purged,
            self.delivered_outbox_purged,
            self.dead_letter_outbox_purged,
        ]
        .into_iter()
        .any(|affected| affected >= limit)
    }

    fn accumulate(&mut self, pass: Self) {
        self.notes_purged = self.notes_purged.saturating_add(pass.notes_purged);
        self.attachments_purged = self
            .attachments_purged
            .saturating_add(pass.attachments_purged);
        self.activity_changes_redacted = self
            .activity_changes_redacted
            .saturating_add(pass.activity_changes_redacted);
        self.activity_purged = self.activity_purged.saturating_add(pass.activity_purged);
        self.todos_redacted = self.todos_redacted.saturating_add(pass.todos_redacted);
        self.idempotency_purged = self
            .idempotency_purged
            .saturating_add(pass.idempotency_purged);
        self.audit_purged = self.audit_purged.saturating_add(pass.audit_purged);
        self.delivered_outbox_purged = self
            .delivered_outbox_purged
            .saturating_add(pass.delivered_outbox_purged);
        self.dead_letter_outbox_purged = self
            .dead_letter_outbox_purged
            .saturating_add(pass.dead_letter_outbox_purged);
        self.testing_environments_expired = self
            .testing_environments_expired
            .saturating_add(pass.testing_environments_expired);
        self.testing_environments_purged = self
            .testing_environments_purged
            .saturating_add(pass.testing_environments_purged);
    }
}

/// Aggregated outcome of one scheduled, multi-transaction maintenance cycle.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RetentionCycleReport {
    /// Number of bounded transactions executed.
    pub passes: usize,
    /// Whether the cycle stopped at its safety budget while a pass was full.
    pub pass_budget_exhausted: bool,
    /// Total rows affected across the cycle's passes.
    pub totals: RetentionReport,
}

/// Drains full maintenance batches through successive bounded transactions.
///
/// A cycle stops when every statement affects fewer rows than the configured
/// batch, or after 128 passes. It yields between full passes so delivery and
/// shutdown tasks can run. Rows locked by another transaction may be skipped
/// and are reconsidered on the next scheduled cycle.
///
/// # Errors
///
/// Returns the first database or policy-conversion error from a bounded pass.
pub async fn run_cycle(
    pool: &PgPool,
    policy: RetentionPolicy,
) -> anyhow::Result<RetentionCycleReport> {
    let mut totals = RetentionReport::default();

    for pass in 1..=MAX_PASSES_PER_CYCLE {
        let report = run_once(pool, policy).await?;
        let reached_batch_limit = report.reached_batch_limit(policy.batch_size);
        totals.accumulate(report);
        if !reached_batch_limit {
            return Ok(RetentionCycleReport {
                passes: pass,
                pass_budget_exhausted: false,
                totals,
            });
        }
        tokio::task::yield_now().await;
    }

    Ok(RetentionCycleReport {
        passes: MAX_PASSES_PER_CYCLE,
        pass_budget_exhausted: true,
        totals,
    })
}

/// Executes one owner-defined, bounded retention pass.
///
/// Runtime workers receive `EXECUTE` on the database routine rather than
/// direct todo-content, activity, audit, idempotency, or deletion authority.
/// The routine uses stored content/audit deadlines and enforces the live replay
/// gate inside the same transaction.
///
/// # Errors
///
/// Returns an error when the policy cannot be represented by PostgreSQL or the
/// database cannot complete the maintenance statement.
pub async fn run_once(pool: &PgPool, policy: RetentionPolicy) -> anyhow::Result<RetentionReport> {
    let batch_size = i32::try_from(policy.batch_size.get())?;
    let row = sqlx::query_as::<_, RetentionRow>(
        r#"
        SELECT notes_purged,
               attachments_purged,
               activity_changes_redacted,
               activity_purged,
               todos_redacted,
               idempotency_purged,
               audit_purged,
               delivered_outbox_purged,
               dead_letter_outbox_purged
        FROM commit.run_retention_pass($1)
        "#,
    )
    .bind(batch_size)
    .fetch_one(pool)
    .await?;
    let mut report: RetentionReport = row.try_into()?;
    let (testing_environments_expired, testing_environments_purged): (i64, i64) =
        sqlx::query_as("SELECT expired, purged FROM commit.run_testing_environment_retention($1)")
            .bind(batch_size)
            .fetch_one(pool)
            .await?;
    report.testing_environments_expired = u64::try_from(testing_environments_expired)?;
    report.testing_environments_purged = u64::try_from(testing_environments_purged)?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_statement_requires_another_pass() {
        let batch_size = NonZeroUsize::MIN;
        let partial = RetentionReport {
            audit_purged: 0,
            ..RetentionReport::default()
        };
        let full = RetentionReport {
            dead_letter_outbox_purged: 1,
            ..RetentionReport::default()
        };

        assert!(!partial.reached_batch_limit(batch_size));
        assert!(full.reached_batch_limit(batch_size));
    }

    #[test]
    fn cycle_totals_accumulate_without_wrapping() {
        let mut totals = RetentionReport {
            notes_purged: u64::MAX,
            audit_purged: 2,
            ..RetentionReport::default()
        };
        totals.accumulate(RetentionReport {
            notes_purged: 1,
            audit_purged: 3,
            ..RetentionReport::default()
        });

        assert_eq!(totals.notes_purged, u64::MAX);
        assert_eq!(totals.audit_purged, 5);
    }
}
