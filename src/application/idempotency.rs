//! Idempotency-key validation and deterministic request fingerprinting.

use std::fmt;

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use thiserror::Error;

const MIN_KEY_BYTES: usize = 8;
const MAX_KEY_BYTES: usize = 255;

/// A validated caller-provided idempotency key.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    /// Validates a key without normalizing it.
    ///
    /// Keys are opaque and case-sensitive. Visible ASCII is required so the
    /// value remains safe in headers, diagnostics, and database indexes.
    pub fn new(value: impl Into<String>) -> Result<Self, IdempotencyKeyError> {
        let value = value.into();
        if !(MIN_KEY_BYTES..=MAX_KEY_BYTES).contains(&value.len()) {
            return Err(IdempotencyKeyError::Length);
        }
        if !value.bytes().all(|byte| (0x21..=0x7e).contains(&byte)) {
            return Err(IdempotencyKeyError::Characters);
        }
        Ok(Self(value))
    }

    /// Borrows the opaque key.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for IdempotencyKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Invalid idempotency header value.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum IdempotencyKeyError {
    /// Value is shorter than 8 or longer than 255 bytes.
    #[error("idempotency key must contain between 8 and 255 bytes")]
    Length,
    /// Value includes whitespace, a control byte, or non-ASCII data.
    #[error("idempotency key must contain visible ASCII characters only")]
    Characters,
}

/// SHA-256 digest binding a replay key to its exact semantic request.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RequestFingerprint([u8; 32]);

impl RequestFingerprint {
    /// Hashes the API operation, concrete resource path, and serialized input.
    ///
    /// # Errors
    ///
    /// Returns an error only if the request type's serializer fails.
    pub fn calculate<T: Serialize>(
        operation: &str,
        resource_path: &str,
        input: &T,
    ) -> Result<Self, serde_json::Error> {
        let body = serde_json::to_vec(input)?;
        let mut hasher = Sha256::new();
        append_framed(&mut hasher, operation.as_bytes());
        append_framed(&mut hasher, resource_path.as_bytes());
        append_framed(&mut hasher, &body);
        Ok(Self(hasher.finalize().into()))
    }

    /// Borrows the 32 digest bytes for persistence.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Metadata binding one mutation to its replay scope.
#[derive(Clone, Debug)]
pub struct MutationIdentity {
    /// Public `OpenAPI` operation identifier.
    pub operation: &'static str,
    /// Concrete organization-scoped resource path.
    pub resource_path: String,
    /// Validated caller key.
    pub key: IdempotencyKey,
    /// Semantic request fingerprint.
    pub fingerprint: RequestFingerprint,
}

impl MutationIdentity {
    /// Creates mutation metadata by hashing its request document.
    ///
    /// # Errors
    ///
    /// Returns an error if the request cannot be serialized.
    pub fn new<T: Serialize>(
        operation: &'static str,
        resource_path: impl Into<String>,
        key: IdempotencyKey,
        input: &T,
    ) -> Result<Self, serde_json::Error> {
        let resource_path = resource_path.into();
        let fingerprint = RequestFingerprint::calculate(operation, &resource_path, input)?;
        Ok(Self {
            operation,
            resource_path,
            key,
            fingerprint,
        })
    }
}

/// JSON response committed with an idempotent mutation.
#[derive(Clone, Debug)]
pub struct MutationResponse {
    /// HTTP status to replay.
    pub status: u16,
    /// Complete public response body.
    pub body: Value,
    /// Whether this response came from a prior committed request.
    pub replayed: bool,
}

impl MutationResponse {
    /// Creates a newly committed response.
    #[must_use]
    pub const fn created(status: u16, body: Value) -> Self {
        Self {
            status,
            body,
            replayed: false,
        }
    }

    /// Creates a replay response read from durable idempotency state.
    #[must_use]
    pub const fn replayed(status: u16, body: Value) -> Self {
        Self {
            status,
            body,
            replayed: true,
        }
    }
}

fn append_framed(hasher: &mut Sha256, value: &[u8]) {
    hasher.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
    hasher.update(value);
}

#[cfg(test)]
mod tests {
    use serde::Serialize;

    use super::{IdempotencyKey, RequestFingerprint};

    #[derive(Serialize)]
    struct Input<'a> {
        title: &'a str,
    }

    #[test]
    fn key_validation_matches_the_public_contract_and_header_safety() {
        assert!(IdempotencyKey::new("1234567").is_err());
        assert!(IdempotencyKey::new("contains space").is_err());
        assert!(IdempotencyKey::new("request-123").is_ok());
    }

    #[test]
    fn fingerprint_is_bound_to_operation_path_and_body() {
        let first =
            RequestFingerprint::calculate("createTodo", "/todos", &Input { title: "first" });
        let changed_body =
            RequestFingerprint::calculate("createTodo", "/todos", &Input { title: "second" });
        let changed_path =
            RequestFingerprint::calculate("createTodo", "/other", &Input { title: "first" });

        assert!(first.is_ok());
        assert_ne!(first.ok(), changed_body.ok());
        assert_ne!(
            RequestFingerprint::calculate("createTodo", "/todos", &Input { title: "first" },).ok(),
            changed_path.ok()
        );
    }
}
