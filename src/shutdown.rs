//! Cross-platform graceful-shutdown signal handling.

/// Resolves after SIGINT or SIGTERM asks the process to stop.
pub async fn signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let terminate = signal(SignalKind::terminate());
        if let Ok(mut terminate) = terminate {
            tokio::select! {
                result = tokio::signal::ctrl_c() => {
                    if let Err(error) = result {
                        tracing::error!(%error, "failed to install SIGINT handler");
                    }
                }
                _ = terminate.recv() => {}
            }
            return;
        }
    }

    if let Err(error) = tokio::signal::ctrl_c().await {
        tracing::error!(%error, "failed to wait for shutdown signal");
    }
}
