//! HTTP routing and process lifecycle.

use std::{future::IntoFuture as _, sync::Arc, time::Duration};

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Request, State},
    http::{
        HeaderMap, HeaderName, HeaderValue, Method, StatusCode,
        header::{self, CONTENT_TYPE},
    },
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, patch, post, put},
};
use secrecy::SecretString;
use serde::Serialize;
use sqlx::PgPool;
use thiserror::Error;
use tokio::{net::TcpListener, sync::Semaphore};
use tower_http::{
    catch_panic::CatchPanicLayer, cors::CorsLayer,
    sensitive_headers::SetSensitiveRequestHeadersLayer, trace::TraceLayer,
};
use url::Url;
use uuid::Uuid;

use crate::{
    application::{
        accounts::{AccountService, authentication_error},
        idempotency::MutationResponse,
        notifications::NotificationSettingsService,
        ports::{IdentityProvider, VerifiedActor},
        projects::ProjectService,
        todos::TodoService,
    },
    config::{RuntimeProfile, ServerSettings, Settings},
    error::AppError,
    infrastructure::{
        clients::{ClientBuildError, accounts::AccountsIdentity},
        postgres,
    },
    request_context, shutdown,
};

mod accounts;
pub mod auth;
mod contracts;
mod email;
pub mod extract;
pub mod notifications;
pub mod projects;
mod telemetry;
pub mod todos;
mod webhooks;

const REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-request-id");
const IDEMPOTENCY_REPLAYED_HEADER: HeaderName = HeaderName::from_static("idempotency-replayed");
const MAX_REQUEST_ID_BYTES: usize = 128;

/// Immutable dependencies shared by all request handlers.
#[derive(Clone)]
pub struct AppState {
    pub(crate) pool: PgPool,
    contract_store: bool,
    pub(crate) identity: Arc<dyn IdentityProvider>,
    pub(crate) todos: Arc<TodoService>,
    pub(crate) projects: Arc<ProjectService>,
    pub(crate) notifications: Arc<NotificationSettingsService>,
    pub(crate) accounts: Arc<AccountService>,
    pub(crate) webhook_secret: Option<SecretString>,
    pub(crate) app_id: String,
    pub(crate) accounts_url: String,
    public_base_url: Url,
}

impl AppState {
    /// Creates an application state from already-composed dependencies.
    ///
    /// This constructor keeps transport tests independent from live Silicon
    /// Accounts and PostgreSQL services.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pool: PgPool,
        identity: Arc<dyn IdentityProvider>,
        todos: Arc<TodoService>,
        projects: Arc<ProjectService>,
        notifications: Arc<NotificationSettingsService>,
        accounts: Arc<AccountService>,
        public_base_url: Url,
    ) -> Self {
        Self {
            pool,
            contract_store: false,
            identity,
            todos,
            projects,
            notifications,
            accounts,
            webhook_secret: None,
            app_id: crate::config::DEFAULT_APP_ID.to_owned(),
            accounts_url: crate::config::DEFAULT_ACCOUNTS_URL.to_owned(),
            public_base_url: normalized_base_url(public_base_url),
        }
    }

    /// Sets the secret Silicon Accounts signs Commit's webhook deliveries with.
    #[must_use]
    pub fn with_webhook_secret(mut self, secret: Option<SecretString>) -> Self {
        self.webhook_secret = secret;
        self
    }

    /// Sets the app id and the public Silicon Accounts URL clients sign in with.
    #[must_use]
    pub fn with_accounts(
        mut self,
        app_id: impl Into<String>,
        accounts_url: impl Into<String>,
    ) -> Self {
        self.app_id = app_id.into();
        self.accounts_url = accounts_url.into();
        self
    }

    /// Builds all API-facing services from validated settings and a database pool.
    ///
    /// # Errors
    ///
    /// Returns a redacted construction error when an external client cannot
    /// be built safely.
    pub fn from_settings(settings: &Settings, pool: PgPool) -> Result<Self, ApiBuildError> {
        if settings.runtime_profile != RuntimeProfile::Api {
            return Err(ApiBuildError::WrongSettingsProfile);
        }
        let integrations = &settings.integrations;
        let identity: Arc<dyn IdentityProvider> = Arc::new(AccountsIdentity::new(
            &integrations.accounts,
            pool.clone(),
            integrations.connect_timeout,
            integrations.request_timeout,
        )?);
        let limits = settings.limits.domain_limits();
        let idempotency_ttl = settings.limits.idempotency_ttl;
        let audit_retention = settings.worker.audit_retention;
        let tombstone_retention = settings.worker.todo_tombstone_retention;
        let todos = Arc::new(TodoService::new(
            pool.clone(),
            Arc::clone(&identity),
            limits,
            idempotency_ttl,
            audit_retention,
            tombstone_retention,
        ));
        let projects = Arc::new(ProjectService::new(
            pool.clone(),
            Arc::clone(&identity),
            limits,
            idempotency_ttl,
            audit_retention,
        ));
        let notifications = Arc::new(NotificationSettingsService::new(
            pool.clone(),
            Arc::clone(&identity),
            audit_retention,
        ));
        let accounts = Arc::new(AccountService::new(
            pool.clone(),
            Arc::clone(&identity),
            tombstone_retention,
            audit_retention,
        ));

        let mut state = Self::new(
            pool,
            identity,
            todos,
            projects,
            notifications,
            accounts,
            settings.server.public_base_url.clone(),
        )
        .with_webhook_secret(integrations.accounts.webhook_secret.clone())
        .with_accounts(
            integrations.accounts.app_id.clone(),
            integrations.accounts.issuer.clone(),
        );
        state.contract_store = true;
        Ok(state)
    }

    /// Authenticates the request for one action (its proof scope).
    pub(crate) async fn authenticate(
        &self,
        headers: &HeaderMap,
        scope: &'static str,
        resource: Option<String>,
    ) -> Result<VerifiedActor, AppError> {
        self.authenticate_with(headers, scope, resource, false)
            .await
    }

    /// Authenticates a request that changes who can see or reach something:
    /// bearer tokens are also checked online, so a revoked sign-in is refused at once.
    pub(crate) async fn authenticate_sensitive(
        &self,
        headers: &HeaderMap,
        scope: &'static str,
        resource: Option<String>,
    ) -> Result<VerifiedActor, AppError> {
        self.authenticate_with(headers, scope, resource, true).await
    }

    async fn authenticate_with(
        &self,
        headers: &HeaderMap,
        scope: &'static str,
        resource: Option<String>,
        sensitive: bool,
    ) -> Result<VerifiedActor, AppError> {
        let request = auth::request(headers, scope, sensitive)?;
        self.identity.authenticate(&request).await.map_err(|error| {
            tracing::debug!(
                scope,
                resource = resource.as_deref(),
                ?error,
                "authentication refused"
            );
            authentication_error(error)
        })
    }
}

/// Redacted API construction failure.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ApiBuildError {
    /// Settings loaded for a different process were supplied to the API.
    #[error("settings were not loaded for the API process")]
    WrongSettingsProfile,
    /// An external platform HTTP adapter could not be built safely.
    #[error("failed to construct an external service client")]
    Client(#[from] ClientBuildError),
    /// A configured browser origin is not representable as an HTTP header.
    #[error("a configured browser origin is invalid")]
    CorsOrigin,
}

/// Runs the API listener until shutdown and bounds connection draining.
///
/// # Errors
///
/// Returns an error when dependency composition, PostgreSQL connection, socket
/// binding, serving, or graceful shutdown fails.
pub async fn serve(settings: Settings) -> anyhow::Result<()> {
    for name in crate::config::retired_variables_still_set() {
        tracing::warn!(
            variable = name,
            "environment variable is set but no longer read; remove it"
        );
    }
    if settings.integrations.accounts.webhook_secret.is_none() {
        tracing::warn!(
            "COMMIT_ACCOUNTS_WEBHOOK_SECRET is not set: POST /webhook/ answers 503 until it is"
        );
    }
    if settings.integrations.accounts.proof_issuers.is_empty() {
        tracing::info!("COMMIT_PROOF_ISSUERS is empty: no app may act for an account with a proof");
    }
    let pool = postgres::connect(&settings.database, "silicon-commit-api").await?;
    let state = AppState::from_settings(&settings, pool)?;
    let app = router(state, &settings.server)?;
    let listener = TcpListener::bind(settings.server.bind_addr).await?;
    let local_addr = listener.local_addr()?;
    tracing::info!(%local_addr, "Silicon Commit API listening");

    let (shutdown_sender, shutdown_receiver) = tokio::sync::oneshot::channel::<()>();
    let server = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = shutdown_receiver.await;
        })
        .into_future();
    tokio::pin!(server);

    tokio::select! {
        result = &mut server => result?,
        () = shutdown::signal() => {
            tracing::info!("shutdown requested; draining HTTP connections");
            let _ = shutdown_sender.send(());
            match tokio::time::timeout(settings.server.shutdown_timeout, &mut server).await {
                Ok(result) => result?,
                Err(_) => return Err(anyhow::anyhow!("HTTP graceful shutdown deadline exceeded")),
            }
        }
    }
    Ok(())
}

/// Builds the public router with production middleware and injected state.
///
/// # Errors
///
/// Returns an error if a configured CORS origin cannot be represented as an
/// HTTP header value.
pub fn router(state: AppState, settings: &ServerSettings) -> Result<Router, ApiBuildError> {
    let api = Router::new()
        .route("/contracts", get(contracts::describe))
        .route("/accounts", get(accounts::metadata))
        .route("/me", get(accounts::me))
        .route(
            "/silicons/{silicon}/allowed-accounts",
            get(accounts::allowlist),
        )
        .route(
            "/silicons/{silicon}/allowed-accounts/{account}",
            put(accounts::allow).delete(accounts::disallow),
        )
        .route("/email-settings", get(email::get).put(email::put))
        .route("/reports", post(email::report))
        .route("/version", get(version))
        .route(
            "/notification-settings",
            get(notifications::get_settings).put(notifications::replace_settings),
        )
        .route("/todos", get(todos::list).post(todos::create))
        .route(
            "/todos/{todo_id}",
            get(todos::get).patch(todos::update).delete(todos::delete),
        )
        .route(
            "/todos/{todo_id}/notes",
            get(todos::list_notes).post(todos::add_note),
        )
        .route(
            "/todos/{todo_id}/notification-subscription",
            get(notifications::get_todo_subscription).put(notifications::replace_todo_subscription),
        )
        .route("/projects", get(projects::list).post(projects::create))
        .route(
            "/projects/{project_id}",
            get(projects::get).patch(projects::update),
        )
        .route(
            "/projects/{project_id}/diary",
            get(projects::get_diary).put(projects::replace_diary),
        )
        .route(
            "/projects/{project_id}/entries",
            get(projects::list_entries),
        )
        .route(
            "/projects/{project_id}/tasks",
            get(projects::list_tasks).post(projects::create_task),
        )
        .route(
            "/projects/{project_id}/tasks/{task_id}/claim",
            post(projects::claim_task),
        )
        .route("/projects/{project_id}/versions", get(projects::versions))
        .route(
            "/projects/{project_id}/versions/{version}",
            get(projects::version),
        )
        .route(
            "/projects/{project_id}/tasks/{task_id}",
            patch(projects::update_task).delete(projects::delete_task),
        )
        .route(
            "/projects/{project_id}/blockers",
            post(projects::create_blocker),
        )
        .route(
            "/projects/{project_id}/updates",
            post(projects::create_update),
        )
        .route(
            "/projects/{project_id}/completion",
            post(projects::complete),
        )
        // Transition aliases of the IAM era (one release): each path performs exactly the
        // action of its canonical route and accepts the same Bearer or Proof credentials.
        .route("/obo/todos/list", get(todos::list))
        .route("/obo/todos/create", post(todos::create))
        .route("/obo/todos/{todo_id}/read", get(todos::get))
        .route("/obo/todos/{todo_id}/update", patch(todos::update))
        .route("/obo/todos/{todo_id}/delete", axum::routing::delete(todos::delete))
        .route("/obo/todos/{todo_id}/notes/list", get(todos::list_notes))
        .route("/obo/todos/{todo_id}/notes/create", post(todos::add_note))
        .route("/obo/notification-settings/read", get(notifications::get_settings))
        .route("/obo/notification-settings/update", put(notifications::replace_settings))
        .route("/obo/todos/{todo_id}/notification-subscription/read", get(notifications::get_todo_subscription))
        .route("/obo/todos/{todo_id}/notification-subscription/update", put(notifications::replace_todo_subscription))
        .route("/obo/projects/list", get(projects::list))
        .route("/obo/projects/create", post(projects::create))
        .route("/obo/projects/{project_id}/read", get(projects::get))
        .route("/obo/projects/{project_id}/update", patch(projects::update))
        .route("/obo/projects/{project_id}/diary/read", get(projects::get_diary))
        .route("/obo/projects/{project_id}/diary/update", put(projects::replace_diary))
        .route("/obo/projects/{project_id}/tasks/list", get(projects::list_tasks))
        .route("/obo/projects/{project_id}/tasks/create", post(projects::create_task))
        .route("/obo/projects/{project_id}/tasks/{task_id}/update", patch(projects::update_task))
        .route("/obo/projects/{project_id}/blockers/create", post(projects::create_blocker))
        .route("/obo/projects/{project_id}/updates/create", post(projects::create_update))
        .route("/obo/projects/{project_id}/completion/create", post(projects::complete))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            contracts::negotiate,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            telemetry::capture,
        ));

    let sensitive_headers = [
        header::AUTHORIZATION,
        HeaderName::from_static("idempotency-key"),
        HeaderName::from_static("x-accounts-signature"),
    ];
    let concurrency = Arc::new(Semaphore::new(settings.concurrency_limit));
    let product_api = Router::new()
        .nest("/api/v1", api)
        .route("/webhook/", post(webhooks::receive))
        .layer(DefaultBodyLimit::max(settings.max_body_bytes))
        .layer(middleware::from_fn_with_state(
            concurrency,
            concurrency_limit,
        ))
        .layer(middleware::from_fn_with_state(
            settings.request_timeout,
            request_timeout,
        ));
    let app = Router::new()
        .route("/healthz", get(health))
        .route("/readyz", get(readiness))
        .merge(product_api)
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .with_state(state)
        .layer(TraceLayer::new_for_http())
        .layer(SetSensitiveRequestHeadersLayer::new(sensitive_headers))
        .layer(CatchPanicLayer::custom(|_panic| {
            AppError::Internal(anyhow::anyhow!("request handler panicked")).into_response()
        }))
        .layer(middleware::from_fn(request_id))
        .layer(cors_layer(settings)?)
        .layer(middleware::from_fn(ensure_response_request_id))
        .layer(middleware::from_fn(no_store));
    Ok(app)
}

async fn health() -> Json<Health> {
    Json(Health { status: "ok" })
}

async fn readiness(State(state): State<AppState>) -> Response {
    if postgres::ready(&state.pool).await {
        Json(Health { status: "ready" }).into_response()
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(Health {
                status: "not_ready",
            }),
        )
            .into_response()
    }
}

async fn version() -> Json<Version> {
    Json(Version {
        service: "silicon-commit",
        api_version: "v1",
        contract: contracts::CURRENT,
        version: env!("CARGO_PKG_VERSION"),
        commit: option_env!("GIT_COMMIT_SHA").unwrap_or("unknown"),
    })
}

async fn not_found() -> AppError {
    AppError::NotFound
}

async fn method_not_allowed() -> AppError {
    AppError::MethodNotAllowed
}

#[derive(Debug, Serialize)]
struct Health {
    status: &'static str,
}

#[derive(Debug, Serialize)]
struct Version {
    service: &'static str,
    api_version: &'static str,
    contract: u16,
    version: &'static str,
    commit: &'static str,
}

async fn request_id(mut request: Request, next: Next) -> Response {
    let generated = Uuid::now_v7().hyphenated().to_string();
    let request_id = match inbound_request_id(request.headers()) {
        Ok(Some(request_id)) => request_id,
        Ok(None) => generated,
        Err(error) => {
            return request_context::scope(generated.clone(), async move {
                let mut response = error.into_response();
                insert_request_id(&mut response, &generated);
                response
            })
            .await;
        }
    };
    if let Ok(header_value) = HeaderValue::from_str(&request_id) {
        request
            .headers_mut()
            .insert(REQUEST_ID_HEADER, header_value);
    }
    request_context::scope(request_id.clone(), async move {
        let mut response = next.run(request).await;
        insert_request_id(&mut response, &request_id);
        response
    })
    .await
}

async fn no_store(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

async fn ensure_response_request_id(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    if !response.headers().contains_key(&REQUEST_ID_HEADER) {
        let generated = Uuid::now_v7().hyphenated().to_string();
        insert_request_id(&mut response, &generated);
    }
    response
}

async fn request_timeout(
    State(timeout): State<Duration>,
    request: Request,
    next: Next,
) -> Response {
    match tokio::time::timeout(timeout, next.run(request)).await {
        Ok(response) => response,
        Err(_) => AppError::Timeout.into_response(),
    }
}

async fn concurrency_limit(
    State(semaphore): State<Arc<Semaphore>>,
    request: Request,
    next: Next,
) -> Response {
    let Ok(_permit) = semaphore.acquire_owned().await else {
        return AppError::Internal(anyhow::anyhow!("request concurrency limiter closed"))
            .into_response();
    };
    next.run(request).await
}

fn inbound_request_id(headers: &HeaderMap) -> Result<Option<String>, AppError> {
    let mut values = headers.get_all(&REQUEST_ID_HEADER).iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(AppError::BadRequest {
            code: "invalid_request_id".into(),
        });
    }
    let value = value.to_str().map_err(|_| AppError::BadRequest {
        code: "invalid_request_id".into(),
    })?;
    if value.is_empty()
        || value.len() > MAX_REQUEST_ID_BYTES
        || !value.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err(AppError::BadRequest {
            code: "invalid_request_id".into(),
        });
    }
    Ok(Some(value.to_owned()))
}

fn insert_request_id(response: &mut Response, request_id: &str) {
    if let Ok(value) = HeaderValue::from_str(request_id) {
        response.headers_mut().insert(REQUEST_ID_HEADER, value);
    }
}

fn cors_layer(settings: &ServerSettings) -> Result<CorsLayer, ApiBuildError> {
    let origins = settings
        .cors_allowed_origins
        .iter()
        .map(|origin| {
            HeaderValue::from_str(&origin.origin().ascii_serialization())
                .map_err(|_| ApiBuildError::CorsOrigin)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(CorsLayer::new()
        .allow_origin(origins)
        .allow_credentials(true)
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PATCH,
            Method::PUT,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([
            header::AUTHORIZATION,
            CONTENT_TYPE,
            HeaderName::from_static("idempotency-key"),
            header::IF_MATCH,
            REQUEST_ID_HEADER,
            HeaderName::from_static("x-commit-api-version"),
            HeaderName::from_static("x-commit-supported-versions"),
            HeaderName::from_static("x-commit-client"),
            HeaderName::from_static("x-commit-telemetry"),
        ])
        .expose_headers([
            header::ETAG,
            header::LOCATION,
            IDEMPOTENCY_REPLAYED_HEADER,
            REQUEST_ID_HEADER,
            header::RETRY_AFTER,
            header::WWW_AUTHENTICATE,
        ]))
}

pub(crate) fn required_request_id() -> Result<String, AppError> {
    request_context::current_request_id().ok_or_else(|| {
        AppError::Internal(anyhow::anyhow!(
            "mutation handler executed without request context"
        ))
    })
}

pub(crate) fn mutation_response(
    state: &AppState,
    mutation: MutationResponse,
    location: Option<&str>,
) -> Result<Response, AppError> {
    let status = StatusCode::from_u16(mutation.status).map_err(|_| {
        AppError::Internal(anyhow::anyhow!(
            "stored mutation response contained an invalid HTTP status"
        ))
    })?;
    let mut response = (status, Json(mutation.body)).into_response();
    response.headers_mut().insert(
        IDEMPOTENCY_REPLAYED_HEADER,
        HeaderValue::from_static(if mutation.replayed { "true" } else { "false" }),
    );
    if let Some(location) = location {
        let location = state.public_base_url.join(location).map_err(|_| {
            AppError::Internal(anyhow::anyhow!(
                "resource location could not be joined to the public base URL"
            ))
        })?;
        let value = HeaderValue::from_str(location.as_str()).map_err(|_| {
            AppError::Internal(anyhow::anyhow!(
                "resource location could not be represented as an HTTP header"
            ))
        })?;
        response.headers_mut().insert(header::LOCATION, value);
    }
    Ok(response)
}

fn normalized_base_url(mut url: Url) -> Url {
    if !url.path().ends_with('/') {
        url.set_path(&format!("{}/", url.path()));
    }
    url
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use axum::{
        body::Body,
        http::{Request as HttpRequest, header},
    };
    use http_body_util::BodyExt as _;
    use sqlx::postgres::PgPoolOptions;
    use tokio::sync::Semaphore;
    use tower::ServiceExt as _;

    use crate::{
        api::auth::action,
        application::ports::{AuthenticationRequest, ProviderError, ResolvedAccount},
        config::ServerSettings,
        domain::{ActorId, ActorType, DomainLimits},
    };

    use super::*;

    #[derive(Debug, Default)]
    struct RecordingIdentity {
        calls: Mutex<Vec<(String, bool)>>,
    }

    impl RecordingIdentity {
        fn take_calls(&self) -> Vec<(String, bool)> {
            self.calls
                .lock()
                .map_or_else(|_| Vec::new(), |mut calls| std::mem::take(&mut *calls))
        }
    }

    #[async_trait]
    impl IdentityProvider for RecordingIdentity {
        async fn authenticate(
            &self,
            request: &AuthenticationRequest,
        ) -> Result<VerifiedActor, ProviderError> {
            let mut calls = self.calls.lock().map_err(|_| ProviderError::Unavailable)?;
            calls.push((request.scope.clone(), request.sensitive));
            Err(ProviderError::Forbidden)
        }

        async fn resolve_accounts(
            &self,
            _ids: &[ActorId],
            _required_type: Option<ActorType>,
        ) -> Result<Vec<ResolvedAccount>, ProviderError> {
            Err(ProviderError::Forbidden)
        }
    }

    #[derive(Debug)]
    struct BlockingIdentity {
        entered: Semaphore,
        release: Semaphore,
    }

    impl BlockingIdentity {
        fn new() -> Self {
            Self {
                entered: Semaphore::new(0),
                release: Semaphore::new(0),
            }
        }
    }

    #[async_trait]
    impl IdentityProvider for BlockingIdentity {
        async fn authenticate(
            &self,
            _request: &AuthenticationRequest,
        ) -> Result<VerifiedActor, ProviderError> {
            self.entered.add_permits(1);
            let _permit = self
                .release
                .acquire()
                .await
                .map_err(|_| ProviderError::Unavailable)?;
            Err(ProviderError::Forbidden)
        }

        async fn resolve_accounts(
            &self,
            _ids: &[ActorId],
            _required_type: Option<ActorType>,
        ) -> Result<Vec<ResolvedAccount>, ProviderError> {
            Err(ProviderError::Forbidden)
        }
    }

    struct Case {
        method: Method,
        uri: &'static str,
        alias: Option<&'static str>,
        scope: &'static str,
        body: Option<&'static str>,
        idempotent: bool,
        if_match: bool,
    }

    const TODO: &str = "018f268d-715a-7b72-8f0f-41f16f9af553";

    fn case(
        method: Method,
        uri: &'static str,
        alias: Option<&'static str>,
        scope: &'static str,
    ) -> Case {
        Case {
            method,
            uri,
            alias,
            scope,
            body: None,
            idempotent: false,
            if_match: false,
        }
    }

    fn write(
        method: Method,
        uri: &'static str,
        alias: Option<&'static str>,
        scope: &'static str,
        body: &'static str,
    ) -> Case {
        Case {
            method,
            uri,
            alias,
            scope,
            body: Some(body),
            idempotent: true,
            if_match: false,
        }
    }

    fn cases() -> Vec<Case> {
        vec![
            case(Method::GET, "/api/v1/me", None, action::ME_READ),
            case(
                Method::GET,
                "/api/v1/email-settings",
                None,
                action::EMAIL_SETTINGS_READ,
            ),
            Case {
                body: Some(r#"{"email":""}"#),
                ..case(
                    Method::PUT,
                    "/api/v1/email-settings",
                    None,
                    action::EMAIL_SETTINGS_UPDATE,
                )
            },
            write(
                Method::POST,
                "/api/v1/reports",
                None,
                action::REPORTS_CREATE,
                r#"{"message":"broken"}"#,
            ),
            case(
                Method::GET,
                "/api/v1/silicons/si:scout/allowed-accounts",
                None,
                action::ALLOWLIST_READ,
            ),
            case(
                Method::PUT,
                "/api/v1/silicons/si:scout/allowed-accounts/c:ada",
                None,
                action::ALLOWLIST_UPDATE,
            ),
            case(
                Method::DELETE,
                "/api/v1/silicons/si:scout/allowed-accounts/c:ada",
                None,
                action::ALLOWLIST_UPDATE,
            ),
            case(
                Method::GET,
                "/api/v1/todos",
                Some("/api/v1/obo/todos/list"),
                action::TODOS_LIST,
            ),
            write(
                Method::POST,
                "/api/v1/todos",
                Some("/api/v1/obo/todos/create"),
                action::TODOS_CREATE,
                r#"{"title":"work","assigned_to":"si:one"}"#,
            ),
            case(
                Method::GET,
                "/api/v1/todos/018f268d-715a-7b72-8f0f-41f16f9af553",
                Some("/api/v1/obo/todos/018f268d-715a-7b72-8f0f-41f16f9af553/read"),
                action::TODOS_READ,
            ),
            write(
                Method::PATCH,
                "/api/v1/todos/018f268d-715a-7b72-8f0f-41f16f9af553",
                Some("/api/v1/obo/todos/018f268d-715a-7b72-8f0f-41f16f9af553/update"),
                action::TODOS_UPDATE,
                r#"{"title":"changed"}"#,
            ),
            case(
                Method::DELETE,
                "/api/v1/todos/018f268d-715a-7b72-8f0f-41f16f9af553",
                Some("/api/v1/obo/todos/018f268d-715a-7b72-8f0f-41f16f9af553/delete"),
                action::TODOS_DELETE,
            ),
            case(
                Method::GET,
                "/api/v1/todos/018f268d-715a-7b72-8f0f-41f16f9af553/notes",
                Some("/api/v1/obo/todos/018f268d-715a-7b72-8f0f-41f16f9af553/notes/list"),
                action::TODO_NOTES_LIST,
            ),
            write(
                Method::POST,
                "/api/v1/todos/018f268d-715a-7b72-8f0f-41f16f9af553/notes",
                Some("/api/v1/obo/todos/018f268d-715a-7b72-8f0f-41f16f9af553/notes/create"),
                action::TODO_NOTES_CREATE,
                r#"{"body":"note"}"#,
            ),
            case(
                Method::GET,
                "/api/v1/notification-settings",
                Some("/api/v1/obo/notification-settings/read"),
                action::NOTIFICATION_SETTINGS_READ,
            ),
            Case {
                body: Some(r#"{"webhook_url":null,"todo_list_subscription":null}"#),
                if_match: true,
                ..case(
                    Method::PUT,
                    "/api/v1/notification-settings",
                    Some("/api/v1/obo/notification-settings/update"),
                    action::NOTIFICATION_SETTINGS_UPDATE,
                )
            },
            case(
                Method::GET,
                "/api/v1/todos/018f268d-715a-7b72-8f0f-41f16f9af553/notification-subscription",
                Some(
                    "/api/v1/obo/todos/018f268d-715a-7b72-8f0f-41f16f9af553/notification-subscription/read",
                ),
                action::TODO_SUBSCRIPTION_READ,
            ),
            Case {
                body: Some(r#"{"subscription":null}"#),
                if_match: true,
                ..case(
                    Method::PUT,
                    "/api/v1/todos/018f268d-715a-7b72-8f0f-41f16f9af553/notification-subscription",
                    Some(
                        "/api/v1/obo/todos/018f268d-715a-7b72-8f0f-41f16f9af553/notification-subscription/update",
                    ),
                    action::TODO_SUBSCRIPTION_UPDATE,
                )
            },
            case(
                Method::GET,
                "/api/v1/projects",
                Some("/api/v1/obo/projects/list"),
                action::PROJECTS_LIST,
            ),
            write(
                Method::POST,
                "/api/v1/projects",
                Some("/api/v1/obo/projects/create"),
                action::PROJECTS_CREATE,
                r#"{"name":"project","silicon_ids":["si:one"]}"#,
            ),
            case(
                Method::GET,
                "/api/v1/projects/018f268d-715a-7b72-8f0f-41f16f9af554",
                Some("/api/v1/obo/projects/018f268d-715a-7b72-8f0f-41f16f9af554/read"),
                action::PROJECTS_READ,
            ),
            write(
                Method::PATCH,
                "/api/v1/projects/018f268d-715a-7b72-8f0f-41f16f9af554",
                Some("/api/v1/obo/projects/018f268d-715a-7b72-8f0f-41f16f9af554/update"),
                action::PROJECTS_UPDATE,
                r#"{"name":"changed"}"#,
            ),
            case(
                Method::GET,
                "/api/v1/projects/018f268d-715a-7b72-8f0f-41f16f9af554/diary",
                Some("/api/v1/obo/projects/018f268d-715a-7b72-8f0f-41f16f9af554/diary/read"),
                action::DIARY_READ,
            ),
            Case {
                body: Some(r#"{"markdown":"entry"}"#),
                if_match: true,
                ..case(
                    Method::PUT,
                    "/api/v1/projects/018f268d-715a-7b72-8f0f-41f16f9af554/diary",
                    Some("/api/v1/obo/projects/018f268d-715a-7b72-8f0f-41f16f9af554/diary/update"),
                    action::DIARY_UPDATE,
                )
            },
            case(
                Method::GET,
                "/api/v1/projects/018f268d-715a-7b72-8f0f-41f16f9af554/entries",
                None,
                action::PROJECTS_READ,
            ),
            case(
                Method::GET,
                "/api/v1/projects/018f268d-715a-7b72-8f0f-41f16f9af554/versions",
                None,
                action::PROJECTS_READ,
            ),
            case(
                Method::GET,
                "/api/v1/projects/018f268d-715a-7b72-8f0f-41f16f9af554/versions/3",
                None,
                action::PROJECTS_READ,
            ),
            case(
                Method::GET,
                "/api/v1/projects/018f268d-715a-7b72-8f0f-41f16f9af554/tasks",
                Some("/api/v1/obo/projects/018f268d-715a-7b72-8f0f-41f16f9af554/tasks/list"),
                action::PROJECT_TASKS_LIST,
            ),
            write(
                Method::POST,
                "/api/v1/projects/018f268d-715a-7b72-8f0f-41f16f9af554/tasks",
                Some("/api/v1/obo/projects/018f268d-715a-7b72-8f0f-41f16f9af554/tasks/create"),
                action::PROJECT_TASKS_CREATE,
                r#"{"title":"task"}"#,
            ),
            Case {
                body: Some(r#"{"title":"changed"}"#),
                ..case(
                    Method::PATCH,
                    "/api/v1/projects/018f268d-715a-7b72-8f0f-41f16f9af554/tasks/018f268d-715a-7b72-8f0f-41f16f9af555",
                    Some(
                        "/api/v1/obo/projects/018f268d-715a-7b72-8f0f-41f16f9af554/tasks/018f268d-715a-7b72-8f0f-41f16f9af555/update",
                    ),
                    action::PROJECT_TASKS_UPDATE,
                )
            },
            Case {
                idempotent: true,
                ..case(
                    Method::POST,
                    "/api/v1/projects/018f268d-715a-7b72-8f0f-41f16f9af554/tasks/018f268d-715a-7b72-8f0f-41f16f9af555/claim",
                    None,
                    action::PROJECT_TASKS_CLAIM,
                )
            },
            case(
                Method::DELETE,
                "/api/v1/projects/018f268d-715a-7b72-8f0f-41f16f9af554/tasks/018f268d-715a-7b72-8f0f-41f16f9af555",
                None,
                action::PROJECT_TASKS_DELETE,
            ),
            write(
                Method::POST,
                "/api/v1/projects/018f268d-715a-7b72-8f0f-41f16f9af554/blockers",
                Some("/api/v1/obo/projects/018f268d-715a-7b72-8f0f-41f16f9af554/blockers/create"),
                action::PROJECT_BLOCKERS_CREATE,
                r#"{"title":"blocked","description":"dependency"}"#,
            ),
            write(
                Method::POST,
                "/api/v1/projects/018f268d-715a-7b72-8f0f-41f16f9af554/updates",
                Some("/api/v1/obo/projects/018f268d-715a-7b72-8f0f-41f16f9af554/updates/create"),
                action::PROJECT_UPDATES_CREATE,
                r#"{"title":"milestone","description":"shipped"}"#,
            ),
            write(
                Method::POST,
                "/api/v1/projects/018f268d-715a-7b72-8f0f-41f16f9af554/completion",
                Some("/api/v1/obo/projects/018f268d-715a-7b72-8f0f-41f16f9af554/completion/create"),
                action::PROJECT_COMPLETION_CREATE,
                r#"{"title":"complete","description":"done"}"#,
            ),
        ]
    }

    #[tokio::test]
    async fn every_product_route_requires_its_exact_scope_with_bearer_or_proof()
    -> anyhow::Result<()> {
        let identity = Arc::new(RecordingIdentity::default());
        let state = test_state(identity.clone())?;
        let app = router(state, &test_server_settings(1_048_576)?)?;
        let mut covered = std::collections::HashSet::new();
        for case in cases() {
            covered.insert(case.scope);
            let uris = std::iter::once(case.uri).chain(case.alias);
            for uri in uris {
                for credential in ["Bearer eyJ.test.token", "Proof sap_test"] {
                    let mut builder = HttpRequest::builder()
                        .method(case.method.clone())
                        .uri(uri)
                        .header(CONTENT_TYPE, "application/json")
                        .header(header::AUTHORIZATION, credential);
                    if case.idempotent {
                        builder = builder.header("idempotency-key", "route-test-key");
                    }
                    if case.if_match {
                        builder = builder.header(header::IF_MATCH, "\"1\"");
                    }
                    let response = app
                        .clone()
                        .oneshot(builder.body(Body::from(case.body.unwrap_or_default()))?)
                        .await?;
                    assert_eq!(
                        response.status(),
                        StatusCode::FORBIDDEN,
                        "{} {uri} did not reach authentication",
                        case.method
                    );
                    let calls = identity.take_calls();
                    assert_eq!(calls.len(), 1, "{} {uri}", case.method);
                    assert_eq!(
                        calls[0].0, case.scope,
                        "{} {uri} used the wrong scope",
                        case.method
                    );
                }
            }
            if let Some(alias) = case.alias {
                let wrong = if case.method == Method::GET {
                    Method::POST
                } else {
                    Method::GET
                };
                let response = app
                    .clone()
                    .oneshot(
                        HttpRequest::builder()
                            .method(wrong)
                            .uri(alias)
                            .body(Body::empty())?,
                    )
                    .await?;
                assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
                assert!(identity.take_calls().is_empty());
            }
        }
        for scope in action::ALL {
            assert!(covered.contains(scope), "no route test covers {scope}");
        }
        Ok(())
    }

    #[tokio::test]
    async fn visibility_and_membership_changes_are_checked_online() -> anyhow::Result<()> {
        let identity = Arc::new(RecordingIdentity::default());
        let app = router(
            test_state(identity.clone())?,
            &test_server_settings(1_048_576)?,
        )?;
        for (body, sensitive) in [
            (r#"{"name":"renamed"}"#, false),
            (r#"{"private":true}"#, true),
            (r#"{"silicon_ids":["si:one"]}"#, true),
            (r#"{"carbon_ids":["c:ada"]}"#, true),
        ] {
            let response = app
                .clone()
                .oneshot(
                    HttpRequest::builder()
                        .method(Method::PATCH)
                        .uri(format!("/api/v1/projects/{TODO}"))
                        .header(CONTENT_TYPE, "application/json")
                        .header(header::AUTHORIZATION, "Bearer eyJ.test.token")
                        .header("idempotency-key", "sensitive-key")
                        .body(Body::from(body))?,
                )
                .await?;
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
            assert_eq!(
                identity.take_calls(),
                vec![(action::PROJECTS_UPDATE.to_owned(), sensitive)],
                "{body}"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn retired_iam_and_testing_routes_and_headers_are_gone() -> anyhow::Result<()> {
        let identity = Arc::new(RecordingIdentity::default());
        let app = router(test_state(identity.clone())?, &test_server_settings(4096)?)?;
        for uri in [
            "/api/v1/iam",
            "/api/v1/auth/status",
            "/api/v1/auth/organizations",
            "/api/v1/testing-context",
            "/api/v1/test-environments",
            "/internal/honeycomb/organizations/tos/testing-environments/x/operations/prepare",
        ] {
            let response = app
                .clone()
                .oneshot(HttpRequest::builder().uri(uri).body(Body::empty())?)
                .await?;
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{uri}");
        }
        let response = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/v1/todos")
                    .header("x-org-id", "tos")
                    .header(header::AUTHORIZATION, "Bearer eyJ.test.token")
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = response_json(response).await?;
        assert_eq!(body["error"]["code"], "retired_header");
        assert!(identity.take_calls().is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn accounts_metadata_is_public() -> anyhow::Result<()> {
        let state = test_state(Arc::new(RecordingIdentity::default()))?
            .with_accounts("commit", "http://localhost:9590");
        let app = router(state, &test_server_settings(4096)?)?;
        let response = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/v1/accounts")
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        let value = response_json(response).await?;
        assert_eq!(value["app_id"], "commit");
        assert_eq!(value["accounts_url"], "http://localhost:9590");
        assert!(
            value["scopes"]
                .as_array()
                .is_some_and(|scopes| scopes.len() == action::ALL.len())
        );
        Ok(())
    }

    #[tokio::test]
    async fn public_errors_request_ids_cors_and_mutation_headers_are_stable() -> anyhow::Result<()>
    {
        let identity = Arc::new(RecordingIdentity::default());
        let state = test_state(identity)?;
        let settings = test_server_settings(64)?;
        let app = router(state.clone(), &settings)?;

        let health_response = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/healthz")
                    .header(&REQUEST_ID_HEADER, "caller-request-1")
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(health_response.status(), StatusCode::OK);
        assert_eq!(
            health_response.headers().get(&REQUEST_ID_HEADER),
            Some(&HeaderValue::from_static("caller-request-1"))
        );
        assert_eq!(
            health_response.headers().get(header::CACHE_CONTROL),
            Some(&HeaderValue::from_static("no-store"))
        );

        let preflight = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method(Method::OPTIONS)
                    .uri("/api/v1/todos")
                    .header(header::ORIGIN, "https://app.example")
                    .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(preflight.status(), StatusCode::OK);
        assert_eq!(
            preflight.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN),
            Some(&HeaderValue::from_static("https://app.example"))
        );

        let unauthorized = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/v1/todos")
                    .header(header::ORIGIN, "https://app.example")
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            unauthorized.headers().get(header::WWW_AUTHENTICATE),
            Some(&HeaderValue::from_static("Bearer"))
        );
        let exposed = unauthorized
            .headers()
            .get(header::ACCESS_CONTROL_EXPOSE_HEADERS)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        assert!(
            exposed
                .split(',')
                .map(str::trim)
                .any(|name| name.eq_ignore_ascii_case("www-authenticate"))
        );

        let oversized = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method(Method::POST)
                    .uri("/api/v1/todos")
                    .header("idempotency-key", "oversized-key")
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(format!(r#"{{"padding":"{}"}}"#, "x".repeat(80))))?,
            )
            .await?;
        assert_eq!(oversized.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let oversized_body = response_json(oversized).await?;
        assert_eq!(oversized_body["error"]["code"], "payload_too_large");

        let invalid_request_id = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/v1/todos")
                    .header(header::ORIGIN, "https://app.example")
                    .header(&REQUEST_ID_HEADER, "contains a space")
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(invalid_request_id.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            invalid_request_id
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN),
            Some(&HeaderValue::from_static("https://app.example"))
        );
        let response_request_id = invalid_request_id
            .headers()
            .get(&REQUEST_ID_HEADER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let invalid_body = response_json(invalid_request_id).await?;
        assert_eq!(invalid_body["error"]["code"], "invalid_request_id");
        assert_eq!(
            invalid_body["error"]["request_id"].as_str(),
            response_request_id.as_deref()
        );

        let not_found = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/not-a-route")
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(not_found.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            response_json(not_found).await?["error"]["code"],
            "not_found"
        );

        let mutation = MutationResponse::created(
            201,
            serde_json::json!({ "id": "018f268d-715a-7b72-8f0f-41f16f9af553" }),
        );
        let response = mutation_response(&state, mutation, Some("todos/example"))?;
        assert_eq!(response.status(), StatusCode::CREATED);
        assert_eq!(
            response.headers().get(&IDEMPOTENCY_REPLAYED_HEADER),
            Some(&HeaderValue::from_static("false"))
        );
        assert_eq!(
            response.headers().get(header::LOCATION),
            Some(&HeaderValue::from_static(
                "https://commit.example/api/v1/todos/example"
            ))
        );
        Ok(())
    }

    #[tokio::test]
    async fn health_bypasses_product_admission_control() -> anyhow::Result<()> {
        let identity = Arc::new(BlockingIdentity::new());
        let state = test_state(Arc::clone(&identity))?;
        let mut settings = test_server_settings(64)?;
        settings.concurrency_limit = 1;
        settings.request_timeout = Duration::from_secs(5);
        let app = router(state, &settings)?;

        let product_request = HttpRequest::builder()
            .uri("/api/v1/todos")
            .header(header::AUTHORIZATION, "Bearer eyJ.test.token")
            .body(Body::empty())?;
        let product_app = app.clone();
        let product = tokio::spawn(async move { product_app.oneshot(product_request).await });
        let entered =
            tokio::time::timeout(Duration::from_secs(1), identity.entered.acquire()).await??;
        entered.forget();

        let health = tokio::time::timeout(
            Duration::from_millis(250),
            app.oneshot(HttpRequest::builder().uri("/healthz").body(Body::empty())?),
        )
        .await;
        identity.release.add_permits(1);
        let product_response = tokio::time::timeout(Duration::from_secs(1), product).await???;
        let health_response = health??;

        assert_eq!(health_response.status(), StatusCode::OK);
        assert_eq!(product_response.status(), StatusCode::FORBIDDEN);
        Ok(())
    }

    fn test_state<I>(identity: Arc<I>) -> anyhow::Result<AppState>
    where
        I: IdentityProvider + 'static,
    {
        let pool = PgPoolOptions::new()
            .connect_lazy("postgresql://postgres:postgres@127.0.0.1:1/commit")?;
        test_state_with_pool(identity, pool)
    }

    pub(super) fn test_state_with_pool<I>(
        identity: Arc<I>,
        pool: PgPool,
    ) -> anyhow::Result<AppState>
    where
        I: IdentityProvider + 'static,
    {
        let identity: Arc<dyn IdentityProvider> = identity;
        let limits = DomainLimits::default();
        let ttl = Duration::from_secs(60);
        let todos = Arc::new(TodoService::new(
            pool.clone(),
            Arc::clone(&identity),
            limits,
            ttl,
            Duration::from_secs(60),
            Duration::from_secs(60),
        ));
        let projects = Arc::new(ProjectService::new(
            pool.clone(),
            Arc::clone(&identity),
            limits,
            ttl,
            Duration::from_secs(60),
        ));
        let notifications = Arc::new(NotificationSettingsService::new(
            pool.clone(),
            Arc::clone(&identity),
            Duration::from_secs(60),
        ));
        let accounts = Arc::new(AccountService::new(
            pool.clone(),
            Arc::clone(&identity),
            Duration::from_secs(60),
            Duration::from_secs(60),
        ));
        Ok(AppState::new(
            pool,
            identity,
            todos,
            projects,
            notifications,
            accounts,
            Url::parse("https://commit.example/api/v1/")?,
        ))
    }

    fn test_server_settings(max_body_bytes: usize) -> anyhow::Result<ServerSettings> {
        Ok(ServerSettings {
            bind_addr: "127.0.0.1:0".parse()?,
            public_base_url: Url::parse("https://commit.example/api/v1/")?,
            cors_allowed_origins: vec![Url::parse("https://app.example/")?],
            request_timeout: Duration::from_secs(1),
            max_body_bytes,
            concurrency_limit: 8,
            shutdown_timeout: Duration::from_secs(1),
        })
    }

    async fn response_json(response: Response) -> anyhow::Result<serde_json::Value> {
        let bytes = response.into_body().collect().await?.to_bytes();
        Ok(serde_json::from_slice(&bytes)?)
    }
}
