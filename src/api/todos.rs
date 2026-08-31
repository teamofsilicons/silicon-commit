//! Todo HTTP handlers.

use crate::{
    application::idempotency::MutationResponse,
    domain::{CollectionQuery, TodoCreate, TodoId, TodoNoteCreate, TodoPatch, TodoQuery},
    error::AppError,
};
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Response,
};

use super::{
    AppState,
    auth::action,
    extract::{Idempotency, StrictJson, StrictPath, StrictQuery},
    mutation_response, required_request_id,
};

/// `GET /api/v1/todos`.
pub(crate) async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    StrictQuery(query): StrictQuery<TodoQuery>,
) -> Result<Json<crate::domain::TodoPage>, AppError> {
    let actor = state
        .authenticate(&headers, action::TODOS_LIST, None)
        .await?;
    state.todos.list(&actor, query).await.map(Json)
}

/// `POST /api/v1/todos`.
pub(crate) async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Idempotency(key): Idempotency,
    StrictJson(input): StrictJson<TodoCreate>,
) -> Result<Response, AppError> {
    let actor = state
        .authenticate(&headers, action::TODOS_CREATE, None)
        .await?;
    let request_id = required_request_id()?;
    let mutation = state.todos.create(&actor, input, key, &request_id).await?;
    let location = created_resource_location(&mutation, "todos")?;
    mutation_response(&state, mutation, Some(&location))
}

/// `GET /api/v1/todos/{todo_id}`.
pub(crate) async fn get(
    State(state): State<AppState>,
    headers: HeaderMap,
    StrictPath(todo_id): StrictPath<TodoId>,
) -> Result<Json<crate::domain::Todo>, AppError> {
    let resource = todo_id.to_string();
    let actor = state
        .authenticate(&headers, action::TODOS_READ, Some(resource))
        .await?;
    state.todos.get(&actor, todo_id).await.map(Json)
}

/// `PATCH /api/v1/todos/{todo_id}`.
pub(crate) async fn update(
    State(state): State<AppState>,
    headers: HeaderMap,
    StrictPath(todo_id): StrictPath<TodoId>,
    Idempotency(key): Idempotency,
    StrictJson(input): StrictJson<TodoPatch>,
) -> Result<Response, AppError> {
    let resource = todo_id.to_string();
    let actor = state
        .authenticate(&headers, action::TODOS_UPDATE, Some(resource))
        .await?;
    let request_id = required_request_id()?;
    let mutation = state
        .todos
        .update(&actor, todo_id, input, key, &request_id)
        .await?;
    mutation_response(&state, mutation, None)
}

/// `DELETE /api/v1/todos/{todo_id}`.
pub(crate) async fn delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    StrictPath(todo_id): StrictPath<TodoId>,
) -> Result<StatusCode, AppError> {
    let resource = todo_id.to_string();
    let actor = state
        .authenticate(&headers, action::TODOS_DELETE, Some(resource))
        .await?;
    let request_id = required_request_id()?;
    state.todos.delete(&actor, todo_id, &request_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/v1/todos/{todo_id}/notes`.
pub(crate) async fn list_notes(
    State(state): State<AppState>,
    headers: HeaderMap,
    StrictPath(todo_id): StrictPath<TodoId>,
    StrictQuery(query): StrictQuery<CollectionQuery>,
) -> Result<Json<crate::domain::Page<crate::domain::TodoNote>>, AppError> {
    let resource = todo_id.to_string();
    let actor = state
        .authenticate(&headers, action::TODO_NOTES_LIST, Some(resource))
        .await?;
    state
        .todos
        .list_notes(&actor, todo_id, query)
        .await
        .map(Json)
}

/// `POST /api/v1/todos/{todo_id}/notes`.
pub(crate) async fn add_note(
    State(state): State<AppState>,
    headers: HeaderMap,
    StrictPath(todo_id): StrictPath<TodoId>,
    Idempotency(key): Idempotency,
    StrictJson(input): StrictJson<TodoNoteCreate>,
) -> Result<Response, AppError> {
    let resource = todo_id.to_string();
    let actor = state
        .authenticate(&headers, action::TODO_NOTES_CREATE, Some(resource))
        .await?;
    let request_id = required_request_id()?;
    let mutation = state
        .todos
        .add_note(&actor, todo_id, input, key, &request_id)
        .await?;
    mutation_response(&state, mutation, None)
}

fn created_resource_location(
    mutation: &MutationResponse,
    collection: &str,
) -> Result<String, AppError> {
    let id = mutation
        .body
        .get("id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            AppError::Internal(anyhow::anyhow!(
                "stored mutation response omitted its resource identifier"
            ))
        })?;
    Ok(format!("{collection}/{id}"))
}

#[cfg(test)]
mod tests {
    use crate::application::idempotency::MutationResponse;

    use super::created_resource_location;

    #[test]
    fn creation_location_uses_the_replayed_resource_identifier() {
        let response = MutationResponse::replayed(
            201,
            serde_json::json!({ "id": "018f268d-715a-7b72-8f0f-41f16f9af553" }),
        );
        let location = created_resource_location(&response, "todos");
        assert!(matches!(
            location.as_deref(),
            Ok("todos/018f268d-715a-7b72-8f0f-41f16f9af553")
        ));
    }
}
