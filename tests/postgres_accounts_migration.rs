//! Migration 0033 (Silicon Accounts) on an empty database and on IAM-era data, and
//! `commit-migrate link-identities`, which re-points IAM-era rows to accounts.

mod common;

use std::collections::BTreeMap;

use anyhow::{Context as _, ensure};
use sqlx::PgPool;

use common::with_isolated_database;
use silicon_commit::{
    domain::ActorType,
    infrastructure::postgres::identity_links::{self, KnownAccount, LinkReport},
};

const TOS: &str = "11111111-1111-4111-8111-111111111111";
const SANDBOX: &str = "22222222-2222-4222-8222-222222222222";
const SAKET: &str = "a0000000-0000-4000-8000-000000000001";
const CHEF: &str = "a0000000-0000-4000-8000-000000000003";
const SCOUT: &str = "a0000000-0000-4000-8000-000000000004";
const SANDBOX_SAKET: &str = "b0000000-0000-4000-8000-000000000001";
const SHIP: &str = "c0000000-0000-4000-8000-000000000001";
const SCOUTING: &str = "c0000000-0000-4000-8000-000000000003";
const SANDBOX_TODO: &str = "c0000000-0000-4000-8000-000000000004";
const COPY_TODO: &str = "c0000000-0000-4000-8000-000000000005";
const LAUNCH: &str = "d0000000-0000-4000-8000-000000000001";
const SECRET: &str = "d0000000-0000-4000-8000-000000000002";

/// Every table holding IAM-era data that 0033 must keep byte for byte.
const RETAINED: &[&str] = &[
    "actor_projection",
    "organization_projection",
    "testing_environments",
    "testing_organizations",
    "todos",
    "todo_notes",
    "todo_activity",
    "todo_attachments",
    "idempotency_records",
    "audit_events",
    "outbox_events",
    "projects",
    "project_participants",
    "project_diaries",
    "project_tasks",
    "project_entries",
    "project_collaborators",
    "project_versions",
    "silicon_notification_settings",
    "todo_notification_subscriptions",
    "email_preferences",
    "email_jobs",
];

fn placeholder(organization: &str, principal: &str) -> String {
    format!("iam:{organization}:{principal}")
}

async fn migrate_before_accounts(pool: &PgPool) -> anyhow::Result<()> {
    let mut previous = sqlx::migrate!("./migrations");
    previous
        .migrations
        .to_mut()
        .retain(|migration| migration.version < 33);
    previous.run(pool).await?;
    Ok(())
}

async fn columns(pool: &PgPool, table: &str) -> anyhow::Result<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT column_name::text FROM information_schema.columns WHERE table_schema = 'commit' AND table_name = $1 ORDER BY 1",
    )
    .bind(table)
    .fetch_all(pool)
    .await?)
}

/// Each row of `table` as JSON text restricted to `keep`, sorted.
async fn rows(pool: &PgPool, table: &str, keep: &[String]) -> anyhow::Result<Vec<String>> {
    // `table` comes from the fixed RETAINED list.
    Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        r"
        SELECT coalesce(array_agg(row_text ORDER BY row_text), '{{}}')
          FROM (SELECT (SELECT coalesce(jsonb_object_agg(field.key, field.value), '{{}}')
                          FROM jsonb_each(to_jsonb(t)) AS field
                         WHERE field.key = ANY($1))::text AS row_text
                  FROM commit.{table} AS t) AS projected
        "
    )))
    .bind(keep)
    .fetch_one(pool)
    .await?)
}

async fn scalar_i64(pool: &PgPool, sql: &'static str) -> anyhow::Result<i64> {
    Ok(sqlx::query_scalar(sql).fetch_one(pool).await?)
}

async fn account_of(pool: &PgPool, sql: &'static str, id: &str) -> anyhow::Result<String> {
    Ok(sqlx::query_scalar(sql)
        .bind(uuid::Uuid::parse_str(id)?)
        .fetch_one(pool)
        .await?)
}

#[tokio::test]
async fn accounts_migration_applies_to_an_empty_database() -> anyhow::Result<()> {
    with_isolated_database("commit_acct_empty", |options| async move {
        let pool = PgPool::connect_with(options).await?;
        let outcome = async {
            let migrator = sqlx::migrate!("./migrations");
            migrator.run(&pool).await?;
            migrator.run(&pool).await?;
            ensure!(scalar_i64(&pool, "SELECT count(*) FROM commit.accounts").await? == 0);
            ensure!(
                scalar_i64(&pool, "SELECT count(*) FROM commit_private.identity_links").await? == 0
            );
            let contracts: Vec<(i32, String)> = sqlx::query_as(
                "SELECT version, status FROM commit.contract_versions ORDER BY version",
            )
            .fetch_all(&pool)
            .await?;
            ensure!(
                contracts == vec![(1, "deprecated".to_owned()), (2, "active".to_owned())],
                "contract 2 replaces contract 1: {contracts:?}"
            );
            // Nothing to link: an empty mapping is a no-op with an empty report.
            let rows = identity_links::parse_mapping("iam_principal_id,accounts_uuid\n")?;
            let report =
                identity_links::apply(&pool, &rows, "empty", &BTreeMap::new(), false).await?;
            ensure!(report.links_changed == 0 && report.rows_repointed.is_empty());
            ensure!(report.unlinked_principals.is_empty());
            anyhow::Ok(())
        }
        .await;
        pool.close().await;
        outcome
    })
    .await
}

fn known(kind: ActorType, id: &str, custodian: Option<&str>) -> KnownAccount {
    KnownAccount {
        kind,
        public_id: id.to_owned(),
        custodian: custodian.map(str::to_owned),
    }
}

fn directory() -> BTreeMap<String, KnownAccount> {
    BTreeMap::from([
        (
            "SaKet1".to_owned(),
            known(ActorType::Carbon, "c:saket", None),
        ),
        (
            "ShUbh3".to_owned(),
            known(ActorType::Carbon, "c:shubham", None),
        ),
        (
            "ChEf9".to_owned(),
            known(ActorType::Silicon, "si:chef", Some("SaKet1")),
        ),
        (
            "ScOut7".to_owned(),
            known(ActorType::Silicon, "si:scout", Some("ShUbh3")),
        ),
        (
            "ScOut8".to_owned(),
            known(ActorType::Silicon, "si:scout", Some("ShUbh3")),
        ),
    ])
}

async fn link(pool: &PgPool, mapping: &str, dry_run: bool) -> anyhow::Result<LinkReport> {
    let rows = identity_links::parse_mapping(mapping)?;
    identity_links::apply(
        pool,
        &rows,
        &identity_links::mapping_digest(mapping),
        &directory(),
        dry_run,
    )
    .await
}

const MAPPING: &str = "iam_principal_id,accounts_uuid\nc:saket,SaKet1\nc:shubham,ShUbh3\nsi:chef,ChEf9\nsi:scout,ScOut7\n";

#[tokio::test]
async fn accounts_migration_keeps_iam_era_data_and_links_it_to_accounts() -> anyhow::Result<()> {
    with_isolated_database("commit_acct_upgrade", |options| async move {
        let pool = PgPool::connect_with(options).await?;
        let outcome = upgrade_and_link(&pool).await;
        pool.close().await;
        outcome
    })
    .await
}

async fn upgrade_and_link(pool: &PgPool) -> anyhow::Result<()> {
    migrate_before_accounts(pool).await?;
    sqlx::raw_sql(include_str!("postgres_accounts_migration_fixture.sql"))
        .execute(pool)
        .await
        .context("load the IAM-era fixture")?;

    // A project UID reused across former organizations must be fixed before the cutover.
    let mut transaction = pool.begin().await?;
    // Only the fixed SANDBOX constant is interpolated.
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        r"
        INSERT INTO commit.actor_projection (organization_id, principal_id, membership_id, actor_type, actor_id)
        VALUES ('{SANDBOX}', 'b0000000-0000-4000-8000-0000000000ff', 'si:chef-two[tos]', 'silicon', 'si:chef-two');
        INSERT INTO commit.projects (id, organization_id, name, slug, uid, created_by_principal_id)
        VALUES ('d0000000-0000-4000-8000-0000000000ff', '{SANDBOX}', 'Launch', 'launch', 'launch:si:chef:1788000000000',
                'b0000000-0000-4000-8000-0000000000ff');
        INSERT INTO commit.project_participants (id, organization_id, project_id, silicon_principal_id, added_by_principal_id)
        VALUES (gen_random_uuid(), '{SANDBOX}', 'd0000000-0000-4000-8000-0000000000ff',
                'b0000000-0000-4000-8000-0000000000ff', 'b0000000-0000-4000-8000-0000000000ff');
        "
    )))
    .execute(&mut *transaction)
    .await?;
    let refused = sqlx::raw_sql(include_str!("../migrations/0033_silicon_accounts.sql"))
        .execute(&mut *transaction)
        .await
        .err()
        .context("0033 must refuse duplicate project UIDs")?;
    ensure!(
        refused.to_string().contains("project UIDs must be unique"),
        "unexpected refusal: {refused}"
    );
    transaction.rollback().await?;

    let mut kept_columns = BTreeMap::new();
    let mut before = BTreeMap::new();
    for table in RETAINED {
        let names = columns(pool, table).await?;
        before.insert(*table, rows(pool, table, &names).await?);
        kept_columns.insert(*table, names);
    }
    ensure!(before["todos"].len() == 5 && before["project_versions"].len() == 2);

    let migrator = sqlx::migrate!("./migrations");
    migrator.run(pool).await?;
    migrator.run(pool).await?;

    // Every IAM-era value is still there (new columns aside).
    for table in RETAINED {
        let after = rows(pool, table, &kept_columns[table]).await?;
        ensure!(
            after == before[table],
            "0033 changed IAM-era data in commit.{table}"
        );
    }

    // One unlinked placeholder per IAM principal, carrying its IAM-era id.
    let placeholders: Vec<(String, String, String, String)> = sqlx::query_as(
        "SELECT uuid, kind::text, public_id, status FROM commit.accounts ORDER BY uuid",
    )
    .fetch_all(pool)
    .await?;
    ensure!(placeholders.len() == 6, "{placeholders:?}");
    ensure!(
        placeholders
            .iter()
            .all(|(_, _, _, status)| status == "unlinked")
    );
    ensure!(placeholders.contains(&(
        placeholder(TOS, CHEF),
        "silicon".to_owned(),
        "si:chef".to_owned(),
        "unlinked".to_owned()
    )));
    ensure!(
        account_of(
            pool,
            "SELECT assigned_by_account FROM commit.todos WHERE id = $1",
            SHIP
        )
        .await?
            == placeholder(TOS, SAKET)
    );
    ensure!(
        account_of(
            pool,
            "SELECT owner_account FROM commit.projects WHERE id = $1",
            LAUNCH
        )
        .await?
            == placeholder(TOS, CHEF)
    );
    let stray = scalar_i64(
        pool,
        "SELECT count(*) FROM commit.todos WHERE assigned_to_account <> 'iam:' || organization_id || ':' || assigned_to_principal_id",
    )
    .await?;
    ensure!(stray == 0);
    // Placeholders are nobody's circle: until linked, IAM-era work is visible to no account.
    let open: bool = sqlx::query_scalar("SELECT commit.in_circle($1, $2)")
        .bind(placeholder(TOS, CHEF))
        .bind(placeholder(TOS, SCOUT))
        .fetch_one(pool)
        .await?;
    ensure!(!open);

    // Dry run: a report, and nothing changes.
    let dry = link(pool, MAPPING, true).await?;
    ensure!(dry.dry_run && dry.links_changed == 4, "{dry:?}");
    ensure!(
        dry.rows_repointed.get("todos.assigned_by_account") == Some(&4),
        "{dry:?}"
    );
    ensure!(
        scalar_i64(
            pool,
            "SELECT count(*) FROM commit.accounts WHERE status <> 'unlinked'"
        )
        .await?
            == 0
    );
    ensure!(
        scalar_i64(
            pool,
            "SELECT count(*) FROM commit_private.identity_links WHERE accounts_uuid IS NOT NULL"
        )
        .await?
            == 0
    );

    let applied = link(pool, MAPPING, false).await?;
    ensure!(applied.links_changed == 4, "{applied:?}");
    ensure!(applied.accounts_created.len() == 4);
    ensure!(
        account_of(
            pool,
            "SELECT assigned_by_account FROM commit.todos WHERE id = $1",
            SHIP
        )
        .await?
            == "SaKet1"
    );
    ensure!(
        account_of(
            pool,
            "SELECT assigned_to_account FROM commit.todos WHERE id = $1",
            SHIP
        )
        .await?
            == "ChEf9"
    );
    ensure!(
        account_of(
            pool,
            "SELECT owner_account FROM commit.projects WHERE id = $1",
            LAUNCH
        )
        .await?
            == "ChEf9"
    );
    ensure!(
        account_of(
            pool,
            "SELECT silicon_account FROM commit.todo_notification_subscriptions WHERE todo_id = $1",
            COPY_TODO
        )
        .await?
            == "ChEf9"
    );
    // An id names production principals only: the sandbox keeps its placeholders.
    ensure!(
        account_of(
            pool,
            "SELECT assigned_by_account FROM commit.todos WHERE id = $1",
            SANDBOX_TODO
        )
        .await?
            == placeholder(SANDBOX, SANDBOX_SAKET)
    );
    ensure!(applied.unlinked_principals.len() == 2, "{applied:?}");
    // Visibility now follows the circle: c:saket is si:chef's custodian.
    for (project, account, expected) in [
        (LAUNCH, "SaKet1", true),
        (LAUNCH, "ScOut7", true),
        (LAUNCH, "ShUbh3", true),
        (SECRET, "ScOut7", true),
        (SECRET, "SaKet1", false),
    ] {
        let access: bool = sqlx::query_scalar("SELECT commit.project_access($1, $2)")
            .bind(uuid::Uuid::parse_str(project)?)
            .bind(account)
            .fetch_one(pool)
            .await?;
        ensure!(
            access == expected,
            "project_access({project}, {account}) = {access}"
        );
    }
    ensure!(
        applied.public_projects_needing_shares.is_empty(),
        "{applied:?}"
    );

    // Idempotent: the same file changes nothing.
    let again = link(pool, MAPPING, false).await?;
    ensure!(
        again.links_changed == 0 && again.rows_repointed.is_empty(),
        "{again:?}"
    );

    // The sandbox Carbon, named by its UUID, joins the same account; its older email
    // preference stays on the placeholder because the account already has one.
    let sandbox = link(
        pool,
        &format!("iam_principal_id,accounts_uuid\n{SANDBOX_SAKET},SaKet1\n"),
        false,
    )
    .await?;
    ensure!(
        sandbox.kept_on_placeholder.get("email_preferences.account") == Some(&1),
        "{sandbox:?}"
    );
    let email: String =
        sqlx::query_scalar("SELECT email FROM commit.email_preferences WHERE account = 'SaKet1'")
            .fetch_one(pool)
            .await?;
    ensure!(email == "saket@example.test");

    // Work changed after the cutover is never clobbered by a later re-link.
    sqlx::query("UPDATE commit.todos SET assigned_to_account = 'ChEf9' WHERE id = $1")
        .bind(uuid::Uuid::parse_str(SCOUTING)?)
        .execute(pool)
        .await?;
    let relinked = link(
        pool,
        "iam_principal_id,accounts_uuid\nsi:scout,ScOut8\n",
        false,
    )
    .await?;
    ensure!(relinked.links_changed == 1, "{relinked:?}");
    ensure!(
        account_of(
            pool,
            "SELECT assigned_to_account FROM commit.todos WHERE id = $1",
            SCOUTING
        )
        .await?
            == "ChEf9"
    );
    ensure!(
        account_of(
            pool,
            "SELECT owner_account FROM commit.projects WHERE id = $1",
            SECRET
        )
        .await?
            == "ScOut8"
    );

    // Refusals change nothing: kinds must match, and one principal maps to one account.
    let wrong_kind = link(
        pool,
        "iam_principal_id,accounts_uuid\nc:shubham,ChEf9\n",
        false,
    )
    .await;
    ensure!(
        wrong_kind.is_err_and(|error| error.to_string().contains("a Carbon links to a Carbon")),
        "a Carbon must not link to a Silicon"
    );
    let conflicting = link(
        pool,
        "iam_principal_id,accounts_uuid\nc:shubham,ShUbh3\nc:shubham,SaKet1\n",
        false,
    )
    .await;
    ensure!(conflicting.is_err_and(|error| error.to_string().contains("to different accounts")));
    ensure!(
        account_of(
            pool,
            "SELECT assigned_by_account FROM commit.todos WHERE id = $1",
            SCOUTING
        )
        .await?
            == "ShUbh3"
    );

    // Unknown lines are reported; an empty uuid unlinks (rows go back to the placeholder).
    let unlinked = link(
        pool,
        "iam_principal_id,accounts_uuid\nsi:nobody,NoBody1\nsi:scout,\n",
        false,
    )
    .await?;
    ensure!(unlinked.unmatched_mapping_lines.len() == 1, "{unlinked:?}");
    ensure!(
        account_of(
            pool,
            "SELECT owner_account FROM commit.projects WHERE id = $1",
            SECRET
        )
        .await?
            == placeholder(TOS, SCOUT)
    );
    ensure!(
        account_of(
            pool,
            "SELECT assigned_to_account FROM commit.todos WHERE id = $1",
            SCOUTING
        )
        .await?
            == "ChEf9"
    );
    // With si:scout unlinked, c:shubham no longer looks after a Launch member: the report
    // names it so the owner can share the formerly organization-wide project explicitly.
    ensure!(
        unlinked.public_projects_needing_shares.len() == 1,
        "{unlinked:?}"
    );
    ensure!(
        unlinked.public_projects_needing_shares[0]["former_readers_without_access"]
            == serde_json::json!(["c:shubham"]),
        "{unlinked:?}"
    );
    let plan = identity_links::plan(pool).await?;
    ensure!(
        plan.len() == 4,
        "the plan lists production principals only: {plan:?}"
    );
    ensure!(
        plan.iter()
            .any(|(_, id, _, _, linked)| id == "si:chef" && linked.as_deref() == Some("ChEf9"))
    );
    ensure!(
        plan.iter()
            .any(|(_, id, _, _, linked)| id == "si:scout" && linked.is_none())
    );
    Ok(())
}

/// The operator command end to end: migrate, print a plan, dry-run and apply a mapping file.
#[tokio::test]
async fn commit_migrate_link_identities_runs_from_the_command_line() -> anyhow::Result<()> {
    let Ok(base_url) = std::env::var("COMMIT_TEST_DATABASE_URL") else {
        eprintln!("skipping PostgreSQL migration test: COMMIT_TEST_DATABASE_URL is not set");
        return Ok(());
    };
    with_isolated_database("commit_acct_cli", |options| async move {
        let database = options
            .get_database()
            .context("isolated database name")?
            .to_owned();
        let pool = PgPool::connect_with(options).await?;
        migrate_before_accounts(&pool).await?;
        sqlx::raw_sql(include_str!("postgres_accounts_migration_fixture.sql"))
            .execute(&pool)
            .await?;
        let owner: String = sqlx::query_scalar("SELECT current_user::text").fetch_one(&pool).await?;
        let mut url = url::Url::parse(&base_url)?;
        url.set_path(&format!("/{database}"));
        let scratch = std::env::temp_dir().join(format!("commit-link-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&scratch)?;
        let mapping = scratch.join("mapping.csv");
        let run = |arguments: &[&str]| -> anyhow::Result<std::process::Output> {
            Ok(std::process::Command::new(env!("CARGO_BIN_EXE_commit-migrate"))
                .args(arguments)
                .env_clear()
                .env("PATH", std::env::var("PATH").unwrap_or_default())
                .env("COMMIT_ENVIRONMENT", "test")
                .env("COMMIT_MIGRATOR_DATABASE_URL", url.as_str())
                .env("COMMIT_SCHEMA_OWNER", &owner)
                .env("COMMIT_LOG", "warn")
                .current_dir(&scratch)
                .output()?)
        };
        let outcome = async {
            let migrated = run(&[])?;
            ensure!(migrated.status.success(), "{}", String::from_utf8_lossy(&migrated.stderr));

            let plan = run(&["link-identities", "--plan", "--offline"])?;
            ensure!(plan.status.success(), "{}", String::from_utf8_lossy(&plan.stderr));
            let plan = String::from_utf8(plan.stdout)?;
            ensure!(plan.starts_with("iam_principal_id,accounts_uuid,org_id\n"), "{plan}");
            ensure!(plan.lines().count() == 5, "production principals only: {plan}");
            // Fill the template the way an operator would, keeping its `# id` comments.
            let filled = plan
                .lines()
                .map(|line| match line.split_once("    # ") {
                    Some((data, id)) => {
                        let fields = data.split(',').collect::<Vec<_>>();
                        let uuid = match id {
                            "c:saket" => "SaKet1",
                            "c:shubham" => "ShUbh3",
                            "si:chef" => "ChEf9",
                            _ => "",
                        };
                        format!("{},{uuid},{}    # {id}", fields[0], fields[2])
                    }
                    None => line.to_owned(),
                })
                .collect::<Vec<_>>()
                .join("\n");
            std::fs::write(&mapping, filled)?;
            let mapping_path = mapping.to_string_lossy().into_owned();

            let dry = run(&["link-identities", "--file", &mapping_path, "--dry-run", "--offline"])?;
            ensure!(dry.status.success(), "{}", String::from_utf8_lossy(&dry.stderr));
            let report: serde_json::Value = serde_json::from_slice(&dry.stdout)?;
            ensure!(report["dry_run"] == true && report["links_changed"] == 3, "{report}");
            ensure!(scalar_i64(&pool, "SELECT count(*) FROM commit.accounts WHERE status <> 'unlinked'").await? == 0);

            let applied = run(&["link-identities", "--file", &mapping_path, "--offline"])?;
            ensure!(applied.status.success(), "{}", String::from_utf8_lossy(&applied.stderr));
            let report: serde_json::Value = serde_json::from_slice(&applied.stdout)?;
            ensure!(report["dry_run"] == false && report["links_changed"] == 3, "{report}");
            ensure!(
                account_of(&pool, "SELECT assigned_by_account FROM commit.todos WHERE id = $1", SHIP).await? == "SaKet1"
            );
            // Offline, a new account starts from its IAM-era id and is refreshed on first sight.
            let (id, refreshed): (String, bool) = sqlx::query_as(
                "SELECT public_id, refreshed_at = 'epoch' FROM commit.accounts WHERE uuid = 'ChEf9'",
            )
            .fetch_one(&pool)
            .await?;
            ensure!(id == "si:chef" && refreshed);

            let unknown = run(&["link-identities", "--bogus"])?;
            ensure!(!unknown.status.success());
            ensure!(String::from_utf8_lossy(&unknown.stderr).contains("unknown option `--bogus`"));
            anyhow::Ok(())
        }
        .await;
        pool.close().await;
        let _ = std::fs::remove_dir_all(&scratch);
        outcome
    })
    .await
}
