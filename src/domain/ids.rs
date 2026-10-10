//! Strongly typed resource and identity identifiers.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// Maximum length of a public account identifier stored as a snapshot.
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

/// Maximum length of a Silicon Accounts uuid (placeholders for IAM-era principals are longer).
pub const MAX_ACCOUNT_UUID_BYTES: usize = 160;

/// Permanent Silicon Accounts account identifier: the access token `sub`.
///
/// Accounts uuids are short, case-sensitive strings such as `zQo`, not RFC 4122
/// UUIDs: they are compared exactly and never lowercased. Rows created before
/// the move to Silicon Accounts belong to placeholders named
/// `iam:<organization>:<principal>` until an operator links them.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, sqlx::Type)]
#[serde(transparent)]
#[sqlx(transparent)]
pub struct AccountUuid(String);

/// Invalid account uuid.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum AccountUuidError {
    /// The value is empty.
    #[error("account uuid must not be empty")]
    Empty,
    /// The value is longer than any account uuid.
    #[error("account uuid must contain at most {MAX_ACCOUNT_UUID_BYTES} bytes")]
    TooLong,
    /// Whitespace and control characters never occur in account uuids.
    #[error("account uuid must not contain whitespace or control characters")]
    InvalidCharacter,
}

impl AccountUuid {
    /// Validates an exact (untrimmed, case-preserved) account uuid.
    pub fn new(value: impl Into<String>) -> Result<Self, AccountUuidError> {
        let value = value.into();
        if value.is_empty() {
            return Err(AccountUuidError::Empty);
        }
        if value.len() > MAX_ACCOUNT_UUID_BYTES {
            return Err(AccountUuidError::TooLong);
        }
        if value
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
        {
            return Err(AccountUuidError::InvalidCharacter);
        }
        Ok(Self(value))
    }

    /// Borrows the exact uuid.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consumes the uuid.
    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }

    /// Reports whether this is an unlinked IAM-era placeholder, which can never sign in.
    #[must_use]
    pub fn is_placeholder(&self) -> bool {
        self.0.starts_with("iam:")
    }

    /// A stable UUID naming this account as an audited resource (`UUIDv5` of the uuid).
    #[must_use]
    pub fn resource_id(&self) -> Uuid {
        // Fixed namespace for Commit account resources; never change it.
        const NAMESPACE: Uuid = Uuid::from_u128(0x5c0a_7c11_2b6e_4f0e_9d1a_0c0f_ac0c_0a75);
        Uuid::new_v5(&NAMESPACE, self.0.as_bytes())
    }
}

impl<'de> Deserialize<'de> for AccountUuid {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::new(raw).map_err(serde::de::Error::custom)
    }
}

impl FromStr for AccountUuid {
    type Err = AccountUuidError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl fmt::Display for AccountUuid {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Public `c:`/`si:` account id, or (in requests) an account uuid.
///
/// Ids are display data: they can change, and the account uuid is what Commit
/// stores. A deleted account's id is empty in responses.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, sqlx::Type)]
#[serde(transparent)]
#[sqlx(transparent)]
pub struct ActorId(String);

impl ActorId {
    /// Validates and normalizes a public actor ID.
    pub fn new(value: impl Into<String>) -> Result<Self, PublicIdError> {
        normalize_public_id(value.into()).map(Self)
    }

    /// Wraps an id read back from Commit's own storage; a deleted account's id is empty.
    #[must_use]
    pub fn from_persisted(value: String) -> Self {
        Self(value)
    }

    /// The account kind named by a `c:` or `si:` prefix, if any.
    #[must_use]
    pub fn prefixed_kind(&self) -> Option<super::actor::ActorType> {
        let lower = self.0.to_ascii_lowercase();
        if lower.starts_with("c:") {
            Some(super::actor::ActorType::Carbon)
        } else if lower.starts_with("si:") {
            Some(super::actor::ActorType::Silicon)
        } else {
            None
        }
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
