//! Strongly typed resource and identity identifiers.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// Maximum length of a public IAM identifier stored as a snapshot.
pub const MAX_PUBLIC_ID_CHARS: usize = 255;

/// Failure to parse or normalize an opaque public identifier.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum PublicIdError {
    /// The value is empty after trimming.
    #[error("identifier must not be empty")]
    Empty,
    /// The value exceeds the storage and protocol limit.
    #[error("identifier must contain at most {MAX_PUBLIC_ID_CHARS} characters")]
    TooLong,
    /// Control characters are not valid in an identifier.
    #[error("identifier must not contain control characters")]
    ControlCharacter,
}

macro_rules! uuid_id {
    ($name:ident, $docs:literal) => {
        #[doc = $docs]
        #[derive(
            Clone,
            Copy,
            Debug,
            Deserialize,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
            Serialize,
            sqlx::Type,
        )]
        #[serde(transparent)]
        #[sqlx(transparent)]
        pub struct $name(Uuid);

        impl $name {
            /// Wraps a UUID received from a trusted identity or persistence boundary.
            #[must_use]
            pub const fn from_uuid(value: Uuid) -> Self {
                Self(value)
            }

            /// Returns the underlying UUID.
            #[must_use]
            pub const fn into_uuid(self) -> Uuid {
                self.0
            }

            /// Borrows the underlying UUID.
            #[must_use]
            pub const fn as_uuid(&self) -> &Uuid {
                &self.0
            }

            /// Reports whether this value uses the `UUIDv7` layout required for new resources.
            #[must_use]
            pub const fn is_v7(self) -> bool {
                matches!(self.0.get_version_num(), 7)
            }
        }

        impl From<Uuid> for $name {
            fn from(value: Uuid) -> Self {
                Self::from_uuid(value)
            }
        }

        impl From<$name> for Uuid {
            fn from(value: $name) -> Self {
                value.into_uuid()
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(value).map(Self)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, formatter)
            }
        }
    };
}

uuid_id!(TodoId, "Unique persistent identifier for a todo.");
uuid_id!(TodoNoteId, "Unique persistent identifier for a todo note.");
uuid_id!(ProjectId, "Unique persistent identifier for a project.");
uuid_id!(
    ProjectTaskId,
    "Unique persistent identifier for a project task or subtask."
);
uuid_id!(
    ProjectEntryId,
    "Unique persistent identifier for a project blocker, update, or completion entry."
);
uuid_id!(
    PrincipalId,
    "Internal IAM identifier for an authenticated Carbon or Silicon principal."
);
uuid_id!(
    OrganizationId,
    "Internal IAM identifier used as the authoritative tenant key."
);

macro_rules! new_commit_id {
    ($name:ident) => {
        impl $name {
            /// Generates a time-sortable server-owned `UUIDv7`.
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }
    };
}

new_commit_id!(TodoId);
new_commit_id!(TodoNoteId);
new_commit_id!(ProjectId);
new_commit_id!(ProjectTaskId);
new_commit_id!(ProjectEntryId);

/// Public IAM actor ID exposed by the v1 API.
///
/// The internal [`PrincipalId`] remains the authorization and relationship key;
/// this value is retained as a display/API snapshot.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, sqlx::Type)]
#[serde(transparent)]
#[sqlx(transparent)]
pub struct ActorId(String);

impl ActorId {
    /// Validates and normalizes a public actor ID.
    pub fn new(value: impl Into<String>) -> Result<Self, PublicIdError> {
        normalize_public_id(value.into()).map(Self)
    }

    /// Borrows the normalized public ID.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consumes the ID.
    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl<'de> Deserialize<'de> for ActorId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::new(raw).map_err(serde::de::Error::custom)
    }
}

impl TryFrom<String> for ActorId {
    type Error = PublicIdError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl FromStr for ActorId {
    type Err = PublicIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl fmt::Display for ActorId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Public organization ID supplied in `X-Org-ID` and verified with IAM.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, sqlx::Type)]
#[serde(transparent)]
#[sqlx(transparent)]
pub struct PublicOrganizationId(String);

impl PublicOrganizationId {
    /// Validates and normalizes a public organization ID.
    pub fn new(value: impl Into<String>) -> Result<Self, PublicIdError> {
        normalize_public_id(value.into()).map(Self)
    }

    /// Borrows the normalized public ID.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consumes the ID.
    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl<'de> Deserialize<'de> for PublicOrganizationId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::new(raw).map_err(serde::de::Error::custom)
    }
}

impl TryFrom<String> for PublicOrganizationId {
    type Error = PublicIdError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl FromStr for PublicOrganizationId {
    type Err = PublicIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl fmt::Display for PublicOrganizationId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

fn normalize_public_id(value: String) -> Result<String, PublicIdError> {
    if value.chars().count() > MAX_PUBLIC_ID_CHARS {
        return Err(PublicIdError::TooLong);
    }
    let normalized = value.trim().to_owned();
    if normalized.is_empty() {
        return Err(PublicIdError::Empty);
    }
    if normalized.chars().any(char::is_control) {
        return Err(PublicIdError::ControlCharacter);
    }
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use uuid::Version;

    use super::{ActorId, TodoId};

    #[test]
    fn generated_resource_ids_are_uuid_v7() {
        let id = TodoId::new();
        assert!(id.is_v7());
        assert_eq!(id.as_uuid().get_version(), Some(Version::SortRand));
    }

    #[test]
    fn public_actor_id_is_trimmed() {
        let id = ActorId::new("  silicon-42  ");
        assert_eq!(id.map(ActorId::into_inner), Ok("silicon-42".to_owned()));
        assert!(matches!(
            ActorId::new(format!(" {} ", "x".repeat(super::MAX_PUBLIC_ID_CHARS))),
            Err(super::PublicIdError::TooLong)
        ));
    }

    proptest! {
        #[test]
        fn nonempty_printable_actor_ids_are_stable(value in "[a-z0-9:_-]{1,100}") {
            let id = ActorId::new(value.clone());
            prop_assert_eq!(id.map(ActorId::into_inner), Ok(value));
        }
    }
}
