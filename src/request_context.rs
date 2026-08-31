//! Request-local correlation context.

use std::future::Future;

tokio::task_local! {
    static REQUEST_ID: String;
}

/// Runs a request future under its validated correlation identifier.
pub async fn scope<T>(request_id: String, future: impl Future<Output = T>) -> T {
    REQUEST_ID.scope(request_id, future).await
}

/// Returns the current request identifier, when inside an HTTP request.
#[must_use]
pub fn current_request_id() -> Option<String> {
    REQUEST_ID.try_with(Clone::clone).ok()
}
