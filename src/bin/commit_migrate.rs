//! Silicon Commit one-shot privileged migration process.

use silicon_commit::{config::MigrationSettings, infrastructure::postgres, telemetry};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    let settings = MigrationSettings::from_env()?;
    telemetry::init(&settings.log_filter)?;
    let pool = postgres::connect_migrator(&settings.database, "commit-migrate").await?;
    postgres::migrate(&pool, &settings.schema_owner).await?;
    pool.close().await;
    Ok(())
}
