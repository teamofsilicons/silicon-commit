//! Strict JSON, query, and conditional-header extractors.

use std::ops::{Deref, DerefMut};

use axum::{
    Json,
    extract::{
        FromRequest, FromRequestParts, Path, Query, Request,
        rejection::{JsonRejection, PathRejection},
    },
    http::{StatusCode, request::Parts},
};
use serde::de::DeserializeOwned;

use crate::{
    application::idempotency::IdempotencyKey, domain::ExpectedDiaryVersion, error::AppError,
};

/// JSON extractor that maps framework rejections into the public error shape.
#[derive(Clone, Copy, Debug)]
pub struct StrictJson<T>(pub T);

impl<T> Deref for StrictJson<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> DerefMut for StrictJson<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<S, T> FromRequest<S> for StrictJson<T>
where
    S: Send + Sync,
    T: DeserializeOwned + Send,
{
    type Rejection = AppError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(request, state).await {
            Ok(Json(value)) => Ok(Self(value)),
            Err(rejection) => Err(map_json_rejection(&rejection)),
        }
    }
}

fn map_json_rejection(rejection: &JsonRejection) -> AppError {
    match rejection.status() {
        StatusCode::PAYLOAD_TOO_LARGE => AppError::PayloadTooLarge,
        StatusCode::UNSUPPORTED_MEDIA_TYPE => AppError::BadRequest {
            code: "unsupported_media_type".into(),
        },
        StatusCode::UNPROCESSABLE_ENTITY => AppError::Validation {
            details: serde_json::json!({ "body": "The JSON document does not match the schema." }),
        },
        _ => AppError::BadRequest {
            code: "invalid_json".into(),
        },
    }
}

/// Query extractor with stable validation errors.
#[derive(Clone, Copy, Debug)]
pub struct StrictQuery<T>(pub T);

impl<T> Deref for StrictQuery<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<S, T> FromRequestParts<S> for StrictQuery<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        Query::<T>::from_request_parts(parts, state)
            .await
            .map(|Query(value)| Self(value))
            .map_err(|_| AppError::Validation {
                details: serde_json::json!({ "query": "The query parameters are invalid." }),
            })
    }
}

/// Path extractor that normalizes framework parsing failures.
#[derive(Clone, Copy, Debug)]
pub struct StrictPath<T>(pub T);

impl<T> Deref for StrictPath<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<S, T> FromRequestParts<S> for StrictPath<T>
where
    S: Send + Sync,
    T: DeserializeOwned + Send,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        Path::<T>::from_request_parts(parts, state)
            .await
            .map(|Path(value)| Self(value))
            .map_err(|rejection: PathRejection| AppError::BadRequest {
                code: match rejection {
                    PathRejection::FailedToDeserializePathParams(_) => "invalid_path_parameter",
                    _ => "invalid_path",
                }
                .into(),
            })
    }
}

/// Required mutation replay key.
#[derive(Clone, Debug)]
pub struct Idempotency(pub IdempotencyKey);

impl<S> FromRequestParts<S> for Idempotency
where
    S: Send + Sync,
{
    type Rejection = AppError;

    #[allow(clippy::unused_async_trait_impl)]
    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let mut values = parts.headers.get_all("idempotency-key").iter();
        let value = values.next().ok_or_else(|| AppError::BadRequest {
            code: "idempotency_key_required".into(),
        })?;
        if values.next().is_some() {
            return Err(AppError::BadRequest {
                code: "invalid_idempotency_key".into(),
            });
        }
        let value = value.to_str().map_err(|_| AppError::BadRequest {
            code: "invalid_idempotency_key".into(),
        })?;
        IdempotencyKey::new(value)
            .map(Self)
            .map_err(|_| AppError::Validation {
                details: serde_json::json!({
                    "idempotency_key": "Must contain 8–255 visible ASCII characters."
                }),
            })
    }
}

/// Required positive diary version from `If-Match`.
#[derive(Clone, Copy, Debug)]
pub struct IfMatch(pub ExpectedDiaryVersion);

impl<S> FromRequestParts<S> for IfMatch
where
    S: Send + Sync,
{
    type Rejection = AppError;

    #[allow(clippy::unused_async_trait_impl)]
    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let mut values = parts.headers.get_all(http::header::IF_MATCH).iter();
        let raw = values.next().ok_or(AppError::PreconditionRequired)?;
        if values.next().is_some() {
            return Err(AppError::BadRequest {
                code: "invalid_if_match".into(),
            });
        }
        let raw = raw.to_str().map_err(|_| AppError::BadRequest {
            code: "invalid_if_match".into(),
        })?;
        parse_if_match(raw).map(Self)
    }
}

fn parse_if_match(raw: &str) -> Result<ExpectedDiaryVersion, AppError> {
    if raw.starts_with("W/") || raw == "*" {
        return Err(AppError::BadRequest {
            code: "invalid_if_match".into(),
        });
    }
    let raw = raw
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .ok_or_else(|| AppError::BadRequest {
            code: "invalid_if_match".into(),
        })?;
    if !matches!(raw.as_bytes(), [b'1'..=b'9', rest @ ..] if rest.iter().all(u8::is_ascii_digit)) {
        return Err(AppError::BadRequest {
            code: "invalid_if_match".into(),
        });
    }
    let version = raw.parse::<u64>().map_err(|_| AppError::BadRequest {
        code: "invalid_if_match".into(),
    })?;
    ExpectedDiaryVersion::new(version).map_err(|_| AppError::BadRequest {
        code: "invalid_if_match".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::{IfMatch, parse_if_match};

    #[test]
    fn if_match_wrapper_is_copyable() {
        let Ok(version) = crate::domain::ExpectedDiaryVersion::new(3) else {
            return;
        };
        let version = IfMatch(version);
        assert_eq!(version.0.get(), 3);
    }

    #[test]
    fn if_match_requires_one_strong_quoted_version() {
        assert!(matches!(
            parse_if_match("\"3\"").map(crate::domain::ExpectedDiaryVersion::get),
            Ok(3)
        ));
        for invalid in [
            "3", "W/\"3\"", "*", "\"\"", "\"0\"", "\"01\"", "\"+1\"", "\"3\"\"",
        ] {
            assert!(parse_if_match(invalid).is_err(), "accepted {invalid:?}");
        }
    }
}
