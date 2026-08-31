//! Project HTTP handlers.

use std::str::FromStr as _;

use axum::{
    Json,
    extract::State,
    http::{HeaderMap, HeaderValue, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;

use crate::{
    application::idempotency::MutationResponse,
    domain::{
        BlockerCreate, CollectionQuery, Diary, DiaryUpdate, ProjectCompletionCreate, ProjectCreate,
        ProjectLocator, ProjectPatch, ProjectQuery, ProjectTask, ProjectTaskCreate, ProjectTaskId,
        ProjectTaskPatch, ProjectUpdateCreate,
    },
    error::AppError,
};

use super::{
    AppState,
    auth::action,
    extract::{Idempotency, IfMatch, StrictJson, StrictPath, StrictQuery},
    mutation_response, required_request_id,
};

/// `GET /api/v1/projects`.
pub(crate) async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    StrictQuery(query): StrictQuery<ProjectQuery>,
) -> Result<Json<crate::domain::ProjectPage>, AppError> {
    let actor = state
        .authenticate(&headers, action::PROJECTS_LIST, None)
        .await?;
    state.projects.list_projects(&actor, query).await.map(Json)
}

/// `POST /api/v1/projects`.
pub(crate) async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Idempotency(key): Idempotency,
    StrictJson(input): StrictJson<ProjectCreate>,
) -> Result<Response, AppError> {
    let actor = state
        .authenticate(&headers, action::PROJECTS_CREATE, None)
        .await?;
    let request_id = required_request_id()?;
    let mutation = state
        .projects
        .create_project(&actor, input, key, &request_id)
        .await?;
    let location = resource_location(&mutation, "projects")?;
    mutation_response(&state, mutation, Some(&location))
}

/// `GET /api/v1/projects/{project_id}`.
pub(crate) async fn get(
    State(state): State<AppState>,
    headers: HeaderMap,
    StrictPath(raw_locator): StrictPath<String>,
) -> Result<Json<crate::domain::Project>, AppError> {
    let locator = parse_locator(&raw_locator)?;
    let actor = state
        .authenticate(&headers, action::PROJECTS_READ, Some(raw_locator))
        .await?;
    state.projects.get_project(&actor, &locator).await.map(Json)
}

/// `PATCH /api/v1/projects/{project_id}`.
pub(crate) async fn update(
    State(state): State<AppState>,
    headers: HeaderMap,
    StrictPath(raw_locator): StrictPath<String>,
    Idempotency(key): Idempotency,
    StrictJson(input): StrictJson<ProjectPatch>,
) -> Result<Response, AppError> {
    let locator = parse_locator(&raw_locator)?;
    let actor = state
        .authenticate(&headers, action::PROJECTS_UPDATE, Some(raw_locator))
        .await?;
    let request_id = required_request_id()?;
    let mutation = state
        .projects
        .update_project(&actor, &locator, input, key, &request_id)
        .await?;
    mutation_response(&state, mutation, None)
}

/// `GET /api/v1/projects/{project_id}/diary`.
pub(crate) async fn get_diary(
    State(state): State<AppState>,
    headers: HeaderMap,
    StrictPath(raw_locator): StrictPath<String>,
) -> Result<Response, AppError> {
    let locator = parse_locator(&raw_locator)?;
    let actor = state
        .authenticate(&headers, action::DIARY_READ, Some(raw_locator))
        .await?;
    let diary = state.projects.get_diary(&actor, &locator).await?;
    diary_response(diary)
}

/// `PUT /api/v1/projects/{project_id}/diary`.
pub(crate) async fn replace_diary(
    State(state): State<AppState>,
    headers: HeaderMap,
    StrictPath(raw_locator): StrictPath<String>,
    IfMatch(expected_version): IfMatch,
    StrictJson(input): StrictJson<DiaryUpdate>,
) -> Result<Response, AppError> {
    let locator = parse_locator(&raw_locator)?;
    let actor = state
        .authenticate(&headers, action::DIARY_UPDATE, Some(raw_locator))
        .await?;
    let request_id = required_request_id()?;
    let diary = state
        .projects
        .replace_diary(&actor, &locator, input, expected_version, &request_id)
        .await?;
    diary_response(diary)
}

/// `GET /api/v1/projects/{project_id}/tasks`.
pub(crate) async fn list_tasks(
    State(state): State<AppState>,
    headers: HeaderMap,
    StrictPath(raw_locator): StrictPath<String>,
    StrictQuery(query): StrictQuery<CollectionQuery>,
) -> Result<Json<crate::domain::Page<crate::domain::ProjectTask>>, AppError> {
    let locator = parse_locator(&raw_locator)?;
    let actor = state
        .authenticate(&headers, action::PROJECT_TASKS_LIST, Some(raw_locator))
        .await?;
    state
        .projects
        .list_tasks(&actor, &locator, query)
        .await
        .map(Json)
}

/// `POST /api/v1/projects/{project_id}/tasks`.
pub(crate) async fn create_task(
    State(state): State<AppState>,
    headers: HeaderMap,
    StrictPath(raw_locator): StrictPath<String>,
    Idempotency(key): Idempotency,
    StrictJson(input): StrictJson<ProjectTaskCreate>,
) -> Result<Response, AppError> {
    let locator = parse_locator(&raw_locator)?;
    let actor = state
        .authenticate(&headers, action::PROJECT_TASKS_CREATE, Some(raw_locator))
        .await?;
    let request_id = required_request_id()?;
    let mutation = state
        .projects
        .create_task(&actor, &locator, input, key, &request_id)
        .await?;
    let location = nested_task_location(&mutation)?;
    mutation_response(&state, mutation, Some(&location))
}

/// `PATCH /api/v1/projects/{project_id}/tasks/{task_id}`.
pub(crate) async fn update_task(
    State(state): State<AppState>,
    headers: HeaderMap,
    StrictPath(path): StrictPath<TaskPath>,
    StrictJson(input): StrictJson<ProjectTaskPatch>,
) -> Result<Json<ProjectTask>, AppError> {
    let locator = parse_locator(&path.project_id)?;
    let resource = format!("{}/tasks/{}", path.project_id, path.task_id);
    let actor = state
        .authenticate(&headers, action::PROJECT_TASKS_UPDATE, Some(resource))
        .await?;
    let request_id = required_request_id()?;
    state
        .projects
        .update_task(&actor, &locator, path.task_id, input, &request_id)
        .await
        .map(Json)
}

/// `POST /api/v1/projects/{project_id}/blockers`.
pub(crate) async fn create_blocker(
    State(state): State<AppState>,
    headers: HeaderMap,
    StrictPath(raw_locator): StrictPath<String>,
    Idempotency(key): Idempotency,
    StrictJson(input): StrictJson<BlockerCreate>,
) -> Result<Response, AppError> {
    let locator = parse_locator(&raw_locator)?;
    let actor = state
        .authenticate(&headers, action::PROJECT_BLOCKERS_CREATE, Some(raw_locator))
        .await?;
    let request_id = required_request_id()?;
    let mutation = state
        .projects
        .create_blocker(&actor, &locator, input, key, &request_id)
        .await?;
    mutation_response(&state, mutation, None)
}

/// `POST /api/v1/projects/{project_id}/updates`.
pub(crate) async fn create_update(
    State(state): State<AppState>,
    headers: HeaderMap,
    StrictPath(raw_locator): StrictPath<String>,
    Idempotency(key): Idempotency,
    StrictJson(input): StrictJson<ProjectUpdateCreate>,
) -> Result<Response, AppError> {
    let locator = parse_locator(&raw_locator)?;
    let actor = state
        .authenticate(&headers, action::PROJECT_UPDATES_CREATE, Some(raw_locator))
        .await?;
    let request_id = required_request_id()?;
    let mutation = state
        .projects
        .create_update(&actor, &locator, input, key, &request_id)
        .await?;
    mutation_response(&state, mutation, None)
}

/// `POST /api/v1/projects/{project_id}/completion`.
pub(crate) async fn complete(
    State(state): State<AppState>,
    headers: HeaderMap,
    StrictPath(raw_locator): StrictPath<String>,
    Idempotency(key): Idempotency,
    StrictJson(input): StrictJson<ProjectCompletionCreate>,
) -> Result<Response, AppError> {
    let locator = parse_locator(&raw_locator)?;
    let actor = state
        .authenticate(
            &headers,
            action::PROJECT_COMPLETION_CREATE,
            Some(raw_locator),
        )
        .await?;
    let request_id = required_request_id()?;
    let mutation = state
        .projects
        .complete_project(&actor, &locator, input, key, &request_id)
        .await?;
    mutation_response(&state, mutation, None)
}

#[derive(Debug, Deserialize)]
pub(crate) struct TaskPath {
    project_id: String,
    task_id: ProjectTaskId,
}

fn parse_locator(raw: &str) -> Result<ProjectLocator, AppError> {
    ProjectLocator::from_str(raw).map_err(|_| AppError::BadRequest {
        code: "invalid_project_id".into(),
    })
}

fn diary_response(diary: Diary) -> Result<Response, AppError> {
    let etag = HeaderValue::from_str(&format!("\"{}\"", diary.version.get())).map_err(|_| {
        AppError::Internal(anyhow::anyhow!(
            "diary version could not be represented as an ETag"
        ))
    })?;
    let mut response = Json(diary).into_response();
    response.headers_mut().insert(header::ETAG, etag);
    Ok(response)
}

fn resource_location(mutation: &MutationResponse, collection: &str) -> Result<String, AppError> {
    let id = body_identifier(&mutation.body, "id")?;
    Ok(format!("{collection}/{id}"))
}

fn nested_task_location(mutation: &MutationResponse) -> Result<String, AppError> {
    let project_id = body_identifier(&mutation.body, "project_id")?;
    let task_id = body_identifier(&mutation.body, "id")?;
    Ok(format!("projects/{project_id}/tasks/{task_id}"))
}

fn body_identifier<'a>(body: &'a serde_json::Value, field: &str) -> Result<&'a str, AppError> {
    body.get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            AppError::Internal(anyhow::anyhow!(
                "stored mutation response omitted its {field} identifier"
            ))
        })
}

#[cfg(test)]
mod tests {
    use crate::domain::ProjectLocator;

    use super::parse_locator;

    #[test]
    fn project_paths_reject_bare_slugs() {
        assert!(parse_locator("the-project").is_err());
        assert!(matches!(
            parse_locator("018f268d-715a-7b72-8f0f-41f16f9af553"),
            Ok(ProjectLocator::Id(_))
        ));
    }
}
