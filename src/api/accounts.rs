//! Account HTTP handlers: public sign-in metadata, `GET /me`, and the Silicon allow-list.

use axum::{Json, extract::State, http::HeaderMap};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    application::accounts::{AllowlistView, MeView},
    domain::ActorId,
    error::AppError,
};

use super::{AppState, auth::action, extract::StrictPath};

/// `GET /api/v1/accounts`: how to sign in to Commit (public).
pub(crate) async fn metadata(State(state): State<AppState>) -> Json<Value> {
    Json(json!({
        "app_id": state.app_id,
        "accounts_url": state.accounts_url,
        "authorize_url": format!("{}/authorize", state.accounts_url.trim_end_matches('/')),
        "token_url": format!("{}/v1/oauth/token", state.accounts_url.trim_end_matches('/')),
        "device_authorization_url": format!("{}/v1/device/authorize", state.accounts_url.trim_end_matches('/')),
        "scopes": action::ALL,
        "credentials": ["Authorization: Bearer <Silicon Accounts access token issued to commit>", "Authorization: Proof <sap_… User verification proof>"],
    }))
}

/// `GET /api/v1/me`: the authenticated account as Commit sees it.
pub(crate) async fn me(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<MeView>, AppError> {
    let actor = state.authenticate(&headers, action::ME_READ, None).await?;
    state.accounts.me(&actor, &state.app_id).await.map(Json)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SiliconPath {
    silicon: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AllowPath {
    silicon: String,
    account: String,
}

fn account_id(field: &'static str, value: &str) -> Result<ActorId, AppError> {
    ActorId::new(value).map_err(|error| AppError::Validation {
        details: json!({ (field): error.to_string() }),
    })
}

/// `GET /api/v1/silicons/{silicon}/allowed-accounts`.
pub(crate) async fn allowlist(
    State(state): State<AppState>,
    headers: HeaderMap,
    StrictPath(path): StrictPath<SiliconPath>,
) -> Result<Json<AllowlistView>, AppError> {
    let actor = state
        .authenticate(&headers, action::ALLOWLIST_READ, Some(path.silicon.clone()))
        .await?;
    let silicon = account_id("silicon", &path.silicon)?;
    state.accounts.allowlist(&actor, &silicon).await.map(Json)
}

/// `PUT /api/v1/silicons/{silicon}/allowed-accounts/{account}`.
pub(crate) async fn allow(
    State(state): State<AppState>,
    headers: HeaderMap,
    StrictPath(path): StrictPath<AllowPath>,
) -> Result<Json<AllowlistView>, AppError> {
    let actor = state
        .authenticate_sensitive(
            &headers,
            action::ALLOWLIST_UPDATE,
            Some(path.silicon.clone()),
        )
        .await?;
    let silicon = account_id("silicon", &path.silicon)?;
    let account = account_id("account", &path.account)?;
    state
        .accounts
        .allow(&actor, &silicon, &account)
        .await
        .map(Json)
}

/// `DELETE /api/v1/silicons/{silicon}/allowed-accounts/{account}`.
pub(crate) async fn disallow(
    State(state): State<AppState>,
    headers: HeaderMap,
    StrictPath(path): StrictPath<AllowPath>,
) -> Result<Json<AllowlistView>, AppError> {
    let actor = state
        .authenticate_sensitive(
            &headers,
            action::ALLOWLIST_UPDATE,
            Some(path.silicon.clone()),
        )
        .await?;
    let silicon = account_id("silicon", &path.silicon)?;
    let account = account_id("account", &path.account)?;
    state
        .accounts
        .disallow(&actor, &silicon, &account)
        .await
        .map(Json)
}
