//! Interfaces and value types for Silicon platform dependencies.

use std::{collections::BTreeSet, fmt, time::Duration};

use async_trait::async_trait;
use secrecy::{ExposeSecret as _, SecretString};
use serde_json::Value;
use thiserror::Error;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::{
    NotificationScope, NotificationSubscriptionLevel, NotificationVersion, WebhookUrl,
    actor::{Actor, ActorType},
    ids::{ActorId, OrganizationId, PublicOrganizationId},
};

// Retained for backwards-compatible IAM child-proof clients. Commit no longer
// exposes a Briefcase capability or invokes this scope.
const BRIEFCASE_AUDIENCE: &str = "silicon-briefcase";
const BRIEFCASE_TEMPORARY_URL_ACTION: &str = "briefcase.file.temporary_url";

/// IAM capability which grants organization-wide todo management.
pub const TODO_MANAGE_CAPABILITY: &str = "commit.todos.manage";
/// IAM capability which grants organization-wide project management.
pub const PROJECT_MANAGE_CAPABILITY: &str = "commit.projects.manage";

/// A single credential accepted at Commit's public authentication boundary.
///
/// The bearer and OBO variants are mutually exclusive by construction. The
/// trusted variant is reserved for the explicitly configured development/test
/// identity provider and must never be accepted in production.
#[derive(Clone)]
pub enum InboundCredential {
    /// Opaque IAM access token.
    Bearer(SecretString),
    /// Single-use proof presented by an upstream application.
    Obo {
        /// IAM application which obtained the proof.
        app_id: String,
        /// Opaque OBO access proof.
        proof: SecretString,
    },
    /// Explicit non-production identity supplied through trusted headers.
    Trusted(TrustedIdentity),
}

impl InboundCredential {
    /// Builds exactly one public credential from parsed HTTP authentication
    /// values.
    ///
    /// # Errors
    ///
    /// Returns an error when no complete credential, more than one credential,
    /// or only half of the OBO pair is present.
    pub fn from_external_parts(
        bearer: Option<SecretString>,
        obo_app_id: Option<String>,
        obo_proof: Option<SecretString>,
    ) -> Result<Self, CredentialError> {
        if bearer.is_some() && (obo_app_id.is_some() || obo_proof.is_some()) {
            return Err(CredentialError::Multiple);
        }

        match (bearer, obo_app_id, obo_proof) {
            (Some(token), None, None) => {
                validate_secret(&token)?;
                Ok(Self::Bearer(token))
            }
            (None, Some(app_id), Some(proof)) => {
                validate_app_id(&app_id)?;
                validate_secret(&proof)?;
                Ok(Self::Obo { app_id, proof })
            }
            (None, None, None) => Err(CredentialError::Missing),
            (None, _, _) => Err(CredentialError::IncompleteObo),
            (Some(_), _, _) => Err(CredentialError::Multiple),
        }
    }

    /// Creates an explicitly trusted development/test identity credential.
    #[must_use]
    pub const fn trusted(identity: TrustedIdentity) -> Self {
        Self::Trusted(identity)
    }
}

impl fmt::Debug for InboundCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bearer(_) => formatter.write_str("Bearer([REDACTED])"),
            Self::Obo { app_id, .. } => formatter
                .debug_struct("Obo")
                .field("app_id", app_id)
                .field("proof", &"[REDACTED]")
                .finish(),
            Self::Trusted(identity) => formatter.debug_tuple("Trusted").field(identity).finish(),
        }
    }
}

/// Invalid public authentication-header combination.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum CredentialError {
    /// Neither authentication mechanism was supplied.
    #[error("one authentication credential is required")]
    Missing,
    /// Bearer and OBO credentials were supplied together.
    #[error("bearer and OBO credentials are mutually exclusive")]
    Multiple,
    /// OBO authentication requires both application ID and proof.
    #[error("OBO authentication requires both application ID and proof")]
    IncompleteObo,
    /// An opaque credential is empty or contains whitespace.
    #[error("the authentication credential is malformed")]
    Malformed,
}

/// Organization authorization tier reported by IAM.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum OrganizationRole {
    /// Sole organization owner.
    Owner,
    /// Administrator whose authority is limited to explicit capabilities.
    Admin,
    /// Regular Carbon or Silicon member.
    Member,
}

/// Validated IAM capability names.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CapabilitySet(BTreeSet<String>);

impl CapabilitySet {
    /// Validates a provider capability collection.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed or duplicate capability values.
    pub fn try_from_names<I, S>(names: I) -> Result<Self, CapabilityError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut capabilities = BTreeSet::new();
        for name in names {
            let name = name.into();
            if !valid_capability(&name) {
                return Err(CapabilityError::Malformed);
            }
            if !capabilities.insert(name) {
                return Err(CapabilityError::Duplicate);
            }
        }
        Ok(Self(capabilities))
    }

    /// Reports whether IAM explicitly granted a capability.
    #[must_use]
    pub fn contains(&self, capability: &str) -> bool {
        self.0.contains(capability)
    }

    /// Iterates over capability names in deterministic order.
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(String::as_str)
    }
}

/// Invalid IAM capability collection.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum CapabilityError {
    /// A capability does not use the bounded IAM name format.
    #[error("IAM returned a malformed capability")]
    Malformed,
    /// A capability occurred more than once.
    #[error("IAM returned a duplicate capability")]
    Duplicate,
}

/// Identity accepted only by the trusted-header provider in development/test.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrustedIdentity {
    /// Stable IAM-compatible organization UUID supplied by the test harness.
    pub organization_id: OrganizationId,
    /// Requested public organization handle.
    pub org_id: PublicOrganizationId,
    /// Stable IAM-compatible membership UUID supplied by the test harness.
    pub membership_id: Uuid,
    /// Carbon or Silicon identity.
    pub actor: Actor,
    /// Organization authorization tier.
    pub organization_role: OrganizationRole,
    /// Explicit Commit capabilities.
    pub capabilities: CapabilitySet,
}

/// Action and resource against which an inbound grant is verified.
#[derive(Clone, Debug)]
pub struct AuthenticationRequest {
    /// Exactly one inbound credential.
    pub credential: InboundCredential,
    /// Untrusted public organization requested by the caller.
    pub org_id: PublicOrganizationId,
    /// Commit action the request intends to perform.
    pub action: String,
    /// Optional organization-qualified Commit resource identifier.
    pub resource: Option<String>,
}

/// Fully verified identity and current organization authorization.
#[derive(Clone)]
pub struct VerifiedActor {
    /// IAM's internal organization UUID used by persistent relationships.
    pub organization_id: OrganizationId,
    /// Immutable public organization handle.
    pub org_id: PublicOrganizationId,
    /// IAM's internal membership UUID.
    pub membership_id: Uuid,
    /// Resolved Carbon or Silicon principal.
    pub actor: Actor,
    /// Current organization role.
    pub organization_role: OrganizationRole,
    /// Current explicit IAM capabilities.
    pub capabilities: CapabilitySet,
    /// Live IAM membership tags; never supplied by a request header.
    pub tags: BTreeSet<String>,
    grant: InboundCredential,
}

impl VerifiedActor {
    /// Creates a verified identity while retaining its opaque grant solely for
    /// a subsequent IAM child-proof exchange.
    #[must_use]
    pub fn new(
        organization_id: OrganizationId,
        org_id: PublicOrganizationId,
        membership_id: Uuid,
        actor: Actor,
        organization_role: OrganizationRole,
        capabilities: CapabilitySet,
        grant: InboundCredential,
    ) -> Self {
        Self {
            organization_id,
            org_id,
            membership_id,
            actor,
            organization_role,
            capabilities,
            tags: BTreeSet::new(),
            grant,
        }
    }

    /// Attaches tags obtained from the same verified IAM authorization.
    #[must_use]
    pub fn with_tags(mut self, tags: BTreeSet<String>) -> Self {
        self.tags = tags;
        self
    }

    /// Returns whether the actor has organization-wide todo management power.
    #[must_use]
    pub fn manages_todos(&self) -> bool {
        self.organization_role == OrganizationRole::Owner
            || self.capabilities.contains(TODO_MANAGE_CAPABILITY)
    }

    /// Returns whether the actor has organization-wide project management power.
    #[must_use]
    pub fn manages_projects(&self) -> bool {
        self.organization_role == OrganizationRole::Owner
            || self.capabilities.contains(PROJECT_MANAGE_CAPABILITY)
    }

    /// Borrows the redacted actor grant for an IAM-only child exchange.
    pub(crate) const fn grant(&self) -> &InboundCredential {
        &self.grant
    }
}

impl fmt::Debug for VerifiedActor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerifiedActor")
            .field("organization_id", &self.organization_id)
            .field("org_id", &self.org_id)
            .field("membership_id", &self.membership_id)
            .field("actor", &self.actor)
            .field("organization_role", &self.organization_role)
            .field("capabilities", &self.capabilities)
            .field("tags", &self.tags)
            .field("grant", &"[REDACTED]")
            .finish()
    }
}

/// Current IAM directory member suitable for a persistent relationship.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActiveMember {
    /// IAM's internal organization UUID.
    pub organization_id: OrganizationId,
    /// Immutable public organization handle.
    pub org_id: PublicOrganizationId,
    /// IAM's internal membership UUID.
    pub membership_id: Uuid,
    /// Resolved Carbon or Silicon principal.
    pub actor: Actor,
}

/// Requested child OBO proof scope.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChildProofRequest {
    /// Target service audience.
    pub audience: String,
    /// One action delegated to the target service.
    pub action: String,
    /// Exact target-service resource ID.
    pub resource: String,
}

impl ChildProofRequest {
    /// Creates the fixed Briefcase temporary-URL delegation scope.
    #[must_use]
    pub fn briefcase_temporary_url(entry_id: Uuid) -> Self {
        Self {
            audience: BRIEFCASE_AUDIENCE.to_owned(),
            action: BRIEFCASE_TEMPORARY_URL_ACTION.to_owned(),
            resource: entry_id.hyphenated().to_string(),
        }
    }
}

/// Newly exchanged proof which can be sent only to its target service.
#[derive(Clone)]
#[allow(dead_code)]
pub struct DelegatedOboProof {
    app_id: String,
    proof: SecretString,
    expires_at: OffsetDateTime,
}

impl DelegatedOboProof {
    /// Creates a validated delegated proof returned by IAM.
    ///
    /// # Errors
    ///
    /// Returns an error when the application identifier or proof is malformed.
    pub fn new(
        app_id: String,
        proof: SecretString,
        expires_at: OffsetDateTime,
    ) -> Result<Self, CredentialError> {
        validate_app_id(&app_id)?;
        validate_secret(&proof)?;
        Ok(Self {
            app_id,
            proof,
            expires_at,
        })
    }

    /// Application ID sent with the proof to the target service.
    #[must_use]
    pub fn app_id(&self) -> &str {
        &self.app_id
    }

    /// Proof expiry asserted by IAM.
    #[must_use]
    pub const fn expires_at(&self) -> OffsetDateTime {
        self.expires_at
    }

    /// Exposes the proof only to infrastructure adapters.
    #[allow(dead_code)]
    pub(crate) fn expose_proof(&self) -> &str {
        self.proof.expose_secret()
    }
}

impl fmt::Debug for DelegatedOboProof {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DelegatedOboProof")
            .field("app_id", &self.app_id)
            .field("proof", &"[REDACTED]")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// Redacted failure returned by an IAM or Briefcase dependency.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ProviderError {
    /// Credential is absent, expired, revoked, or otherwise invalid.
    #[error("provider rejected authentication")]
    Unauthenticated,
    /// The represented actor lacks the requested authority.
    #[error("provider denied the requested action")]
    Forbidden,
    /// The organization-scoped provider resource is absent.
    #[error("provider resource was not found")]
    NotFound,
    /// A single-use proof or idempotency key conflicted with provider state.
    #[error("provider reported a state conflict")]
    Conflict,
    /// Provider rate limiting prevented a decision.
    #[error("provider rate limited the request")]
    RateLimited {
        /// Provider-supplied bounded retry delay, when valid.
        retry_after: Option<Duration>,
    },
    /// A successful provider response did not satisfy its contract.
    #[error("provider returned an invalid response")]
    InvalidResponse,
    /// Transport, timeout, or provider server failure prevented a decision.
    #[error("provider is unavailable")]
    Unavailable,
}

/// IAM authentication, directory, and child-delegation boundary.
#[async_trait]
pub trait IdentityProvider: Send + Sync {
    /// Verifies one inbound grant online and returns current membership power.
    async fn authenticate(
        &self,
        request: &AuthenticationRequest,
    ) -> Result<VerifiedActor, ProviderError>;

    /// Resolves current members by immutable public handle in one directory operation.
    ///
    /// The result contains exactly one member for every distinct requested ID,
    /// ordered by each ID's first appearance in `actor_ids`. An empty request
    /// returns an empty result without consulting the provider. Missing or
    /// ambiguous members fail the whole operation rather than returning a
    /// partial identity set.
    async fn resolve_active_members(
        &self,
        org_id: &PublicOrganizationId,
        actor_ids: &[ActorId],
        required_type: Option<ActorType>,
    ) -> Result<Vec<ActiveMember>, ProviderError>;

    /// Resolves one current member by immutable public handle.
    async fn resolve_active_member(
        &self,
        org_id: &PublicOrganizationId,
        actor_id: &ActorId,
        required_type: Option<ActorType>,
    ) -> Result<ActiveMember, ProviderError> {
        let mut members = self
            .resolve_active_members(org_id, std::slice::from_ref(actor_id), required_type)
            .await?;
        if members.len() != 1 {
            return Err(ProviderError::InvalidResponse);
        }
        members.pop().ok_or(ProviderError::InvalidResponse)
    }

    /// Exchanges the retained actor grant for a narrower target-service proof.
    async fn exchange_child_proof(
        &self,
        actor: &VerifiedActor,
        request: &ChildProofRequest,
    ) -> Result<DelegatedOboProof, ProviderError>;
}

/// Immutable endpoint and subscription decision captured with an outbox event.
#[derive(Clone, Eq, PartialEq)]
pub struct WebhookRoutingSnapshot {
    webhook_url: WebhookUrl,
    destination_version: NotificationVersion,
    subscription_level: NotificationSubscriptionLevel,
    subscription_scope: NotificationScope,
    subscription_version: NotificationVersion,
}

impl fmt::Debug for WebhookRoutingSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WebhookRoutingSnapshot")
            .field("webhook_url", &"[REDACTED]")
            .field("destination_version", &self.destination_version)
            .field("subscription_level", &self.subscription_level)
            .field("subscription_scope", &self.subscription_scope)
            .field("subscription_version", &self.subscription_version)
            .finish()
    }
}

impl WebhookRoutingSnapshot {
    /// Creates a complete immutable routing decision for one outbox event.
    ///
    /// # Errors
    ///
    /// Returns an error when either persisted resource version is zero. Zero
    /// identifies a virtual, absent resource and can never supply a route.
    pub fn new(
        webhook_url: WebhookUrl,
        destination_version: NotificationVersion,
        subscription_level: NotificationSubscriptionLevel,
        subscription_scope: NotificationScope,
        subscription_version: NotificationVersion,
    ) -> Result<Self, WebhookRoutingSnapshotError> {
        if destination_version.get() == 0 || subscription_version.get() == 0 {
            return Err(WebhookRoutingSnapshotError::NonPositiveVersion);
        }
        Ok(Self {
            webhook_url,
            destination_version,
            subscription_level,
            subscription_scope,
            subscription_version,
        })
    }

    /// Returns the endpoint selected when the event was committed.
    #[must_use]
    pub const fn webhook_url(&self) -> &WebhookUrl {
        &self.webhook_url
    }

    /// Returns the Silicon-level settings version which supplied the endpoint.
    #[must_use]
    pub const fn destination_version(&self) -> NotificationVersion {
        self.destination_version
    }

    /// Returns whether a list-wide or todo-specific rule selected the event.
    #[must_use]
    pub const fn subscription_level(&self) -> NotificationSubscriptionLevel {
        self.subscription_level
    }

    /// Returns the effective matching scope copied into the event.
    #[must_use]
    pub const fn subscription_scope(&self) -> NotificationScope {
        self.subscription_scope
    }

    /// Returns the version of the resource which supplied the effective rule.
    #[must_use]
    pub const fn subscription_version(&self) -> NotificationVersion {
        self.subscription_version
    }
}

/// Invalid immutable webhook routing snapshot.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum WebhookRoutingSnapshotError {
    /// A virtual version-zero settings resource cannot supply a delivery route.
    #[error("Webhook routing snapshot versions must be positive")]
    NonPositiveVersion,
}

/// Minimal durable event submitted to a configured webhook endpoint.
#[derive(Clone, Debug, PartialEq)]
pub struct WebhookEvent {
    /// Stable outbox event and downstream idempotency identifier.
    pub event_id: Uuid,
    /// Public organization handle associated with the event.
    pub org_id: PublicOrganizationId,
    /// Public Silicon handle which owns the snapshotted endpoint.
    pub silicon_id: ActorId,
    /// Versioned event type such as `todo.status_changed`.
    pub event_type: String,
    /// Positive payload schema version.
    pub payload_version: u16,
    /// Event occurrence time retained from the outbox row.
    pub occurred_at: OffsetDateTime,
    /// Cross-service trace identifier, when present.
    pub trace_id: Option<String>,
    /// Immutable destination and subscription decision; absent only for legacy rows.
    pub routing_snapshot: Option<WebhookRoutingSnapshot>,
    /// Minimal versioned event data; never credentials or a request body copy.
    pub payload: Value,
}

/// Stable redacted webhook delivery failure categories.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum WebhookPublishError {
    /// Transport, credential, routing, or server state may recover on retry.
    #[error("Webhook delivery is temporarily unavailable")]
    Unavailable,
    /// Webhook endpoint rate limited the request.
    #[error("Webhook publication was rate limited")]
    RateLimited {
        /// Provider-supplied bounded retry delay, when valid.
        retry_after: Option<Duration>,
    },
    /// Webhook endpoint returned an invalid response.
    #[error("Webhook endpoint returned an invalid response")]
    InvalidResponse,
    /// Webhook endpoint definitively rejected the event payload.
    #[error("Webhook endpoint rejected the event payload")]
    Rejected,
}

impl WebhookPublishError {
    /// Whether the durable outbox worker should retry this failure.
    #[must_use]
    pub const fn is_retryable(self) -> bool {
        !matches!(self, Self::Rejected)
    }

    /// Stable bounded code safe to retain in the outbox.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Unavailable => "webwebhook_unavailable",
            Self::RateLimited { .. } => "webwebhook_rate_limited",
            Self::InvalidResponse => "webwebhook_invalid_response",
            Self::Rejected => "webwebhook_event_rejected",
        }
    }
}

/// Authenticated Silicon webhook publication boundary.
#[async_trait]
pub trait WebhookPublisher: Send + Sync {
    /// Publishes one immutable event, reusing `event_id` as the idempotency key.
    async fn publish(&self, event: &WebhookEvent) -> Result<(), WebhookPublishError>;
}

fn validate_secret(secret: &SecretString) -> Result<(), CredentialError> {
    let exposed = secret.expose_secret();
    if exposed.is_empty()
        || exposed.len() > 4_096
        || exposed.bytes().any(|byte| byte.is_ascii_whitespace())
    {
        return Err(CredentialError::Malformed);
    }
    Ok(())
}

fn validate_app_id(app_id: &str) -> Result<(), CredentialError> {
    if !(3..=80).contains(&app_id.len())
        || !app_id.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
    {
        return Err(CredentialError::Malformed);
    }
    Ok(())
}

fn valid_capability(capability: &str) -> bool {
    (2..=200).contains(&capability.len())
        && capability
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase())
        && capability.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-' | b'_')
        })
}

#[cfg(test)]
mod tests {
    use secrecy::SecretString;

    use super::{
        CapabilityError, CapabilitySet, CredentialError, InboundCredential,
        PROJECT_MANAGE_CAPABILITY,
    };

    #[test]
    fn public_credentials_are_mutually_exclusive() {
        let bearer = Some(SecretString::from("iat_opaque".to_owned()));
        let proof = Some(SecretString::from("obo_opaque".to_owned()));
        assert!(matches!(
            InboundCredential::from_external_parts(
                bearer,
                Some("silicon-interface".to_owned()),
                proof
            ),
            Err(CredentialError::Multiple)
        ));
    }

    #[test]
    fn obo_requires_both_headers() {
        assert!(matches!(
            InboundCredential::from_external_parts(
                None,
                Some("silicon-interface".to_owned()),
                None
            ),
            Err(CredentialError::IncompleteObo)
        ));
    }

    #[test]
    fn capability_set_rejects_duplicate_or_malformed_provider_data() {
        assert_eq!(
            CapabilitySet::try_from_names([PROJECT_MANAGE_CAPABILITY, PROJECT_MANAGE_CAPABILITY]),
            Err(CapabilityError::Duplicate)
        );
        assert_eq!(
            CapabilitySet::try_from_names(["Commit.Admin"]),
            Err(CapabilityError::Malformed)
        );
    }
}
