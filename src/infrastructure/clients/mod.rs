//! Hardened HTTP adapters for Silicon IAM and direct webhooks.

use std::time::Duration;

use http::{HeaderMap, header};
use thiserror::Error;

pub mod iam;
pub mod webhook;

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

pub mod scoped_identity;
