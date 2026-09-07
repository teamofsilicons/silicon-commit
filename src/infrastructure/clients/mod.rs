//! Hardened HTTP adapters for Silicon IAM, Briefcase, and Hook.

use std::time::Duration;

use http::{HeaderMap, StatusCode, header};
use thiserror::Error;
use url::Url;

use crate::application::ports::ProviderError;

pub mod briefcase;
pub mod hook;
pub mod iam;

/// Redacted adapter construction failure.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ClientBuildError {
    /// A configured URL cannot safely identify the expected HTTP endpoint.
    #[error("invalid provider endpoint configuration")]
    InvalidEndpoint,
    /// A configured provider credential cannot be represented safely in HTTP.
    #[error("invalid provider credential configuration")]
    InvalidCredential,
    /// The hardened HTTP client could not be constructed.
    #[error("failed to construct provider HTTP client")]
    HttpClient,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BoundedBodyError {
    Transport,
    TooLarge,
}

pub(crate) fn http_client(
    connect_timeout: Duration,
    request_timeout: Duration,
) -> Result<reqwest::Client, ClientBuildError> {
    reqwest::Client::builder()
        .connect_timeout(connect_timeout)
        .timeout(request_timeout)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("silicon-commit/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|_| ClientBuildError::HttpClient)
}

pub(crate) fn endpoint(base: &Url, relative: &str) -> Result<Url, ClientBuildError> {
    if !valid_http_url(base) || base.query().is_some() || base.fragment().is_some() {
        return Err(ClientBuildError::InvalidEndpoint);
    }

    let mut normalized = base.clone();
    if !normalized.path().ends_with('/') {
        normalized.set_path(&format!("{}/", normalized.path()));
    }
    normalized
        .join(relative)
        .map_err(|_| ClientBuildError::InvalidEndpoint)
}

pub(crate) fn exact_endpoint(url: &Url) -> Result<Url, ClientBuildError> {
    if !valid_http_url(url) || url.query().is_some() || url.fragment().is_some() {
        return Err(ClientBuildError::InvalidEndpoint);
    }
    Ok(url.clone())
}

pub(crate) async fn read_bounded(
    mut response: reqwest::Response,
    maximum: usize,
) -> Result<Vec<u8>, BoundedBodyError> {
    if maximum == 0
        || response
            .content_length()
            .is_some_and(|length| length > maximum as u64)
    {
        return Err(BoundedBodyError::TooLarge);
    }

    let mut body = Vec::with_capacity(maximum.min(8_192));
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| BoundedBodyError::Transport)?
    {
        if body.len().saturating_add(chunk.len()) > maximum {
            return Err(BoundedBodyError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

pub(crate) fn is_json(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|media_type| media_type.trim().eq_ignore_ascii_case("application/json"))
}

pub(crate) fn provider_status(status: StatusCode, headers: &HeaderMap) -> ProviderError {
    match status {
        StatusCode::UNAUTHORIZED => ProviderError::Unauthenticated,
        StatusCode::FORBIDDEN => ProviderError::Forbidden,
        StatusCode::NOT_FOUND | StatusCode::GONE => ProviderError::NotFound,
        StatusCode::CONFLICT => ProviderError::Conflict,
        StatusCode::TOO_MANY_REQUESTS => ProviderError::RateLimited {
            retry_after: retry_after(headers),
        },
        _ => ProviderError::Unavailable,
    }
}

pub(crate) fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    const MAX_RETRY_AFTER_SECONDS: u64 = 86_400;
    let seconds = headers
        .get(header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .parse::<u64>()
        .ok()?;
    (seconds <= MAX_RETRY_AFTER_SECONDS).then(|| Duration::from_secs(seconds))
}

fn valid_http_url(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
        && url.password().is_none()
        && url.username().is_empty()
        && !url.cannot_be_a_base()
}

pub mod scoped_identity;
