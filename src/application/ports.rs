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
    ids::{AccountUuid, ActorId},
};

/// The one credential accepted at Commit's public authentication boundary.
#[derive(Clone)]
pub enum InboundCredential {
    /// `Authorization: Bearer <Silicon Accounts access token issued to Commit>`.
    Bearer(SecretString),
    /// `Authorization: Proof sap_…`: a User verification proof issued by another app for one account.
    Proof(SecretString),
}

impl InboundCredential {
    /// Exposes the raw token only to the identity adapter.
    pub(crate) fn expose(&self) -> &str {
        match self {
            Self::Bearer(token) | Self::Proof(token) => token.expose_secret(),
        }
    }
}

impl fmt::Debug for InboundCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bearer(_) => formatter.write_str("Bearer([REDACTED])"),
            Self::Proof(_) => formatter.write_str("Proof([REDACTED])"),
        }
    }
}

/// Verified request authentication input.
#[derive(Clone, Debug)]
pub struct AuthenticationRequest {
    /// Exactly one inbound credential.
    pub credential: InboundCredential,
    /// Commit action the request performs; also the proof scope it needs.
    pub scope: String,
    /// The route changes who can see or reach something: check revocation online.
    pub sensitive: bool,
}

/// How the caller authenticated.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Grant {
    /// The caller's own Silicon Accounts access token for Commit.
    Bearer {
        /// Token issue time (unix seconds), compared with the account's revocation cutoff.
        issued_at: Option<i64>,
        /// Sign-in (token family) the token belongs to.
        family: Option<String>,
    },
    /// Another app acting for the account with a User verification proof.
    Proof {
        /// The app that issued the proof (e.g. `interface`).
        issuing_app: String,
        /// Proof family id.
        proof_id: String,
        /// Scopes the proof grants.
        scopes: Vec<String>,
    },
}

/// The authenticated account and what Commit knows about its custodian circle.
#[derive(Clone)]
pub struct VerifiedActor {
    /// The account the request acts as.
    pub actor: Actor,
    /// For a Silicon: its custodian.
    pub custodian: Option<AccountUuid>,
    /// For a Carbon: the Silicons it is custodian of (as known to Commit). The
    /// custodian rule lets it read and manage their work in Commit.
    pub managed_silicons: BTreeSet<AccountUuid>,
    /// How the request authenticated.
    pub grant: Grant,
}

impl VerifiedActor {
    /// Creates a verified account for a bearer-token caller.
    #[must_use]
    pub fn new(actor: Actor, grant: Grant) -> Self {
        Self {
            actor,
            custodian: None,
            managed_silicons: BTreeSet::new(),
            grant,
        }
    }

    /// Records the Silicon's custodian.
    #[must_use]
    pub fn with_custodian(mut self, custodian: Option<AccountUuid>) -> Self {
        self.custodian = custodian;
        self
    }

    /// Records the Silicons this Carbon is custodian of.
    #[must_use]
    pub fn with_managed_silicons(mut self, silicons: BTreeSet<AccountUuid>) -> Self {
        self.managed_silicons = silicons;
        self
    }

    /// The account uuid the request acts as.
    #[must_use]
    pub const fn uuid(&self) -> &AccountUuid {
        &self.actor.uuid
    }

    /// True when the caller is `subject` or is the custodian of the Silicon `subject`.
    ///
    /// A custodian manages its Silicons' work in Commit but always acts as itself:
    /// notes, audit rows and history name the custodian, never the Silicon.
    #[must_use]
    pub fn acts_for(&self, subject: &AccountUuid) -> bool {
        &self.actor.uuid == subject || self.managed_silicons.contains(subject)
    }

    /// The app acting for the account, when the request carries a proof.
    #[must_use]
    pub fn via_app(&self) -> Option<&str> {
        match &self.grant {
            Grant::Proof { issuing_app, .. } => Some(issuing_app),
            Grant::Bearer { .. } => None,
        }
    }
}

impl fmt::Debug for VerifiedActor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerifiedActor")
            .field("actor", &self.actor)
            .field("custodian", &self.custodian)
            .field("managed_silicons", &self.managed_silicons)
            .field("grant", &self.grant)
            .finish()
    }
}

/// Account lifecycle state reported by Silicon Accounts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccountStatus {
    /// A usable account.
    Active,
    /// Imported but never signed in.
    Unclaimed,
    /// A Silicon waiting for its custodian.
    PendingCustodian,
    /// Deleted; its id is empty.
    Deleted,
}

impl AccountStatus {
    /// Parses the Accounts spelling; unknown future values are treated as not active.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "active" | "" => Some(Self::Active),
            "unclaimed" => Some(Self::Unclaimed),
            "pending_custodian" => Some(Self::PendingCustodian),
            "deleted" => Some(Self::Deleted),
            _ => None,
        }
    }

    /// Storage spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Unclaimed => "unclaimed",
            Self::PendingCustodian => "pending_custodian",
            Self::Deleted => "deleted",
        }
    }
}

/// An account resolved through Silicon Accounts (lookup by id or uuid).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedAccount {
    /// uuid, kind and current id.
    pub actor: Actor,
    /// Lifecycle state.
    pub status: AccountStatus,
    /// A Silicon's custodian.
    pub custodian: Option<AccountUuid>,
    /// Display name, when known.
    pub display_name: String,
    /// Profile photo URL, when known.
    pub pfp_url: String,
}

impl ResolvedAccount {
    /// A resolved account built from what is already known (tests, the caller itself).
    #[must_use]
    pub fn known(actor: Actor, custodian: Option<AccountUuid>) -> Self {
        Self {
            actor,
            status: AccountStatus::Active,
            custodian,
            display_name: String::new(),
            pfp_url: String::new(),
        }
    }
}

/// Redacted failure returned by Silicon Accounts or another provider.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum ProviderError {
    /// No credential, or one that is not a Commit credential at all.
    #[error("provider rejected authentication")]
    Unauthenticated,
    /// A recognisable credential that is not acceptable, with a precise reason.
    #[error("{message}")]
    Rejected {
        /// Stable machine-readable code.
        code: &'static str,
        /// Exact human/Silicon-readable reason.
        message: String,
    },
    /// The caller is authenticated but may not do this, with a precise reason.
    #[error("{message}")]
    Denied {
        /// Stable machine-readable code.
        code: &'static str,
        /// Exact reason.
        message: String,
    },
    /// The represented actor lacks the requested authority.
    #[error("provider denied the requested action")]
    Forbidden,
    /// No account has this id (or it was deleted).
    #[error("no Silicon Accounts account has the id {id}")]
    UnknownAccount {
        /// The id or uuid the caller supplied.
        id: String,
    },
    /// The account exists but cannot be used here.
    #[error("{id} {reason}")]
    UnusableAccount {
        /// The id or uuid the caller supplied.
        id: String,
        /// Why, phrased to follow the id (e.g. "is a Carbon; `silicon_ids` takes Silicons").
        reason: String,
    },
    /// The provider resource is absent.
    #[error("provider resource was not found")]
    NotFound,
    /// A single-use value or idempotency key conflicted with provider state.
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

/// Silicon Accounts authentication and account directory boundary.
#[async_trait]
pub trait IdentityProvider: Send + Sync {
    /// Verifies one inbound credential and returns the account it acts as.
    async fn authenticate(
        &self,
        request: &AuthenticationRequest,
    ) -> Result<VerifiedActor, ProviderError>;

    /// Resolves `c:`/`si:` ids (or account uuids) to current accounts.
    ///
    /// The result has exactly one account per distinct requested id, in first
    /// appearance order. An unknown or deleted id fails the whole call with
    /// [`ProviderError::UnknownAccount`]; a kind mismatch with `required_type`
    /// fails it the same way.
    async fn resolve_accounts(
        &self,
        ids: &[ActorId],
        required_type: Option<ActorType>,
    ) -> Result<Vec<ResolvedAccount>, ProviderError>;

    /// Resolves one id or uuid.
    async fn resolve_account(
        &self,
        id: &ActorId,
        required_type: Option<ActorType>,
    ) -> Result<ResolvedAccount, ProviderError> {
        let mut accounts = self
            .resolve_accounts(std::slice::from_ref(id), required_type)
            .await?;
        if accounts.len() != 1 {
            return Err(ProviderError::InvalidResponse);
        }
        accounts.pop().ok_or(ProviderError::InvalidResponse)
    }
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
    /// Permanent uuid of the Silicon which owns the snapshotted endpoint.
    pub silicon_uuid: AccountUuid,
    /// That Silicon's current public id.
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
