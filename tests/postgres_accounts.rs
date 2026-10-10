//! Silicon Accounts identity in PostgreSQL: the custodian circle, the Silicon
//! allow-list, Accounts webhook events and account deletion.

mod common;

use std::sync::Arc;

use time::{Duration as TimeDuration, OffsetDateTime};
use uuid::Uuid;

use common::{
    AUDIT_RETENTION, Directory, IDEMPOTENCY_TTL, TOMBSTONE_RETENTION, World, assert_denied,
    assert_not_found, response_uuid, test_pool, unique_key,
};
use silicon_commit::{
    application::{
        accounts::{AccountEvent, AccountService},
        ports::VerifiedActor,
        projects::ProjectService,
        todos::TodoService,
    },
    domain::{DomainLimits, ProjectId, ProjectLocator, TodoCreate, TodoId, TodoStatus},
    error::AppError,
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

fn accounts(pool: &sqlx::PgPool, directory: Arc<Directory>) -> AccountService {
    AccountService::new(
        pool.clone(),
        directory,
        TOMBSTONE_RETENTION,
        AUDIT_RETENTION,
    )
}

fn todo_for(assignee: &VerifiedActor, title: &str) -> TodoCreate {
    TodoCreate {
        project_id: None,
        title: title.to_owned(),
        description: None,
        assigned_to: assignee.actor.id.clone(),
        status: TodoStatus::YetToDo,
        attachments: Vec::new(),
    }
}

async fn assign(
    service: &TodoService,
    from: &VerifiedActor,
    to: &VerifiedActor,
    title: &str,
) -> Result<TodoId, AppError> {
    let key = unique_key("assign").map_err(AppError::Internal)?;
    let created = service
        .create(from, todo_for(to, title), key, "req-assign")
        .await?;
    Ok(TodoId::from_uuid(
        response_uuid(&created, "id").map_err(AppError::Internal)?,
    ))
}

#[tokio::test]
async fn the_custodian_circle_is_symmetric_and_bounded() -> anyhow::Result<()> {
    let Some(pool) = test_pool().await? else {
        return Ok(());
    };
    let world = World::new(&pool);
    let carbon = world.carbon("circle-carbon").await?;
    let first = world.silicon("circle-first", &carbon).await?;
    let second = world.silicon("circle-second", &carbon).await?;
    let other_carbon = world.carbon("circle-other").await?;
    let foreign = world.silicon("circle-foreign", &other_carbon).await?;

    let pairs = [
        (&carbon, &first, true),
        (&first, &carbon, true),
        (&first, &second, true),
        (&carbon, &carbon, true),
        (&first, &foreign, false),
        (&carbon, &other_carbon, false),
        (&carbon, &foreign, false),
    ];
    for (a, b, expected) in pairs {
        let in_circle: bool = sqlx::query_scalar("SELECT commit.in_circle($1, $2)")
            .bind(a.uuid().as_str())
            .bind(b.uuid().as_str())
            .fetch_one(&pool)
            .await?;
        assert_eq!(in_circle, expected, "{} / {}", a.actor.id, b.actor.id);
    }
    let mut circle: Vec<String> = sqlx::query_scalar("SELECT commit.circle_of($1)")
        .bind(first.uuid().as_str())
        .fetch_one(&pool)
        .await?;
    assert_eq!(
        circle.remove(0),
        first.uuid().as_str(),
        "the account itself comes first"
    );
    circle.sort();
    let mut expected = vec![
        carbon.uuid().as_str().to_owned(),
        second.uuid().as_str().to_owned(),
    ];
    expected.sort();
    assert_eq!(circle, expected);

    // A Silicon is reachable from its circle only; Carbons from anyone.
    for (from, to, expected) in [
        (&first, &second, true),
        (&carbon, &first, true),
        (&other_carbon, &first, false),
        (&foreign, &carbon, true),
    ] {
        let reachable: bool = sqlx::query_scalar("SELECT commit.may_reach($1, $2)")
            .bind(from.uuid().as_str())
            .bind(to.uuid().as_str())
            .fetch_one(&pool)
            .await?;
        assert_eq!(reachable, expected, "{} -> {}", from.actor.id, to.actor.id);
    }

    // Placeholders for IAM-era principals never join anyone's circle.
    let placeholder = format!("iam:{}:{}", Uuid::new_v4(), Uuid::new_v4());
    sqlx::query(
        "INSERT INTO commit.accounts (uuid, kind, public_id, status, custodian_uuid) VALUES ($1, 'silicon', 'si:legacy', 'unlinked', $2)",
    )
    .bind(&placeholder)
    .bind(carbon.uuid().as_str())
    .execute(&pool)
    .await?;
    let joined: bool = sqlx::query_scalar("SELECT commit.in_circle($1, $2)")
        .bind(carbon.uuid().as_str())
        .bind(&placeholder)
        .fetch_one(&pool)
        .await?;
    assert!(!joined);
    Ok(())
}

#[tokio::test]
async fn silicons_take_work_only_from_their_circle_and_allow_list() -> anyhow::Result<()> {
    let Some(pool) = test_pool().await? else {
        return Ok(());
    };
    let world = World::new(&pool);
    let ada = world.carbon("allow-ada").await?;
    let delegate = world.silicon("allow-delegate", &ada).await?;
    let teammate = world.silicon("allow-teammate", &ada).await?;
    let bea = world.carbon("allow-bea").await?;
    let helper = world.silicon("allow-helper", &bea).await?;
    let outsider = world.carbon("allow-outsider").await?;
    let ada = world.refreshed(&ada).await?;
    let bea = world.refreshed(&bea).await?;
    let directory = Arc::new(Directory::with(&[
        &ada, &delegate, &teammate, &bea, &helper, &outsider,
    ]));
    let todo_service = todos(&pool, Arc::clone(&directory));
    let account_service = accounts(&pool, Arc::clone(&directory));

    // Inside the circle: allowed, and visible to the whole circle.
    let inside = assign(&todo_service, &delegate, &teammate, "Inside the circle").await?;
    assert_eq!(todo_service.get(&ada, inside).await?.id, inside);
    assert_not_found(todo_service.get(&bea, inside).await)?;
    assert_not_found(todo_service.get(&outsider, inside).await)?;

    // Carbons are open to everyone; the assignee's circle sees the work too.
    let to_carbon = assign(&todo_service, &delegate, &bea, "For a Carbon").await?;
    assert_eq!(todo_service.get(&helper, to_carbon).await?.id, to_carbon);
    assert_not_found(todo_service.get(&outsider, to_carbon).await)?;

    // A Silicon outside the circle refuses work until it (or its custodian) allows the sender.
    assert_denied(
        assign(&todo_service, &delegate, &helper, "Not yet").await,
        "silicon_not_reachable",
    )?;
    assert_denied(
        account_service
            .allow(&delegate, &helper.actor.id, &delegate.actor.id)
            .await,
        "not_custodian",
    )?;
    let listed = account_service
        .allow(&helper, &helper.actor.id, &delegate.actor.id)
        .await?;
    assert_eq!(listed.allowed.len(), 1);
    assert_eq!(listed.allowed[0].added_by.uuid, *helper.uuid());
    let allowed = assign(&todo_service, &delegate, &helper, "Now allowed").await?;
    assert_eq!(todo_service.get(&bea, allowed).await?.id, allowed);
    // Allowing one Silicon does not open the helper to the rest of its circle.
    assert_denied(
        assign(&todo_service, &teammate, &helper, "Still closed").await,
        "silicon_not_reachable",
    )?;

    let seen_by_custodian = account_service.allowlist(&bea, &helper.actor.id).await?;
    assert_eq!(seen_by_custodian.allowed.len(), 1);
    assert_eq!(seen_by_custodian.allowed[0].account.uuid, *delegate.uuid());
    assert!(matches!(
        account_service
            .allow(&helper, &helper.actor.id, &helper.actor.id)
            .await,
        Err(AppError::Validation { .. })
    ));

    // Removing the entry stops new work; work already assigned stays.
    let emptied = account_service
        .disallow(&bea, &helper.actor.id, &delegate.actor.id)
        .await?;
    assert!(emptied.allowed.is_empty());
    assert_denied(
        assign(&todo_service, &delegate, &helper, "Closed again").await,
        "silicon_not_reachable",
    )?;
    assert_eq!(todo_service.get(&helper, allowed).await?.id, allowed);

    // `me` describes the caller and the Silicons it is custodian of.
    let me = account_service.me(&ada, "commit").await?;
    assert_eq!(me.uuid, *ada.uuid());
    let mut silicons = me
        .silicons
        .iter()
        .map(|s| s.uuid.as_str().to_owned())
        .collect::<Vec<_>>();
    silicons.sort();
    let mut expected = vec![
        delegate.uuid().as_str().to_owned(),
        teammate.uuid().as_str().to_owned(),
    ];
    expected.sort();
    assert_eq!(silicons, expected);
    let silicon_me = account_service.me(&delegate, "commit").await?;
    assert_eq!(
        silicon_me.custodian.map(|custodian| custodian.uuid),
        Some(ada.uuid().clone())
    );
    Ok(())
}

async fn account_row(
    pool: &sqlx::PgPool,
    uuid: &str,
) -> anyhow::Result<(
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<OffsetDateTime>,
)> {
    Ok(sqlx::query_as(
        "SELECT public_id, display_name, status, email, custodian_uuid, revoked_before FROM commit.accounts WHERE uuid = $1",
    )
    .bind(uuid)
    .fetch_one(pool)
    .await?)
}

#[tokio::test]
async fn accounts_webhook_events_apply_once_and_never_go_backwards() -> anyhow::Result<()> {
    let Some(pool) = test_pool().await? else {
        return Ok(());
    };
    let world = World::new(&pool);
    let carbon = world.carbon("hook-carbon").await?;
    let silicon = world.silicon("hook-silicon", &carbon).await?;
    let new_custodian = world.carbon("hook-new-custodian").await?;
    let service = accounts(&pool, Arc::new(Directory::default()));
    let uuid = carbon.uuid().as_str().to_owned();
    let now = OffsetDateTime::now_utc();
    let event = |prefix: &str| format!("evt_{prefix}_{}", Uuid::new_v4().simple());

    // account.id_changed: applied once, then deduplicated by event id.
    let renamed = AccountEvent::IdChanged {
        uuid: uuid.clone(),
        new_id: "c:hook-renamed".to_owned(),
    };
    let rename_id = event("rename");
    let first = service
        .apply_webhook(
            &rename_id,
            "account.id_changed",
            Some(now),
            &renamed,
            "hash-1",
        )
        .await?;
    assert!(first.applied, "{}", first.outcome);
    let duplicate = service
        .apply_webhook(
            &rename_id,
            "account.id_changed",
            Some(now),
            &renamed,
            "hash-1",
        )
        .await?;
    assert!(!duplicate.applied);
    let reused = service
        .apply_webhook(
            &rename_id,
            "account.id_changed",
            Some(now),
            &renamed,
            "another-body",
        )
        .await?;
    assert!(!reused.applied);
    assert_eq!(account_row(&pool, &uuid).await?.0, "c:hook-renamed");

    // An older rename delivered late never overwrites the newer id.
    let stale = service
        .apply_webhook(
            &event("stale-rename"),
            "account.id_changed",
            Some(now - TimeDuration::minutes(5)),
            &AccountEvent::IdChanged {
                uuid: uuid.clone(),
                new_id: "c:hook-stale".to_owned(),
            },
            "hash-2",
        )
        .await?;
    assert!(
        stale.applied && stale.outcome.starts_with("ignored"),
        "{}",
        stale.outcome
    );
    assert_eq!(account_row(&pool, &uuid).await?.0, "c:hook-renamed");

    // account.updated: versions only move forward; the shared email is kept.
    let profile = |version: i64, name: &str| AccountEvent::Updated {
        uuid: uuid.clone(),
        account: serde_json::json!({
            "uuid": uuid, "kind": "carbon", "id": "c:hook-renamed", "display_name": name,
            "pfp_url": "https://accounts.example/pfp.png", "email": "hook@example.test"
        }),
        version,
    };
    assert!(
        service
            .apply_webhook(
                &event("profile-2"),
                "account.updated",
                Some(now),
                &profile(2, "Second"),
                "hash-3"
            )
            .await?
            .applied
    );
    let older = service
        .apply_webhook(
            &event("profile-1"),
            "account.updated",
            Some(now),
            &profile(1, "First"),
            "hash-4",
        )
        .await?;
    assert!(older.outcome.starts_with("ignored"), "{}", older.outcome);
    let row = account_row(&pool, &uuid).await?;
    assert_eq!(
        (row.1.as_str(), row.3.as_deref()),
        ("Second", Some("hook@example.test"))
    );

    // silicon.custodian_changed moves the Silicon to another circle.
    let moved = service
        .apply_webhook(
            &event("custodian"),
            "silicon.custodian_changed",
            Some(OffsetDateTime::now_utc()),
            &AccountEvent::CustodianChanged {
                silicon: silicon.uuid().as_str().to_owned(),
                to: Some((
                    new_custodian.uuid().as_str().to_owned(),
                    new_custodian.actor.id.as_str().to_owned(),
                )),
            },
            "hash-5",
        )
        .await?;
    assert!(moved.applied, "{}", moved.outcome);
    let circle: (bool, bool) =
        sqlx::query_as("SELECT commit.in_circle($1, $2), commit.in_circle($1, $3)")
            .bind(silicon.uuid().as_str())
            .bind(carbon.uuid().as_str())
            .bind(new_custodian.uuid().as_str())
            .fetch_one(&pool)
            .await?;
    assert_eq!(circle, (false, true));

    // membership.signed_out: Commit's own revocation (one sign-in) keeps the others valid;
    // any other reason refuses every earlier token.
    assert!(
        service
            .apply_webhook(
                &event("signed-out-app"),
                "membership.signed_out",
                Some(now),
                &AccountEvent::SignedOut {
                    uuid: uuid.clone(),
                    reason: Some("app_revoked".to_owned())
                },
                "hash-6",
            )
            .await?
            .applied
    );
    assert_eq!(account_row(&pool, &uuid).await?.5, None);
    service
        .apply_webhook(
            &event("signed-out-rotated"),
            "membership.signed_out",
            Some(now),
            &AccountEvent::SignedOut {
                uuid: uuid.clone(),
                reason: Some("stk_rotated".to_owned()),
            },
            "hash-7",
        )
        .await?;
    let revoked = account_row(&pool, &uuid)
        .await?
        .5
        .ok_or_else(|| anyhow::anyhow!("revoked_before"))?;
    assert!((revoked - now).abs() < TimeDuration::seconds(1));
    let later = now + TimeDuration::seconds(30);
    service
        .apply_webhook(
            &event("access-removed"),
            "membership.access_removed",
            Some(later),
            &AccountEvent::AccessRemoved { uuid: uuid.clone() },
            "hash-8",
        )
        .await?;
    let revoked = account_row(&pool, &uuid)
        .await?
        .5
        .ok_or_else(|| anyhow::anyhow!("revoked_before"))?;
    assert!((revoked - later).abs() < TimeDuration::seconds(1));

    for (kind, event_value) in [
        ("ping", AccountEvent::Ping),
        ("account.created", AccountEvent::Unknown),
    ] {
        let outcome = service
            .apply_webhook(&event("misc"), kind, None, &event_value, "hash-9")
            .await?;
        assert!(outcome.applied);
    }
    let recorded: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM commit.accounts_webhook_events WHERE account_uuid = $1",
    )
    .bind(&uuid)
    .fetch_one(&pool)
    .await?;
    assert_eq!(recorded, 7);
    Ok(())
}

#[tokio::test]
async fn deleting_an_account_forgets_its_data_and_hands_projects_on() -> anyhow::Result<()> {
    let Some(pool) = test_pool().await? else {
        return Ok(());
    };
    let world = World::new(&pool);
    let leaving = world.carbon("leaving").await?;
    let staying = world.carbon("staying").await?;
    let leaving_silicon = world.silicon("leaving-silicon", &leaving).await?;
    let leaving = world.refreshed(&leaving).await?;
    let directory = Arc::new(Directory::with(&[&leaving, &staying, &leaving_silicon]));
    let todo_service = todos(&pool, Arc::clone(&directory));
    let project_service = projects(&pool, Arc::clone(&directory));
    let account_service = accounts(&pool, Arc::clone(&directory));

    let personal = assign(&todo_service, &leaving, &leaving, "Personal").await?;
    let delegated = assign(&todo_service, &leaving, &staying, "Delegated").await?;
    let received = assign(&todo_service, &staying, &leaving, "Received").await?;
    let create = |name: &str,
                  members: Vec<String>|
     -> anyhow::Result<silicon_commit::domain::ProjectCreate> {
        Ok(serde_json::from_value(
            serde_json::json!({"name": name, "carbon_ids": members}),
        )?)
    };
    let shared = project_service
        .create_project(
            &leaving,
            create("Shared", vec![staying.actor.id.as_str().to_owned()])?,
            unique_key("forget-shared")?,
            "forget-shared",
        )
        .await?;
    let shared = ProjectId::from_uuid(response_uuid(&shared, "id")?);
    let alone = project_service
        .create_project(
            &leaving,
            create("Alone", Vec::new())?,
            unique_key("forget-alone")?,
            "forget-alone",
        )
        .await?;
    let alone = ProjectId::from_uuid(response_uuid(&alone, "id")?);
    let theirs = project_service
        .create_project(
            &staying,
            create("Theirs", vec![leaving.actor.id.as_str().to_owned()])?,
            unique_key("forget-theirs")?,
            "forget-theirs",
        )
        .await?;
    let theirs = ProjectId::from_uuid(response_uuid(&theirs, "id")?);
    sqlx::query(
        "INSERT INTO commit.email_preferences(account, email) VALUES ($1, 'leaving@example.test')",
    )
    .bind(leaving.uuid().as_str())
    .execute(&pool)
    .await?;
    account_service
        .allow(&leaving, &leaving_silicon.actor.id, &staying.actor.id)
        .await?;

    let deleted = account_service
        .apply_webhook(
            &format!("evt_deleted_{}", Uuid::new_v4().simple()),
            "account.deleted",
            Some(OffsetDateTime::now_utc()),
            &AccountEvent::Deleted {
                uuid: leaving.uuid().as_str().to_owned(),
            },
            "hash-deleted",
        )
        .await?;
    assert!(deleted.applied, "{}", deleted.outcome);
    let summary: serde_json::Value = serde_json::from_str(
        deleted
            .outcome
            .strip_prefix("account deleted: ")
            .ok_or_else(|| anyhow::anyhow!("unexpected outcome {}", deleted.outcome))?,
    )?;
    assert_eq!(summary["todos_deleted"], 1);
    assert_eq!(summary["projects_transferred"], 1);
    assert_eq!(summary["projects_deleted"], 1);
    assert_eq!(summary["memberships_ended"], 2);
    assert_eq!(summary["email_preferences_removed"], 1);
    assert_eq!(summary["allowlist_entries_removed"], 0);

    let row = account_row(&pool, leaving.uuid().as_str()).await?;
    assert_eq!(
        (row.0.as_str(), row.2.as_str(), row.3),
        ("", "deleted", None)
    );
    assert!(row.5.is_some());

    // Personal work is gone; work shared with others stays, credited to a deleted account.
    assert_not_found(todo_service.get(&staying, personal).await)?;
    let kept = todo_service.get(&staying, delegated).await?;
    assert_eq!(kept.assigned_by.uuid, *leaving.uuid());
    assert_eq!(kept.assigned_by.id.as_str(), "");
    assert_eq!(todo_service.get(&staying, received).await?.id, received);

    let handed_on = project_service
        .get_project(&staying, &ProjectLocator::Id(shared))
        .await?;
    assert_eq!(handed_on.owner.uuid, *staying.uuid());
    assert!(!handed_on.has_member(leaving.uuid()));
    let still_theirs = project_service
        .get_project(&staying, &ProjectLocator::Id(theirs))
        .await?;
    assert!(!still_theirs.has_member(leaving.uuid()));
    let alone_deleted: bool =
        sqlx::query_scalar("SELECT deleted_at IS NOT NULL FROM commit.projects WHERE id = $1")
            .bind(alone.into_uuid())
            .fetch_one(&pool)
            .await?;
    assert!(alone_deleted);

    Ok(())
}

#[tokio::test]
async fn a_lookup_answered_from_the_cache_never_undoes_a_newer_event() -> anyhow::Result<()> {
    let Some(pool) = test_pool().await? else {
        return Ok(());
    };
    let world = World::new(&pool);
    let ada = world.carbon("cache-ada").await?;
    let bea = world.carbon("cache-bea").await?;
    let scout = world.silicon("cache-scout", &ada).await?;
    let silicon = scout.uuid().as_str().to_owned();
    let now = OffsetDateTime::now_utc();
    // Commit learned about the Silicon two minutes ago; a lookup of it was cached a minute ago.
    sqlx::query("UPDATE commit.accounts SET refreshed_at = $2 WHERE uuid = $1")
        .bind(&silicon)
        .bind(now - TimeDuration::minutes(2))
        .execute(&pool)
        .await?;
    let directory = Arc::new(Directory::with(&[&ada, &bea]));
    directory.add_observed(&scout, now - TimeDuration::minutes(1));
    let todo_service = todos(&pool, Arc::clone(&directory));
    let service = accounts(&pool, Arc::clone(&directory));

    // Ada assigns the Silicon work: Commit stores the cached lookup, true a minute ago.
    assign(&todo_service, &ada, &scout, "before the transfer").await?;

    // Forty seconds ago Ada handed the Silicon to Bea, and thirty seconds ago its id
    // changed; the events arrive only now.
    let moved = service
        .apply_webhook(
            &format!("evt_transfer_{}", Uuid::new_v4().simple()),
            "silicon.custodian_changed",
            Some(now - TimeDuration::seconds(40)),
            &AccountEvent::CustodianChanged {
                silicon: silicon.clone(),
                to: Some((
                    bea.uuid().as_str().to_owned(),
                    bea.actor.id.as_str().to_owned(),
                )),
            },
            "hash-transfer",
        )
        .await?;
    assert!(
        moved.outcome.starts_with("custodian is now"),
        "{}",
        moved.outcome
    );
    let renamed = service
        .apply_webhook(
            &format!("evt_rename_{}", Uuid::new_v4().simple()),
            "account.id_changed",
            Some(now - TimeDuration::seconds(30)),
            &AccountEvent::IdChanged {
                uuid: silicon.clone(),
                new_id: "si:cache-scout-renamed".to_owned(),
            },
            "hash-rename",
        )
        .await?;
    assert!(renamed.outcome == "id changed", "{}", renamed.outcome);

    // Bea, its custodian now, assigns it work; the same cached lookup (Ada, the old id)
    // answers again and must not undo either event.
    let bea = world.refreshed(&bea).await?;
    assign(&todo_service, &bea, &scout, "after the transfer").await?;
    let row = account_row(&pool, &silicon).await?;
    assert_eq!(
        (row.0.as_str(), row.4.as_deref()),
        ("si:cache-scout-renamed", Some(bea.uuid().as_str()))
    );
    Ok(())
}

#[tokio::test]
async fn account_rows_never_hold_infinite_times() -> anyhow::Result<()> {
    let Some(pool) = test_pool().await? else {
        return Ok(());
    };
    // An infinite timestamp cannot be decoded into a time, so every later request of the
    // account would fail: the table refuses one outright.
    for column in ["refreshed_at", "revoked_before"] {
        let uuid = common::new_uuid();
        let refused = sqlx::query(sqlx::AssertSqlSafe(format!(
            "INSERT INTO commit.accounts (uuid, kind, public_id, {column}) VALUES ($1, 'carbon', $2, '-infinity')"
        )))
        .bind(&uuid)
        .bind(format!("c:infinite-{}", uuid.to_ascii_lowercase()))
        .execute(&pool)
        .await;
        common::assert_database_code(refused, "23514")?;
    }
    Ok(())
}
