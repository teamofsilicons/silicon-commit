//! Silicon Commit outbox and retention worker process.

use silicon_commit::{config::Settings, telemetry, worker};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    let settings = Settings::from_env_for_worker()?;
    telemetry::init(&settings.log_filter)?;
    worker::run(settings).await
}
