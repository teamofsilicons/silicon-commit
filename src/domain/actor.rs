//! Carbon and Silicon identity types.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

use super::ids::{AccountUuid, ActorId};

/// Account kinds which may own or receive Commit work.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "actor_type", rename_all = "snake_case")]
pub enum ActorType {
    /// A Carbon account (`c:` ids).
    Carbon,
    /// A Silicon account (`si:` ids).
    Silicon,
}

impl ActorType {
    /// Returns the stable wire/database spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Carbon => "carbon",
            Self::Silicon => "silicon",
        }
    }
}

impl fmt::Display for ActorType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Failure to parse an actor type from a protocol value.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("actor type must be 'carbon' or 'silicon'")]
pub struct ParseActorTypeError;

impl FromStr for ActorType {
    type Err = ParseActorTypeError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "carbon" => Ok(Self::Carbon),
            "silicon" => Ok(Self::Silicon),
            _ => Err(ParseActorTypeError),
        }
    }
}

/// Public account reference in API responses: `{"type", "id", "uuid"}`.
///
/// `uuid` is permanent; `id` is the account's current `c:`/`si:` id (empty for
/// a deleted account).
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ActorRef {
    /// Account kind.
    #[serde(rename = "type")]
    pub actor_type: ActorType,
    /// Current public id.
    #[serde(deserialize_with = "persisted_actor_id")]
    pub id: ActorId,
    /// Permanent account uuid.
    pub uuid: AccountUuid,
}

fn persisted_actor_id<'de, D>(deserializer: D) -> Result<ActorId, D::Error>
where
    D: Deserializer<'de>,
{
    String::deserialize(deserializer).map(ActorId::from_persisted)
}

impl ActorRef {
    /// Creates a public account reference.
    #[must_use]
    pub const fn new(actor_type: ActorType, id: ActorId, uuid: AccountUuid) -> Self {
        Self {
            actor_type,
            id,
            uuid,
        }
    }
}

/// Resolved account used for authorization and persistence.
///
/// Relationships are keyed by `uuid`; `id` is display data that can change.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize)]
pub struct Actor {
    /// Permanent Silicon Accounts uuid.
    pub uuid: AccountUuid,
    /// Account kind.
    #[serde(rename = "type")]
    pub actor_type: ActorType,
    /// Current public id.
    pub id: ActorId,
}

impl Actor {
    /// Creates a resolved account.
    #[must_use]
    pub const fn new(uuid: AccountUuid, actor_type: ActorType, id: ActorId) -> Self {
        Self {
            uuid,
            actor_type,
            id,
        }
    }

    /// Returns the public representation.
    #[must_use]
    pub fn public_ref(&self) -> ActorRef {
        ActorRef::new(self.actor_type, self.id.clone(), self.uuid.clone())
    }

    /// Reports whether this account is a Silicon.
    #[must_use]
    pub const fn is_silicon(&self) -> bool {
        matches!(self.actor_type, ActorType::Silicon)
    }
}

impl From<&Actor> for ActorRef {
    fn from(actor: &Actor) -> Self {
        actor.public_ref()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{Actor, ActorRef, ActorType};
    use crate::domain::ids::{AccountUuid, ActorId};

    #[test]
    fn serialized_accounts_carry_type_current_id_and_permanent_uuid() {
        let (Ok(uuid), Ok(id)) = (AccountUuid::new("zQo"), ActorId::new("si:scout")) else {
            panic!("fixture ids are valid");
        };
        let actor = Actor::new(uuid, ActorType::Silicon, id);
        assert_eq!(
            serde_json::to_value(&actor).ok(),
            Some(json!({ "uuid": "zQo", "type": "silicon", "id": "si:scout" }))
        );
        assert_eq!(
            serde_json::to_value(actor.public_ref()).ok(),
            Some(json!({ "type": "silicon", "id": "si:scout", "uuid": "zQo" }))
        );
    }

    #[test]
    fn persisted_references_accept_the_empty_id_of_a_deleted_account() {
        let parsed: Result<ActorRef, _> =
            serde_json::from_value(json!({ "type": "carbon", "id": "", "uuid": "8HV" }));
        assert_eq!(
            parsed.ok().map(|actor| actor.id.into_inner()),
            Some(String::new())
        );
    }

    #[test]
    fn account_uuids_are_exact_and_case_sensitive() {
        assert_ne!(AccountUuid::new("zQo").ok(), AccountUuid::new("zqo").ok());
        assert!(AccountUuid::new(" zQo").is_err());
        assert!(AccountUuid::new("").is_err());
        assert!(AccountUuid::new("iam:org:principal").is_ok_and(|uuid| uuid.is_placeholder()));
    }
}
