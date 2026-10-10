//! Regression coverage for task/todo subtree deletion and legacy repair.

mod common;

use std::sync::Arc;

use anyhow::Context as _;
use sqlx::{PgPool, Row as _};

use common::{
    AUDIT_RETENTION, Directory, IDEMPOTENCY_TTL, TOMBSTONE_RETENTION, World, assert_not_found,
    response_uuid, test_pool, unique_key, with_isolated_database,
};
use silicon_commit::{
    application::{projects::ProjectService, todos::TodoService},
    domain::{CollectionQuery, DomainLimits, PageLimit, ProjectCreate, ProjectId, ProjectLocator},
};

/// The fixture and the repair in migration 0030 are IAM-era SQL, so they run on an
/// isolated database at schema 32 (the last IAM-era schema).
#[tokio::test]
async fn todo_deletion_removes_descendants_and_repairs_existing_orphans() -> anyhow::Result<()> {
    with_isolated_database("commit_subtree", |options| async move {
        let pool = PgPool::connect_with(options).await?;
        let outcome = legacy_subtree(&pool).await;
        pool.close().await;
        outcome
    })
    .await
}

async fn legacy_subtree(pool: &PgPool) -> anyhow::Result<()> {
    let mut legacy = sqlx::migrate!("./migrations");
    legacy
        .migrations
        .to_mut()
        .retain(|migration| migration.version <= 32);
    legacy.run(pool).await?;
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
    Ok(())
}

/// The same guarantee on the account-keyed schema: deleting a task-backed todo removes the
/// whole task subtree (assigned and unassigned), keeps tombstoned content, leaves the
/// sibling alone and records exactly one project revision.
#[tokio::test]
async fn deleting_a_task_backed_todo_removes_its_subtree() -> anyhow::Result<()> {
    let Some(pool) = test_pool().await? else {
        return Ok(());
    };
    let world = World::new(&pool);
    let custodian = world.carbon("subtree-custodian").await?;
    let worker = world.silicon("subtree-worker", &custodian).await?;
    let directory = Arc::new(Directory::with(&[&worker]));
    let projects = ProjectService::new(
        pool.clone(),
        Arc::clone(&directory) as _,
        DomainLimits::default(),
        IDEMPOTENCY_TTL,
        AUDIT_RETENTION,
    );
    let todos = TodoService::new(
        pool.clone(),
        directory,
        DomainLimits::default(),
        IDEMPOTENCY_TTL,
        AUDIT_RETENTION,
        TOMBSTONE_RETENTION,
    );
    let request: ProjectCreate = serde_json::from_value(serde_json::json!({
        "name": "Subtree",
        "silicon_ids": [worker.actor.id],
        "tasks": [
            {"title": "Parent", "assigned_to": worker.actor.id, "subtasks": [
                {"title": "Unassigned child", "subtasks": [
                    {"title": "Grandchild", "assigned_to": worker.actor.id}
                ]}
            ]},
            {"title": "Sibling", "assigned_to": worker.actor.id}
        ]
    }))?;
    let created = projects
        .create_project(
            &worker,
            request,
            unique_key("subtree-create")?,
            "subtree-create",
        )
        .await?;
    let project_id = ProjectId::from_uuid(response_uuid(&created, "id")?);
    let locator = ProjectLocator::Id(project_id);
    let all = CollectionQuery {
        limit: PageLimit::new(50)?,
        cursor: None,
    };
    let tasks = projects.list_tasks(&worker, &locator, all).await?.items;
    assert_eq!(tasks.len(), 4);
    let todo_of = |title: &str| {
        tasks
            .iter()
            .find(|task| task.title.as_str() == title)
            .and_then(|task| task.todo_id)
            .with_context(|| format!("task {title} has a todo"))
    };
    let parent_todo = todo_of("Parent")?;
    let grandchild_todo = todo_of("Grandchild")?;
    let sibling_todo = todo_of("Sibling")?;
    let versions = |pool: PgPool| async move {
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM commit.project_versions WHERE project_id = $1",
        )
        .bind(project_id.into_uuid())
        .fetch_one(&pool)
        .await
    };
    let before = versions(pool.clone()).await?;

    todos.delete(&worker, parent_todo, "subtree-delete").await?;

    let remaining = projects.list_tasks(&worker, &locator, all).await?.items;
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].todo_id, Some(sibling_todo));
    assert_not_found(todos.get(&worker, grandchild_todo).await)?;
    assert_eq!(todos.get(&worker, sibling_todo).await?.id, sibling_todo);
    let tombstone = sqlx::query(
        "SELECT title, deleted_by_account, content_retain_until - deleted_at AS kept FROM commit.todos WHERE id = $1",
    )
    .bind(grandchild_todo.into_uuid())
    .fetch_one(&pool)
    .await?;
    assert_eq!(tombstone.get::<String, _>("title"), "Grandchild");
    assert_eq!(
        tombstone
            .get::<Option<String>, _>("deleted_by_account")
            .as_deref(),
        Some(worker.uuid().as_str())
    );
    let after = versions(pool.clone()).await?;
    assert_eq!(
        after,
        before + 1,
        "one deletion appends exactly one revision"
    );
    let snapshot_tasks: i32 = sqlx::query_scalar(
        "SELECT jsonb_array_length(snapshot->'tasks') FROM commit.project_versions WHERE project_id = $1 ORDER BY version DESC LIMIT 1",
    )
    .bind(project_id.into_uuid())
    .fetch_one(&pool)
    .await?;
    assert_eq!(snapshot_tasks, 1);
    // Deleting again is an idempotent no-op for the owner: no new revision or tombstone.
    todos
        .delete(&worker, parent_todo, "subtree-delete-again")
        .await?;
    assert_eq!(versions(pool.clone()).await?, after);
    Ok(())
}
