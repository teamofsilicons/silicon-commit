//! Upgrade coverage for retained IAM membership identities across all data planes.

use std::{env, str::FromStr};

use anyhow::{Context as _, ensure};
use serde_json::Value;
use sqlx::{PgPool, postgres::PgConnectOptions};
use uuid::Uuid;

#[tokio::test]
async fn public_id_migration_preserves_keys_history_and_project_aliases() -> anyhow::Result<()> {
    let Ok(database_url) = env::var("COMMIT_TEST_DATABASE_URL") else {
        eprintln!("skipping PostgreSQL migration test: COMMIT_TEST_DATABASE_URL is not set");
        return Ok(());
    };
    // A separate database exercises the actual pre-upgrade schema while other
    // integration tests continue using the current schema on their own pool.
    let options = PgConnectOptions::from_str(&database_url)?;
    let admin = PgPool::connect_with(options.clone()).await?;
    let database = format!("commit_public_ids_{}", Uuid::new_v4().simple());
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
    sqlx::raw_sql(sqlx::AssertSqlSafe(include_str!("postgres_membership_migration_fixture.sql").replace("request_fingerprint, response_status, response_body", "request_fingerprint, response_status, response_body, created_at, expires_at").replace("jsonb_build_object('id', todo, 'title', 'Retain this work')", "jsonb_build_object('id', todo, 'title', 'Retain this work'), clock_timestamp()-interval '2 days', clock_timestamp()-interval '1 day'")))
        .execute(pool)
        .await?;
    let mut previous = sqlx::migrate!("./migrations");
    previous
        .migrations
        .to_mut()
        .retain(|migration| migration.version < 32);
    previous.run(pool).await?;
    sqlx::raw_sql(r#"
      BEGIN;
      INSERT INTO commit.projects(id,organization_id,name,slug,uid,created_by_principal_id)
      SELECT gen_random_uuid(),organization_id,'Workspace','workspace',
        'workspace:'||actor_id||':1788000000000',principal_id
      FROM commit.actor_projection WHERE actor_type='silicon';
      INSERT INTO commit.project_participants(id,organization_id,project_id,silicon_principal_id,added_by_principal_id)
      SELECT gen_random_uuid(),organization_id,id,created_by_principal_id,created_by_principal_id FROM commit.projects;
      UPDATE commit.testing_environments SET iam_app_id='shared>commit',
        iam_app_secret_ciphertext=decode(repeat('ab',32),'hex');
      INSERT INTO commit.honeycomb_environments(environment_id,org_id,app_id,environment_revision,generation,key_version,state,operation_id,root_key_ciphertext,root_key_digest)
      SELECT environment_id,'shared','shared>commit',1,1,1,'active',gen_random_uuid(),decode(repeat('cd',32),'hex'),repeat('d',64)
      FROM commit.testing_environments;
      INSERT INTO commit.honeycomb_operations(environment_id,operation_id,request_hash,receipt)
      SELECT environment_id,operation_id,repeat('e',64),'{"app_id":"shared>commit","state":"completed"}' FROM commit.honeycomb_environments;
      INSERT INTO commit.iam_webhook_events(event_id,event_type,organization_id,aggregate_id,aggregate_version,payload,payload_sha256)
      SELECT gen_random_uuid(),'silicon.updated',organization_id,'chef:shared',1,'{"actor":{"id":"chef:shared"}}',repeat('f',64)
      FROM commit.organization_projection;
      COMMIT;
    "#).execute(pool).await?;
    let before = retained_state(pool).await?;
    rejected_upgrade(
        pool,
        "ALTER TABLE commit.projects DISABLE TRIGGER projects_preserve_identity",
        "ordinary enabled mode",
    )
    .await?;
    rejected_upgrade(pool, r#"
      INSERT INTO commit.idempotency_records
      SELECT gen_random_uuid(),organization_id,todo_id,actor_principal_id,operation,resource_path,
      'still-live',request_fingerprint,response_status,response_body,clock_timestamp(),clock_timestamp()+interval '1 hour'
      FROM commit.idempotency_records LIMIT 1
    "#, "expired idempotency replay").await?;
    rejected_upgrade(pool, r#"
      INSERT INTO commit.organization_projection(organization_id,org_id) VALUES
        ('aaaaaaaa-0000-0000-0000-000000000001','other');
      INSERT INTO commit.actor_projection(organization_id,principal_id,membership_id,actor_type,actor_id)
      VALUES ('aaaaaaaa-0000-0000-0000-000000000001',gen_random_uuid(),'chef:other[other]','silicon','chef:other');
    "#, "public actor ID collision").await?;
    let upgrade = sqlx::migrate!("./migrations");
    upgrade.run(pool).await?;
    upgrade.run(pool).await?;
    ensure!(
        before == retained_state(pool).await?,
        "retained UUIDs, history, versions, ciphertext or permissions changed"
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM commit.actor_projection WHERE actor_id IN ('c:alice','si:chef') AND membership_id=actor_id||'[shared]'").fetch_one(pool).await?;
    ensure!(
        count == 6,
        "all production and independent testing actors must migrate"
    );
    let aliases: i64 = sqlx::query_scalar("SELECT count(*) FROM commit.projects WHERE uid='workspace:si:chef:1788000000000' AND legacy_uid='workspace:chef:shared:1788000000000'").fetch_one(pool).await?;
    ensure!(aliases == 3, "all project URLs must retain old aliases");
    let apps: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM commit.testing_environments WHERE iam_app_id='commit'",
    )
    .fetch_one(pool)
    .await?;
    ensure!(apps == 2);
    for statement in [
        "UPDATE commit.actor_projection SET actor_id='si:replacement' WHERE actor_id='si:chef'",
        "UPDATE commit.projects SET legacy_uid='replacement'",
    ] {
        ensure!(
            sqlx::query(statement).execute(pool).await.is_err(),
            "identity immutability must be restored"
        );
    }
    ensure!(before == retained_state(pool).await?);
    Ok(())
}

async fn rejected_upgrade(
    pool: &PgPool,
    setup: &'static str,
    expected: &str,
) -> anyhow::Result<()> {
    let mut transaction = pool.begin().await?;
    sqlx::raw_sql(setup).execute(&mut *transaction).await?;
    let error = sqlx::raw_sql(include_str!(
        "../migrations/0032_prefixed_public_identifiers.sql"
    ))
    .execute(&mut *transaction)
    .await
    .err()
    .context("unsafe upgrade must fail")?;
    ensure!(
        error.to_string().contains(expected),
        "unexpected upgrade failure: {error}"
    );
    transaction.rollback().await?;
    let absent: bool =
        sqlx::query_scalar("SELECT to_regclass('commit_private.public_id_schema_map') IS NULL")
            .fetch_one(pool)
            .await?;
    ensure!(absent, "failed migration must roll back all state");
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
          'actors', (SELECT jsonb_agg(to_jsonb(a) - 'membership_id' - 'actor_id' ORDER BY organization_id, principal_id) FROM commit.actor_projection a),
          'projects', (SELECT jsonb_agg(to_jsonb(p)-'uid'-'legacy_uid' ORDER BY id) FROM commit.projects p),
          'honeycomb', (SELECT jsonb_agg(to_jsonb(h)-'app_id' ORDER BY environment_id) FROM commit.honeycomb_environments h),
          'receipts', (SELECT jsonb_agg(to_jsonb(h) ORDER BY environment_id,operation_id) FROM commit.honeycomb_operations h),
          'inbox', (SELECT jsonb_agg(to_jsonb(e) ORDER BY event_id) FROM commit.iam_webhook_events e),
          'credentials', (SELECT jsonb_agg(to_jsonb(t)-'iam_app_id' ORDER BY environment_id) FROM commit.testing_environments t),
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
