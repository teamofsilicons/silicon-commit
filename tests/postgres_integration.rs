//! PostgreSQL-backed integration coverage for Commit's transactional workflows.

mod common;

use std::{num::NonZeroUsize, sync::Arc};

use anyhow::Context as _;
use time::OffsetDateTime;
use uuid::Uuid;

use common::{
    AUDIT_RETENTION, Directory, IDEMPOTENCY_TTL, TOMBSTONE_RETENTION, World, assert_conflict,
    assert_database_code, assert_denied, assert_not_found, response_uuid, test_pool, unique_key,
};
use silicon_commit::{
    application::{
        accounts::AccountService, notifications::NotificationSettingsService,
        projects::ProjectService, todos::TodoService,
    },
    domain::{
        AccountUuid, Actor, ActorType, AttachmentUrl, BlockerCreate, BlockerStatus,
        CollectionQuery, DiaryUpdate, DomainLimits, ExpectedDiaryVersion,
        ExpectedNotificationVersion, NotificationRuleInput, NotificationScope,
        NotificationSettingsUpdate, NullablePatch, PageLimit, ProjectCompletionCreate,
        ProjectCreate, ProjectId, ProjectLocator, ProjectPatch, ProjectQuery, ProjectStatus,
        ProjectTaskCreate, ProjectTaskId, ProjectTaskPatch, ProjectUpdateCreate, TodoCreate,
        TodoId, TodoNoteCreate, TodoNotificationSubscriptionUpdate, TodoPatch, TodoQuery,
        TodoStatus,
    },
    error::AppError,
    worker::retention::{self, RetentionPolicy},
};

fn todos(pool: &sqlx::PgPool, directory: Arc<Directory>) -> TodoService {
    TodoService::new(
        pool.clone(),
        directory,
        DomainLimits::default(),
        IDEMPOTENCY_TTL,
        AUDIT_RETENTION,
        TOMBSTONE_RETENTION,
    )
}

fn projects(pool: &sqlx::PgPool, directory: Arc<Directory>) -> ProjectService {
    ProjectService::new(
        pool.clone(),
        directory,
        DomainLimits::default(),
        IDEMPOTENCY_TTL,
        AUDIT_RETENTION,
    )
}

fn notifications(pool: &sqlx::PgPool, directory: Arc<Directory>) -> NotificationSettingsService {
    NotificationSettingsService::new(pool.clone(), directory, AUDIT_RETENTION)
}

#[tokio::test]
async fn migrations_establish_the_account_keyed_schema() -> anyhow::Result<()> {
    let Some(pool) = test_pool().await? else {
        return Ok(());
    };

    let successful_migrations =
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM public._sqlx_migrations WHERE success")
            .fetch_one(&pool)
            .await?;
    assert!(successful_migrations >= 33);

    for relation in [
        "commit.todos",
        "commit.projects",
        "commit.idempotency_records",
        "commit.outbox_events",
        "commit.silicon_notification_settings",
        "commit.todo_notification_subscriptions",
        "commit.accounts",
        "commit.accounts_webhook_events",
        "commit.silicon_allowed_accounts",
        "commit_private.identity_links",
    ] {
        let exists = sqlx::query_scalar::<_, bool>("SELECT to_regclass($1) IS NOT NULL")
            .bind(relation)
            .fetch_one(&pool)
            .await?;
        assert!(exists, "migration did not create {relation}");
    }
    let migration_ledgers = sqlx::query_as::<_, (bool, bool)>(
        r"
        SELECT to_regclass('public._sqlx_migrations') IS NOT NULL,
               to_regclass('commit._sqlx_migrations') IS NOT NULL
        ",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(migration_ledgers, (true, false));

    // Every identity column has a NOT NULL account twin; IAM-era columns are nullable provenance.
    let shape = sqlx::query_as::<_, (String, String, String)>(
        r"
        SELECT table_name::text, column_name::text, is_nullable::text
          FROM information_schema.columns
         WHERE table_schema = 'commit'
           AND (column_name LIKE '%\_account' OR column_name IN ('account', 'organization_id'))
           AND table_name IN ('todos', 'todo_notes', 'todo_activity', 'projects', 'project_participants',
                              'project_tasks', 'project_entries', 'project_diaries', 'idempotency_records',
                              'audit_events', 'outbox_events', 'silicon_notification_settings',
                              'todo_notification_subscriptions', 'email_preferences', 'project_collaborators')
        ",
    )
    .fetch_all(&pool)
    .await?;
    for (table, column, nullable) in &shape {
        let optional = matches!(
            column.as_str(),
            "organization_id" | "deleted_by_account" | "removed_by_account"
        ) || (table == "project_tasks" && column == "assigned_to_account");
        assert_eq!(nullable == "YES", optional, "{table}.{column} nullability");
    }
    assert!(
        shape.len() >= 30,
        "expected every identity column, found {}",
        shape.len()
    );

    let public_privileges = sqlx::query_as::<_, (bool, bool)>(
        r"
        SELECT
            NOT EXISTS (
                SELECT 1
                FROM pg_catalog.pg_class AS relation
                JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace
                CROSS JOIN LATERAL aclexplode(COALESCE(relation.relacl, acldefault('r', relation.relowner))) AS privilege
                WHERE namespace.nspname IN ('commit', 'commit_private')
                  AND relation.relkind IN ('r', 'p', 'v', 'm', 'S', 'f')
                  AND privilege.grantee = 0
            ),
            NOT EXISTS (
                SELECT 1
                FROM pg_catalog.pg_proc AS routine
                JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = routine.pronamespace
                CROSS JOIN LATERAL aclexplode(COALESCE(routine.proacl, acldefault('f', routine.proowner))) AS privilege
                WHERE namespace.nspname IN ('commit', 'commit_private')
                  AND routine.proname IN ('forget_account', 'run_retention_pass', 'claim_email')
                  AND privilege.grantee = 0
            )
        ",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(public_privileges, (true, true));

    let contracts = sqlx::query_as::<_, (i32, String)>(
        "SELECT version, status FROM commit.contract_versions WHERE version IN (1, 2) ORDER BY version",
    )
    .fetch_all(&pool)
    .await?;
    assert_eq!(
        contracts.first().map(|(_, status)| status.as_str()),
        Some("deprecated")
    );
    assert!(
        contracts
            .iter()
            .any(|(version, status)| *version == 2 && status == "active")
    );

    // Placeholders and real accounts are told apart by the uuid alone.
    let bad_placeholder = sqlx::query(
        "INSERT INTO commit.accounts (uuid, kind, public_id, status) VALUES ('iam:x:y', 'carbon', 'c:x', 'active')",
    )
    .execute(&pool)
    .await;
    assert_database_code(bad_placeholder, "23514")?;
    Ok(())
}

#[tokio::test]
async fn versioned_timestamps_do_not_regress_in_an_older_transaction() -> anyhow::Result<()> {
    let Some(pool) = test_pool().await? else {
        return Ok(());
    };
    let world = World::new(&pool);
    let actor = world.carbon("timestamp").await?;
    let todo_id = TodoId::new();
    sqlx::query(
        "INSERT INTO commit.todos (id, title, assigned_by_account, assigned_to_account) VALUES ($1, 'initial', $2, $2)",
    )
    .bind(todo_id.into_uuid())
    .bind(actor.uuid().as_str())
    .execute(&pool)
    .await?;

    let mut older_transaction = pool.begin().await?;
    let _: OffsetDateTime = sqlx::query_scalar("SELECT transaction_timestamp()")
        .fetch_one(&mut *older_transaction)
        .await?;
    let newer_mutation = sqlx::query_as::<_, (OffsetDateTime, i64)>(
        "UPDATE commit.todos SET title = 'newer transaction' WHERE id = $1 RETURNING updated_at, version",
    )
    .bind(todo_id.into_uuid())
    .fetch_one(&pool)
    .await?;
    let older_mutation = sqlx::query_as::<_, (OffsetDateTime, i64)>(
        "UPDATE commit.todos SET title = 'older transaction, later mutation' WHERE id = $1 RETURNING updated_at, version",
    )
    .bind(todo_id.into_uuid())
    .fetch_one(&mut *older_transaction)
    .await?;
    older_transaction.commit().await?;

    assert!(older_mutation.0 >= newer_mutation.0);
    assert_eq!(newer_mutation.1, 2);
    assert_eq!(older_mutation.1, 3);
    Ok(())
}

#[tokio::test]
async fn account_rows_keep_their_uuid_and_kind() -> anyhow::Result<()> {
    let Some(pool) = test_pool().await? else {
        return Ok(());
    };
    let world = World::new(&pool);
    let carbon = world.carbon("immutable").await?;

    let kind_change = sqlx::query("UPDATE commit.accounts SET kind = 'silicon' WHERE uuid = $1")
        .bind(carbon.uuid().as_str())
        .execute(&pool)
        .await;
    assert_database_code(kind_change, "23514")?;
    let deletion = sqlx::query("DELETE FROM commit.accounts WHERE uuid = $1")
        .bind(carbon.uuid().as_str())
        .execute(&pool)
        .await;
    assert_database_code(deletion, "23514")?;

    // A token that names a known uuid with another kind is a provider contradiction.
    let contradiction = Actor::new(
        carbon.uuid().clone(),
        ActorType::Silicon,
        carbon.actor.id.clone(),
    );
    let directory = Arc::new(Directory::default());
    let service = todos(&pool, directory);
    let as_silicon =
        silicon_commit::application::ports::VerifiedActor::new(contradiction, common::bearer());
    let attempt = service
        .create(
            &as_silicon,
            TodoCreate {
                project_id: None,
                title: "contradiction".to_owned(),
                description: None,
                assigned_to: carbon.actor.id.clone(),
                status: TodoStatus::YetToDo,
                attachments: Vec::new(),
            },
            unique_key("contradiction")?,
            "req-contradiction",
        )
        .await;
    assert!(matches!(attempt, Err(AppError::BadGateway)), "{attempt:?}");

    // Uuids are case-sensitive: a differently cased uuid is another (unknown) account.
    let upper = AccountUuid::new(carbon.uuid().as_str().to_ascii_uppercase())?;
    let lower = AccountUuid::new(carbon.uuid().as_str().to_ascii_lowercase())?;
    let exists = |uuid: AccountUuid| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM commit.accounts WHERE uuid = $1)",
            )
            .bind(uuid.into_inner())
            .fetch_one(&pool)
            .await
        }
    };
    assert!(exists(carbon.uuid().clone()).await?);
    assert!(!(exists(upper).await? && exists(lower).await?));
    Ok(())
}

#[tokio::test]
async fn todo_lifecycle_enforces_replay_visibility_and_actor_boundaries() -> anyhow::Result<()> {
    let Some(pool) = test_pool().await? else {
        return Ok(());
    };
    let world = World::new(&pool);
    let custodian = world.carbon("todo-custodian").await?;
    let assigner = world.silicon("delegating", &custodian).await?;
    let sibling = world.silicon("sibling", &custodian).await?;
    let custodian = world.refreshed(&custodian).await?;
    let assignee = world.carbon("assigned").await?;
    let outsider = world.carbon("unrelated").await?;
    let directory = Arc::new(Directory::with(&[&assignee, &assigner, &outsider]));
    let service = todos(&pool, Arc::clone(&directory));
    let notification_service = notifications(&pool, Arc::clone(&directory));
    let notification_settings = notification_service
        .replace_settings(
            &assigner,
            None,
            NotificationSettingsUpdate {
                webhook_url: Some(format!(
                    "https://hook.example.com/silicon/{}/A1B2C3",
                    assigner.actor.id
                )),
                todo_list_subscription: Some(NotificationRuleInput {
                    scope: NotificationScope::AnyUpdate,
                    statuses: Vec::new(),
                }),
            },
            ExpectedNotificationVersion::new(0)?,
            "req-notification-settings-create",
        )
        .await?;
    assert_eq!(notification_settings.version.get(), 1);
    let attachment = format!(
        "https://briefcase.example/api/v1/entries/{}",
        Uuid::new_v4().hyphenated()
    );
    let external_attachment = format!(
        "https://images.example/assets/{}.png?variant=large",
        Uuid::new_v4().simple()
    );
    let request = TodoCreate {
        project_id: None,
        title: "Prepare launch review".to_owned(),
        description: Some("Preserve this formatting.\n\n- first\n- second".to_owned()),
        assigned_to: assignee.actor.id.clone(),
        status: TodoStatus::YetToDo,
        attachments: vec![
            AttachmentUrl::new(&attachment)?,
            AttachmentUrl::new(&external_attachment)?,
        ],
    };
    let create_key = unique_key("todo-create")?;

    let created = service
        .create(
            &assigner,
            request.clone(),
            create_key.clone(),
            "req-todo-create",
        )
        .await?;
    assert_eq!(created.status, 201);
    assert!(!created.replayed);
    assert_eq!(
        created.body["assigned_by"]["uuid"],
        assigner.uuid().as_str()
    );
    assert_eq!(
        created.body["assigned_to"]["uuid"],
        assignee.uuid().as_str()
    );
    assert_eq!(
        created.body["assigned_to"]["id"],
        assignee.actor.id.as_str()
    );
    assert!(created.body.get("org_id").is_none());
    let todo_id = TodoId::from_uuid(response_uuid(&created, "id")?);
    let round_tripped = service.get(&assigner, todo_id).await?;
    assert_eq!(
        round_tripped
            .attachments
            .iter()
            .map(AttachmentUrl::as_str)
            .collect::<Vec<_>>(),
        vec![attachment.as_str(), external_attachment.as_str()]
    );
    let legacy_columns = sqlx::query_as::<_, (Option<Uuid>, Option<Uuid>)>(
        "SELECT organization_id, assigned_by_principal_id FROM commit.todos WHERE id = $1",
    )
    .bind(todo_id.into_uuid())
    .fetch_one(&pool)
    .await?;
    assert_eq!(
        legacy_columns,
        (None, None),
        "new rows carry no organization"
    );

    let replay = service
        .create(
            &assigner,
            request.clone(),
            create_key.clone(),
            "req-todo-replay",
        )
        .await?;
    assert!(replay.replayed);
    assert_eq!(replay.body, created.body);

    let mut conflicting_request = request;
    conflicting_request.title = "Different semantic request".to_owned();
    assert_conflict(
        service
            .create(
                &assigner,
                conflicting_request,
                create_key,
                "req-todo-conflict",
            )
            .await,
        "idempotency_key_reused",
    )?;

    // Visibility: owner, assignee, the owner's custodian and the custodian's other Silicons.
    for reader in [&assigner, &assignee, &custodian, &sibling] {
        assert_eq!(service.get(reader, todo_id).await?.id, todo_id);
    }
    assert_not_found(service.get(&outsider, todo_id).await)?;

    assert_denied(
        service
            .update(
                &sibling,
                todo_id,
                TodoPatch {
                    title: Some("A sibling only reads".to_owned()),
                    ..TodoPatch::default()
                },
                unique_key("todo-sibling")?,
                "req-todo-sibling",
            )
            .await,
        "todo_change_not_allowed",
    )?;
    assert_not_found(
        service
            .update(
                &outsider,
                todo_id,
                TodoPatch {
                    title: Some("Unauthorized rewrite".to_owned()),
                    ..TodoPatch::default()
                },
                unique_key("todo-forbidden")?,
                "req-todo-forbidden",
            )
            .await,
    )?;

    let updated = service
        .update(
            &assignee,
            todo_id,
            TodoPatch {
                project_id: NullablePatch::Absent,
                title: None,
                description: NullablePatch::Absent,
                assigned_to: None,
                status: Some(TodoStatus::InProgress),
                attachments: None,
            },
            unique_key("todo-update")?,
            "req-todo-update",
        )
        .await?;
    assert_eq!(
        updated.body.get("status"),
        Some(&serde_json::json!("in_progress"))
    );
    assert_denied(
        service
            .update(
                &assignee,
                todo_id,
                TodoPatch {
                    title: Some("assignee cannot rename".to_owned()),
                    ..TodoPatch::default()
                },
                unique_key("todo-assignee-rename")?,
                "req-todo-assignee-rename",
            )
            .await,
        "todo_change_not_allowed",
    )?;

    // The custodian manages its Silicon's todo, as itself.
    let renamed = service
        .update(
            &custodian,
            todo_id,
            TodoPatch {
                title: Some("Prepare the launch review (custodian)".to_owned()),
                ..TodoPatch::default()
            },
            unique_key("todo-custodian-rename")?,
            "req-todo-custodian-rename",
        )
        .await?;
    assert_eq!(
        renamed.body["assigned_by"]["uuid"],
        assigner.uuid().as_str()
    );
    let audit_actor = sqlx::query_scalar::<_, String>(
        "SELECT actor_account FROM commit.audit_events WHERE resource_id = $1 AND request_id = 'req-todo-custodian-rename'",
    )
    .bind(todo_id.into_uuid())
    .fetch_one(&pool)
    .await?;
    assert_eq!(audit_actor, custodian.uuid().as_str());

    let note = service
        .add_note(
            &assignee,
            todo_id,
            TodoNoteCreate {
                body: "Work has started".to_owned(),
            },
            unique_key("todo-note")?,
            "req-todo-note",
        )
        .await?;
    assert_eq!(note.status, 201);
    assert_eq!(note.body["author"]["uuid"], assignee.uuid().as_str());
    let custodian_note = service
        .add_note(
            &custodian,
            todo_id,
            TodoNoteCreate {
                body: "Custodian checking in".to_owned(),
            },
            unique_key("todo-custodian-note")?,
            "req-todo-custodian-note",
        )
        .await?;
    assert_eq!(
        custodian_note.body["author"]["uuid"],
        custodian.uuid().as_str()
    );
    assert_denied(
        service
            .add_note(
                &sibling,
                todo_id,
                TodoNoteCreate {
                    body: "siblings read, they do not write".to_owned(),
                },
                unique_key("todo-sibling-note")?,
                "req-todo-sibling-note",
            )
            .await,
        "todo_note_not_allowed",
    )?;

    let page_limit = PageLimit::new(1)?;
    let first_note_page = service
        .list_notes(
            &assigner,
            todo_id,
            CollectionQuery {
                cursor: None,
                limit: page_limit,
            },
        )
        .await?;
    assert_eq!(first_note_page.items.len(), 1);
    assert!(first_note_page.next_cursor.is_some());
    let second_note_page = service
        .list_notes(
            &assigner,
            todo_id,
            CollectionQuery {
                cursor: first_note_page.next_cursor,
                limit: page_limit,
            },
        )
        .await?;
    assert_eq!(second_note_page.items.len(), 1);
    assert_ne!(first_note_page.items[0].id, second_note_page.items[0].id);
    assert_not_found(
        service
            .list_notes(&outsider, todo_id, CollectionQuery::default())
            .await,
    )?;

    let assigned_page = service.list(&assignee, TodoQuery::default()).await?;
    assert!(assigned_page.items.iter().any(|todo| todo.id == todo_id));
    let custodian_all = service
        .list(
            &custodian,
            serde_json::from_value(serde_json::json!({ "view": "all" }))?,
        )
        .await?;
    assert!(custodian_all.items.iter().any(|todo| todo.id == todo_id));
    let custodian_mine = service.list(&custodian, TodoQuery::default()).await?;
    assert!(custodian_mine.items.iter().all(|todo| todo.id != todo_id));
    let outsider_all = service
        .list(
            &outsider,
            serde_json::from_value(serde_json::json!({ "view": "all" }))?,
        )
        .await?;
    assert!(outsider_all.items.iter().all(|todo| todo.id != todo_id));
    let filtered = service
        .list(
            &custodian,
            serde_json::from_value(serde_json::json!({
                "view": "all",
                "assigned_to": assignee.actor.id.as_str().to_ascii_uppercase(),
                "assigned_by": assigner.uuid().as_str(),
            }))?,
        )
        .await?;
    assert_eq!(filtered.items.len(), 1);

    assert_not_found(
        service
            .delete(&outsider, todo_id, "req-todo-delete-outsider")
            .await,
    )?;
    assert_denied(
        service
            .delete(&assignee, todo_id, "req-todo-delete-assignee")
            .await,
        "not_todo_owner",
    )?;
    service
        .delete(&assigner, todo_id, "req-todo-delete")
        .await?;
    service
        .delete(&assigner, todo_id, "req-todo-delete-retry")
        .await?;
    assert_not_found(service.get(&assigner, todo_id).await)?;

    let routing = sqlx::query_as::<_, (i16, String, String)>(
        r"
        SELECT payload_version, recipient_silicon_account, payload->'silicon'->>'uuid'
          FROM commit.outbox_events
         WHERE todo_id = $1
         ORDER BY created_at, id
        ",
    )
    .bind(todo_id.into_uuid())
    .fetch_all(&pool)
    .await?;
    // status change, custodian rename, two notes and the deletion notify the delegating Silicon.
    assert_eq!(routing.len(), 5);
    assert!(routing.iter().all(|(version, recipient, silicon)| {
        *version == 3
            && recipient == assigner.uuid().as_str()
            && silicon == assigner.uuid().as_str()
    }));
    let expected_audit_seconds = i64::try_from(AUDIT_RETENTION.as_secs())?;
    let retention_windows_match = sqlx::query_scalar::<_, bool>(
        r"
        SELECT COALESCE(bool_and(round(extract(epoch FROM retain_until - occurred_at))::bigint = $2), false)
          FROM commit.audit_events
         WHERE resource_id = $1 AND action LIKE 'todo.%'
        ",
    )
    .bind(todo_id.into_uuid())
    .bind(expected_audit_seconds)
    .fetch_one(&pool)
    .await?;
    assert!(retention_windows_match);
    Ok(())
}

#[derive(Debug, Eq, PartialEq, sqlx::FromRow)]
struct PersistedRoutingSnapshot {
    event_id: Uuid,
    event_type: String,
    payload_version: i16,
    webhook_url: String,
    destination_version: i64,
    subscription_level: String,
    subscription_scope: String,
    subscription_version: i64,
}

async fn outbox_count(pool: &sqlx::PgPool, todo_id: TodoId) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT count(*) FROM commit.outbox_events WHERE todo_id = $1")
        .bind(todo_id.into_uuid())
        .fetch_one(pool)
        .await
}

async fn routing_snapshots(
    pool: &sqlx::PgPool,
    todo_id: TodoId,
) -> Result<Vec<PersistedRoutingSnapshot>, sqlx::Error> {
    sqlx::query_as(
        r"
        SELECT id AS event_id, event_type, payload_version, webhook_url, destination_version,
               subscription_level::text AS subscription_level,
               subscription_scope::text AS subscription_scope,
               subscription_version
          FROM commit.outbox_events
         WHERE todo_id = $1
         ORDER BY created_at, id
        ",
    )
    .bind(todo_id.into_uuid())
    .fetch_all(pool)
    .await
}

#[tokio::test]
async fn notification_settings_drive_effective_routing_and_immutable_snapshots()
-> anyhow::Result<()> {
    let Some(pool) = test_pool().await? else {
        return Ok(());
    };
    let world = World::new(&pool);
    let custodian = world.carbon("notification-custodian").await?;
    let delegating_silicon = world.silicon("notification-owner", &custodian).await?;
    let custodian = world.refreshed(&custodian).await?;
    let stranger = world.carbon("notification-stranger").await?;
    let other_custodian = world.carbon("notification-other-custodian").await?;
    let other_silicon = world
        .silicon("notification-outsider", &other_custodian)
        .await?;
    let assignee = world.carbon("notification-assignee").await?;
    let directory = Arc::new(Directory::with(&[
        &delegating_silicon,
        &assignee,
        &other_silicon,
    ]));
    let settings_service = notifications(&pool, Arc::clone(&directory));

    let empty_settings = settings_service
        .get_settings(&delegating_silicon, None)
        .await?;
    assert_eq!(empty_settings.version.get(), 0);
    assert!(empty_settings.webhook_url.is_none());
    assert!(matches!(
        settings_service.get_settings(&assignee, None).await,
        Err(AppError::Validation { .. })
    ));
    // The custodian reads its Silicon's settings; a stranger cannot.
    let by_id = Some(&delegating_silicon.actor.id);
    assert_eq!(
        settings_service
            .get_settings(&custodian, by_id)
            .await?
            .version
            .get(),
        0
    );
    assert_denied(
        settings_service.get_settings(&stranger, by_id).await,
        "not_custodian",
    )?;

    let webhook_v1 = format!(
        "https://hook.example.com/silicon/{}/A1B2C3",
        delegating_silicon.actor.id
    );
    let initial_settings = NotificationSettingsUpdate {
        webhook_url: Some(webhook_v1.clone()),
        todo_list_subscription: Some(NotificationRuleInput {
            scope: NotificationScope::StatusUpdates,
            statuses: Vec::new(),
        }),
    };
    // Written by the custodian on the Silicon's behalf; audited as the custodian.
    let created_settings = settings_service
        .replace_settings(
            &custodian,
            by_id,
            initial_settings.clone(),
            ExpectedNotificationVersion::new(0)?,
            "req-notification-create",
        )
        .await?;
    assert_eq!(created_settings.version.get(), 1);
    let audit_actor = sqlx::query_scalar::<_, String>(
        "SELECT actor_account FROM commit.audit_events WHERE request_id = 'req-notification-create' ORDER BY occurred_at DESC LIMIT 1",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(audit_actor, custodian.uuid().as_str());
    assert_eq!(
        settings_service
            .get_settings(&delegating_silicon, None)
            .await?
            .version,
        created_settings.version
    );
    let stale_identical = settings_service
        .replace_settings(
            &delegating_silicon,
            None,
            initial_settings,
            ExpectedNotificationVersion::new(0)?,
            "req-notification-stale-identical",
        )
        .await?;
    assert_eq!(stale_identical, created_settings);
    let stale_different = settings_service
        .replace_settings(
            &delegating_silicon,
            None,
            NotificationSettingsUpdate {
                webhook_url: Some(webhook_v1.clone()),
                todo_list_subscription: Some(NotificationRuleInput {
                    scope: NotificationScope::AnyUpdate,
                    statuses: Vec::new(),
                }),
            },
            ExpectedNotificationVersion::new(0)?,
            "req-notification-stale-different",
        )
        .await;
    assert!(matches!(
        stale_different,
        Err(AppError::Conflict { ref code }) if code.as_ref() == "notification_settings_version_conflict"
    ));

    let todo_service = todos(&pool, Arc::clone(&directory));
    let created_todo = todo_service
        .create(
            &delegating_silicon,
            TodoCreate {
                project_id: None,
                title: "Exercise notification routing".to_owned(),
                description: None,
                assigned_to: assignee.actor.id.clone(),
                status: TodoStatus::YetToDo,
                attachments: Vec::new(),
            },
            unique_key("notification-todo-create")?,
            "req-notification-todo-create",
        )
        .await?;
    let todo_id = TodoId::from_uuid(response_uuid(&created_todo, "id")?);
    assert_eq!(outbox_count(&pool, todo_id).await?, 0);

    let empty_override = settings_service
        .get_todo_subscription(&delegating_silicon, todo_id)
        .await?;
    assert_eq!(empty_override.version.get(), 0);
    assert_not_found(
        settings_service
            .get_todo_subscription(&other_silicon, todo_id)
            .await,
    )?;
    assert_denied(
        settings_service
            .get_todo_subscription(&assignee, todo_id)
            .await,
        "not_todo_owner",
    )?;

    todo_service
        .add_note(
            &assignee,
            todo_id,
            TodoNoteCreate {
                body: "A status-only list rule must ignore this note.".to_owned(),
            },
            unique_key("notification-note-ignored")?,
            "req-notification-note-ignored",
        )
        .await?;
    assert_eq!(outbox_count(&pool, todo_id).await?, 0);

    todo_service
        .update(
            &assignee,
            todo_id,
            TodoPatch {
                status: Some(TodoStatus::InProgress),
                ..TodoPatch::default()
            },
            unique_key("notification-status-first")?,
            "req-notification-status-first",
        )
        .await?;
    assert_eq!(outbox_count(&pool, todo_id).await?, 1);

    let override_resource = settings_service
        .replace_todo_subscription(
            &custodian,
            todo_id,
            TodoNotificationSubscriptionUpdate {
                subscription: Some(NotificationRuleInput {
                    scope: NotificationScope::SpecificStatuses,
                    statuses: vec![TodoStatus::Blocked],
                }),
            },
            ExpectedNotificationVersion::new(0)?,
            "req-notification-override-create",
        )
        .await?;
    assert_eq!(override_resource.version.get(), 1);

    todo_service
        .update(
            &assignee,
            todo_id,
            TodoPatch {
                status: Some(TodoStatus::Completed),
                ..TodoPatch::default()
            },
            unique_key("notification-override-nonmatch")?,
            "req-notification-override-nonmatch",
        )
        .await?;
    assert_eq!(
        outbox_count(&pool, todo_id).await?,
        1,
        "a nonmatching override suppresses the list rule"
    );
    todo_service
        .update(
            &assignee,
            todo_id,
            TodoPatch {
                status: Some(TodoStatus::Blocked),
                ..TodoPatch::default()
            },
            unique_key("notification-override-match")?,
            "req-notification-override-match",
        )
        .await?;
    assert_eq!(outbox_count(&pool, todo_id).await?, 2);

    let snapshots = routing_snapshots(&pool, todo_id).await?;
    assert_eq!(
        snapshots
            .iter()
            .map(|snapshot| (
                snapshot.payload_version,
                snapshot.destination_version,
                snapshot.subscription_level.as_str(),
                snapshot.subscription_scope.as_str(),
                snapshot.subscription_version,
            ))
            .collect::<Vec<_>>(),
        vec![
            (3, 1, "list", "status_updates", 1),
            (3, 1, "todo", "specific_statuses", 1)
        ]
    );
    assert!(
        snapshots
            .iter()
            .all(|snapshot| snapshot.webhook_url == webhook_v1)
    );
    let latest = snapshots.last().context("routed event")?;
    let forged_routing_rewrite = sqlx::query(
        "UPDATE commit.outbox_events SET webhook_url = 'https://hook.example.com/silicon/forged/ABCDEF' WHERE id = $1",
    )
    .bind(latest.event_id)
    .execute(&pool)
    .await;
    assert_database_code(forged_routing_rewrite, "23514")?;
    assert_eq!(latest.event_type, "todo.status_changed");
    Ok(())
}

fn patch() -> ProjectPatch {
    ProjectPatch::default()
}

#[tokio::test]
async fn project_lifecycle_enforces_authorization_diary_cas_and_atomic_completion()
-> anyhow::Result<()> {
    let Some(pool) = test_pool().await? else {
        return Ok(());
    };
    let world = World::new(&pool);
    let custodian = world.carbon("project-custodian").await?;
    let creator = world.silicon("project-silicon", &custodian).await?;
    let sibling = world.silicon("project-sibling", &custodian).await?;
    let stranger = world.carbon("project-stranger").await?;
    let directory = Arc::new(Directory::with(&[&creator, &sibling, &stranger]));
    let service = projects(&pool, Arc::clone(&directory));
    let request = ProjectCreate {
        details: silicon_commit::domain::project::ProjectDetails {
            private: true,
            ..Default::default()
        },
        tasks: Vec::new(),
        name: "Ship the Commit backend".to_owned(),
        silicon_ids: vec![creator.actor.id.clone()],
    };
    let create_key = unique_key("project-create")?;
    let created = service
        .create_project(
            &creator,
            request.clone(),
            create_key.clone(),
            "req-project-create",
        )
        .await?;
    assert_eq!(created.status, 201);
    assert_eq!(created.body["owner"]["uuid"], creator.uuid().as_str());
    assert_eq!(created.body["created_by"]["uuid"], creator.uuid().as_str());
    assert!(created.body.get("tags").is_none() && created.body.get("org_id").is_none());
    let project_id = ProjectId::from_uuid(response_uuid(&created, "id")?);
    let locator = ProjectLocator::Id(project_id);

    let replay = service
        .create_project(
            &creator,
            request.clone(),
            create_key.clone(),
            "req-project-replay",
        )
        .await?;
    assert!(replay.replayed);
    assert_eq!(replay.body, created.body);
    assert_conflict(
        service
            .create_project(
                &creator,
                ProjectCreate {
                    details: silicon_commit::domain::project::ProjectDetails::default(),
                    tasks: Vec::new(),
                    name: "A different project".to_owned(),
                    silicon_ids: request.silicon_ids,
                },
                create_key,
                "req-project-conflict",
            )
            .await,
        "idempotency_key_reused",
    )?;

    // Private: members and the custodians of member Silicons only.
    assert_not_found(service.get_project(&stranger, &locator).await)?;
    assert_not_found(service.get_project(&sibling, &locator).await)?;
    let custodian = world.refreshed(&custodian).await?;
    assert_eq!(
        service.get_project(&custodian, &locator).await?.id,
        project_id
    );
    assert_not_found(
        service
            .update_project(
                &stranger,
                &locator,
                ProjectPatch {
                    name: Some("Unauthorized name".to_owned()),
                    ..patch()
                },
                unique_key("project-forbidden")?,
                "req-project-forbidden",
            )
            .await,
    )?;

    let owner_removal = service
        .update_project(
            &creator,
            &locator,
            ProjectPatch {
                silicon_ids: Some(vec![sibling.actor.id.clone()]),
                ..patch()
            },
            unique_key("project-remove-owner")?,
            "req-project-remove-owner",
        )
        .await;
    assert!(matches!(
        owner_removal,
        Err(AppError::Validation { ref details }) if details.get("participants").is_some()
    ));

    let mut owner_removal_transaction = pool.begin().await?;
    sqlx::query(
        r"
        UPDATE commit.project_participants
           SET removed_by_account = $2, removed_at = transaction_timestamp()
         WHERE project_id = $1 AND participant_account = $2 AND removed_at IS NULL
        ",
    )
    .bind(project_id.into_uuid())
    .bind(creator.uuid().as_str())
    .execute(&mut *owner_removal_transaction)
    .await?;
    let owner_constraint = sqlx::query("SET CONSTRAINTS ALL IMMEDIATE")
        .execute(&mut *owner_removal_transaction)
        .await;
    assert_database_code(owner_constraint, "23514")?;
    owner_removal_transaction.rollback().await?;

    let updated = service
        .update_project(
            &creator,
            &locator,
            ProjectPatch {
                name: Some("Ship the production Commit backend".to_owned()),
                status: Some(ProjectStatus::InProgress),
                ..patch()
            },
            unique_key("project-update")?,
            "req-project-update",
        )
        .await?;
    assert_eq!(
        updated.body.get("status"),
        Some(&serde_json::json!("in_progress"))
    );
    // The custodian of a member Silicon changes the project as itself.
    service
        .update_project(
            &custodian,
            &locator,
            ProjectPatch {
                description: Some("Edited by the custodian".to_owned()),
                ..patch()
            },
            unique_key("project-custodian-update")?,
            "req-project-custodian-update",
        )
        .await?;

    let initial_diary = service.get_diary(&creator, &locator).await?;
    assert_eq!(initial_diary.version.get(), 1);
    let diary = service
        .replace_diary(
            &creator,
            &locator,
            DiaryUpdate {
                markdown: "# Delivery log\n\nThe first production milestone is live.".to_owned(),
            },
            ExpectedDiaryVersion::new(1)?,
            "req-diary-update",
        )
        .await?;
    assert_eq!(diary.version.get(), 2);
    assert_eq!(diary.updated_by.uuid, *creator.uuid());
    assert!(matches!(
        service
            .replace_diary(
                &creator,
                &locator,
                DiaryUpdate {
                    markdown: "stale replacement".to_owned(),
                },
                ExpectedDiaryVersion::new(1)?,
                "req-diary-stale",
            )
            .await,
        Err(AppError::Conflict { ref code }) if code == "diary_version_mismatch"
    ));

    let task_request = ProjectTaskCreate {
        assigned_to: None,
        parent_task_id: None,
        title: "Deploy PostgreSQL migrations".to_owned(),
        description: "Apply all forward-only migrations.".to_owned(),
        status: TodoStatus::YetToDo,
    };
    let task_key = unique_key("project-task")?;
    let task = service
        .create_task(
            &creator,
            &locator,
            task_request.clone(),
            task_key.clone(),
            "req-task-create",
        )
        .await?;
    let task_id = ProjectTaskId::from_uuid(response_uuid(&task, "id")?);
    assert!(
        service
            .create_task(
                &creator,
                &locator,
                task_request,
                task_key,
                "req-task-replay"
            )
            .await?
            .replayed
    );
    let completed_task = service
        .update_task(
            &creator,
            &locator,
            task_id,
            ProjectTaskPatch {
                assigned_to: NullablePatch::Absent,
                title: None,
                description: None,
                status: Some(TodoStatus::Completed),
            },
            "req-task-update",
        )
        .await?;
    assert_eq!(completed_task.status, TodoStatus::Completed);
    let subtask = service
        .create_task(
            &creator,
            &locator,
            ProjectTaskCreate {
                assigned_to: None,
                parent_task_id: Some(task_id),
                title: "Verify the migrated schema".to_owned(),
                description: "Run the PostgreSQL integration gate.".to_owned(),
                status: TodoStatus::Completed,
            },
            unique_key("project-subtask")?,
            "req-subtask-create",
        )
        .await?;
    let subtask_id = ProjectTaskId::from_uuid(response_uuid(&subtask, "id")?);

    service
        .create_blocker(
            &creator,
            &locator,
            BlockerCreate {
                title: "Production credential".to_owned(),
                description: "Awaiting the deployment secret.".to_owned(),
                status: BlockerStatus::Open,
            },
            unique_key("project-blocker")?,
            "req-blocker-create",
        )
        .await?;
    service
        .create_update(
            &creator,
            &locator,
            ProjectUpdateCreate {
                title: "Credential supplied".to_owned(),
                description: "Deployment may continue.".to_owned(),
            },
            unique_key("project-entry-update")?,
            "req-entry-update",
        )
        .await?;
    let completion_request = ProjectCompletionCreate {
        title: "Backend shipped".to_owned(),
        description: "The production service is operational.".to_owned(),
    };
    let completion_key = unique_key("project-completion")?;
    let completion = service
        .complete_project(
            &creator,
            &locator,
            completion_request.clone(),
            completion_key.clone(),
            "req-project-complete",
        )
        .await?;
    assert_eq!(completion.status, 201);
    assert!(
        service
            .complete_project(
                &creator,
                &locator,
                completion_request.clone(),
                completion_key,
                "req-project-complete-replay"
            )
            .await?
            .replayed
    );
    assert!(matches!(
        service
            .complete_project(
                &creator,
                &locator,
                completion_request,
                unique_key("project-completion-duplicate")?,
                "req-project-complete-duplicate",
            )
            .await,
        Err(AppError::Conflict { ref code }) if code == "project_already_completed"
    ));

    let first_entries = service
        .list_entries(
            &creator,
            &locator,
            CollectionQuery {
                limit: PageLimit::new(1)?,
                cursor: None,
            },
        )
        .await?;
    assert_eq!(first_entries.items.len(), 1);
    assert_eq!(
        first_entries.items[0].entry_type,
        silicon_commit::domain::ProjectEntryType::Completion
    );
    let remaining_entries = service
        .list_entries(
            &creator,
            &locator,
            CollectionQuery {
                limit: PageLimit::new(10)?,
                cursor: first_entries.next_cursor,
            },
        )
        .await?;
    assert_eq!(remaining_entries.items.len(), 2);
    assert_not_found(
        service
            .list_entries(&stranger, &locator, CollectionQuery::default())
            .await,
    )?;
    assert_conflict(
        service
            .update_project(
                &creator,
                &locator,
                ProjectPatch {
                    status: Some(ProjectStatus::InProgress),
                    ..patch()
                },
                unique_key("project-reopen")?,
                "req-project-reopen",
            )
            .await,
        "project_already_completed",
    )?;
    let direct_reopen =
        sqlx::query("UPDATE commit.projects SET status = 'in_progress' WHERE id = $1")
            .bind(project_id.into_uuid())
            .execute(&pool)
            .await;
    assert_database_code(direct_reopen, "23514")?;

    let completed = service.get_project(&creator, &locator).await?;
    assert_eq!(completed.status, ProjectStatus::Completed);
    let listed = service
        .list_projects(&creator, ProjectQuery::default())
        .await?;
    assert!(listed.items.iter().any(|project| project.id == project_id));
    let by_member: ProjectQuery =
        serde_json::from_value(serde_json::json!({ "silicon_id": creator.actor.id }))?;
    assert!(
        service
            .list_projects(&custodian, by_member)
            .await?
            .items
            .iter()
            .any(|project| project.id == project_id)
    );
    assert!(
        service
            .list_projects(&stranger, ProjectQuery::default())
            .await?
            .items
            .iter()
            .all(|project| project.id != project_id)
    );
    let tasks = service
        .list_tasks(&creator, &locator, CollectionQuery::default())
        .await?;
    assert_eq!(tasks.items.len(), 2);
    assert!(tasks.items.iter().any(|task| task.id == subtask_id));
    let entry_count = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM commit.project_entries WHERE project_id = $1",
    )
    .bind(project_id.into_uuid())
    .fetch_one(&pool)
    .await?;
    assert_eq!(entry_count, 3);
    Ok(())
}

#[tokio::test]
async fn project_retries_replay_after_membership_is_revoked() -> anyhow::Result<()> {
    let Some(pool) = test_pool().await? else {
        return Ok(());
    };
    let world = World::new(&pool);
    let custodian = world.carbon("replay-custodian").await?;
    let creator = world.silicon("replay-creator", &custodian).await?;
    let member = world.silicon("replay-member", &custodian).await?;
    let service = projects(&pool, Arc::new(Directory::with(&[&creator, &member])));
    let created = service
        .create_project(
            &creator,
            ProjectCreate {
                details: silicon_commit::domain::project::ProjectDetails::default(),
                tasks: Vec::new(),
                name: "Replay authorization boundary".to_owned(),
                silicon_ids: vec![creator.actor.id.clone(), member.actor.id.clone()],
            },
            unique_key("project-replay-create")?,
            "req-project-replay-create",
        )
        .await?;
    let project_id = ProjectId::from_uuid(response_uuid(&created, "id")?);
    let locator = ProjectLocator::Id(project_id);

    let patch_request = ProjectPatch {
        name: Some("Replay authorization boundary updated".to_owned()),
        status: Some(ProjectStatus::InProgress),
        ..patch()
    };
    let patch_key = unique_key("project-replay-patch")?;
    let patch_response = service
        .update_project(
            &member,
            &locator,
            patch_request.clone(),
            patch_key.clone(),
            "req-project-replay-patch",
        )
        .await?;
    let task_request = ProjectTaskCreate {
        assigned_to: None,
        parent_task_id: None,
        title: "Persist retry response".to_owned(),
        description: "The response survives mutable authorization.".to_owned(),
        status: TodoStatus::YetToDo,
    };
    let task_key = unique_key("project-replay-task")?;
    let task_response = service
        .create_task(
            &member,
            &locator,
            task_request.clone(),
            task_key.clone(),
            "req-project-replay-task",
        )
        .await?;
    let blocker_request = BlockerCreate {
        title: "Mutable membership".to_owned(),
        description: "Membership may be revoked after commit.".to_owned(),
        status: BlockerStatus::Open,
    };
    let blocker_key = unique_key("project-replay-blocker")?;
    let blocker_response = service
        .create_blocker(
            &member,
            &locator,
            blocker_request.clone(),
            blocker_key.clone(),
            "req-project-replay-blocker",
        )
        .await?;
    let update_request = ProjectUpdateCreate {
        title: "Retry captured".to_owned(),
        description: "The original response is durable.".to_owned(),
    };
    let update_key = unique_key("project-replay-update")?;
    let update_response = service
        .create_update(
            &member,
            &locator,
            update_request.clone(),
            update_key.clone(),
            "req-project-replay-update",
        )
        .await?;
    let completion_request = ProjectCompletionCreate {
        title: "Replay test completed".to_owned(),
        description: "Completion is immutable and replayable.".to_owned(),
    };
    let completion_key = unique_key("project-replay-completion")?;
    let completion_response = service
        .complete_project(
            &member,
            &locator,
            completion_request.clone(),
            completion_key.clone(),
            "req-project-replay-completion",
        )
        .await?;

    service
        .update_project(
            &creator,
            &locator,
            ProjectPatch {
                silicon_ids: Some(vec![creator.actor.id.clone()]),
                ..patch()
            },
            unique_key("project-revoke-member")?,
            "req-project-revoke-member",
        )
        .await?;

    // The former member is still in the owner's circle: it reads the (public) project but no
    // longer changes it.
    assert_eq!(service.get_project(&member, &locator).await?.id, project_id);
    assert_denied(
        service
            .create_task(
                &member,
                &locator,
                ProjectTaskCreate {
                    title: "Must not commit".to_owned(),
                    ..task_request.clone()
                },
                unique_key("project-new-task-after-revocation")?,
                "req-project-new-task-after-revocation",
            )
            .await,
        "project_not_writable",
    )?;

    let replays = [
        service
            .update_project(
                &member,
                &locator,
                patch_request,
                patch_key,
                "req-project-replay-patch-again",
            )
            .await?,
        service
            .create_task(
                &member,
                &locator,
                task_request,
                task_key,
                "req-project-replay-task-again",
            )
            .await?,
        service
            .create_blocker(
                &member,
                &locator,
                blocker_request,
                blocker_key,
                "req-project-replay-blocker-again",
            )
            .await?,
        service
            .create_update(
                &member,
                &locator,
                update_request,
                update_key,
                "req-project-replay-update-again",
            )
            .await?,
        service
            .complete_project(
                &member,
                &locator,
                completion_request,
                completion_key,
                "req-project-replay-completion-again",
            )
            .await?,
    ];
    for (replay, original) in replays.into_iter().zip([
        patch_response,
        task_response,
        blocker_response,
        update_response,
        completion_response,
    ]) {
        assert!(replay.replayed);
        assert_eq!(replay.status, original.status);
        assert_eq!(replay.body, original.body);
    }
    Ok(())
}

#[tokio::test]
async fn retention_redacts_only_expired_tombstone_content() -> anyhow::Result<()> {
    let Some(pool) = test_pool().await? else {
        return Ok(());
    };
    let world = World::new(&pool);
    let actor = world.carbon("retention-carbon").await?;
    let recipient = world.silicon("retention-silicon", &actor).await?;

    let todo_id = TodoId::new();
    let note_id = Uuid::now_v7();
    let activity_id = Uuid::now_v7();
    let expired_activity_id = Uuid::now_v7();
    // Rows created under Silicon Accounts carry no organization.
    sqlx::query(
        r"
        INSERT INTO commit.todos (
            id, title, description, assigned_by_account, assigned_to_account, status,
            created_at, updated_at, deleted_at, content_retain_until, deleted_by_account
        ) VALUES (
            $1, 'private retired title', 'private retired description', $2, $2,
            'completed'::commit.todo_status, '1900-01-01 UTC', '1901-01-01 UTC',
            '1901-01-01 UTC', '1902-01-01 UTC', $2
        )
        ",
    )
    .bind(todo_id.into_uuid())
    .bind(actor.uuid().as_str())
    .execute(&pool)
    .await?;
    sqlx::query(
        r"
        INSERT INTO commit.todo_attachments (todo_id, position, url, created_at)
        VALUES ($1, 0, 'https://briefcase.example/api/v1/entries/018f268d-715a-7b72-8f0f-41f16f9af553', '1900-01-01 UTC')
        ",
    )
    .bind(todo_id.into_uuid())
    .execute(&pool)
    .await?;
    sqlx::query(
        r"
        INSERT INTO commit.todo_notes (id, todo_id, author_account, body, created_at)
        VALUES ($1, $2, $3, 'private retired note', '1900-01-01 UTC')
        ",
    )
    .bind(note_id)
    .bind(todo_id.into_uuid())
    .bind(actor.uuid().as_str())
    .execute(&pool)
    .await?;
    sqlx::query(
        r#"
        INSERT INTO commit.todo_activity (
            id, todo_id, activity_type, actor_account, request_id, changes, created_at, retain_until
        ) VALUES (
            $1, $2, 'deleted'::commit.todo_activity_type, $3, 'req-retention-fixture',
            '{"private":"retired detail"}'::jsonb, '1901-01-01 UTC', '2300-01-01 UTC'
        ), (
            $4, $2, 'deleted'::commit.todo_activity_type, $3, 'req-expired-activity-fixture',
            '{"private":"expired detail"}'::jsonb, '1901-01-01 UTC', '1902-01-01 UTC'
        )
        "#,
    )
    .bind(activity_id)
    .bind(todo_id.into_uuid())
    .bind(actor.uuid().as_str())
    .bind(expired_activity_id)
    .execute(&pool)
    .await?;

    let canonical_activity_redaction =
        sqlx::query("UPDATE commit.todo_activity SET changes = '{}'::jsonb WHERE id = $1")
            .bind(expired_activity_id)
            .execute(&pool)
            .await?;
    assert_eq!(canonical_activity_redaction.rows_affected(), 1);
    let forged_activity_rewrite = sqlx::query(
        r#"UPDATE commit.todo_activity SET changes = '{"forged":true}'::jsonb WHERE id = $1"#,
    )
    .bind(expired_activity_id)
    .execute(&pool)
    .await;
    assert_database_code(forged_activity_rewrite, "55000")?;
    let immutable_note_update = sqlx::query("UPDATE commit.todo_notes SET id = id WHERE id = $1")
        .bind(note_id)
        .execute(&pool)
        .await;
    assert_database_code(immutable_note_update, "55000")?;

    let live_replay_id = Uuid::now_v7();
    sqlx::query(
        r#"
        INSERT INTO commit.idempotency_records (
            id, todo_id, actor_account, operation, resource_path, idempotency_key,
            request_fingerprint, response_status, response_body, created_at, expires_at
        ) VALUES (
            $1, $2, $3, 'updateTodo', '/todos/' || $2::text, 'retention-replay-key',
            decode(repeat('00', 32), 'hex'), 200,
            '{"title":"private retired title","description":"private retired description"}'::jsonb,
            '1900-01-01 UTC', '2300-01-01 UTC'
        )
        "#,
    )
    .bind(live_replay_id)
    .bind(todo_id.into_uuid())
    .bind(actor.uuid().as_str())
    .execute(&pool)
    .await?;

    for (status, column) in [
        ("delivered", "delivered_at"),
        ("dead_letter", "dead_lettered_at"),
    ] {
        let error_code = if status == "dead_letter" {
            "'provider_unavailable'"
        } else {
            "NULL"
        };
        // Fixed identifiers only; the loop chooses between two literal shapes.
        sqlx::query(sqlx::AssertSqlSafe(format!(
            r"
            INSERT INTO commit.outbox_events (
                id, todo_id, recipient_silicon_account, event_type, payload, status, attempt_count,
                available_at, last_error_code, created_at, updated_at, {column}, purge_after
            )
            SELECT event.id, $2, $3, 'todo.retention_test', '{{}}'::jsonb,
                   '{status}'::commit.outbox_status, 1, '1900-01-01 UTC', {error_code},
                   '1900-01-01 UTC', '1901-01-01 UTC', '1901-01-01 UTC', '2002-01-01 UTC'
              FROM unnest($1::uuid[]) AS event(id)
            "
        )))
        .bind(vec![Uuid::now_v7(), Uuid::now_v7()])
        .bind(todo_id.into_uuid())
        .bind(recipient.uuid().as_str())
        .execute(&pool)
        .await?;
    }

    let batch_size = NonZeroUsize::new(1)
        .ok_or_else(|| anyhow::anyhow!("the fixed maintenance batch must be non-zero"))?;
    let policy = RetentionPolicy::new(batch_size);
    let report = retention::run_once(&pool, policy).await?;
    assert_eq!(report.todos_redacted, 0);
    assert_eq!(report.notes_purged, 0);
    assert_eq!(report.attachments_purged, 0);
    assert_eq!(report.activity_changes_redacted, 0);
    assert_eq!(report.activity_purged, 1);
    assert_eq!(report.delivered_outbox_purged, 1);
    assert_eq!(report.dead_letter_outbox_purged, 1);

    let tombstone = sqlx::query_as::<_, (String, Option<String>, i64, bool)>(
        "SELECT title, description, version, updated_at = '1901-01-01 UTC'::timestamptz FROM commit.todos WHERE id = $1",
    )
    .bind(todo_id.into_uuid())
    .fetch_one(&pool)
    .await?;
    assert_eq!(
        tombstone,
        (
            "private retired title".to_owned(),
            Some("private retired description".to_owned()),
            1,
            true
        )
    );
    let retained = sqlx::query_as::<_, (i64, i64, serde_json::Value, i64, i64)>(
        r"
        SELECT (SELECT count(*) FROM commit.todo_notes WHERE todo_id = $1),
               (SELECT count(*) FROM commit.todo_attachments WHERE todo_id = $1),
               (SELECT changes FROM commit.todo_activity WHERE id = $2),
               (SELECT count(*) FROM commit.outbox_events WHERE todo_id = $1 AND status = 'delivered'),
               (SELECT count(*) FROM commit.outbox_events WHERE todo_id = $1 AND status = 'dead_letter')
        ",
    )
    .bind(todo_id.into_uuid())
    .bind(activity_id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(
        retained,
        (1, 1, serde_json::json!({"private": "retired detail"}), 1, 1)
    );

    let removed_replay = sqlx::query("DELETE FROM commit.idempotency_records WHERE id = $1")
        .bind(live_replay_id)
        .execute(&pool)
        .await?;
    assert_eq!(removed_replay.rows_affected(), 1);

    let second_cycle = retention::run_cycle(&pool, policy).await?;
    assert_eq!(second_cycle.passes, 2);
    assert!(!second_cycle.pass_budget_exhausted);
    assert_eq!(second_cycle.totals.activity_purged, 0);
    assert_eq!(second_cycle.totals.todos_redacted, 1);
    assert_eq!(second_cycle.totals.notes_purged, 1);
    assert_eq!(second_cycle.totals.attachments_purged, 1);
    assert_eq!(second_cycle.totals.activity_changes_redacted, 1);
    assert_eq!(second_cycle.totals.delivered_outbox_purged, 1);
    assert_eq!(second_cycle.totals.dead_letter_outbox_purged, 1);

    let redacted = sqlx::query_as::<_, (String, Option<String>, i64, i64, serde_json::Value, i64)>(
        r"
        SELECT todo.title, todo.description,
               (SELECT count(*) FROM commit.todo_notes AS note WHERE note.todo_id = todo.id),
               (SELECT count(*) FROM commit.todo_attachments AS attachment WHERE attachment.todo_id = todo.id),
               (SELECT changes FROM commit.todo_activity AS activity WHERE activity.id = $2),
               (SELECT count(*) FROM commit.todo_activity AS activity WHERE activity.todo_id = todo.id)
          FROM commit.todos AS todo
         WHERE todo.id = $1
        ",
    )
    .bind(todo_id.into_uuid())
    .bind(activity_id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(
        redacted,
        ("[deleted]".to_owned(), None, 0, 0, serde_json::json!({}), 1)
    );
    Ok(())
}

fn accounts(pool: &sqlx::PgPool, directory: Arc<Directory>) -> AccountService {
    AccountService::new(
        pool.clone(),
        directory,
        TOMBSTONE_RETENTION,
        AUDIT_RETENTION,
    )
}

#[tokio::test]
async fn collaborative_projects_keep_private_work_history_and_claims_scoped() -> anyhow::Result<()>
{
    let Some(pool) = test_pool().await? else {
        return Ok(());
    };
    let world = World::new(&pool);
    let creator = world.carbon("alice").await?;
    let bob = world.carbon("bob").await?;
    let worker = world.silicon("worker", &bob).await?;
    let worker_sibling = world.silicon("worker-sibling", &bob).await?;
    let invited = world.carbon("invited").await?;
    let outsider = world.carbon("outsider").await?;
    let bob = world.refreshed(&bob).await?;
    let directory = Arc::new(Directory::with(&[
        &creator,
        &bob,
        &worker,
        &worker_sibling,
        &invited,
        &outsider,
    ]));
    let projects = projects(&pool, Arc::clone(&directory));
    let todos = todos(&pool, Arc::clone(&directory));
    let request = || -> anyhow::Result<ProjectCreate> {
        Ok(serde_json::from_value(serde_json::json!({
            "name": "Private release",
            "private": true,
            "description": "A private release",
            "carbon_ids": [invited.actor.id],
            "tasks": [{"title": "Build", "assigned_to": worker.actor.id, "subtasks": [{"title": "Verify"}]}]
        }))?)
    };

    // Silicons are not open to the world: the worker's custodian must allow the creator first.
    assert_denied(
        projects
            .create_project(
                &creator,
                request()?,
                unique_key("collaborative-unreachable")?,
                "collaborative-unreachable",
            )
            .await,
        "silicon_not_reachable",
    )?;
    let allowlist = accounts(&pool, Arc::clone(&directory))
        .allow(&bob, &worker.actor.id, &creator.actor.id)
        .await?;
    assert_eq!(allowlist.allowed.len(), 1);
    assert_eq!(allowlist.allowed[0].account.uuid, *creator.uuid());
    assert_eq!(allowlist.allowed[0].added_by.uuid, *bob.uuid());

    let created = projects
        .create_project(
            &creator,
            request()?,
            unique_key("collaborative-create")?,
            "collaborative-create",
        )
        .await?;
    let id = ProjectId::from_uuid(response_uuid(&created, "id")?);
    let locator = ProjectLocator::Id(id);
    assert_eq!(created.body["created_by"]["type"], "carbon");
    assert_eq!(
        created.body["collaborators"][0]["uuid"],
        creator.uuid().as_str()
    );
    assert_not_found(projects.get_project(&outsider, &locator).await)?;
    assert!(
        projects
            .list_projects(&outsider, ProjectQuery::default())
            .await?
            .items
            .is_empty()
    );
    assert_not_found(projects.get_project(&worker_sibling, &locator).await)?;
    assert_eq!(projects.get_project(&invited, &locator).await?.id, id);
    // The custodian of a member Silicon reads (and may change) the project.
    assert_eq!(projects.get_project(&bob, &locator).await?.id, id);

    let tasks = projects
        .list_tasks(&worker, &locator, CollectionQuery::default())
        .await?
        .items;
    assert_eq!(tasks.len(), 2);
    let parent = tasks
        .iter()
        .find(|t| t.parent_task_id.is_none())
        .context("parent")?;
    let child = tasks
        .iter()
        .find(|t| t.parent_task_id.is_some())
        .context("child")?;
    let todo_id = parent.todo_id.context("assigned todo")?;
    assert_eq!(todos.get(&worker, todo_id).await?.project_id, Some(id));
    assert_not_found(todos.get(&outsider, todo_id).await)?;
    assert_not_found(
        todos
            .list_notes(&outsider, todo_id, CollectionQuery::default())
            .await,
    )?;
    todos
        .update(
            &worker,
            todo_id,
            TodoPatch {
                status: Some(TodoStatus::Completed),
                ..Default::default()
            },
            unique_key("complete-linked")?,
            "complete-linked",
        )
        .await?;
    assert_eq!(
        projects
            .list_tasks(&creator, &locator, CollectionQuery::default())
            .await?
            .items
            .iter()
            .find(|t| t.id == parent.id)
            .context("updated task")?
            .status,
        TodoStatus::Completed
    );
    let (a, b) = tokio::join!(
        projects.claim_task(
            &creator,
            &locator,
            child.id,
            unique_key("claim-a")?,
            "claim-a"
        ),
        projects.claim_task(
            &worker,
            &locator,
            child.id,
            unique_key("claim-b")?,
            "claim-b"
        ),
    );
    assert_ne!(a.is_ok(), b.is_ok(), "only one concurrent claimant wins");

    let replay_key = unique_key("private-patch")?;
    let contribution = ProjectPatch {
        description: Some("worker contribution".into()),
        ..patch()
    };
    projects
        .update_project(
            &worker,
            &locator,
            contribution.clone(),
            replay_key.clone(),
            "private-patch",
        )
        .await?;
    projects
        .update_project(
            &creator,
            &locator,
            ProjectPatch {
                silicon_ids: Some(vec![]),
                ..patch()
            },
            unique_key("revoke")?,
            "revoke",
        )
        .await?;
    // A removed member loses the private project, its history and its replays...
    assert_not_found(
        projects
            .update_project(
                &worker,
                &locator,
                contribution,
                replay_key,
                "private-replay",
            )
            .await,
    )?;
    assert_not_found(projects.versions(&worker, &locator, None, 20).await)?;
    assert_not_found(projects.get_project(&bob, &locator).await)?;
    // ...but keeps the todo assigned to it (the assignee always sees its own work).
    assert_eq!(todos.get(&worker, todo_id).await?.id, todo_id);

    let assigned = todos
        .create(
            &creator,
            serde_json::from_value(serde_json::json!({
                "title": "Follow up", "assigned_to": worker.actor.id, "project_id": id
            }))?,
            unique_key("private-followup")?,
            "private-followup",
        )
        .await?;
    let assigned_id = TodoId::from_uuid(response_uuid(&assigned, "id")?);
    assert_eq!(todos.get(&worker, assigned_id).await?.project_id, Some(id));
    // Linking the todo shared the project with its assignee again.
    assert_eq!(projects.get_project(&worker, &locator).await?.id, id);
    let read = projects.get_project(&creator, &locator).await?;
    assert!(read.collaborators.iter().any(|a| a.uuid == *worker.uuid()));
    assert!(read.version >= 5);

    projects
        .delete_task(&creator, &locator, parent.id, "delete-subtree")
        .await?;
    assert!(
        projects
            .list_tasks(&creator, &locator, CollectionQuery::default())
            .await?
            .items
            .is_empty()
    );
    assert_not_found(todos.get(&creator, todo_id).await)?;
    let metadata = projects.versions(&creator, &locator, None, 1).await?;
    let version = metadata["items"][0]["version"]
        .as_i64()
        .context("version")?;
    assert_eq!(
        projects.version(&creator, &locator, version).await?["tasks"],
        serde_json::json!([])
    );
    Ok(())
}

#[tokio::test]
async fn contract_sunset_and_email_preferences_enforce_their_lifecycle() -> anyhow::Result<()> {
    let Some(pool) = test_pool().await? else {
        return Ok(());
    };
    let contracts = sqlx::query_as::<_, (i16, String)>(
        "SELECT version::smallint, status::text FROM commit.contract_versions WHERE version IN (1, 2) ORDER BY version",
    )
    .fetch_all(&pool)
    .await?;
    assert_eq!(
        contracts,
        vec![(1, "deprecated".to_owned()), (2, "active".to_owned())]
    );
    let mut tx = pool.begin().await?;
    sqlx::query("INSERT INTO commit.contract_versions(version,status,introduced_at,deprecated_at) VALUES(77,'deprecated',clock_timestamp()-interval '9 days',clock_timestamp()-interval '8 days'),(78,'deprecated',clock_timestamp()-interval '9 days',clock_timestamp())").execute(&mut *tx).await?;
    let retired: String = sqlx::query_scalar("SELECT commit.admit_contract(77,true)")
        .fetch_one(&mut *tx)
        .await?;
    assert_eq!(retired, "sunset");
    let active: String = sqlx::query_scalar("SELECT commit.admit_contract(78,true)")
        .fetch_one(&mut *tx)
        .await?;
    assert_eq!(active, "deprecated");
    let requests: i64 =
        sqlx::query_scalar("SELECT requests FROM commit.contract_versions WHERE version=78")
            .fetch_one(&mut *tx)
            .await?;
    assert_eq!(requests, 0);
    tx.rollback().await?;

    let world = World::new(&pool);
    let actor = world.carbon("recipient").await?;
    let service = projects(&pool, Arc::new(Directory::default()));
    let created = service
        .create_project(
            &actor,
            serde_json::from_value(serde_json::json!({"name":"Email release"}))?,
            unique_key("email-project")?,
            "email-project",
        )
        .await?;
    let id = ProjectId::from_uuid(response_uuid(&created, "id")?);
    // One preference per account (the email the Carbon chose for Commit).
    sqlx::query(
        "INSERT INTO commit.email_preferences(account,email) VALUES($1,'recipient@example.test')",
    )
    .bind(actor.uuid().as_str())
    .execute(&pool)
    .await?;
    service
        .complete_project(
            &actor,
            &ProjectLocator::Id(id),
            ProjectCompletionCreate {
                title: "Done".into(),
                description: "Release complete".into(),
            },
            unique_key("email-completion")?,
            "email-completion",
        )
        .await?;
    let job: Uuid = sqlx::query_scalar("SELECT id FROM commit.email_jobs WHERE account=$1 AND recipient='recipient@example.test' AND kind='project_completed'")
        .bind(actor.uuid().as_str())
        .fetch_one(&pool)
        .await?;
    sqlx::query("UPDATE commit.email_preferences SET enabled=false WHERE account=$1")
        .bind(actor.uuid().as_str())
        .execute(&pool)
        .await?;
    // Claim until this job leaves the queue (other tests may queue jobs in the same database).
    let mut status = String::from("pending");
    for _ in 0..100 {
        let claimed: Option<serde_json::Value> = sqlx::query_scalar("SELECT commit.claim_email()")
            .fetch_optional(&pool)
            .await?;
        status = sqlx::query_scalar("SELECT status FROM commit.email_jobs WHERE id=$1")
            .bind(job)
            .fetch_one(&pool)
            .await?;
        if status != "pending" || claimed.is_none() {
            break;
        }
    }
    assert_eq!(status, "suppressed");
    Ok(())
}

#[tokio::test]
async fn project_history_retains_only_the_latest_thousand_snapshots() -> anyhow::Result<()> {
    let Some(pool) = test_pool().await? else {
        return Ok(());
    };
    let world = World::new(&pool);
    let actor = world.carbon("author").await?;
    let service = projects(&pool, Arc::new(Directory::default()));
    let project = service
        .create_project(
            &actor,
            serde_json::from_value(serde_json::json!({"name":"History"}))?,
            unique_key("history-create")?,
            "history-create",
        )
        .await?;
    let id = response_uuid(&project, "id")?;
    sqlx::query("INSERT INTO commit.audit_events(id,actor_account,action,resource_type,resource_id,request_id) SELECT gen_random_uuid(),$1,'project.updated','project',$2,'history-cap-'||n FROM generate_series(1,1005) n")
        .bind(actor.uuid().as_str())
        .bind(id)
        .execute(&pool)
        .await?;
    let (count, min, max): (i64, i64, i64) = sqlx::query_as(
        "SELECT count(*),min(version),max(version) FROM commit.project_versions WHERE project_id=$1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await?;
    assert_eq!((count, min, max), (1000, 7, 1006));
    let latest_actor: serde_json::Value = sqlx::query_scalar(
        "SELECT actor FROM commit.project_versions WHERE project_id=$1 AND version=1006",
    )
    .bind(id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(
        latest_actor,
        serde_json::json!({"type": "carbon", "id": actor.actor.id.as_str(), "uuid": actor.uuid().as_str()})
    );
    Ok(())
}
