//! Request-local correlation context.

use std::{cell::RefCell, future::Future};

#[derive(Default)]
struct Context {
    request_id: String,
    environment_key: RefCell<Option<String>>,
    iam_environment_key: RefCell<Option<String>>,
    testing_scope: RefCell<Option<TestingScope>>,
}

tokio::task_local! {
    static REQUEST: Context;
}

/// Runs a request future under its validated correlation identifier.
pub async fn scope<T>(request_id: String, future: impl Future<Output = T>) -> T {
    REQUEST
        .scope(
            Context {
                request_id,
                environment_key: RefCell::new(None),
                iam_environment_key: RefCell::new(None),
                testing_scope: RefCell::new(None),
            },
            future,
        )
        .await
}

/// Returns the current request identifier, when inside an HTTP request.
#[must_use]
pub fn current_request_id() -> Option<String> {
    REQUEST.try_with(|context| context.request_id.clone()).ok()
}

/// Installs the validated testing-environment key for this request task.
pub fn set_environment_key(key: Option<String>) {
    let _ = REQUEST.try_with(|context| *context.environment_key.borrow_mut() = key);
}

/// Returns the request's validated testing-environment key for provider calls.
#[must_use]
pub fn current_environment_key() -> Option<String> {
    REQUEST
        .try_with(|context| context.environment_key.borrow().clone())
        .ok()
        .flatten()
}
/// Installs the decrypted `IAm` test key for outbound provider requests.
pub fn set_iam_environment_key(key: Option<String>) {
    let _ = REQUEST.try_with(|c| *c.iam_environment_key.borrow_mut() = key);
}
/// Returns the request's decrypted `IAm` test key, when present.
#[must_use]
pub fn current_iam_environment_key() -> Option<String> {
    REQUEST
        .try_with(|c| c.iam_environment_key.borrow().clone())
        .ok()
        .flatten()
}

/// Bound environment identity and lifecycle generation for this request.
#[derive(Clone, Copy)]
pub struct TestingScope {
    /// Commit environment UUID, independent of the owning organization.
    pub id: uuid::Uuid,
    /// Lifecycle generation validated with the incoming key.
    pub version: i64,
}
/// Installs a resolved scope without retaining the external key in SQL state.
pub fn set_testing_scope(scope: Option<TestingScope>) {
    let _ = REQUEST.try_with(|c| *c.testing_scope.borrow_mut() = scope);
}
/// Returns the resolved environment, never inferred from an organization handle.
#[must_use]
pub fn testing_scope() -> Option<TestingScope> {
    REQUEST
        .try_with(|c| *c.testing_scope.borrow())
        .ok()
        .flatten()
}
