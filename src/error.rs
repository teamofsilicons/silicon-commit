//! Stable application errors and their redacted HTTP representation.

use std::borrow::Cow;

use axum::{Json, http::StatusCode, response::IntoResponse};
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;
use tracing::error;

/// Error returned across application boundaries.
#[derive(Debug, Error)]
pub enum AppError {
    /// HTTP syntax or protocol input is malformed.
    #[error("the request is malformed")]
    BadRequest {
        /// Stable error code.
        code: Cow<'static, str>,
    },
    /// Syntactically valid input violates domain validation.
    #[error("request validation failed")]
    Validation {
        /// Structured field-level detail safe to expose.
        details: Value,
    },
    /// Credential is absent, invalid, expired, or revoked.
    #[error("authentication is required")]
    Unauthenticated,
    /// Authenticated actor lacks authority.
    #[error("the actor is not authorized for this action")]
    Forbidden,
    /// Resource does not exist in the caller's organization-visible scope.
    #[error("resource was not found")]
    NotFound,
    /// Mutation conflicts with current state.
    #[error("request conflicts with current state")]
    Conflict {
        /// Stable conflict reason.
        code: Cow<'static, str>,
    },
    /// An optimistic concurrency precondition is missing.
    #[error("a request precondition is required")]
    PreconditionRequired,
    /// Request exceeded an abuse-control limit.
    #[error("rate limit exceeded")]
    RateLimited {
        /// Delay before retrying.
        retry_after_seconds: u64,
    },
    /// Request processing exceeded the deadline.
    #[error("request processing deadline exceeded")]
    Timeout,
    /// Request body exceeds configured maximum.
    #[error("request body is too large")]
    PayloadTooLarge,
    /// Route exists but not for this method.
    #[error("method is not allowed for this route")]
    MethodNotAllowed,
    /// Provider returned an invalid response.
    #[error("an external provider returned an invalid response")]
    BadGateway,
    /// Required provider is temporarily unavailable.
    #[error("an external provider is unavailable")]
    ProviderUnavailable,
    /// Unexpected failure whose details must not cross the API boundary.
    #[error("internal service error")]
    Internal(#[source] anyhow::Error),
}

/// Documented JSON error envelope.
#[derive(Debug, Serialize)]
struct ErrorEnvelope {
    error: PublicError,
}

#[derive(Debug, Serialize)]
struct PublicError {
    code: Cow<'static, str>,
    message: Cow<'static, str>,
    request_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<Value>,
}

impl IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        let retry_after_seconds = match &self {
            Self::RateLimited {
                retry_after_seconds,
            } => Some(*retry_after_seconds),
            _ => None,
        };
        let (status, code, message, details) = self.public_parts();
        let mut response = (
            status,
            Json(ErrorEnvelope {
                error: PublicError {
                    code,
                    message,
                    request_id: request_id(),
                    details,
                },
            }),
        )
            .into_response();
        if let Some(seconds) = retry_after_seconds
            && let Ok(value) = seconds.to_string().parse()
        {
            response
                .headers_mut()
                .insert(http::header::RETRY_AFTER, value);
        }
        if status == StatusCode::UNAUTHORIZED {
            response.headers_mut().insert(
                http::header::WWW_AUTHENTICATE,
                http::HeaderValue::from_static("Bearer"),
            );
        }
        response
    }
}

impl AppError {
    fn public_parts(
        self,
    ) -> (
        StatusCode,
        Cow<'static, str>,
        Cow<'static, str>,
        Option<Value>,
    ) {
        match self {
            Self::BadRequest { code } => (
                StatusCode::BAD_REQUEST,
                code,
                Cow::Borrowed("The request is malformed."),
                None,
            ),
            Self::Validation { details } => (
                StatusCode::UNPROCESSABLE_ENTITY,
                Cow::Borrowed("validation_failed"),
                Cow::Borrowed("The request contains invalid data."),
                Some(details),
            ),
            Self::Unauthenticated => (
                StatusCode::UNAUTHORIZED,
                Cow::Borrowed("unauthenticated"),
                Cow::Borrowed("Authentication is required."),
                None,
            ),
            Self::Forbidden => (
                StatusCode::FORBIDDEN,
                Cow::Borrowed("forbidden"),
                Cow::Borrowed("The actor is not authorized for this action."),
                None,
            ),
            Self::NotFound => (
                StatusCode::NOT_FOUND,
                Cow::Borrowed("not_found"),
                Cow::Borrowed("The requested resource was not found."),
                None,
            ),
            Self::Conflict { code } => (
                StatusCode::CONFLICT,
                code,
                Cow::Borrowed("The request conflicts with the current resource state."),
                None,
            ),
            Self::PreconditionRequired => (
                StatusCode::PRECONDITION_REQUIRED,
                Cow::Borrowed("precondition_required"),
                Cow::Borrowed("A required request precondition is missing."),
                None,
            ),
            Self::RateLimited {
                retry_after_seconds,
            } => (
                StatusCode::TOO_MANY_REQUESTS,
                Cow::Borrowed("rate_limited"),
                Cow::Borrowed("Too many requests. Retry later."),
                Some(serde_json::json!({ "retry_after_seconds": retry_after_seconds })),
            ),
            Self::Timeout => (
                StatusCode::REQUEST_TIMEOUT,
                Cow::Borrowed("request_timeout"),
                Cow::Borrowed("The request exceeded its processing deadline."),
                None,
            ),
            Self::PayloadTooLarge => (
                StatusCode::PAYLOAD_TOO_LARGE,
                Cow::Borrowed("payload_too_large"),
                Cow::Borrowed("The request body exceeds the allowed size."),
                None,
            ),
            Self::MethodNotAllowed => (
                StatusCode::METHOD_NOT_ALLOWED,
                Cow::Borrowed("method_not_allowed"),
                Cow::Borrowed("The HTTP method is not allowed for this route."),
                None,
            ),
            Self::BadGateway => (
                StatusCode::BAD_GATEWAY,
                Cow::Borrowed("invalid_provider_response"),
                Cow::Borrowed("A required provider returned an invalid response."),
                None,
            ),
            Self::ProviderUnavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                Cow::Borrowed("provider_unavailable"),
                Cow::Borrowed("A required provider is temporarily unavailable."),
                None,
            ),
            Self::Internal(source) => {
                error!(error = ?source, "unhandled internal application error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Cow::Borrowed("internal_error"),
                    Cow::Borrowed("An internal service error occurred."),
                    None,
                )
            }
        }
    }
}

fn request_id() -> String {
    crate::request_context::current_request_id().unwrap_or_else(|| "unavailable".to_owned())
}

impl From<sqlx::Error> for AppError {
    fn from(error: sqlx::Error) -> Self {
        Self::Internal(error.into())
    }
}

impl From<crate::domain::ValidationError> for AppError {
    fn from(error: crate::domain::ValidationError) -> Self {
        let field = error.field;
        let message = error.kind.to_string();
        Self::Validation {
            details: serde_json::json!({ (field): message }),
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::response::IntoResponse as _;

    use super::AppError;

    #[test]
    fn unauthenticated_responses_advertise_bearer_authentication() {
        let response = AppError::Unauthenticated.into_response();

        assert_eq!(
            response
                .headers()
                .get(http::header::WWW_AUTHENTICATE)
                .and_then(|value| value.to_str().ok()),
            Some("Bearer")
        );
    }
}
