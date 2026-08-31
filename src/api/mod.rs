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
    routing::{get, patch, post},
};
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
        attachments::{AttachmentService, map_provider_error},
        idempotency::MutationResponse,
        ports::{BriefcaseProvider, IdentityProvider, VerifiedActor},
        projects::ProjectService,
        todos::TodoService,
    },
    config::{AuthenticationMode, RuntimeProfile, ServerSettings, Settings},
    error::AppError,
    infrastructure::{
        clients::{
            ClientBuildError,
            briefcase::{BriefcaseClient, permanent_url_policy},
            iam::{IamClient, TrustedHeaderIdentityProvider},
        },
        postgres,
    },
    request_context, shutdown,
};

pub mod attachments;
pub mod auth;
pub mod extract;
pub mod projects;
pub mod todos;

const REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-request-id");
const IDEMPOTENCY_REPLAYED_HEADER: HeaderName = HeaderName::from_static("idempotency-replayed");
const MAX_REQUEST_ID_BYTES: usize = 128;

/// Immutable dependencies shared by all request handlers.
#[derive(Clone)]
pub struct AppState {
    pub(crate) pool: PgPool,
    pub(crate) identity: Arc<dyn IdentityProvider>,
    pub(crate) todos: Arc<TodoService>,
    pub(crate) projects: Arc<ProjectService>,
    pub(crate) attachments: Arc<AttachmentService>,
    authentication_mode: AuthenticationMode,
    public_base_url: Url,
}

impl AppState {
    /// Creates an application state from already-composed dependencies.
    ///
    /// This constructor keeps transport tests independent from live IAM,
    /// Briefcase, and PostgreSQL services.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pool: PgPool,
        identity: Arc<dyn IdentityProvider>,
        todos: Arc<TodoService>,
        projects: Arc<ProjectService>,
        attachments: Arc<AttachmentService>,
        authentication_mode: AuthenticationMode,
        public_base_url: Url,
    ) -> Self {
        Self {
            pool,
            identity,
            todos,
            projects,
            attachments,
            authentication_mode,
            public_base_url: normalized_base_url(public_base_url),
        }
    }

    /// Builds all API-facing services from validated settings and a database
    /// pool.
    ///
    /// # Errors
    ///
    /// Returns a redacted construction error when an external client or the
    /// permanent attachment URL policy cannot be built safely.
    pub fn from_settings(settings: &Settings, pool: PgPool) -> Result<Self, ApiBuildError> {
        if settings.runtime_profile != RuntimeProfile::Api {
            return Err(ApiBuildError::WrongSettingsProfile);
        }
        let integrations = &settings.integrations;
        let identity: Arc<dyn IdentityProvider> = match integrations.iam.mode {
            AuthenticationMode::Iam => Arc::new(IamClient::new(
                &integrations.iam,
                integrations.connect_timeout,
                integrations.request_timeout,
                integrations.max_response_bytes,
            )?),
            AuthenticationMode::TrustedHeaders => {
                Arc::new(TrustedHeaderIdentityProvider::default())
            }
        };
        let briefcase: Arc<dyn BriefcaseProvider> = Arc::new(BriefcaseClient::new(
            &integrations.briefcase,
            integrations.connect_timeout,
            integrations.request_timeout,
            integrations.max_response_bytes,
        )?);
        let attachment_policy = permanent_url_policy(&integrations.briefcase)?;
        let todo_limits = settings.limits.domain_limits();
        let project_limits = settings.limits.domain_limits();
        let idempotency_ttl = settings.limits.idempotency_ttl;
        let audit_retention = settings.worker.audit_retention;
        let tombstone_retention = settings.worker.todo_tombstone_retention;
        let todos = Arc::new(TodoService::new(
            pool.clone(),
            Arc::clone(&identity),
            todo_limits,
            attachment_policy.clone(),
            idempotency_ttl,
            audit_retention,
            tombstone_retention,
        ));
        let projects = Arc::new(ProjectService::new(
            pool.clone(),
            Arc::clone(&identity),
            project_limits,
            idempotency_ttl,
            audit_retention,
        ));
        let attachments = Arc::new(AttachmentService::new(
            pool.clone(),
            Arc::clone(&identity),
            briefcase,
            attachment_policy,
        ));

        Ok(Self::new(
            pool,
            identity,
            todos,
            projects,
            attachments,
            integrations.iam.mode,
            settings.server.public_base_url.clone(),
        ))
    }

    pub(crate) async fn authenticate(
        &self,
        headers: &HeaderMap,
        action: &'static str,
        resource: Option<String>,
    ) -> Result<VerifiedActor, AppError> {
        let request = auth::request(headers, self.authentication_mode, action, resource)?;
        let actor = self
            .identity
            .authenticate(&request)
            .await
            .map_err(map_provider_error)?;
        postgres::assert_identity_consistency(&self.pool, &actor).await?;
        Ok(actor)
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
        .route("/version", get(version))
        .route("/todos", get(todos::list).post(todos::create))
        .route(
            "/todos/{todo_id}",
            get(todos::get).patch(todos::update).delete(todos::delete),
        )
        .route(
            "/todos/{todo_id}/notes",
            get(todos::list_notes).post(todos::add_note),
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
            "/projects/{project_id}/tasks",
            get(projects::list_tasks).post(projects::create_task),
        )
        .route(
            "/projects/{project_id}/tasks/{task_id}",
            patch(projects::update_task),
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
        .route(
            "/attachments/temporary-url",
            post(attachments::temporary_url),
        );

    let sensitive_headers = [
        header::AUTHORIZATION,
        HeaderName::from_static("x-iam-obo-access-proof"),
        HeaderName::from_static("idempotency-key"),
        HeaderName::from_static("x-test-organization-id"),
        HeaderName::from_static("x-test-membership-id"),
        HeaderName::from_static("x-test-principal-id"),
        HeaderName::from_static("x-test-actor-id"),
    ];
    let concurrency = Arc::new(Semaphore::new(settings.concurrency_limit));
    let product_api = Router::new()
        .nest("/api/v1", api)
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
            HeaderName::from_static("x-org-id"),
            HeaderName::from_static("x-app-id"),
            HeaderName::from_static("x-iam-obo-access-proof"),
            HeaderName::from_static("idempotency-key"),
            header::IF_MATCH,
            REQUEST_ID_HEADER,
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
        application::ports::{
            ActiveMember, AuthenticationRequest, ChildProofRequest, DelegatedOboProof,
            ProviderError, TemporaryUrl,
        },
        config::ServerSettings,
        domain::{
            ActorId, ActorType, AttachmentUrlPolicy, DomainLimits, PermanentAttachmentUrl,
            PublicOrganizationId,
        },
    };

    use super::*;

    #[derive(Debug, Default)]
    struct RecordingIdentity {
        calls: Mutex<Vec<(String, Option<String>)>>,
    }

    impl RecordingIdentity {
        fn take_calls(&self) -> Vec<(String, Option<String>)> {
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
            calls.push((request.action.clone(), request.resource.clone()));
            Err(ProviderError::Forbidden)
        }

        async fn resolve_active_members(
            &self,
            _org_id: &PublicOrganizationId,
            _actor_ids: &[ActorId],
            _required_type: Option<ActorType>,
        ) -> Result<Vec<ActiveMember>, ProviderError> {
            Err(ProviderError::Forbidden)
        }

        async fn exchange_child_proof(
            &self,
            _actor: &VerifiedActor,
            _request: &ChildProofRequest,
        ) -> Result<DelegatedOboProof, ProviderError> {
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

        async fn resolve_active_members(
            &self,
            _org_id: &PublicOrganizationId,
            _actor_ids: &[ActorId],
            _required_type: Option<ActorType>,
        ) -> Result<Vec<ActiveMember>, ProviderError> {
            Err(ProviderError::Forbidden)
        }

        async fn exchange_child_proof(
            &self,
            _actor: &VerifiedActor,
            _request: &ChildProofRequest,
        ) -> Result<DelegatedOboProof, ProviderError> {
            Err(ProviderError::Forbidden)
        }
    }

    #[derive(Debug)]
    struct RejectingBriefcase;

    #[async_trait]
    impl BriefcaseProvider for RejectingBriefcase {
        async fn temporary_url(
            &self,
            _org_id: &PublicOrganizationId,
            _attachment: &PermanentAttachmentUrl,
            _proof: &DelegatedOboProof,
        ) -> Result<TemporaryUrl, ProviderError> {
            Err(ProviderError::Forbidden)
        }
    }

    struct OperationCase {
        method: Method,
        uri: &'static str,
        action: &'static str,
        resource: Option<String>,
        body: Option<&'static str>,
        idempotent: bool,
        if_match: bool,
    }

    #[tokio::test]
    async fn every_product_route_uses_its_exact_iam_action_and_resource() -> anyhow::Result<()> {
        const TODO_ID: &str = "018f268d-715a-7b72-8f0f-41f16f9af553";
        const PROJECT_ID: &str = "018f268d-715a-7b72-8f0f-41f16f9af554";
        const TASK_ID: &str = "018f268d-715a-7b72-8f0f-41f16f9af555";
        const PERMANENT_URL: &str =
            "https://briefcase.example/api/v1/entries/018f268d-715a-7b72-8f0f-41f16f9af556";

        let cases = vec![
            operation(Method::GET, "/api/v1/todos", action::TODOS_LIST, None),
            mutation(
                Method::POST,
                "/api/v1/todos",
                action::TODOS_CREATE,
                None,
                r#"{"title":"work","assigned_to":"silicon-one"}"#,
            ),
            operation(
                Method::GET,
                "/api/v1/todos/018f268d-715a-7b72-8f0f-41f16f9af553",
                action::TODOS_READ,
                Some(TODO_ID),
            ),
            mutation(
                Method::PATCH,
                "/api/v1/todos/018f268d-715a-7b72-8f0f-41f16f9af553",
                action::TODOS_UPDATE,
                Some(TODO_ID),
                r#"{"title":"changed"}"#,
            ),
            operation(
                Method::DELETE,
                "/api/v1/todos/018f268d-715a-7b72-8f0f-41f16f9af553",
                action::TODOS_DELETE,
                Some(TODO_ID),
            ),
            operation(
                Method::GET,
                "/api/v1/todos/018f268d-715a-7b72-8f0f-41f16f9af553/notes",
                action::TODO_NOTES_LIST,
                Some(TODO_ID),
            ),
            mutation(
                Method::POST,
                "/api/v1/todos/018f268d-715a-7b72-8f0f-41f16f9af553/notes",
                action::TODO_NOTES_CREATE,
                Some(TODO_ID),
                r#"{"body":"note"}"#,
            ),
            operation(Method::GET, "/api/v1/projects", action::PROJECTS_LIST, None),
            mutation(
                Method::POST,
                "/api/v1/projects",
                action::PROJECTS_CREATE,
                None,
                r#"{"name":"project","silicon_ids":["silicon-one"]}"#,
            ),
            operation(
                Method::GET,
                "/api/v1/projects/018f268d-715a-7b72-8f0f-41f16f9af554",
                action::PROJECTS_READ,
                Some(PROJECT_ID),
            ),
            mutation(
                Method::PATCH,
                "/api/v1/projects/018f268d-715a-7b72-8f0f-41f16f9af554",
                action::PROJECTS_UPDATE,
                Some(PROJECT_ID),
                r#"{"name":"changed"}"#,
            ),
            operation(
                Method::GET,
                "/api/v1/projects/018f268d-715a-7b72-8f0f-41f16f9af554/diary",
                action::DIARY_READ,
                Some(PROJECT_ID),
            ),
            OperationCase {
                method: Method::PUT,
                uri: "/api/v1/projects/018f268d-715a-7b72-8f0f-41f16f9af554/diary",
                action: action::DIARY_UPDATE,
                resource: Some(PROJECT_ID.to_owned()),
                body: Some(r#"{"markdown":"entry"}"#),
                idempotent: false,
                if_match: true,
            },
            operation(
                Method::GET,
                "/api/v1/projects/018f268d-715a-7b72-8f0f-41f16f9af554/tasks",
                action::PROJECT_TASKS_LIST,
                Some(PROJECT_ID),
            ),
            mutation(
                Method::POST,
                "/api/v1/projects/018f268d-715a-7b72-8f0f-41f16f9af554/tasks",
                action::PROJECT_TASKS_CREATE,
                Some(PROJECT_ID),
                r#"{"title":"task"}"#,
            ),
            OperationCase {
                method: Method::PATCH,
                uri: "/api/v1/projects/018f268d-715a-7b72-8f0f-41f16f9af554/tasks/018f268d-715a-7b72-8f0f-41f16f9af555",
                action: action::PROJECT_TASKS_UPDATE,
                resource: Some(format!("{PROJECT_ID}/tasks/{TASK_ID}")),
                body: Some(r#"{"title":"changed"}"#),
                idempotent: false,
                if_match: false,
            },
            mutation(
                Method::POST,
                "/api/v1/projects/018f268d-715a-7b72-8f0f-41f16f9af554/blockers",
                action::PROJECT_BLOCKERS_CREATE,
                Some(PROJECT_ID),
                r#"{"title":"blocked","description":"dependency"}"#,
            ),
            mutation(
                Method::POST,
                "/api/v1/projects/018f268d-715a-7b72-8f0f-41f16f9af554/updates",
                action::PROJECT_UPDATES_CREATE,
                Some(PROJECT_ID),
                r#"{"title":"milestone","description":"shipped"}"#,
            ),
            mutation(
                Method::POST,
                "/api/v1/projects/018f268d-715a-7b72-8f0f-41f16f9af554/completion",
                action::PROJECT_COMPLETION_CREATE,
                Some(PROJECT_ID),
                r#"{"title":"complete","description":"done"}"#,
            ),
            OperationCase {
                method: Method::POST,
                uri: "/api/v1/attachments/temporary-url",
                action: action::ATTACHMENTS_TEMPORARY_URL,
                resource: Some(PERMANENT_URL.to_owned()),
                body: Some(
                    r#"{"permanent_url":"https://briefcase.example/api/v1/entries/018f268d-715a-7b72-8f0f-41f16f9af556"}"#,
                ),
                idempotent: false,
                if_match: false,
            },
        ];
        assert_eq!(cases.len(), 20);

        let identity = Arc::new(RecordingIdentity::default());
        let state = test_state(identity.clone())?;
        let app = router(state, &test_server_settings(1_048_576)?)?;
        for case in cases {
            let mut builder = HttpRequest::builder()
                .method(case.method.clone())
                .uri(case.uri)
                .header("x-org-id", "test-org")
                .header(header::AUTHORIZATION, "Bearer opaque-token")
                .header(CONTENT_TYPE, "application/json");
            if case.idempotent {
                builder = builder.header("idempotency-key", "route-test-key");
            }
            if case.if_match {
                builder = builder.header(header::IF_MATCH, "\"1\"");
            }
            let request = builder.body(Body::from(case.body.unwrap_or_default()))?;
            let response = app.clone().oneshot(request).await?;
            assert_eq!(
                response.status(),
                StatusCode::FORBIDDEN,
                "{} {} did not reach IAM",
                case.method,
                case.uri
            );
            assert_eq!(
                identity.take_calls(),
                vec![(case.action.to_owned(), case.resource)],
                "{} {} used the wrong IAM binding",
                case.method,
                case.uri
            );
        }
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
                    .header("x-org-id", "test-org")
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
            .header("x-org-id", "test-org")
            .header(header::AUTHORIZATION, "Bearer opaque-token")
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

    fn operation(
        method: Method,
        uri: &'static str,
        action: &'static str,
        resource: Option<&str>,
    ) -> OperationCase {
        OperationCase {
            method,
            uri,
            action,
            resource: resource.map(str::to_owned),
            body: None,
            idempotent: false,
            if_match: false,
        }
    }

    fn mutation(
        method: Method,
        uri: &'static str,
        action: &'static str,
        resource: Option<&str>,
        body: &'static str,
    ) -> OperationCase {
        OperationCase {
            method,
            uri,
            action,
            resource: resource.map(str::to_owned),
            body: Some(body),
            idempotent: true,
            if_match: false,
        }
    }

    fn test_state<I>(identity: Arc<I>) -> anyhow::Result<AppState>
    where
        I: IdentityProvider + 'static,
    {
        let pool = PgPoolOptions::new()
            .connect_lazy("postgresql://postgres:postgres@127.0.0.1:1/commit")?;
        let identity: Arc<dyn IdentityProvider> = identity;
        let policy = AttachmentUrlPolicy::new([Url::parse("https://briefcase.example/api/v1/")?])?;
        let limits = DomainLimits::default();
        let ttl = Duration::from_secs(60);
        let todos = Arc::new(TodoService::new(
            pool.clone(),
            Arc::clone(&identity),
            limits,
            policy.clone(),
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
        let briefcase: Arc<dyn BriefcaseProvider> = Arc::new(RejectingBriefcase);
        let attachments = Arc::new(AttachmentService::new(
            pool.clone(),
            Arc::clone(&identity),
            briefcase,
            policy,
        ));
        Ok(AppState::new(
            pool,
            identity,
            todos,
            projects,
            attachments,
            AuthenticationMode::Iam,
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
