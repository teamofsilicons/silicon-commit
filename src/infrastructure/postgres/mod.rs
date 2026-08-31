//! PostgreSQL pool, migration, and repository composition.

use std::str::FromStr as _;

use anyhow::{Context as _, bail};
use secrecy::ExposeSecret as _;
use sqlx::{
    PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};

use crate::config::DatabaseSettings;

mod identity_projection;
pub mod projects;
pub mod todos;

/// Fails closed if an IAM-authenticated identity contradicts retained mapping
/// history in either the internal-ID or public-ID direction.
pub async fn assert_identity_consistency(
    pool: &PgPool,
    actor: &crate::application::ports::VerifiedActor,
) -> Result<(), crate::error::AppError> {
    identity_projection::assert_consistent(pool, actor).await
}

/// Creates and verifies a bounded PostgreSQL pool.
///
/// # Errors
///
/// Returns an error when the URL is malformed, the database is unavailable, or
/// required UTC/statement-timeout session settings cannot be applied.
pub async fn connect(
    settings: &DatabaseSettings,
    application_name: &str,
) -> anyhow::Result<PgPool> {
    connect_with_search_path(settings, application_name, "commit,pg_catalog").await
}

/// Creates the privileged migration pool with stable metadata placement.
///
/// Keeping `public` first ensures `SQLx` always reads the same migration ledger,
/// including before the `commit` schema exists on a brand-new database.
///
/// # Errors
///
/// Returns an error under the same conditions as [`connect`].
pub async fn connect_migrator(
    settings: &DatabaseSettings,
    application_name: &str,
) -> anyhow::Result<PgPool> {
    connect_with_search_path(settings, application_name, "public,pg_catalog").await
}

async fn connect_with_search_path(
    settings: &DatabaseSettings,
    application_name: &str,
    search_path: &'static str,
) -> anyhow::Result<PgPool> {
    let options = PgConnectOptions::from_str(settings.url.expose_secret())?
        .application_name(application_name);
    let statement_timeout_ms = i64::try_from(settings.statement_timeout.as_millis())?;

    let pool = PgPoolOptions::new()
        .max_connections(settings.max_connections.get())
        .min_connections(settings.min_connections)
        .acquire_timeout(settings.acquire_timeout)
        .idle_timeout(Some(std::time::Duration::from_secs(300)))
        .max_lifetime(Some(std::time::Duration::from_mins(30)))
        .test_before_acquire(true)
        .after_connect(move |connection, _metadata| {
            Box::pin(async move {
                sqlx::query("SELECT set_config('timezone', 'UTC', false)")
                    .execute(&mut *connection)
                    .await?;
                sqlx::query("SELECT set_config('search_path', $1, false)")
                    .bind(search_path)
                    .execute(&mut *connection)
                    .await?;
                sqlx::query("SELECT set_config('statement_timeout', $1, false)")
                    .bind(statement_timeout_ms.to_string())
                    .execute(&mut *connection)
                    .await?;
                Ok(())
            })
        })
        .connect_with(options)
        .await?;

    Ok(pool)
}

/// Executes all embedded forward-only schema migrations as the configured
/// application schema owner.
///
/// # Errors
///
/// Returns an error if the connection identity is not exactly
/// `expected_owner`, an existing application object has another owner,
/// migration locking or a statement fails, or a migration creates an object
/// under another owner.
pub async fn migrate(pool: &PgPool, expected_owner: &str) -> anyhow::Result<()> {
    assert_migration_identity(pool, expected_owner).await?;
    assert_application_ownership(pool, expected_owner)
        .await
        .context("validate application object ownership before migration")?;
    sqlx::migrate!("./migrations").run(pool).await?;
    assert_migration_identity(pool, expected_owner).await?;
    assert_application_ownership(pool, expected_owner)
        .await
        .context("validate application object ownership after migration")?;
    Ok(())
}

async fn assert_migration_identity(pool: &PgPool, expected_owner: &str) -> anyhow::Result<()> {
    let (current_user, session_user) =
        sqlx::query_as::<_, (String, String)>("SELECT current_user::text, session_user::text")
            .fetch_one(pool)
            .await?;
    if current_user != expected_owner || session_user != expected_owner {
        bail!(
            "migration connection must use schema-owner role {expected_owner:?} directly; \
             current_user={current_user:?}, session_user={session_user:?}"
        );
    }
    Ok(())
}

async fn assert_application_ownership(pool: &PgPool, expected_owner: &str) -> anyhow::Result<()> {
    let violation = sqlx::query_as::<_, (String, String, String)>(
        r"
        WITH application_objects (object_kind, object_name, owner_name) AS (
            SELECT 'schema'::text,
                   namespace.nspname::text,
                   pg_catalog.pg_get_userbyid(namespace.nspowner)::text
            FROM pg_catalog.pg_namespace AS namespace
            WHERE namespace.nspname IN ('commit', 'commit_private')

            UNION ALL

            SELECT 'relation'::text,
                   pg_catalog.format('%I.%I', namespace.nspname, relation.relname),
                   pg_catalog.pg_get_userbyid(relation.relowner)::text
            FROM pg_catalog.pg_class AS relation
            JOIN pg_catalog.pg_namespace AS namespace
              ON namespace.oid = relation.relnamespace
            WHERE namespace.nspname IN ('commit', 'commit_private')
               OR (namespace.nspname = 'public' AND relation.relname = '_sqlx_migrations')

            UNION ALL

            SELECT 'type'::text,
                   pg_catalog.format('%I.%I', namespace.nspname, app_type.typname),
                   pg_catalog.pg_get_userbyid(app_type.typowner)::text
            FROM pg_catalog.pg_type AS app_type
            JOIN pg_catalog.pg_namespace AS namespace
              ON namespace.oid = app_type.typnamespace
            WHERE namespace.nspname IN ('commit', 'commit_private')
              AND app_type.typisdefined

            UNION ALL

            SELECT 'routine'::text,
                   pg_catalog.format('%I.%I(%s)',
                       namespace.nspname,
                       routine.proname,
                       pg_catalog.pg_get_function_identity_arguments(routine.oid)),
                   pg_catalog.pg_get_userbyid(routine.proowner)::text
            FROM pg_catalog.pg_proc AS routine
            JOIN pg_catalog.pg_namespace AS namespace
              ON namespace.oid = routine.pronamespace
            WHERE namespace.nspname IN ('commit', 'commit_private')
        )
        SELECT object_kind, object_name, owner_name
        FROM application_objects
        WHERE owner_name <> $1
        ORDER BY object_kind, object_name
        LIMIT 1
        ",
    )
    .bind(expected_owner)
    .fetch_optional(pool)
    .await?;

    if let Some((object_kind, object_name, owner_name)) = violation {
        bail!(
            "Commit {object_kind} {object_name} is owned by {owner_name:?}, \
             expected {expected_owner:?}"
        );
    }
    Ok(())
}

/// Confirms that PostgreSQL can serve a trivial query.
pub async fn ready(pool: &PgPool) -> bool {
    sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(pool)
        .await
        .is_ok()
}
