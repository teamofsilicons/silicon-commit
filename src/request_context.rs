//! Request-local correlation context.

use std::future::Future;

struct Context {
    request_id: String,
}

tokio::task_local! {
    static REQUEST: Context;
}

/// Runs a request future under its validated correlation identifier.
pub async fn scope<T>(request_id: String, future: impl Future<Output = T>) -> T {
    REQUEST.scope(Context { request_id }, future).await
}

/// Returns the current request identifier, when inside an HTTP request.
#[must_use]
pub fn current_request_id() -> Option<String> {
    REQUEST.try_with(|context| context.request_id.clone()).ok()
}
