//! Upgrade coverage for retained IAM membership identities across all data planes.

use std::{env, str::FromStr};

use anyhow::{Context as _, ensure};
use serde_json::Value;
use sqlx::{PgPool, postgres::PgConnectOptions};
use uuid::Uuid;

#[tokio::test]
async fn public_membership_migration_preserves_production_and_testing_history() -> anyhow::Result<()>
{
    let Ok(database_url) = env::var("COMMIT_TEST_DATABASE_URL") else {
        eprintln!("skipping PostgreSQL migration test: COMMIT_TEST_DATABASE_URL is not set");
        return Ok(());
    };
    // A separate database exercises the actual pre-upgrade schema while other
    // integration tests continue using the current schema on their own pool.
    let options = PgConnectOptions::from_str(&database_url)?;
    let admin = PgPool::connect_with(options.clone()).await?;
    let database = format!("commit_membership_{}", Uuid::new_v4().simple());
    // The identifier contains only this fixed prefix and generated hex digits.
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {database}")))
        .execute(&admin)
        .await
        .context("create isolated migration database; test role needs CREATEDB")?;
    let outcome = verify_migration(options.database(&database)).await;
    let cleanup = sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE {database} WITH (FORCE)"
    )))
    .execute(&admin)
    .await;
    admin.close().await;
    outcome?;
    cleanup.context("remove isolated migration database")?;
    Ok(())
}

async fn verify_migration(options: PgConnectOptions) -> anyhow::Result<()> {
    let pool = PgPool::connect_with(options).await?;
    let outcome = verify_on_pool(&pool).await;
    pool.close().await;
    outcome
}

async fn verify_on_pool(pool: &PgPool) -> anyhow::Result<()> {
    let mut legacy = sqlx::migrate!("./migrations");
    legacy
        .migrations
        .to_mut()
        .retain(|migration| migration.version < 29);
    legacy.run(pool).await?;
    ensure!(membership_column_type(pool).await? == "uuid");
    sqlx::raw_sql(include_str!("postgres_membership_migration_fixture.sql"))
        .execute(pool)
        .await?;
    let before = retained_state(pool).await?;

    let mut upgrade = sqlx::migrate!("./migrations");
    upgrade
        .migrations
        .to_mut()
        .retain(|migration| migration.version <= 29);
    upgrade.run(pool).await?;
    upgrade.run(pool).await?;
    ensure!(membership_column_type(pool).await? == "text");
    ensure!(
        before == retained_state(pool).await?,
        "retained history, keys, or permissions changed"
    );
    let migrated = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM commit.actor_projection a
         JOIN commit.organization_projection o USING (organization_id)
         WHERE a.membership_id = a.actor_id || '[' || o.org_id || ']'",
    )
    .fetch_one(pool)
    .await?;
    ensure!(
        migrated == 6,
        "all six production/testing actor mappings must be backfilled"
    );

    let rewrite = sqlx::query(
        "UPDATE commit.actor_projection
         SET membership_id = actor_id || '[other-org]' WHERE actor_id = 'alice'",
    )
    .execute(pool)
    .await;
    ensure!(
        rewrite
            .err()
            .and_then(|error| error
                .as_database_error()
                .and_then(sqlx::error::DatabaseError::code)
                .map(std::borrow::Cow::into_owned))
            .as_deref()
            == Some("23514"),
        "membership projections must remain immutable after migration"
    );
    ensure!(before == retained_state(pool).await?);
    Ok(())
}

async fn membership_column_type(pool: &PgPool) -> Result<String, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT data_type FROM information_schema.columns
         WHERE table_schema = 'commit' AND table_name = 'actor_projection' AND column_name = 'membership_id'",
    )
    .fetch_one(pool)
    .await
}

async fn retained_state(pool: &PgPool) -> Result<Value, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT jsonb_build_object(
          'actors', (SELECT jsonb_agg(to_jsonb(a) - 'membership_id' ORDER BY organization_id, principal_id) FROM commit.actor_projection a),
          'organizations', (SELECT jsonb_agg(to_jsonb(o) ORDER BY organization_id) FROM commit.organization_projection o),
          'testing', (SELECT jsonb_agg(to_jsonb(t) ORDER BY environment_id) FROM commit.testing_organizations t),
          'todos', (SELECT jsonb_agg(to_jsonb(t) ORDER BY id) FROM commit.todos t),
          'notes', (SELECT jsonb_agg(to_jsonb(n) ORDER BY id) FROM commit.todo_notes n),
          'idempotency', (SELECT jsonb_agg(to_jsonb(i) ORDER BY id) FROM commit.idempotency_records i),
          'access', (SELECT jsonb_build_object('rls', relrowsecurity, 'forced', relforcerowsecurity, 'acl', relacl)
                    FROM pg_class WHERE oid = 'commit.actor_projection'::regclass)
         )",
    )
    .fetch_one(pool)
    .await
}
