//! Stateless client for Silicon Commit's public API.
//!
//! Supply organization and credential context explicitly. Keep a [`Mutation`]
//! across retries of a logical write. Session persistence belongs to callers.

use reqwest::{Client as HttpClient, Method, StatusCode};
use secrecy::{ExposeSecret as _, SecretString};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::{fmt, time::Duration};
use thiserror::Error;
use url::Url;
use uuid::Uuid;

/// crates.io metadata used by the optional hourly updater.
#[derive(Clone, Debug, Deserialize)]
pub struct RegistryRelease {
    pub version: String,
}

/// Returns the latest published client version when the registry is reachable.
/// This is deliberately separate from API requests and never updates a running
/// process; callers decide whether and how to install a newer binary/package.
pub async fn latest_release() -> Result<RegistryRelease, Error> {
    latest_release_for("silicon-commit-client").await
}

/// Returns the latest published CLI version when the registry is reachable.
/// The CLI package is separate from the client library and therefore has its
/// own registry check.
pub async fn latest_cli_release() -> Result<RegistryRelease, Error> {
    latest_release_for("silicon-commit-cli").await
}

async fn latest_release_for(crate_name: &str) -> Result<RegistryRelease, Error> {
    let http = HttpClient::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(transport)?;
    let response = http
        .get(format!("https://crates.io/api/v1/crates/{crate_name}"))
        .header(
            "user-agent",
            format!("{crate_name}/{}", env!("CARGO_PKG_VERSION")),
        )
        .send()
        .await
        .map_err(transport)?;
    if !response.status().is_success() {
        return Err(Error::Api {
            status: response.status(),
            code: "registry_unavailable".into(),
            request_id: None,
        });
    }
    let body = response.bytes().await.map_err(transport)?;
    let value: Value = serde_json::from_slice(&body).map_err(Error::Decode)?;
    let version = value
        .pointer("/crate/newest")
        .and_then(Value::as_str)
        .ok_or(Error::Invalid("registry response missing newest version"))?;
    Ok(RegistryRelease {
        version: version.to_owned(),
    })
}

/// Errors preserve status and correlation but never include credentials or raw proxy bodies.
#[derive(Debug, Error)]
pub enum Error {
    #[error(
        "invalid API URL: use an HTTP(S) origin or /api/v1/ URL without credentials, query, or fragment"
    )]
    InvalidUrl,
    #[error("invalid request: {0}")]
    Invalid(&'static str),
    #[error("request transport failed")]
    Transport(#[source] reqwest::Error),
    #[error("HTTP {status}: {code} (request ID: {request_id:?})")]
    Api {
        status: StatusCode,
        code: String,
        request_id: Option<String>,
    },
    #[error("response exceeds the 8 MiB limit")]
    ResponseTooLarge,
    #[error("response did not match the expected JSON contract")]
    Decode(#[source] serde_json::Error),
}
impl Error {
    /// Whether the current access token needs renewal.
    pub fn is_unauthenticated(&self) -> bool {
        matches!(
            self,
            Self::Api {
                status: StatusCode::UNAUTHORIZED,
                ..
            }
        )
    }
}

/// A logical mutation; reuse this value after an uncertain response.
#[derive(Clone, Debug)]
pub struct Mutation {
    key: String,
    version: Option<i64>,
}
impl Default for Mutation {
    fn default() -> Self {
        Self::new()
    }
}
impl Mutation {
    pub fn new() -> Self {
        Self {
            key: Uuid::new_v4().to_string(),
            version: None,
        }
    }
    pub fn with_key(key: impl Into<String>) -> Result<Self, Error> {
        let key = key.into();
        if !(8..=255).contains(&key.len()) || !key.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(Error::Invalid(
                "idempotency key must be 8–255 visible ASCII characters",
            ));
        }
        Ok(Self { key, version: None })
    }
    pub fn if_match(mut self, version: i64) -> Result<Self, Error> {
        if version < 0 {
            return Err(Error::Invalid("version cannot be negative"));
        }
        self.version = Some(version);
        Ok(self)
    }
    pub fn key(&self) -> &str {
        &self.key
    }
}

/// Explicit connection/credential configuration; no session state is written to disk.
#[derive(Clone, Debug)]
pub struct Client {
    http: HttpClient,
    base: Url,
    bearer: Option<SecretString>,
    test_key: Option<SecretString>,
    org_id: Option<String>,
    mutation: Option<Mutation>,
}

/// Login tokens are serialized only when the caller explicitly saves a session.
#[derive(Clone, Deserialize, Serialize)]
pub struct SessionTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub token_type: Value,
    pub expires_in: i64,
    pub scope: String,
    pub actor: Value,
    pub org_id: Option<String>,
}
impl fmt::Debug for SessionTokens {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionTokens")
            .field("tokens", &"<redacted>")
            .field("org_id", &self.org_id)
            .finish()
    }
}

impl Client {
    /// Accepts a bare backend origin, /api/v1, or /api/v1/.
    pub fn new(base_url: &str) -> Result<Self, Error> {
        let mut base = Url::parse(base_url).map_err(|_| Error::InvalidUrl)?;
        if !matches!(base.scheme(), "http" | "https")
            || base.host_str().is_none()
            || !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
            || !matches!(base.path(), "" | "/" | "/api/v1" | "/api/v1/")
        {
            return Err(Error::InvalidUrl);
        }
        base.set_path("/api/v1/");
        let http = HttpClient::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("silicon-commit-client/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(transport)?;
        Ok(Self {
            http,
            base,
            bearer: None,
            test_key: None,
            org_id: None,
            mutation: None,
        })
    }
    pub fn with_bearer(mut self, token: impl Into<String>) -> Self {
        self.bearer = Some(SecretString::from(token.into()));
        self
    }
    pub fn with_test_key(mut self, key: impl Into<String>) -> Result<Self, Error> {
        let key = key.into();
        if key.len() != 32 || !key.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err(Error::Invalid(
                "test-environment key must be exactly 32 alphanumeric characters",
            ));
        }
        self.test_key = Some(SecretString::from(key));
        Ok(self)
    }
    pub fn with_org_id(mut self, org_id: impl Into<String>) -> Self {
        self.org_id = Some(org_id.into());
        self
    }
    /// Attaches retry/precondition inputs to requests made through this clone.
    pub fn with_mutation(mut self, mutation: Mutation) -> Self {
        self.mutation = Some(mutation);
        self
    }
    pub fn base_url(&self) -> &Url {
        &self.base
    }
    pub fn new_idempotency_key() -> String {
        Mutation::new().key
    }
    pub async fn health(&self) -> Result<Value, Error> {
        self.execute(Method::GET, &["healthz"], None, &[], true)
            .await
    }
    pub async fn ready(&self) -> Result<Value, Error> {
        self.execute(Method::GET, &["readyz"], None, &[], true)
            .await
    }
    pub async fn version(&self) -> Result<Value, Error> {
        // `/version` is intentionally public; do not disclose caller context.
        let mut public = self.clone();
        public.bearer = None;
        public.org_id = None;
        public.test_key = None;
        public.get(&["version"], &[]).await
    }
    /// Public IAM application metadata. Caller credentials and test context are omitted.
    pub async fn iam(&self) -> Result<Value, Error> {
        let mut public = self.clone();
        public.bearer = None;
        public.org_id = None;
        public.test_key = None;
        public.get(&["iam"], &[]).await
    }
    /// Verify the current login against IAM and return its public actor identity.
    /// Missing or rejected credentials return `authenticated: false`; transport,
    /// permission, and server failures remain errors. No tokens are returned.
    pub async fn login_status(&self) -> Result<Value, Error> {
        if self
            .bearer
            .as_ref()
            .is_none_or(|token| token.expose_secret().is_empty())
        {
            return Ok(serde_json::json!({"authenticated": false, "actor": null, "org_id": null}));
        }
        match self.get(&["auth", "status"], &[]).await {
            Err(error) if error.is_unauthenticated() => {
                Ok(serde_json::json!({"authenticated": false, "actor": null, "org_id": null}))
            }
            result => result,
        }
    }
    pub async fn login_with_slt(&self, slt: &str) -> Result<SessionTokens, Error> {
        decode(
            self.write(
                Method::POST,
                &["auth", "login"],
                &serde_json::json!({"slt":slt}),
            )
            .await?,
        )
    }
    pub async fn refresh_session(&self, refresh_token: &str) -> Result<SessionTokens, Error> {
        decode(
            self.write(
                Method::POST,
                &["auth", "refresh"],
                &serde_json::json!({"refresh_token":refresh_token}),
            )
            .await?,
        )
    }
    pub async fn logout(&self, token: &str) -> Result<(), Error> {
        self.write(
            Method::POST,
            &["auth", "logout"],
            &serde_json::json!({"token":token}),
        )
        .await?;
        Ok(())
    }
    /// GET /api/v1/todos.
    pub async fn list_todos(&self, query: &[(&str, &str)]) -> Result<Value, Error> {
        self.get(&["todos"], query).await
    }
    /// GET /api/v1/todos/{todo_id}.
    pub async fn get_todo(&self, todo_id: &str) -> Result<Value, Error> {
        self.get(&["todos", todo_id], &[]).await
    }
    /// POST /api/v1/todos.
    pub async fn create_todo<T: Serialize + ?Sized>(&self, body: &T) -> Result<Value, Error> {
        self.write(Method::POST, &["todos"], body).await
    }
    /// PATCH /api/v1/todos/{todo_id}.
    pub async fn update_todo<T: Serialize + ?Sized>(
        &self,
        todo_id: &str,
        body: &T,
    ) -> Result<Value, Error> {
        self.write(Method::PATCH, &["todos", todo_id], body).await
    }
    /// DELETE /api/v1/todos/{todo_id}.
    pub async fn delete_todo(&self, todo_id: &str) -> Result<Value, Error> {
        self.execute(Method::DELETE, &["todos", todo_id], None, &[], false)
            .await
    }
    /// GET /api/v1/todos/{todo_id}/notes.
    pub async fn list_notes(&self, todo_id: &str, query: &[(&str, &str)]) -> Result<Value, Error> {
        self.get(&["todos", todo_id, "notes"], query).await
    }
    /// POST /api/v1/todos/{todo_id}/notes.
    pub async fn add_note<T: Serialize + ?Sized>(
        &self,
        todo_id: &str,
        body: &T,
    ) -> Result<Value, Error> {
        self.write(Method::POST, &["todos", todo_id, "notes"], body)
            .await
    }
    /// GET /api/v1/todos/{todo_id}/notification-subscription.
    pub async fn todo_subscription(&self, todo_id: &str) -> Result<Value, Error> {
        self.get(&["todos", todo_id, "notification-subscription"], &[])
            .await
    }
    /// PUT /api/v1/todos/{todo_id}/notification-subscription.
    pub async fn replace_todo_subscription<T: Serialize + ?Sized>(
        &self,
        todo_id: &str,
        body: &T,
    ) -> Result<Value, Error> {
        self.write(
            Method::PUT,
            &["todos", todo_id, "notification-subscription"],
            body,
        )
        .await
    }
    /// GET /api/v1/notification-settings.
    pub async fn notification_settings(&self) -> Result<Value, Error> {
        self.get(&["notification-settings"], &[]).await
    }
    /// PUT /api/v1/notification-settings.
    pub async fn update_notification_settings<T: Serialize + ?Sized>(
        &self,
        body: &T,
    ) -> Result<Value, Error> {
        self.write(Method::PUT, &["notification-settings"], body)
            .await
    }
    /// GET /api/v1/projects.
    pub async fn list_projects(&self, query: &[(&str, &str)]) -> Result<Value, Error> {
        self.get(&["projects"], query).await
    }
    /// GET /api/v1/projects/{project_id}.
    pub async fn get_project(&self, project_id: &str) -> Result<Value, Error> {
        self.get(&["projects", project_id], &[]).await
    }
    /// POST /api/v1/projects.
    pub async fn create_project<T: Serialize + ?Sized>(&self, body: &T) -> Result<Value, Error> {
        self.write(Method::POST, &["projects"], body).await
    }
    /// PATCH /api/v1/projects/{project_id}.
    pub async fn update_project<T: Serialize + ?Sized>(
        &self,
        project_id: &str,
        body: &T,
    ) -> Result<Value, Error> {
        self.write(Method::PATCH, &["projects", project_id], body)
            .await
    }
    /// GET /api/v1/projects/{project_id}/diary.
    pub async fn project_diary(&self, project_id: &str) -> Result<Value, Error> {
        self.get(&["projects", project_id, "diary"], &[]).await
    }
    /// PUT /api/v1/projects/{project_id}/diary.
    pub async fn replace_project_diary<T: Serialize + ?Sized>(
        &self,
        project_id: &str,
        body: &T,
    ) -> Result<Value, Error> {
        self.write(Method::PUT, &["projects", project_id, "diary"], body)
            .await
    }
    /// GET /api/v1/projects/{project_id}/tasks.
    pub async fn project_tasks(
        &self,
        project_id: &str,
        query: &[(&str, &str)],
    ) -> Result<Value, Error> {
        self.get(&["projects", project_id, "tasks"], query).await
    }
    /// GET /api/v1/projects/{project_id}/entries.
    pub async fn project_entries(
        &self,
        project_id: &str,
        query: &[(&str, &str)],
    ) -> Result<Value, Error> {
        self.get(&["projects", project_id, "entries"], query).await
    }
    /// POST /api/v1/projects/{project_id}/tasks.
    pub async fn create_project_task<T: Serialize + ?Sized>(
        &self,
        project_id: &str,
        body: &T,
    ) -> Result<Value, Error> {
        self.write(Method::POST, &["projects", project_id, "tasks"], body)
            .await
    }
    /// PATCH /api/v1/projects/{project_id}/tasks/{task_id}.
    pub async fn update_project_task<T: Serialize + ?Sized>(
        &self,
        project_id: &str,
        task_id: &str,
        body: &T,
    ) -> Result<Value, Error> {
        self.write(
            Method::PATCH,
            &["projects", project_id, "tasks", task_id],
            body,
        )
        .await
    }
    /// POST /api/v1/projects/{project_id}/blockers.
    pub async fn create_project_blocker<T: Serialize + ?Sized>(
        &self,
        project_id: &str,
        body: &T,
    ) -> Result<Value, Error> {
        self.write(Method::POST, &["projects", project_id, "blockers"], body)
            .await
    }
    /// POST /api/v1/projects/{project_id}/updates.
    pub async fn create_project_update<T: Serialize + ?Sized>(
        &self,
        project_id: &str,
        body: &T,
    ) -> Result<Value, Error> {
        self.write(Method::POST, &["projects", project_id, "updates"], body)
            .await
    }
    /// POST /api/v1/projects/{project_id}/completion.
    pub async fn complete_project<T: Serialize + ?Sized>(
        &self,
        project_id: &str,
        body: &T,
    ) -> Result<Value, Error> {
        self.write(Method::POST, &["projects", project_id, "completion"], body)
            .await
    }
    /// GET /api/v1/test-environments.
    pub async fn list_test_environments(&self) -> Result<Value, Error> {
        self.get(&["test-environments"], &[]).await
    }
    /// POST /api/v1/test-environments.
    pub async fn create_test_environment<T: Serialize + ?Sized>(
        &self,
        body: &T,
    ) -> Result<Value, Error> {
        self.write(Method::POST, &["test-environments"], body).await
    }
    /// GET /api/v1/test-environments/{id}/key.
    pub async fn retrieve_test_environment_key(&self, id: &str) -> Result<Value, Error> {
        self.get(&["test-environments", id, "key"], &[]).await
    }
    /// POST /api/v1/test-environments/{id}/rotate.
    pub async fn rotate_test_environment(&self, id: &str) -> Result<Value, Error> {
        self.execute(
            Method::POST,
            &["test-environments", id, "rotate"],
            None,
            &[],
            false,
        )
        .await
    }
    /// DELETE /api/v1/test-environments/{id}.
    pub async fn delete_test_environment(&self, id: &str) -> Result<Value, Error> {
        self.execute(Method::DELETE, &["test-environments", id], None, &[], false)
            .await
    }
    pub async fn restore_test_environment(&self, id: &str) -> Result<Value, Error> {
        self.execute(
            Method::POST,
            &["test-environments", id, "restore"],
            None,
            &[],
            false,
        )
        .await
    }
    pub async fn clean_test_environment(&self, id: &str) -> Result<Value, Error> {
        self.execute(
            Method::POST,
            &["test-environments", id, "clean"],
            None,
            &[],
            false,
        )
        .await
    }
    async fn get(&self, path: &[&str], query: &[(&str, &str)]) -> Result<Value, Error> {
        self.execute(Method::GET, path, None, query, false).await
    }
    async fn write<T: Serialize + ?Sized>(
        &self,
        method: Method,
        path: &[&str],
        body: &T,
    ) -> Result<Value, Error> {
        let body = serde_json::to_vec(body).map_err(Error::Decode)?;
        self.execute(method, path, Some(body), &[], false).await
    }
    fn url(&self, path: &[&str], root: bool) -> Result<Url, Error> {
        if path
            .iter()
            .any(|p| p.is_empty() || matches!(*p, "." | ".."))
        {
            return Err(Error::Invalid(
                "empty and dot resource identifiers are forbidden",
            ));
        }
        let mut url = self.base.clone();
        {
            let mut segments = url.path_segments_mut().map_err(|_| Error::InvalidUrl)?;
            if root {
                segments.clear();
            } else {
                segments.pop_if_empty();
            }
            segments.extend(path);
        }
        Ok(url)
    }
    async fn execute(
        &self,
        method: Method,
        path: &[&str],
        body: Option<Vec<u8>>,
        query: &[(&str, &str)],
        root: bool,
    ) -> Result<Value, Error> {
        let is_mutation = !matches!(method, Method::GET | Method::HEAD | Method::OPTIONS);
        let mut req = self
            .http
            .request(method, self.url(path, root)?)
            .query(query);
        if !root {
            if let Some(token) = &self.bearer {
                req = req.bearer_auth(token.expose_secret());
            }
            if let Some(org) = &self.org_id {
                req = req.header("x-org-id", org);
            }
            if let Some(key) = &self.test_key {
                let key = key.expose_secret();
                if key.len() != 32 || !key.bytes().all(|b| b.is_ascii_alphanumeric()) {
                    return Err(Error::Invalid(
                        "testing key must be exactly 32 alphanumeric characters",
                    ));
                }
                req = req.header("x-testing-environment-key", key);
            }
        }
        if is_mutation {
            let mutation = self.mutation.clone().unwrap_or_default();
            req = req.header("idempotency-key", mutation.key);
            if let Some(version) = mutation.version {
                req = req.header("if-match", format!("\"{version}\""));
            }
        }
        if let Some(body) = body {
            req = req.header("content-type", "application/json").body(body);
        }
        let mut response = req.send().await.map_err(transport)?;
        let status = response.status();
        let request_id = response
            .headers()
            .get("x-request-id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        const LIMIT: usize = 8 * 1024 * 1024;
        if response.content_length().is_some_and(|n| n > LIMIT as u64) {
            return Err(Error::ResponseTooLarge);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(transport)? {
            if bytes.len() + chunk.len() > LIMIT {
                return Err(Error::ResponseTooLarge);
            }
            bytes.extend_from_slice(&chunk);
        }
        if !status.is_success() {
            let parsed: Option<Value> = serde_json::from_slice(&bytes).ok();
            let code = parsed
                .as_ref()
                .and_then(|v| v.pointer("/error/code"))
                .and_then(Value::as_str)
                .filter(|s| {
                    s.len() <= 128 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                })
                .unwrap_or("http_error")
                .to_owned();
            return Err(Error::Api {
                status,
                code,
                request_id,
            });
        }
        if bytes.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&bytes).map_err(Error::Decode)
    }
}
fn transport(error: reqwest::Error) -> Error {
    Error::Transport(error.without_url())
}
fn decode<T: DeserializeOwned>(value: Value) -> Result<T, Error> {
    serde_json::from_value(value).map_err(Error::Decode)
}
