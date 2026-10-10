//! Typed errors. Each one says what went wrong and why; API errors keep the service's
//! `{"error":{"code","message","hint","details","request_id"}}` envelope.

use reqwest::StatusCode;
use reqwest::header::HeaderMap;
use serde::Serialize;
use serde_json::Value;
use std::fmt;

/// Largest `details` value kept from an error body (serialized bytes).
const MAX_DETAILS_BYTES: usize = 4096;

/// Everything that can go wrong in this crate.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The Commit API URL cannot be used; nothing was sent.
    #[error("The Commit API URL `{url}` cannot be used: {reason}")]
    InvalidUrl {
        /// The URL as given.
        url: String,
        /// Why it was refused.
        reason: &'static str,
    },
    /// A value passed to the client was refused before anything was sent.
    #[error("{0}")]
    Invalid(String),
    /// The request never got an answer (DNS, connection, TLS, timeout).
    #[error("Could not reach the Commit API at {base}: {cause}")]
    Transport {
        /// The API origin.
        base: String,
        /// The innermost cause, without URLs or credentials.
        cause: String,
        /// The transport error (its URL is removed).
        #[source]
        source: reqwest::Error,
    },
    /// The Commit API answered with an error.
    #[error("{0}")]
    Api(Box<ApiError>),
    /// The Commit API answered with a contract this client does not speak.
    #[error(
        "The Commit API answered with contract {served}, but this client speaks contract 2 (Silicon Accounts sign-in). Update silicon-commit-client or the commit CLI."
    )]
    UnsupportedContract {
        /// The `X-Commit-API-Version` the API answered with.
        served: String,
    },
    /// The response body was larger than 8 MiB, so it was not read.
    #[error("The Commit API response exceeds the 8 MiB limit, so it was not read.")]
    ResponseTooLarge,
    /// The response body was not the JSON this client expects.
    #[error("The Commit API response is not the JSON this client expects: {0}")]
    Decode(#[source] serde_json::Error),
    /// Silicon Accounts refused or failed a sign-in step (device code, short-lived
    /// token, refresh or revocation). The inner error carries the code, a precise message
    /// and a hint.
    #[error("{0}")]
    Accounts(#[from] silicon_accounts_client::Error),
}

impl Error {
    /// Stable machine-readable code: the API's `error.code`, the Silicon Accounts code
    /// (`invalid_grant`, `access_denied`, `expired_token`, …), or a client-side code.
    pub fn code(&self) -> &str {
        match self {
            Self::InvalidUrl { .. } => "invalid_url",
            Self::Invalid(_) => "invalid_input",
            Self::Transport { .. } => "connection_failed",
            Self::Api(api) => &api.code,
            Self::UnsupportedContract { .. } => "unsupported_contract",
            Self::ResponseTooLarge => "response_too_large",
            Self::Decode(_) => "unexpected_response",
            Self::Accounts(error) => error.code(),
        }
    }

    /// HTTP status of the failed response, if there was one.
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Api(api) => Some(api.status),
            Self::Accounts(error) => error.status(),
            _ => None,
        }
    }

    /// The request id of the failed response, if any (quote it when reporting a bug).
    pub fn request_id(&self) -> Option<&str> {
        match self {
            Self::Api(api) => api.request_id.as_deref(),
            Self::Accounts(error) => error.request_id(),
            _ => None,
        }
    }

    /// What to do next, when known.
    pub fn hint(&self) -> Option<String> {
        match self {
            Self::Api(api) => api.hint.clone(),
            Self::Accounts(error) => error.hint(),
            _ => None,
        }
    }

    /// The API error envelope, when this is an API error.
    pub fn as_api(&self) -> Option<&ApiError> {
        match self {
            Self::Api(api) => Some(api),
            _ => None,
        }
    }

    /// True when the Commit API refused the credential (HTTP 401): refresh the access
    /// token (or sign in again) and retry.
    pub fn is_unauthenticated(&self) -> bool {
        matches!(self, Self::Api(api) if api.status == 401)
    }

    /// True when Silicon Accounts refused a refresh token or short-lived token for good
    /// (`invalid_grant`): the sign-in ended or the token cannot be used, so sign in again.
    pub fn is_sign_in_refused(&self) -> bool {
        matches!(self, Self::Accounts(error) if error.code() == "invalid_grant")
    }

    /// True when nobody answered or the answer was a temporary failure (5xx, 429): the
    /// request may or may not have been applied.
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Transport { .. } => true,
            Self::Api(api) => api.status >= 500 || api.status == 429,
            Self::Accounts(error) => {
                error.is_transport() || error.status().is_some_and(|s| s >= 500 || s == 429)
            }
            _ => false,
        }
    }
}

/// The Commit API's error envelope plus transport metadata.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[non_exhaustive]
pub struct ApiError {
    /// HTTP status code.
    pub status: u16,
    /// Stable machine-readable code, e.g. `validation_failed`, `silicon_not_reachable`.
    pub code: String,
    /// What went wrong and why.
    pub message: String,
    /// What to do next, when the service gave one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// Structured details the service marks safe to show (field errors, limits).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    /// The request id (`X-Request-Id` or `error.request_id`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// Seconds to wait before retrying (`Retry-After`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_seconds: Option<u64>,
}

impl ApiError {
    /// Creates an error value (useful in tests and mocks).
    pub fn new(status: u16, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            status,
            code: code.into(),
            message: message.into(),
            hint: None,
            details: None,
            request_id: None,
            retry_after_seconds: None,
        }
    }

    /// Builds the error for a non-2xx answer. Only the documented envelope fields are
    /// kept; any other body (a proxy page, the wrong URL) is dropped, never echoed.
    pub(crate) fn from_response(status: StatusCode, headers: &HeaderMap, body: &[u8]) -> Self {
        let header = |name: &str| {
            headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
        };
        let envelope = serde_json::from_slice::<Value>(body)
            .ok()
            .and_then(|v| v.get("error").cloned())
            .filter(Value::is_object);
        let text = |key: &str| {
            envelope
                .as_ref()
                .and_then(|e| e.get(key))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
        };
        let code = text("code")
            .filter(|c| c.len() <= 128 && c.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
            .unwrap_or_else(|| format!("http_{}", status.as_u16()));
        let reason = status.canonical_reason().unwrap_or("error");
        let message = text("message").unwrap_or_else(|| {
            if envelope.is_some() {
                format!("The Commit API answered HTTP {} {reason} ({code}).", status.as_u16())
            } else {
                format!(
                    "The Commit API answered HTTP {} {reason} without its JSON error body; a proxy answered, or the URL does not point at the Commit API.",
                    status.as_u16()
                )
            }
        });
        let details = envelope
            .as_ref()
            .and_then(|e| e.get("details"))
            .filter(|d| !d.is_null())
            .filter(|d| serde_json::to_vec(d).is_ok_and(|b| b.len() <= MAX_DETAILS_BYTES))
            .cloned();
        Self {
            status: status.as_u16(),
            code,
            message,
            hint: text("hint"),
            details,
            request_id: header("x-request-id").or_else(|| text("request_id")),
            retry_after_seconds: header("retry-after").and_then(|v| v.parse().ok()),
        }
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (HTTP {} {}", self.message, self.status, self.code)?;
        if let Some(id) = &self.request_id {
            write!(f, ", request ID {id}")?;
        }
        f.write_str(")")?;
        if let Some(details) = &self.details {
            write!(f, " Details: {details}")?;
        }
        if let Some(seconds) = self.retry_after_seconds {
            write!(f, " Retry after {seconds} s.")?;
        }
        if let Some(hint) = &self.hint {
            write!(f, " Hint: {hint}")?;
        }
        Ok(())
    }
}

impl From<ApiError> for Error {
    fn from(value: ApiError) -> Self {
        Self::Api(Box::new(value))
    }
}

/// The innermost message of an error chain (e.g. `Connection refused (os error 61)`).
pub(crate) fn root_cause(error: &(dyn std::error::Error + 'static)) -> String {
    let mut current = error;
    while let Some(next) = current.source() {
        current = next;
    }
    current.to_string()
}
