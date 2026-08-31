//! Silicon Commit HTTP API process.

use silicon_commit::{api, config::Settings, telemetry};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    let settings = Settings::from_env_for_api()?;
    telemetry::init(&settings.log_filter)?;
    api::serve(settings).await
}
