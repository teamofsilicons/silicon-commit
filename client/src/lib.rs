//! Stateless client for Silicon Commit, the work manager for Carbons and Silicons.
//!
//! Two parts:
//!
//! * [`Client`]: the Commit HTTP API (`/api/v1`): todos, notes, notification rules,
//!   projects, diaries, tasks, versions, email settings, bug reports, the Silicon
//!   allow-list. Authenticate with a Silicon Accounts access token issued to Commit
//!   ([`Client::with_bearer`]), or, from another app acting for an account, with a User
//!   verification proof ([`Client::with_proof`]).
//! * [`auth::AccountsAuth`]: sign-in for Commit's own tools at Silicon Accounts, with no
//!   app secret: the device flow for Carbons, short-lived token exchange for Silicons,
//!   refresh and revocation.
//!
//! Nothing is stored: callers own tokens and decide where they live. Keep a [`Mutation`]
//! across retries of one logical write.
//!
//! ```no_run
//! # async fn demo(access_token: String) -> Result<(), silicon_commit_client::Error> {
//! use silicon_commit_client::{Client, Mutation};
//! let commit = Client::new("https://backend.commit.teamofsilicons.com")?.with_bearer(access_token);
//! let mine = commit.list_todos(&[("view", "assigned_to_me")]).await?;
//! let create = commit.with_mutation(Mutation::new());
//! create
//!     .create_todo(&serde_json::json!({"title": "Review the release", "assigned_to": "si:builder"}))
//!     .await?;
//! # Ok(()) }
//! ```

pub mod auth;
mod error;

pub use error::{ApiError, Error};
/// Token wrappers used by [`auth::SignIn`] (tokens are redacted from `Debug` output).
pub use secrecy::{ExposeSecret, SecretString};

use error::root_cause;
use reqwest::{Client as HttpClient, Method};
use serde::Serialize;
use serde_json::Value;
use std::{fmt, time::Duration};
use url::Url;
use uuid::Uuid;

/// The production Commit API origin.
pub const DEFAULT_API_URL: &str = "https://backend.commit.teamofsilicons.com";
/// The API contract this client speaks (`X-Commit-API-Version`).
pub const CONTRACT_VERSION: u16 = 2;
/// Largest response body read (8 MiB).
const RESPONSE_LIMIT: usize = 8 * 1024 * 1024;

/// A logical mutation: reuse the same value after an uncertain response so the API
/// applies the write once (`Idempotency-Key`), optionally with `If-Match`.
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
    /// A new logical write with a random key.
    pub fn new() -> Self {
        Self {
            key: Uuid::new_v4().to_string(),
            version: None,
        }
    }

    /// A logical write with your own key (8–255 visible ASCII characters).
    pub fn with_key(key: impl Into<String>) -> Result<Self, Error> {
        let key = key.into();
        if !(8..=255).contains(&key.len()) || !key.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(Error::Invalid(
                "An idempotency key must be 8 to 255 visible ASCII characters (no spaces).".into(),
            ));
        }
        Ok(Self { key, version: None })
    }

    /// Applies the write only if the resource is still at `version` (`If-Match`).
    pub fn if_match(mut self, version: i64) -> Result<Self, Error> {
        if version < 0 {
            return Err(Error::Invalid(format!(
                "If-Match versions start at 0; {version} is negative."
            )));
        }
        self.version = Some(version);
        Ok(self)
    }

    /// The idempotency key.
    pub fn key(&self) -> &str {
        &self.key
    }
}

/// How requests authenticate.
#[derive(Clone)]
enum Credential {
    Bearer(SecretString),
    Proof(SecretString),
}

/// The Commit API client. Holds configuration and a connection pool, never sessions.
#[derive(Clone)]
pub struct Client {
    http: HttpClient,
    base: Url,
    credential: Option<Credential>,
    telemetry: bool,
    source: &'static str,
    mutation: Option<Mutation>,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let credential = match &self.credential {
            None => "none",
            Some(Credential::Bearer(_)) => "bearer <redacted>",
            Some(Credential::Proof(_)) => "proof <redacted>",
        };
        f.debug_struct("Client")
            .field("base_url", &self.base.as_str())
            .field("credential", &credential)
            .field("telemetry", &self.telemetry)
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

/// True for `localhost`, `*.localhost`, `127.0.0.0/8` and `::1`.
pub fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(domain)) => domain == "localhost" || domain.ends_with(".localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

impl Client {
    /// Accepts the API origin, or the origin followed by `/api/v1` or `/api/v1/`.
    /// Plain `http://` is accepted only for this machine (`localhost`, `127.0.0.1`, `::1`).
    pub fn new(base_url: &str) -> Result<Self, Error> {
        let invalid = |reason| Error::InvalidUrl {
            url: base_url.to_owned(),
            reason,
        };
        let mut base =
            Url::parse(base_url.trim()).map_err(|_| invalid("it is not an absolute URL"))?;
        match base.scheme() {
            "https" => {}
            "http" if is_loopback(&base) => {}
            "http" => {
                return Err(invalid(
                    "plain http is only allowed for this machine (localhost, 127.0.0.1, ::1); use https",
                ));
            }
            _ => {
                return Err(invalid(
                    "only https (and http for this machine) are supported",
                ));
            }
        }
        if base.host_str().is_none() || !base.username().is_empty() || base.password().is_some() {
            return Err(invalid("it must name a host and carry no credentials"));
        }
        if base.query().is_some() || base.fragment().is_some() {
            return Err(invalid("it must not have a query string or fragment"));
        }
        if !matches!(base.path(), "" | "/" | "/api/v1" | "/api/v1/") {
            return Err(invalid("use the origin, or the origin followed by /api/v1"));
        }
        base.set_path("/api/v1/");
        let http = HttpClient::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("silicon-commit-client/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| {
                Error::Invalid(format!(
                    "Could not create the HTTP client: {}.",
                    root_cause(&e)
                ))
            })?;
        Ok(Self {
            http,
            base,
            credential: None,
            telemetry: true,
            source: "rust-client",
            mutation: None,
        })
    }

    /// Authenticates with a Silicon Accounts access token issued to Commit
    /// (`Authorization: Bearer …`).
    #[must_use]
    pub fn with_bearer(mut self, access_token: impl Into<String>) -> Self {
        self.credential = Some(Credential::Bearer(SecretString::from(access_token.into())));
        self
    }

    /// Authenticates as another app acting for an account, with a User verification proof
    /// (`Authorization: Proof sap_…`) issued for Commit with the scopes of the actions you
    /// call (each route's action id, e.g. `commit.todos.list`). Commit must allow your app
    /// for those scopes.
    pub fn with_proof(mut self, proof: impl Into<String>) -> Result<Self, Error> {
        let proof = proof.into();
        let proof = proof.trim();
        if proof.starts_with("sapr_") || !proof.starts_with("sap_") {
            return Err(Error::Invalid(
                "A User verification proof starts with `sap_` (a `sapr_…` value is the proof's refresh token, which is never sent to Commit).".into(),
            ));
        }
        self.credential = Some(Credential::Proof(SecretString::from(proof.to_owned())));
        Ok(self)
    }

    /// Opts this client out of diagnostic telemetry (`X-Commit-Telemetry: off`).
    #[must_use]
    pub fn with_telemetry(mut self, enabled: bool) -> Self {
        self.telemetry = enabled;
        self
    }

    /// Names the interface in request diagnostics (`cli`, `browser`, `daemon` or
    /// `rust-client`); no command arguments are sent.
    pub fn with_source(mut self, source: &'static str) -> Result<Self, Error> {
        if !matches!(source, "cli" | "browser" | "daemon" | "rust-client") {
            return Err(Error::Invalid(format!(
                "`{source}` is not a client source; use cli, browser, daemon or rust-client."
            )));
        }
        self.source = source;
        Ok(self)
    }

    /// Attaches retry/precondition inputs to writes made through this clone.
    #[must_use]
    pub fn with_mutation(mut self, mutation: Mutation) -> Self {
        self.mutation = Some(mutation);
        self
    }

    /// The `/api/v1/` base every request goes to.
    pub fn base_url(&self) -> &Url {
        &self.base
    }

    /// A fresh random idempotency key.
    pub fn new_idempotency_key() -> String {
        Mutation::new().key
    }
}

/// Public and account endpoints.
impl Client {
    /// `GET /healthz` (public): the API process is alive.
    pub async fn health(&self) -> Result<Value, Error> {
        self.execute(Method::GET, &["healthz"], None, &[], Scope::Root)
            .await
    }

    /// `GET /readyz` (public): the API can serve requests.
    pub async fn ready(&self) -> Result<Value, Error> {
        self.execute(Method::GET, &["readyz"], None, &[], Scope::Root)
            .await
    }

    /// `GET /api/v1/version` (public): build metadata. Sends no credentials.
    pub async fn version(&self) -> Result<Value, Error> {
        self.execute(Method::GET, &["version"], None, &[], Scope::Public)
            .await
    }

    /// `GET /api/v1/contracts`: the live compatibility matrix.
    pub async fn contracts(&self) -> Result<Value, Error> {
        self.get(&["contracts"], &[]).await
    }

    /// `GET /api/v1/accounts` (public): how to sign in to Commit (app id, Silicon Accounts
    /// URLs, the scopes Commit honours). Sends no credentials.
    pub async fn accounts(&self) -> Result<Value, Error> {
        self.execute(Method::GET, &["accounts"], None, &[], Scope::Public)
            .await
    }

    /// `GET /api/v1/me`: the caller as Commit sees it (uuid, id, kind, name, photo, shared
    /// email, custodian or Silicons, and `via_app` for proofs).
    pub async fn me(&self) -> Result<Value, Error> {
        self.get(&["me"], &[]).await
    }

    /// `GET /api/v1/silicons/{silicon}/allowed-accounts`: the accounts outside a Silicon's
    /// circle it accepts todos and project invitations from (the Silicon or its custodian).
    pub async fn silicon_allowlist(&self, silicon: &str) -> Result<Value, Error> {
        self.get(&["silicons", silicon, "allowed-accounts"], &[])
            .await
    }

    /// `PUT /api/v1/silicons/{silicon}/allowed-accounts/{account}`: lets `account` reach
    /// the Silicon.
    pub async fn allow_account(&self, silicon: &str, account: &str) -> Result<Value, Error> {
        self.execute(
            Method::PUT,
            &["silicons", silicon, "allowed-accounts", account],
            None,
            &[],
            Scope::Api,
        )
        .await
    }

    /// `DELETE /api/v1/silicons/{silicon}/allowed-accounts/{account}`.
    pub async fn disallow_account(&self, silicon: &str, account: &str) -> Result<Value, Error> {
        self.execute(
            Method::DELETE,
            &["silicons", silicon, "allowed-accounts", account],
            None,
            &[],
            Scope::Api,
        )
        .await
    }

    /// `GET /api/v1/email-settings`: the caller's email delivery preferences.
    pub async fn email_settings(&self) -> Result<Value, Error> {
        self.get(&["email-settings"], &[]).await
    }

    /// `PUT /api/v1/email-settings`: replaces the address and the subscribed events.
    pub async fn set_email_settings(&self, body: &Value) -> Result<Value, Error> {
        self.write(Method::PUT, &["email-settings"], body).await
    }

    /// `POST /api/v1/reports`: a bug report for Commit's maintainers (keep the mutation
    /// across retries).
    pub async fn report(&self, body: &Value) -> Result<Value, Error> {
        self.write(Method::POST, &["reports"], body).await
    }
}

/// Todos, notes and notification rules.
impl Client {
    /// `GET /api/v1/todos` (`view`, `status`, `assigned_to`, `assigned_by`, `created_from`,
    /// `created_to`, `limit`, `cursor`).
    pub async fn list_todos(&self, query: &[(&str, &str)]) -> Result<Value, Error> {
        self.get(&["todos"], query).await
    }
    /// `GET /api/v1/todos/{todo_id}`.
    pub async fn get_todo(&self, todo_id: &str) -> Result<Value, Error> {
        self.get(&["todos", todo_id], &[]).await
    }
    /// `POST /api/v1/todos`.
    pub async fn create_todo<T: Serialize + ?Sized>(&self, body: &T) -> Result<Value, Error> {
        self.write(Method::POST, &["todos"], body).await
    }
    /// `PATCH /api/v1/todos/{todo_id}`.
    pub async fn update_todo<T: Serialize + ?Sized>(
        &self,
        todo_id: &str,
        body: &T,
    ) -> Result<Value, Error> {
        self.write(Method::PATCH, &["todos", todo_id], body).await
    }
    /// `DELETE /api/v1/todos/{todo_id}`.
    pub async fn delete_todo(&self, todo_id: &str) -> Result<Value, Error> {
        self.execute(Method::DELETE, &["todos", todo_id], None, &[], Scope::Api)
            .await
    }
    /// `GET /api/v1/todos/{todo_id}/notes`.
    pub async fn list_notes(&self, todo_id: &str, query: &[(&str, &str)]) -> Result<Value, Error> {
        self.get(&["todos", todo_id, "notes"], query).await
    }
    /// `POST /api/v1/todos/{todo_id}/notes`.
    pub async fn add_note<T: Serialize + ?Sized>(
        &self,
        todo_id: &str,
        body: &T,
    ) -> Result<Value, Error> {
        self.write(Method::POST, &["todos", todo_id, "notes"], body)
            .await
    }
    /// `GET /api/v1/todos/{todo_id}/notification-subscription`.
    pub async fn todo_subscription(&self, todo_id: &str) -> Result<Value, Error> {
        self.get(&["todos", todo_id, "notification-subscription"], &[])
            .await
    }
    /// `PUT /api/v1/todos/{todo_id}/notification-subscription`.
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
    /// `GET /api/v1/notification-settings`: the caller's Silicon webhook settings.
    pub async fn notification_settings(&self) -> Result<Value, Error> {
        self.get(&["notification-settings"], &[]).await
    }
    /// `GET /api/v1/notification-settings?silicon=si:…`: a Silicon's settings, read by the
    /// Silicon or its custodian.
    pub async fn notification_settings_of(&self, silicon: &str) -> Result<Value, Error> {
        self.get(&["notification-settings"], &[("silicon", silicon)])
            .await
    }
    /// `PUT /api/v1/notification-settings`.
    pub async fn update_notification_settings<T: Serialize + ?Sized>(
        &self,
        body: &T,
    ) -> Result<Value, Error> {
        self.write(Method::PUT, &["notification-settings"], body)
            .await
    }
    /// `PUT /api/v1/notification-settings?silicon=si:…`: replaces a Silicon's settings as
    /// the Silicon or its custodian.
    pub async fn update_notification_settings_of<T: Serialize + ?Sized>(
        &self,
        silicon: &str,
        body: &T,
    ) -> Result<Value, Error> {
        let body = serde_json::to_vec(body).map_err(Error::Decode)?;
        self.execute(
            Method::PUT,
            &["notification-settings"],
            Some(body),
            &[("silicon", silicon)],
            Scope::Api,
        )
        .await
    }
}

/// Projects, diaries, tasks, entries and versions.
impl Client {
    /// `GET /api/v1/projects` (`status`, `silicon_id`, `limit`, `cursor`).
    pub async fn list_projects(&self, query: &[(&str, &str)]) -> Result<Value, Error> {
        self.get(&["projects"], query).await
    }
    /// `GET /api/v1/projects/{project_id}` (id or UID).
    pub async fn get_project(&self, project_id: &str) -> Result<Value, Error> {
        self.get(&["projects", project_id], &[]).await
    }
    /// `POST /api/v1/projects`.
    pub async fn create_project<T: Serialize + ?Sized>(&self, body: &T) -> Result<Value, Error> {
        self.write(Method::POST, &["projects"], body).await
    }
    /// `PATCH /api/v1/projects/{project_id}`.
    pub async fn update_project<T: Serialize + ?Sized>(
        &self,
        project_id: &str,
        body: &T,
    ) -> Result<Value, Error> {
        self.write(Method::PATCH, &["projects", project_id], body)
            .await
    }
    /// `GET /api/v1/projects/{project_id}/diary`.
    pub async fn project_diary(&self, project_id: &str) -> Result<Value, Error> {
        self.get(&["projects", project_id, "diary"], &[]).await
    }
    /// `PUT /api/v1/projects/{project_id}/diary` (send `If-Match`).
    pub async fn replace_project_diary<T: Serialize + ?Sized>(
        &self,
        project_id: &str,
        body: &T,
    ) -> Result<Value, Error> {
        self.write(Method::PUT, &["projects", project_id, "diary"], body)
            .await
    }
    /// `GET /api/v1/projects/{project_id}/tasks`.
    pub async fn project_tasks(
        &self,
        project_id: &str,
        query: &[(&str, &str)],
    ) -> Result<Value, Error> {
        self.get(&["projects", project_id, "tasks"], query).await
    }
    /// `GET /api/v1/projects/{project_id}/entries`: blockers, updates and completion.
    pub async fn project_entries(
        &self,
        project_id: &str,
        query: &[(&str, &str)],
    ) -> Result<Value, Error> {
        self.get(&["projects", project_id, "entries"], query).await
    }
    /// `POST /api/v1/projects/{project_id}/tasks`.
    pub async fn create_project_task<T: Serialize + ?Sized>(
        &self,
        project_id: &str,
        body: &T,
    ) -> Result<Value, Error> {
        self.write(Method::POST, &["projects", project_id, "tasks"], body)
            .await
    }
    /// `PATCH /api/v1/projects/{project_id}/tasks/{task_id}`.
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
    /// `POST /api/v1/projects/{project}/tasks/{task}/claim`: takes an unassigned task atomically.
    pub async fn claim_project_task(&self, project: &str, task: &str) -> Result<Value, Error> {
        self.write(
            Method::POST,
            &["projects", project, "tasks", task, "claim"],
            &serde_json::json!({}),
        )
        .await
    }
    /// `DELETE /api/v1/projects/{project}/tasks/{task}`: removes a task subtree and its linked todos.
    pub async fn delete_project_task(&self, project: &str, task: &str) -> Result<Value, Error> {
        self.execute(
            Method::DELETE,
            &["projects", project, "tasks", task],
            None,
            &[],
            Scope::Api,
        )
        .await
    }
    /// `POST /api/v1/projects/{project_id}/blockers`.
    pub async fn create_project_blocker<T: Serialize + ?Sized>(
        &self,
        project_id: &str,
        body: &T,
    ) -> Result<Value, Error> {
        self.write(Method::POST, &["projects", project_id, "blockers"], body)
            .await
    }
    /// `POST /api/v1/projects/{project_id}/updates`.
    pub async fn create_project_update<T: Serialize + ?Sized>(
        &self,
        project_id: &str,
        body: &T,
    ) -> Result<Value, Error> {
        self.write(Method::POST, &["projects", project_id, "updates"], body)
            .await
    }
    /// `POST /api/v1/projects/{project_id}/completion`.
    pub async fn complete_project<T: Serialize + ?Sized>(
        &self,
        project_id: &str,
        body: &T,
    ) -> Result<Value, Error> {
        self.write(Method::POST, &["projects", project_id, "completion"], body)
            .await
    }
    /// `GET /api/v1/projects/{project}/versions` (`before`, `limit`): newest first.
    pub async fn project_versions(
        &self,
        project: &str,
        query: &[(&str, &str)],
    ) -> Result<Value, Error> {
        self.get(&["projects", project, "versions"], query).await
    }
    /// `GET /api/v1/projects/{project}/versions/{version}`: one retained snapshot.
    pub async fn project_version(&self, project: &str, version: i64) -> Result<Value, Error> {
        self.get(
            &["projects", project, "versions", &version.to_string()],
            &[],
        )
        .await
    }
}

/// Where a path lives and which credentials it gets.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    /// `/api/v1/…` with the configured credential.
    Api,
    /// `/api/v1/…` without credentials.
    Public,
    /// `/…` at the origin (health checks), without credentials.
    Root,
}

impl Client {
    async fn get(&self, path: &[&str], query: &[(&str, &str)]) -> Result<Value, Error> {
        self.execute(Method::GET, path, None, query, Scope::Api)
            .await
    }

    async fn write<T: Serialize + ?Sized>(
        &self,
        method: Method,
        path: &[&str],
        body: &T,
    ) -> Result<Value, Error> {
        let body = serde_json::to_vec(body).map_err(Error::Decode)?;
        self.execute(method, path, Some(body), &[], Scope::Api)
            .await
    }

    fn url(&self, path: &[&str], scope: Scope) -> Result<Url, Error> {
        if path
            .iter()
            .any(|p| p.is_empty() || matches!(*p, "." | ".."))
        {
            return Err(Error::Invalid(
                "Resource identifiers cannot be empty, `.` or `..`.".into(),
            ));
        }
        let mut url = self.base.clone();
        {
            let mut segments = url.path_segments_mut().map_err(|()| Error::InvalidUrl {
                url: self.base.to_string(),
                reason: "it cannot carry a path",
            })?;
            if scope == Scope::Root {
                segments.clear();
            } else {
                segments.pop_if_empty();
            }
            segments.extend(path);
        }
        Ok(url)
    }

    fn transport(&self, error: reqwest::Error) -> Error {
        let error = error.without_url();
        Error::Transport {
            base: self.base.origin().ascii_serialization(),
            cause: root_cause(&error),
            source: error,
        }
    }

    async fn execute(
        &self,
        method: Method,
        path: &[&str],
        body: Option<Vec<u8>>,
        query: &[(&str, &str)],
        scope: Scope,
    ) -> Result<Value, Error> {
        let is_mutation = !matches!(method, Method::GET | Method::HEAD | Method::OPTIONS);
        let mut request = self
            .http
            .request(method, self.url(path, scope)?)
            .query(query)
            .header("x-commit-supported-versions", CONTRACT_VERSION.to_string())
            .header("x-commit-client", self.source)
            .header(
                "x-commit-telemetry",
                if self.telemetry { "on" } else { "off" },
            );
        if scope == Scope::Api {
            match &self.credential {
                Some(Credential::Bearer(token)) => {
                    request = request.bearer_auth(token.expose_secret())
                }
                Some(Credential::Proof(proof)) => {
                    request =
                        request.header("authorization", format!("Proof {}", proof.expose_secret()));
                }
                None => {}
            }
        }
        if is_mutation {
            let mutation = self.mutation.clone().unwrap_or_default();
            request = request.header("idempotency-key", mutation.key);
            if let Some(version) = mutation.version {
                request = request.header("if-match", format!("\"{version}\""));
            }
        }
        if let Some(body) = body {
            request = request
                .header("content-type", "application/json")
                .body(body);
        }
        let mut response = request.send().await.map_err(|e| self.transport(e))?;
        if let Some(served) = response.headers().get("x-commit-api-version")
            && served.as_bytes() != CONTRACT_VERSION.to_string().as_bytes()
        {
            return Err(Error::UnsupportedContract {
                served: String::from_utf8_lossy(served.as_bytes()).into_owned(),
            });
        }
        let status = response.status();
        let headers = response.headers().clone();
        if response
            .content_length()
            .is_some_and(|n| n > RESPONSE_LIMIT as u64)
        {
            return Err(Error::ResponseTooLarge);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|e| self.transport(e))? {
            if bytes.len() + chunk.len() > RESPONSE_LIMIT {
                return Err(Error::ResponseTooLarge);
            }
            bytes.extend_from_slice(&chunk);
        }
        if !status.is_success() {
            return Err(ApiError::from_response(status, &headers, &bytes).into());
        }
        if bytes.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&bytes).map_err(Error::Decode)
    }
}
