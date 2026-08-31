//! Attachment HTTP handlers.

use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
};

use crate::{
    application::attachments::{TemporaryUrlRequest, TemporaryUrlResponse},
    error::AppError,
};

use super::{AppState, auth::action, extract::StrictJson};

/// `POST /api/v1/attachments/temporary-url`.
pub(crate) async fn temporary_url(
    State(state): State<AppState>,
    headers: HeaderMap,
    StrictJson(input): StrictJson<TemporaryUrlRequest>,
) -> Result<(StatusCode, Json<TemporaryUrlResponse>), AppError> {
    let resource = input.permanent_url.as_str().to_owned();
    let actor = state
        .authenticate(&headers, action::ATTACHMENTS_TEMPORARY_URL, Some(resource))
        .await?;
    let response = state.attachments.temporary_url(&actor, input).await?;
    Ok((StatusCode::CREATED, Json(response)))
}
