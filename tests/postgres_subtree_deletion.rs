//! Regression coverage for task/todo subtree deletion and legacy repair.

use std::{env, num::NonZeroU32, time::Duration};

use secrecy::SecretString;
use silicon_commit::{config::DatabaseSettings, infrastructure::postgres};
use sqlx::Row as _;

#[tokio::test]
async fn todo_deletion_removes_descendants_and_repairs_existing_orphans() -> anyhow::Result<()> {
    let Ok(url) = env::var("COMMIT_TEST_DATABASE_URL") else {
        eprintln!("skipping PostgreSQL integration test: COMMIT_TEST_DATABASE_URL is not set");
        return Ok(());
    };
    let settings = DatabaseSettings {
        url: SecretString::from(url),
        max_connections: NonZeroU32::MIN,
        min_connections: 0,
        acquire_timeout: Duration::from_secs(5),
        statement_timeout: Duration::from_secs(30),
    };
    let pool = postgres::connect_migrator(&settings, "commit-subtree-regression").await?;
    let owner: String = sqlx::query_scalar("SELECT current_user::text")
        .fetch_one(&pool)
        .await?;
    postgres::migrate(&pool, &owner).await?;
    let mut tx = pool.begin().await?;
    sqlx::raw_sql(include_str!("postgres_subtree_deletion.sql"))
        .execute(&mut *tx)
        .await?;

    // The SQL fixture reproduces the historical orphaned state. Reapplying the
    // migration inside this rolled-back transaction exercises its data repair.
    sqlx::raw_sql(include_str!("../migrations/0030_task_subtree_deletion.sql"))
        .execute(&mut *tx)
        .await?;
    let repaired = sqlx::query(
        "SELECT (SELECT count(*) FROM commit.project_tasks WHERE project_id=f.project_id AND deleted_at IS NULL) AS tasks, (SELECT count(*) FROM commit.todos WHERE project_id=f.project_id AND deleted_at IS NULL) AS todos, (SELECT count(*) FROM commit.audit_events WHERE organization_id=f.organization_id AND action='project.task.subtree_repaired') AS repairs, (SELECT count(*) FROM commit.email_jobs WHERE organization_id=f.organization_id) AS emails, (SELECT jsonb_array_length(snapshot->'tasks') FROM commit.project_versions WHERE project_id=f.project_id ORDER BY version DESC LIMIT 1) AS snapshot_tasks FROM subtree_fixture f WHERE label='legacy'",
    )
    .fetch_one(&mut *tx)
    .await?;
    assert_eq!(repaired.get::<i64, _>("tasks"), 1);
    assert_eq!(repaired.get::<i64, _>("todos"), 1);
    assert_eq!(repaired.get::<i64, _>("repairs"), 1);
    assert_eq!(repaired.get::<i64, _>("emails"), 0);
    assert_eq!(repaired.get::<i32, _>("snapshot_tasks"), 1);

    // A second repair must not create another audit, version or tombstone.
    sqlx::raw_sql(include_str!("../migrations/0030_task_subtree_deletion.sql"))
        .execute(&mut *tx)
        .await?;
    let repairs: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM commit.audit_events WHERE organization_id=(SELECT organization_id FROM subtree_fixture WHERE label='legacy') AND action='project.task.subtree_repaired'",
    )
    .fetch_one(&mut *tx)
    .await?;
    assert_eq!(repairs, 1);
    tx.rollback().await?;
    pool.close().await;
    Ok(())
}
