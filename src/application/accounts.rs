//! Account use cases: who the caller is, the Silicon allow-list, Silicon Accounts
//! webhook events, and the shared rules for naming other accounts.

use std::{sync::Arc, time::Duration};

use serde::Serialize;
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool};
use time::OffsetDateTime;

use crate::{
    application::ports::{IdentityProvider, ProviderError, ResolvedAccount, VerifiedActor},
    domain::{AccountUuid, Actor, ActorId, ActorRef, ActorType},
    error::AppError,
    infrastructure::postgres::accounts::{self as store, AllowedAccount},
};

/// Maps an authentication failure to the public error.
#[must_use]
pub fn authentication_error(error: ProviderError) -> AppError {
    match error {
        ProviderError::Rejected { code, message } => AppError::Authentication {
            code: code.into(),
            message,
        },
        ProviderError::Denied { code, message } => AppError::Denied {
            code: code.into(),
            message,
        },
        ProviderError::Unauthenticated
        | ProviderError::UnknownAccount { .. }
        | ProviderError::UnusableAccount { .. } => AppError::Unauthenticated,
        ProviderError::Forbidden => AppError::Forbidden,
        ProviderError::NotFound => AppError::NotFound,
        ProviderError::Conflict => AppError::Conflict {
            code: "provider_conflict".into(),
        },
        ProviderError::RateLimited { retry_after } => AppError::RateLimited {
            retry_after_seconds: retry_after.map_or(1, |duration| duration.as_secs().max(1)),
        },
        ProviderError::InvalidResponse => AppError::BadGateway,
        ProviderError::Unavailable => AppError::ProviderUnavailable,
    }
}

/// Maps a failure to resolve an account named in request field `field`.
#[must_use]
pub fn account_field_error(field: &'static str, error: ProviderError) -> AppError {
    match error {
        ProviderError::UnknownAccount { id } => AppError::Validation {
            details: json!({ (field): format!("No Silicon Accounts account has the id {id}. Use a current c:/si: id (ids can change; the uuid never does).") }),
        },
        ProviderError::UnusableAccount { id, reason } => AppError::Validation {
            details: json!({ (field): format!("{id} {reason}.") }),
        },
        ProviderError::RateLimited { retry_after } => AppError::RateLimited {
            retry_after_seconds: retry_after.map_or(1, |duration| duration.as_secs().max(1)),
        },
        ProviderError::InvalidResponse | ProviderError::Conflict => AppError::BadGateway,
        ProviderError::Rejected { .. }
        | ProviderError::Denied { .. }
        | ProviderError::Unauthenticated
        | ProviderError::Forbidden
        | ProviderError::NotFound
        | ProviderError::Unavailable => AppError::ProviderUnavailable,
    }
}

/// Records the caller and every resolved account inside the current transaction.
pub(crate) async fn remember(
    connection: &mut PgConnection,
    caller: &VerifiedActor,
    accounts: &[ResolvedAccount],
) -> Result<(), AppError> {
    store::ensure_actor(connection, &caller.actor).await?;
    for account in accounts {
        if account.actor.uuid == *caller.uuid() {
            continue;
        }
        store::remember(&mut *connection, account).await?;
    }
    Ok(())
}

/// Refuses to hand a Silicon work or an invitation from outside its circle, unless it allowed the caller.
pub(crate) async fn ensure_reachable(
    connection: &mut PgConnection,
    caller: &VerifiedActor,
    target: &Actor,
    field: &'static str,
) -> Result<(), AppError> {
    if !target.is_silicon() || store::may_reach(connection, caller.uuid(), &target.uuid).await? {
        return Ok(());
    }
    Err(AppError::Denied {
        code: "silicon_not_reachable".into(),
        message: format!(
            "{field}: {silicon} only takes work and invitations from its custodian, the Silicons with the same custodian, and accounts it allowed. Ask {silicon} or its custodian to allow {caller}: PUT /api/v1/silicons/{silicon}/allowed-accounts/{caller}.",
            silicon = display_id(target),
            caller = display_id(&caller.actor),
        ),
    })
}

fn display_id(actor: &Actor) -> String {
    if actor.id.as_str().is_empty() {
        actor.uuid.to_string()
    } else {
        actor.id.as_str().to_owned()
    }
}

/// The Silicon a request is about: the caller itself when `selector` is absent or names
/// the caller, otherwise a Silicon the caller is custodian of.
pub(crate) async fn managed_silicon(
    identity: &dyn IdentityProvider,
    actor: &VerifiedActor,
    selector: Option<&ActorId>,
) -> Result<ResolvedAccount, AppError> {
    let names_caller = selector.is_none_or(|selector| {
        selector.as_str() == "me"
            || selector.as_str() == actor.uuid().as_str()
            || (!actor.actor.id.as_str().is_empty()
                && selector
                    .as_str()
                    .eq_ignore_ascii_case(actor.actor.id.as_str()))
    });
    if names_caller {
        if !actor.actor.is_silicon() {
            return Err(AppError::Validation {
                details: json!({ "silicon": "Carbons have no Silicon settings: name one of your Silicons with ?silicon=si:… (its si: id or uuid)." }),
            });
        }
        return Ok(ResolvedAccount::known(
            actor.actor.clone(),
            actor.custodian.clone(),
        ));
    }
    let Some(selector) = selector else {
        return Err(AppError::Internal(anyhow::anyhow!(
            "selector checked above"
        )));
    };
    let silicon = identity
        .resolve_account(selector, Some(ActorType::Silicon))
        .await
        .map_err(|error| account_field_error("silicon", error))?;
    if silicon.custodian.as_ref() != Some(actor.uuid()) {
        return Err(AppError::Denied {
            code: "not_custodian".into(),
            message: format!(
                "Only {} itself or its custodian can manage this.",
                display_id(&silicon.actor)
            ),
        });
    }
    Ok(silicon)
}

/// Account use-case boundary.
#[derive(Clone)]
pub struct AccountService {
    pool: PgPool,
    identity: Arc<dyn IdentityProvider>,
    tombstone_retention: Duration,
    audit_retention: Duration,
}

/// `GET /api/v1/me`.
#[derive(Clone, Debug, Serialize)]
pub struct MeView {
    /// Always true: the request authenticated.
    pub authenticated: bool,
    /// Commit's app id at Silicon Accounts.
    pub app_id: String,
    /// Permanent account uuid.
    pub uuid: AccountUuid,
    /// Current `c:`/`si:` id.
    pub id: ActorId,
    /// `carbon` or `silicon`.
    pub kind: ActorType,
    /// Display name, when known.
    pub display_name: String,
    /// Profile photo URL, when known.
    pub pfp_url: String,
    /// The email shared with Commit, when the Carbon shared one.
    pub email: Option<String>,
    /// A Silicon's custodian.
    pub custodian: Option<ActorRef>,
    /// The Silicons a Carbon is custodian of (those Commit has seen).
    pub silicons: Vec<ActorRef>,
    /// The app acting for the account, when the request carries a proof.
    pub via_app: Option<String>,
}

/// A Silicon's allow-list.
#[derive(Clone, Debug, Serialize)]
pub struct AllowlistView {
    /// The Silicon.
    pub silicon: ActorRef,
    /// Accounts outside its circle it accepts work and invitations from.
    pub allowed: Vec<AllowedAccount>,
}

impl AccountService {
    /// Creates the account service.
    #[must_use]
    pub fn new(
        pool: PgPool,
        identity: Arc<dyn IdentityProvider>,
        tombstone_retention: Duration,
        audit_retention: Duration,
    ) -> Self {
        Self {
            pool,
            identity,
            tombstone_retention,
            audit_retention,
        }
    }

    /// Describes the caller as Commit knows it.
    pub async fn me(&self, actor: &VerifiedActor, app_id: &str) -> Result<MeView, AppError> {
        let stored = store::load(&self.pool, actor.uuid()).await?;
        let custodian = match &actor.custodian {
            Some(uuid) => match store::load(&self.pool, uuid).await? {
                Some(custodian) => Some(custodian.actor.public_ref()),
                None => Some(self.unstored_custodian(uuid).await?),
            },
            None => None,
        };
        let mut silicons = Vec::with_capacity(actor.managed_silicons.len());
        for uuid in &actor.managed_silicons {
            if let Some(silicon) = store::load(&self.pool, uuid).await? {
                silicons.push(silicon.actor.public_ref());
            }
        }
        Ok(MeView {
            authenticated: true,
            app_id: app_id.to_owned(),
            uuid: actor.uuid().clone(),
            id: actor.actor.id.clone(),
            kind: actor.actor.actor_type,
            display_name: stored
                .as_ref()
                .map(|s| s.display_name.clone())
                .unwrap_or_default(),
            pfp_url: stored
                .as_ref()
                .map(|s| s.pfp_url.clone())
                .unwrap_or_default(),
            email: stored.and_then(|s| s.email),
            custodian,
            silicons,
            via_app: actor.via_app().map(str::to_owned),
        })
    }

    /// A custodian that never used Commit has no stored row: look it up (lookups are
    /// cached) so `/me` names it. The answer is not stored: a row made from a lookup would
    /// delay the custodian's own first refresh from `userinfo` (name, shared email). If
    /// Silicon Accounts cannot answer, the id stays empty rather than failing the request.
    async fn unstored_custodian(&self, uuid: &AccountUuid) -> Result<ActorRef, AppError> {
        let unnamed = ActorRef::new(
            ActorType::Carbon,
            ActorId::from_persisted(String::new()),
            uuid.clone(),
        );
        let Ok(selector) = ActorId::new(uuid.as_str()) else {
            return Ok(unnamed);
        };
        match self
            .identity
            .resolve_account(&selector, Some(ActorType::Carbon))
            .await
        {
            Ok(resolved) if resolved.actor.uuid == *uuid => Ok(resolved.actor.public_ref()),
            Ok(_) | Err(_) => Ok(unnamed),
        }
    }

    /// Resolves the Silicon a caller names and checks the caller may manage it
    /// (it is that Silicon, or its custodian).
    pub async fn managed_silicon(
        &self,
        actor: &VerifiedActor,
        selector: &ActorId,
    ) -> Result<ResolvedAccount, AppError> {
        managed_silicon(self.identity.as_ref(), actor, Some(selector)).await
    }

    /// Lists a Silicon's allow-list.
    pub async fn allowlist(
        &self,
        actor: &VerifiedActor,
        selector: &ActorId,
    ) -> Result<AllowlistView, AppError> {
        let silicon = self.managed_silicon(actor, selector).await?;
        let mut connection = self.pool.acquire().await?;
        remember(&mut connection, actor, std::slice::from_ref(&silicon)).await?;
        let allowed = store::allowlist(&mut *connection, &silicon.actor.uuid).await?;
        Ok(AllowlistView {
            silicon: silicon.actor.public_ref(),
            allowed,
        })
    }

    /// Allows `account` to assign the Silicon work and add it to projects.
    pub async fn allow(
        &self,
        actor: &VerifiedActor,
        selector: &ActorId,
        account: &ActorId,
    ) -> Result<AllowlistView, AppError> {
        let silicon = self.managed_silicon(actor, selector).await?;
        let allowed = self
            .identity
            .resolve_account(account, None)
            .await
            .map_err(|error| account_field_error("account", error))?;
        if allowed.actor.uuid == silicon.actor.uuid {
            return Err(AppError::Validation {
                details: json!({ "account": "A Silicon always reaches itself; name another account." }),
            });
        }
        let mut transaction = self.pool.begin().await?;
        remember(&mut transaction, actor, &[silicon.clone(), allowed.clone()]).await?;
        store::allow(
            &mut transaction,
            &silicon.actor.uuid,
            &allowed.actor.uuid,
            actor.uuid(),
        )
        .await?;
        let list = store::allowlist(&mut *transaction, &silicon.actor.uuid).await?;
        transaction.commit().await?;
        Ok(AllowlistView {
            silicon: silicon.actor.public_ref(),
            allowed: list,
        })
    }

    /// Removes `account` from the Silicon's allow-list (work already assigned stays).
    pub async fn disallow(
        &self,
        actor: &VerifiedActor,
        selector: &ActorId,
        account: &ActorId,
    ) -> Result<AllowlistView, AppError> {
        let silicon = self.managed_silicon(actor, selector).await?;
        let removed = self
            .identity
            .resolve_account(account, None)
            .await
            .map_err(|error| account_field_error("account", error))?;
        let mut transaction = self.pool.begin().await?;
        store::disallow(&mut transaction, &silicon.actor.uuid, &removed.actor.uuid).await?;
        let list = store::allowlist(&mut *transaction, &silicon.actor.uuid).await?;
        transaction.commit().await?;
        Ok(AllowlistView {
            silicon: silicon.actor.public_ref(),
            allowed: list,
        })
    }
}

/// A Silicon Accounts app-webhook event, already verified and parsed.
#[derive(Clone, Debug, PartialEq)]
pub enum AccountEvent {
    /// `account.id_changed`.
    IdChanged {
        /// Account uuid.
        uuid: String,
        /// The new public id.
        new_id: String,
    },
    /// `account.updated` with the account as Commit may see it.
    Updated {
        /// Account uuid.
        uuid: String,
        /// The account object (`AccountForApp`).
        account: Value,
        /// Its version.
        version: i64,
    },
    /// `account.deleted`.
    Deleted {
        /// Account uuid.
        uuid: String,
    },
    /// `membership.signed_out`.
    SignedOut {
        /// Account uuid.
        uuid: String,
        /// Why (e.g. `app_revoked`, `stk_rotated`).
        reason: Option<String>,
    },
    /// `membership.access_removed`.
    AccessRemoved {
        /// Account uuid.
        uuid: String,
    },
    /// `silicon.custodian_changed`.
    CustodianChanged {
        /// The Silicon.
        silicon: String,
        /// The new custodian `(uuid, id)`.
        to: Option<(String, String)>,
    },
    /// `ping`.
    Ping,
    /// An event type this version of Commit does not act on.
    Unknown,
}

impl AccountEvent {
    fn account(&self) -> Option<&str> {
        match self {
            Self::IdChanged { uuid, .. }
            | Self::Updated { uuid, .. }
            | Self::Deleted { uuid }
            | Self::SignedOut { uuid, .. }
            | Self::AccessRemoved { uuid } => Some(uuid),
            Self::CustodianChanged { silicon, .. } => Some(silicon),
            Self::Ping | Self::Unknown => None,
        }
    }
}

/// What Commit did with a webhook delivery.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WebhookOutcome {
    /// False when the event id was already processed.
    pub applied: bool,
    /// Short description, also stored with the event id.
    pub outcome: String,
}

impl AccountService {
    /// Applies one verified Accounts webhook event exactly once (dedupe on `event_id`).
    pub async fn apply_webhook(
        &self,
        event_id: &str,
        event_type: &str,
        occurred_at: Option<OffsetDateTime>,
        event: &AccountEvent,
        payload_sha256: &str,
    ) -> Result<WebhookOutcome, AppError> {
        let mut transaction = self.pool.begin().await?;
        if let Some(stored) = store::webhook_event_hash(&mut transaction, event_id).await? {
            if stored != payload_sha256 {
                tracing::warn!(
                    event_id,
                    "Accounts webhook event id reused with a different body; ignored"
                );
            }
            transaction.commit().await?;
            return Ok(WebhookOutcome {
                applied: false,
                outcome: "duplicate event id: already processed".to_owned(),
            });
        }
        // Deliveries without a time are applied as of now.
        let at = occurred_at.unwrap_or_else(OffsetDateTime::now_utc);
        let outcome = self.apply(&mut transaction, event_id, event, at).await?;
        store::record_webhook_event(
            &mut transaction,
            event_id,
            event_type,
            event.account(),
            occurred_at,
            payload_sha256,
            &outcome,
        )
        .await?;
        transaction.commit().await?;
        tracing::info!(event_id, event_type, outcome = %outcome, "applied Silicon Accounts webhook event");
        Ok(WebhookOutcome {
            applied: true,
            outcome,
        })
    }

    async fn apply(
        &self,
        connection: &mut PgConnection,
        event_id: &str,
        event: &AccountEvent,
        at: OffsetDateTime,
    ) -> Result<String, AppError> {
        Ok(match event {
            AccountEvent::IdChanged { uuid, new_id } => {
                if store::apply_id_change(connection, uuid, new_id, at).await? {
                    "id changed".to_owned()
                } else {
                    "ignored: unknown account or newer data already stored".to_owned()
                }
            }
            AccountEvent::Updated {
                uuid,
                account,
                version,
            } => {
                if store::apply_profile_update(connection, uuid, account, *version, at).await? {
                    format!("profile updated to version {version}")
                } else {
                    "ignored: unknown account or an equal or newer version already stored"
                        .to_owned()
                }
            }
            AccountEvent::CustodianChanged { silicon, to } => match to {
                Some((custodian, custodian_id)) => {
                    if store::apply_custodian_change(
                        connection,
                        silicon,
                        custodian,
                        Some(custodian_id.as_str()),
                        at,
                    )
                    .await?
                    {
                        format!("custodian is now {custodian}")
                    } else {
                        "ignored: unknown Silicon or newer data already stored".to_owned()
                    }
                }
                None => "ignored: no new custodian in the event".to_owned(),
            },
            AccountEvent::SignedOut { uuid, reason } => match reason.as_deref() {
                // Commit itself revoked one sign-in (a CLI logout or the website's sign-out):
                // the account's other sign-ins stay valid.
                Some("app_revoked") => {
                    "one Commit sign-in ended; other sign-ins stay valid".to_owned()
                }
                other => {
                    store::revoke_before(connection, uuid, at).await?;
                    format!(
                        "every sign-in from before the event is refused (reason: {})",
                        other.unwrap_or("unspecified")
                    )
                }
            },
            AccountEvent::AccessRemoved { uuid } => {
                store::block_access(connection, uuid, at, false).await?;
                "access removed: every earlier sign-in is refused".to_owned()
            }
            AccountEvent::Deleted { uuid } => {
                store::block_access(connection, uuid, at, true).await?;
                let summary = store::forget(
                    connection,
                    uuid,
                    at,
                    &format!("accounts-webhook:{event_id}"),
                    seconds(self.tombstone_retention),
                    seconds(self.audit_retention),
                )
                .await?;
                format!("account deleted: {summary}")
            }
            AccountEvent::Ping => "ping".to_owned(),
            AccountEvent::Unknown => "event type not used by Commit; recorded".to_owned(),
        })
    }
}

fn seconds(duration: Duration) -> i64 {
    i64::try_from(duration.as_secs()).unwrap_or(i64::MAX).max(1)
}
