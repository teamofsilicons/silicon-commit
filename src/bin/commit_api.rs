//! Silicon Commit HTTP API process.

use silicon_commit::{api, config::Settings, logging};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    let settings = Settings::from_env_for_api()?;
    logging::init(&settings.log_filter)?;
    api::serve(settings).await
}
