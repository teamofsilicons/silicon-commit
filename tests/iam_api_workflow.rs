//! Real HTTP routing and PostgreSQL workflows authenticated through the IAM 2 SDK.

use std::{env, num::NonZeroU32, sync::Arc, time::Duration};

use anyhow::{Context as _, ensure};
use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Method, Request, StatusCode},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use silicon_commit::{
    api::{self, AppState},
    application::{
        notifications::NotificationSettingsService, ports::IdentityProvider,
        projects::ProjectService, todos::TodoService,
    },
    config::{AuthenticationMode, DatabaseSettings, IamSettings, ServerSettings},
    domain::DomainLimits,
    infrastructure::{
        clients::{iam::IamClient, scoped_identity::ScopedIdentity},
        postgres,
    },
};
use sqlx::PgPool;
use tower::ServiceExt as _;
use uuid::Uuid;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_string, header, method, path},
};

const TOKEN: &str = "oat_local_iam_workflow";
const APP_SECRET: &str = "local-iam-workflow-app-secret";

#[tokio::test]
async fn iam_two_authentication_drives_todos_projects_and_linked_tasks() -> anyhow::Result<()> {
    let Ok(database_url) = env::var("COMMIT_TEST_DATABASE_URL") else {
        eprintln!("skipping IAM API workflow: COMMIT_TEST_DATABASE_URL is not set");
        return Ok(());
    };
    let pool = database(database_url).await?;
    let iam = MockServer::start().await;
    let organization_id = Uuid::new_v4();
    let creator_principal = Uuid::new_v4();
    let worker_principal = Uuid::new_v4();
    let org = format!("iam-workflow-{}", Uuid::new_v4().simple());
    let creator = format!("chef:{org}");
    let worker = format!("helper:{org}");
    // Existing attribution must survive the IAM cutover without rewriting todos.
    sqlx::query("INSERT INTO commit.organization_projection(organization_id,org_id) VALUES($1,$2)")
        .bind(organization_id)
        .bind(&org)
        .execute(&pool)
        .await?;
    for (key, id) in [(creator_principal, &creator), (worker_principal, &worker)] {
        sqlx::query("INSERT INTO commit.actor_projection(organization_id,principal_id,membership_id,actor_type,actor_id) VALUES($1,$2,$3,'silicon',$4)")
            .bind(organization_id).bind(key).bind(format!("{id}[{org}]")).bind(id).execute(&pool).await?;
    }
    let snapshot = json!({
        "organization_id":organization_id,
        "membership_id":format!("{creator}[{org}]"),"actor_type":"silicon","public_id":creator,
        "org_id":org,"audience":"tos>commit","membership_version":1,"authorization_epoch":1,
        "testing_environment_id":null,"org_role":"member","tags":null,
        "scopes":["self.identity.read","self.membership.read","directory.memberships.read","directory.silicons.read"]
    });
    Mock::given(method("POST"))
        .and(path("/api/v1/oauth/introspect"))
        .and(header("x-org-id", org.as_str()))
        .and(header(
            "authorization",
            format!(
                "Basic {}",
                STANDARD.encode(format!("tos>commit:{APP_SECRET}"))
            ),
        ))
        .and(header(
            "user-agent",
            format!(
                "silicon-iam-client/3.0.0 silicon-commit/{}",
                env!("CARGO_PKG_VERSION")
            ),
        ))
        .and(body_string(format!("token={TOKEN}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "active":true,"public_id":creator,"membership_id":format!("{creator}[{org}]"),
            "actor_type":"silicon","org_id":org,"audience":"tos>commit",
            "expires_at":time::OffsetDateTime::now_utc().unix_timestamp()+300,
            "authorization":snapshot
        })))
        .mount(&iam)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/api/v1/organizations/{org}")))
        .and(header("authorization", format!("Bearer {TOKEN}")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"id":organization_id,"org_id":org})),
        )
        .mount(&iam)
        .await;
    // Match the current scope-limited IAM projection: optional management
    // profile, hierarchy, tags, and organization status fields are omitted.
    Mock::given(method("GET"))
        .and(path(format!("/api/v1/organizations/{org}/members")))
        .and(header("authorization", format!("Bearer {TOKEN}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items":[{"id":format!("{worker}[{org}]"),"org_id":org,"status":"active",
                      "principal":{"type":"silicon","public_id":worker}}],
            "page":{"has_more":false,"next_cursor":null}
        })))
        .mount(&iam)
        .await;
    let app = router(pool.clone(), &iam)?;
    let todo_input = json!({"title":"Plan a meal","assigned_to":worker});
    let (status, _, todo) = request(
        &app,
        &org,
        Method::POST,
        "/api/v1/todos",
        Some(todo_input.clone()),
        Some("iam-workflow-todo"),
    )
    .await?;
    ensure!(
        status == StatusCode::CREATED,
        "todo creation failed: {status} {todo}; IAM requests: {:?}",
        iam.received_requests().await.map(|requests| requests
            .into_iter()
            .map(|request| (
                request.method,
                request.url.path().to_owned(),
                request.headers.get("user-agent").cloned(),
                request.headers.get("x-org-id").cloned()
            ))
            .collect::<Vec<_>>())
    );
    let todo_id = id(&todo)?;
    let (status, headers, replay) = request(
        &app,
        &org,
        Method::POST,
        "/api/v1/todos",
        Some(todo_input),
        Some("iam-workflow-todo"),
    )
    .await?;
    ensure!(
        status == StatusCode::CREATED
            && replay == todo
            && headers
                .get("idempotency-replayed")
                .is_some_and(|value| value == "true"),
        "todo replay changed its outcome"
    );

    let (status, _, project) = request(
        &app,
        &org,
        Method::POST,
        "/api/v1/projects",
        Some(json!({"name":"Meal preparation","silicon_ids":[worker]})),
        Some("iam-workflow-project"),
    )
    .await?;
    ensure!(
        status == StatusCode::CREATED,
        "project creation failed: {status} {project}"
    );
    let project_id = id(&project)?;
    let task_path = format!("/api/v1/projects/{project_id}/tasks");
    let (status, _, task) = request(
        &app,
        &org,
        Method::POST,
        &task_path,
        Some(json!({"title":"Prepare dinner","assigned_to":worker})),
        Some("iam-workflow-task"),
    )
    .await?;
    ensure!(
        status == StatusCode::CREATED,
        "assigned task creation failed: {status} {task}"
    );
    let task_id = id(&task)?;
    let linked_todo = task
        .get("todo_id")
        .and_then(Value::as_str)
        .context("assigned task omitted its linked todo")?;

    for (route, expected_ids) in [
        (
            "/api/v1/todos?view=all".to_owned(),
            vec![todo_id.as_str(), linked_todo],
        ),
        ("/api/v1/projects".to_owned(), vec![project_id.as_str()]),
        (task_path, vec![task_id.as_str()]),
    ] {
        let (status, _, page) = request(&app, &org, Method::GET, &route, None, None).await?;
        ensure!(
            status == StatusCode::OK,
            "listing failed: {route} {status} {page}"
        );
        let items = page
            .get("items")
            .and_then(Value::as_array)
            .context("list omitted items")?;
        ensure!(
            items.len() == expected_ids.len()
                && expected_ids.iter().all(|id| items
                    .iter()
                    .any(|item| item.get("id").and_then(Value::as_str) == Some(id))),
            "listing omitted or duplicated created work: {page}"
        );
    }
    let persisted = sqlx::query_as::<_, (String, Uuid, Uuid, Uuid)>(
        "SELECT actor.membership_id, todo.project_id, todo.assigned_to_principal_id, task.assigned_to_principal_id
         FROM commit.project_tasks task JOIN commit.todos todo ON todo.id=task.todo_id AND todo.organization_id=task.organization_id
         JOIN commit.actor_projection actor ON actor.organization_id=todo.organization_id AND actor.principal_id=todo.assigned_to_principal_id
         WHERE task.organization_id=$1 AND task.id=$2",
    ).bind(organization_id).bind(Uuid::parse_str(&task_id)?).fetch_one(&pool).await?;
    ensure!(
        persisted
            == (
                format!("{worker}[{org}]"),
                Uuid::parse_str(&project_id)?,
                worker_principal,
                worker_principal
            ),
        "IAM directory identity or task/todo link changed during persistence"
    );
    let projections = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM commit.actor_projection WHERE organization_id=$1 AND membership_id=actor_id || '[' || $2 || ']'")
        .bind(organization_id).bind(&org).fetch_one(&pool).await?;
    ensure!(
        projections == 2,
        "creator and assignee must retain canonical memberships"
    );
    iam.verify().await;
    pool.close().await;
    Ok(())
}

async fn database(url: String) -> anyhow::Result<PgPool> {
    let settings = DatabaseSettings {
        url: url.into(),
        max_connections: NonZeroU32::new(5).context("pool size")?,
        min_connections: 0,
        acquire_timeout: Duration::from_secs(5),
        statement_timeout: Duration::from_secs(30),
    };
    let migrator = postgres::connect_migrator(&settings, "iam-api-workflow-migrator").await?;
    let owner = sqlx::query_scalar::<_, String>("SELECT current_user::text")
        .fetch_one(&migrator)
        .await?;
    postgres::migrate(&migrator, &owner).await?;
    migrator.close().await;
    postgres::connect(&settings, "iam-api-workflow").await
}

fn router(pool: PgPool, iam: &MockServer) -> anyhow::Result<Router> {
    let provider: Arc<dyn IdentityProvider> = Arc::new(IamClient::new(
        &IamSettings {
            mode: AuthenticationMode::Iam,
            base_url: iam.uri().parse()?,
            app_id: Some("tos>commit".into()),
            app_secret: Some(APP_SECRET.into()),
            audience: "tos>commit".into(),
            webhook_secret: None,
            webhook_key_version: 1,
        },
        Duration::from_secs(1),
        Duration::from_secs(5),
        1_048_576,
    )?);
    let identity: Arc<dyn IdentityProvider> = Arc::new(ScopedIdentity::new(provider, pool.clone()));
    let audit_retention = Duration::from_hours(7 * 365 * 24);
    let settings = ServerSettings {
        bind_addr: "127.0.0.1:0".parse()?,
        public_base_url: "http://127.0.0.1:1/api/v1".parse()?,
        cors_allowed_origins: vec![],
        request_timeout: Duration::from_secs(10),
        max_body_bytes: 1_048_576,
        concurrency_limit: 8,
        shutdown_timeout: Duration::from_secs(1),
    };
    let state = AppState::new(
        pool.clone(),
        identity.clone(),
        Arc::new(TodoService::new(
            pool.clone(),
            identity.clone(),
            DomainLimits::default(),
            Duration::from_hours(24),
            audit_retention,
            Duration::from_hours(48),
        )),
        Arc::new(ProjectService::new(
            pool.clone(),
            identity,
            DomainLimits::default(),
            Duration::from_hours(24),
            audit_retention,
        )),
        Arc::new(NotificationSettingsService::new(pool, audit_retention)),
        AuthenticationMode::Iam,
        settings.public_base_url.clone(),
    );
    Ok(api::router(state, &settings)?)
}

async fn request(
    app: &Router,
    org: &str,
    method: Method,
    path: &str,
    body: Option<Value>,
    key: Option<&str>,
) -> anyhow::Result<(StatusCode, HeaderMap, Value)> {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("authorization", format!("Bearer {TOKEN}"))
        .header("x-org-id", org)
        .header("content-type", "application/json");
    if let Some(key) = key {
        builder = builder.header("idempotency-key", key);
    }
    let body = body.map_or_else(Body::empty, |value| Body::from(value.to_string()));
    let response = app.clone().oneshot(builder.body(body)?).await?;
    let (parts, body) = response.into_parts();
    let bytes = axum::body::to_bytes(body, 1_048_576).await?;
    Ok((parts.status, parts.headers, serde_json::from_slice(&bytes)?))
}

fn id(value: &Value) -> anyhow::Result<String> {
    Ok(value
        .get("id")
        .and_then(Value::as_str)
        .context("created resource omitted id")?
        .to_owned())
}
