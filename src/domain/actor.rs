//! Carbon and Silicon identity types.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::ids::{ActorId, PrincipalId};

/// Principal kinds which may own or receive Commit work.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "actor_type", rename_all = "snake_case")]
pub enum ActorType {
    /// Human account.
    Carbon,
    /// AI-agent account.
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

/// Public actor reference in the v1 API.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ActorRef {
    /// Principal kind.
    #[serde(rename = "type")]
    pub actor_type: ActorType,
    /// Stable public IAM actor ID.
    pub id: ActorId,
}

impl ActorRef {
    /// Creates a public actor reference.
    #[must_use]
    pub const fn new(actor_type: ActorType, id: ActorId) -> Self {
        Self { actor_type, id }
    }
}

/// Resolved actor identity used for authorization and persistence.
///
/// Relationships use `principal_id`, while `id` is a public snapshot used in
/// response documents and durable event payloads.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize)]
pub struct Actor {
    /// Internal IAM relationship key. It is never exposed in v1 JSON.
    #[serde(skip_serializing)]
    pub principal_id: PrincipalId,
    /// Principal kind.
    #[serde(rename = "type")]
    pub actor_type: ActorType,
    /// Stable public IAM actor ID.
    pub id: ActorId,
}

impl Actor {
    /// Creates a resolved actor.
    #[must_use]
    pub const fn new(principal_id: PrincipalId, actor_type: ActorType, id: ActorId) -> Self {
        Self {
            principal_id,
            actor_type,
            id,
        }
    }

    /// Returns the v1 public representation.
    #[must_use]
    pub fn public_ref(&self) -> ActorRef {
        ActorRef::new(self.actor_type, self.id.clone())
    }

    /// Reports whether this actor may create Silicon-managed projects.
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
    use uuid::Uuid;

    use super::{Actor, ActorType};
    use crate::domain::ids::{ActorId, PrincipalId};

    #[test]
    fn resolved_actor_serialization_never_exposes_internal_principal_id() {
        let public_id = ActorId::new("silicon-42");
        assert!(public_id.is_ok());
        let Some(public_id) = public_id.ok() else {
            return;
        };
        let actor = Actor::new(
            PrincipalId::from_uuid(Uuid::nil()),
            ActorType::Silicon,
            public_id,
        );

        let serialized = serde_json::to_value(actor);
        assert!(serialized.is_ok());
        assert_eq!(
            serialized.ok(),
            Some(json!({ "type": "silicon", "id": "silicon-42" }))
        );
    }
}
