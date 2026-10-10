//! Silicon Accounts identity adapter.
//!
//! * Bearer access tokens are verified locally (`EdDSA`, `iss` = `ACCOUNTS_URL`,
//!   `aud` = Commit's app id, `exp`/`nbf`) against a cached JWKS that is
//!   refetched (at most every 30 seconds) when a token names an unknown key.
//! * Revocation: tokens issued before the account's recorded cutoff
//!   (`membership.signed_out` / `access_removed` / `account.deleted` webhooks)
//!   are refused; sensitive routes also introspect the token online (cached 30 s).
//! * `Authorization: Proof sap_…` User verification proofs are verified online
//!   (cached up to 30 s), must be for Commit, carry the route's scope, and come
//!   from an app `COMMIT_PROOF_ISSUERS` allows for that scope.
//! * Accounts named by callers are resolved with app lookups (cached 60 s; Accounts
//!   allows 600 lookups a minute per app).

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use secrecy::{ExposeSecret as _, SecretString};
use sha2::{Digest as _, Sha256};
use silicon_accounts_client::{
    AccountKind, AccountSummary, AccountsClient, Claims, Error as AccountsError, Jwks, ProofKind,
    ProofVerification, TokenError, ValidProof, VerifyOptions, verify_access_token,
};
use sqlx::PgPool;
use time::OffsetDateTime;
use tokio::sync::Mutex;

use super::ClientBuildError;
use crate::{
    application::ports::{
        AccountStatus, AuthenticationRequest, Grant, IdentityProvider, InboundCredential,
        ProviderError, ResolvedAccount, VerifiedActor,
    },
    config::{AccountsSettings, ProofIssuers},
    domain::{AccountUuid, Actor, ActorId, ActorType},
    infrastructure::postgres::accounts as account_store,
};

/// A refetch for an unknown signing key happens at most this often.
const JWKS_REFETCH_INTERVAL: Duration = Duration::from_secs(30);
/// Keys are refreshed in the background of a request after this long.
const JWKS_MAX_AGE: Duration = Duration::from_secs(3_600);
/// Introspection and proof results are reused for at most this long.
const ONLINE_CHECK_TTL: Duration = Duration::from_secs(30);
/// Account lookups are reused for this long.
const LOOKUP_TTL: Duration = Duration::from_secs(60);
/// Stored account details are refreshed from Accounts when older than this.
const ACCOUNT_REFRESH_AFTER: time::Duration = time::Duration::minutes(10);
/// Bound on each in-memory cache.
const CACHE_LIMIT: usize = 10_000;

/// A verified proof and when it was verified.
type CachedProof = (Arc<ValidProof>, Instant);
type CachedLookup = (Result<ResolvedAccount, ProviderError>, Instant);

/// Silicon Accounts-backed [`IdentityProvider`].
pub struct AccountsIdentity {
    client: AccountsClient,
    app_id: String,
    app_secret: SecretString,
    issuer: String,
    proof_issuers: ProofIssuers,
    pool: PgPool,
    keys: Mutex<KeyCache>,
    introspections: Mutex<HashMap<[u8; 32], (bool, Instant)>>,
    proofs: Mutex<HashMap<[u8; 32], CachedProof>>,
    lookups: Mutex<HashMap<String, CachedLookup>>,
}

#[derive(Default)]
struct KeyCache {
    jwks: Option<Arc<Jwks>>,
    fetched_at: Option<Instant>,
    last_attempt: Option<Instant>,
}

impl AccountsIdentity {
    /// Builds the adapter from validated API settings.
    ///
    /// # Errors
    ///
    /// Returns an error when the app secret is missing or the HTTP client cannot be built.
    pub fn new(
        settings: &AccountsSettings,
        pool: PgPool,
        connect_timeout: Duration,
        request_timeout: Duration,
    ) -> Result<Self, ClientBuildError> {
        let app_secret = settings
            .app_secret
            .clone()
            .ok_or(ClientBuildError::InvalidCredential)?;
        let client = AccountsClient::builder()
            .base_url(settings.api_url.as_str())
            .connect_timeout(connect_timeout)
            .timeout(request_timeout)
            .max_retries(1)
            .user_agent(concat!("silicon-commit/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| ClientBuildError::InvalidEndpoint)?;
        Ok(Self {
            client,
            app_id: settings.app_id.clone(),
            app_secret,
            issuer: settings.issuer.clone(),
            proof_issuers: settings.proof_issuers.clone(),
            pool,
            keys: Mutex::new(KeyCache::default()),
            introspections: Mutex::new(HashMap::new()),
            proofs: Mutex::new(HashMap::new()),
            lookups: Mutex::new(HashMap::new()),
        })
    }

    /// Commit's app id at Silicon Accounts.
    #[must_use]
    pub fn app_id(&self) -> &str {
        &self.app_id
    }

    async fn current_keys(&self) -> Result<Arc<Jwks>, ProviderError> {
        let mut cache = self.keys.lock().await;
        let fresh = cache
            .fetched_at
            .is_some_and(|fetched| fetched.elapsed() < JWKS_MAX_AGE);
        if let (Some(jwks), true) = (&cache.jwks, fresh) {
            return Ok(Arc::clone(jwks));
        }
        if cache
            .last_attempt
            .is_some_and(|at| at.elapsed() < JWKS_REFETCH_INTERVAL)
        {
            return cache.jwks.clone().ok_or(ProviderError::Unavailable);
        }
        match self.client.jwks().await {
            Ok(jwks) => {
                let jwks = Arc::new(jwks);
                cache.jwks = Some(Arc::clone(&jwks));
                cache.fetched_at = Some(Instant::now());
                cache.last_attempt = Some(Instant::now());
                Ok(jwks)
            }
            Err(error) => {
                cache.last_attempt = Some(Instant::now());
                tracing::warn!(code = error.code(), "Silicon Accounts JWKS fetch failed");
                // Keys rarely change: keep verifying with the last good set.
                cache.jwks.clone().ok_or(ProviderError::Unavailable)
            }
        }
    }

    /// Refetches the key set for a token signed with a key not in the cached set.
    async fn keys_after_unknown_kid(&self) -> Option<Arc<Jwks>> {
        let mut cache = self.keys.lock().await;
        if cache
            .last_attempt
            .is_some_and(|attempt| attempt.elapsed() < JWKS_REFETCH_INTERVAL)
        {
            return None;
        }
        cache.last_attempt = Some(Instant::now());
        match self.client.jwks().await {
            Ok(jwks) => {
                let jwks = Arc::new(jwks);
                cache.jwks = Some(Arc::clone(&jwks));
                cache.fetched_at = Some(Instant::now());
                Some(jwks)
            }
            Err(error) => {
                tracing::warn!(code = error.code(), "Silicon Accounts JWKS refetch failed");
                None
            }
        }
    }

    fn verify_options(&self) -> VerifyOptions {
        VerifyOptions::for_app(&self.app_id).with_issuer(self.issuer.clone())
    }

    async fn verify_bearer(&self, token: &str) -> Result<Claims, ProviderError> {
        let jwks = self.current_keys().await?;
        match verify_access_token(&jwks, token, &self.verify_options()) {
            Ok(claims) => Ok(claims),
            Err(AccountsError::Token(TokenError::UnknownKey { .. })) => {
                let Some(jwks) = self.keys_after_unknown_kid().await else {
                    return Err(token_rejected(&TokenError::UnknownKey { kid: None }));
                };
                verify_access_token(&jwks, token, &self.verify_options()).map_err(|error| {
                    match error {
                        AccountsError::Token(token_error) => token_rejected(&token_error),
                        _ => ProviderError::Unauthenticated,
                    }
                })
            }
            Err(AccountsError::Token(token_error)) => Err(token_rejected(&token_error)),
            Err(_) => Err(ProviderError::Unauthenticated),
        }
    }

    /// Asks Silicon Accounts whether an access token is still active (cached briefly). With
    /// `fresh_after`, a cached answer older than that instant is not trusted.
    async fn introspect_active(
        &self,
        token: &str,
        fresh_after: Option<OffsetDateTime>,
    ) -> Result<bool, ProviderError> {
        let key = digest(token);
        {
            let cache = self.introspections.lock().await;
            if let Some((active, at)) = cache.get(&key)
                && at.elapsed() < ONLINE_CHECK_TTL
                && fresh_after.is_none_or(|after| OffsetDateTime::now_utc() - at.elapsed() > after)
            {
                return Ok(*active);
            }
        }
        let app = self
            .client
            .as_app(&self.app_id, self.app_secret.expose_secret());
        let active = app
            .introspect(token)
            .await
            .map_err(|error| map_accounts_error(&error))?
            .active;
        let mut cache = self.introspections.lock().await;
        prune(&mut cache, |(_, at)| at.elapsed() < ONLINE_CHECK_TTL);
        cache.insert(key, (active, Instant::now()));
        Ok(active)
    }

    /// Verifies a proof at Silicon Accounts (cached briefly). `fresh` skips the cache: a
    /// request that widens access must not rest on an answer from before a revocation.
    async fn verify_proof(
        &self,
        token: &str,
        fresh: bool,
    ) -> Result<Arc<ValidProof>, ProviderError> {
        let key = digest(token);
        if !fresh {
            let cache = self.proofs.lock().await;
            if let Some((proof, at)) = cache.get(&key)
                && at.elapsed() < ONLINE_CHECK_TTL
                && proof.expires_at > OffsetDateTime::now_utc()
            {
                return Ok(Arc::clone(proof));
            }
        }
        let app = self
            .client
            .as_app(&self.app_id, self.app_secret.expose_secret());
        let verification = app
            .verify_proof(token)
            .await
            .map_err(|error| map_accounts_error(&error))?;
        let proof = match verification {
            ProofVerification::Valid(proof) => Arc::new(*proof),
            _ => {
                return Err(ProviderError::Rejected {
                    code: "proof_invalid",
                    message: "Silicon Accounts says this proof is not valid for Commit: it is unknown, expired, revoked, issued for another app, or speaks for an account that can no longer use Commit. Ask the issuing app for a fresh proof.".to_owned(),
                });
            }
        };
        let mut cache = self.proofs.lock().await;
        prune(&mut cache, |(_, at)| at.elapsed() < ONLINE_CHECK_TTL);
        cache.insert(key, (Arc::clone(&proof), Instant::now()));
        Ok(proof)
    }
}

fn digest(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

fn prune<K, V>(cache: &mut HashMap<K, V>, keep: impl Fn(&V) -> bool)
where
    K: Eq + std::hash::Hash,
{
    if cache.len() >= CACHE_LIMIT {
        cache.retain(|_, value| keep(value));
        if cache.len() >= CACHE_LIMIT {
            cache.clear();
        }
    }
}

/// A precise 401 for a token that is not acceptable.
fn token_rejected(error: &TokenError) -> ProviderError {
    ProviderError::Rejected {
        code: error.code(),
        message: format!("{} {}", error.message(), error.hint()),
    }
}

/// Maps a Silicon Accounts client failure to Commit's provider error.
pub(crate) fn map_accounts_error(error: &AccountsError) -> ProviderError {
    match error.status() {
        Some(404) => ProviderError::NotFound,
        Some(429) => ProviderError::RateLimited {
            retry_after: error.retry_after(),
        },
        Some(401 | 403) => {
            tracing::error!(
                code = error.code(),
                "Silicon Accounts refused Commit's app credentials; check COMMIT_APP_ID and COMMIT_APP_SECRET"
            );
            ProviderError::Unavailable
        }
        Some(_) => ProviderError::Unavailable,
        None if error.is_transport() => ProviderError::Unavailable,
        None => match error {
            AccountsError::Decode { .. } => ProviderError::InvalidResponse,
            _ => ProviderError::Unavailable,
        },
    }
}

/// A lookup that Accounts refuses as malformed (`invalid_uuid`, `invalid_id`) names no account.
fn lookup_error(error: &AccountsError) -> ProviderError {
    if error.status() == Some(400) && matches!(error.code(), "invalid_uuid" | "invalid_id") {
        return ProviderError::NotFound;
    }
    map_accounts_error(error)
}

fn actor_type(kind: AccountKind) -> ActorType {
    match kind {
        AccountKind::Carbon => ActorType::Carbon,
        AccountKind::Silicon => ActorType::Silicon,
    }
}

fn kind_of(kind: Option<AccountKind>, id: &str) -> Option<ActorType> {
    kind.or_else(|| AccountKind::of_id(id)).map(actor_type)
}

/// Converts an account as Commit sees it (`userinfo`, token responses) into a resolved account.
pub(crate) fn resolved_from_app_view(
    account: &silicon_accounts_client::AccountForApp,
) -> Result<ResolvedAccount, ProviderError> {
    let uuid =
        AccountUuid::new(account.uuid.clone()).map_err(|_| ProviderError::InvalidResponse)?;
    let custodian = account
        .custodian
        .as_ref()
        .map(|custodian| AccountUuid::new(custodian.uuid.clone()))
        .transpose()
        .map_err(|_| ProviderError::InvalidResponse)?;
    Ok(ResolvedAccount {
        actor: Actor::new(
            uuid,
            actor_type(account.kind),
            ActorId::from_persisted(account.id.clone()),
        ),
        status: AccountStatus::Active,
        custodian,
        display_name: account.display_name.clone(),
        pfp_url: account.pfp_url.clone(),
        observed_at: OffsetDateTime::now_utc(),
    })
}

/// Converts a lookup result into Commit's resolved account.
pub(crate) fn resolved_from_summary(
    summary: &AccountSummary,
) -> Result<ResolvedAccount, ProviderError> {
    let uuid =
        AccountUuid::new(summary.uuid.clone()).map_err(|_| ProviderError::InvalidResponse)?;
    let custodian = summary
        .custodian
        .as_ref()
        .map(|custodian| AccountUuid::new(custodian.uuid.clone()))
        .transpose()
        .map_err(|_| ProviderError::InvalidResponse)?;
    Ok(ResolvedAccount {
        actor: Actor::new(
            uuid,
            actor_type(summary.kind),
            ActorId::from_persisted(summary.id.clone()),
        ),
        status: account_store::status_or_inactive(&summary.status),
        custodian,
        display_name: summary.display_name.clone(),
        pfp_url: summary.pfp_url.clone(),
        observed_at: OffsetDateTime::now_utc(),
    })
}

/// Who a verified credential speaks for, before Commit's own account checks.
struct Subject {
    actor: Actor,
    grant: Grant,
    /// The caller's own access token, used to read what it shared with Commit (userinfo).
    bearer: Option<SecretString>,
}

impl AccountsIdentity {
    async fn bearer_subject(&self, token: &str, sensitive: bool) -> Result<Subject, ProviderError> {
        let claims = self.verify_bearer(token).await?;
        let uuid = AccountUuid::new(claims.sub.clone()).map_err(|_| ProviderError::Rejected {
            code: "token_missing_claim",
            message: "The access token's subject is not a Silicon Accounts uuid.".to_owned(),
        })?;
        let id = claims.id.clone().unwrap_or_default();
        let kind = kind_of(claims.kind, &id).ok_or_else(|| ProviderError::Rejected {
            code: "token_missing_claim",
            message: "The access token names neither the account kind nor a c:/si: id.".to_owned(),
        })?;
        // A change that widens access asks every time: an answer cached from before a sign-out
        // that just happened must not let it through.
        if sensitive
            && !self
                .introspect_active(token, Some(OffsetDateTime::now_utc()))
                .await?
        {
            return Err(ProviderError::Rejected {
                code: "token_revoked",
                message: "Silicon Accounts says this access token is no longer active: the sign-in was ended or Commit's access was removed. Sign in to Commit again.".to_owned(),
            });
        }
        Ok(Subject {
            actor: Actor::new(uuid, kind, ActorId::from_persisted(id)),
            grant: Grant::Bearer {
                issued_at: claims.iat,
                family: claims.fid.clone(),
            },
            bearer: Some(SecretString::from(token.to_owned())),
        })
    }

    async fn proof_subject(
        &self,
        token: &str,
        scope: &str,
        sensitive: bool,
    ) -> Result<Subject, ProviderError> {
        let proof = self.verify_proof(token, sensitive).await?;
        if proof.receiving_app.app_id != self.app_id {
            return Err(ProviderError::Rejected {
                code: "proof_wrong_receiver",
                message: format!(
                    "The proof was issued for app {}, not for {}.",
                    proof.receiving_app.app_id, self.app_id
                ),
            });
        }
        let issuing_app = proof.issuing_app.app_id.clone();
        let Some(user) = proof
            .user
            .clone()
            .filter(|_| proof.kind == ProofKind::UserVerification)
        else {
            return Err(ProviderError::Rejected {
                code: "proof_without_account",
                message: format!(
                    "{issuing_app} sent an App verification proof, which speaks for no account; Commit only acts for an account, so it needs a User verification proof."
                ),
            });
        };
        if !proof.scopes.iter().any(|granted| granted == scope) {
            return Err(ProviderError::Denied {
                code: "proof_scope_missing",
                message: format!(
                    "The proof from {issuing_app} grants [{}], but this request needs the scope {scope}.",
                    proof.scopes.join(", ")
                ),
            });
        }
        if !self.proof_issuers.allows(scope, &issuing_app) {
            return Err(ProviderError::Denied {
                code: "proof_issuer_not_allowed",
                message: format!(
                    "Commit does not accept proofs from {issuing_app} for {scope}. The operator lists the apps allowed per scope in COMMIT_PROOF_ISSUERS."
                ),
            });
        }
        let uuid =
            AccountUuid::new(user.uuid.clone()).map_err(|_| ProviderError::InvalidResponse)?;
        let kind = kind_of(user.kind, &user.id).ok_or(ProviderError::InvalidResponse)?;
        Ok(Subject {
            actor: Actor::new(uuid, kind, ActorId::from_persisted(user.id.clone())),
            grant: Grant::Proof {
                issuing_app,
                proof_id: proof.proof_id.clone(),
                scopes: proof.scopes.clone(),
            },
            bearer: None,
        })
    }

    /// Applies Commit's own account state and loads the custodian circle.
    async fn verified(&self, subject: Subject) -> Result<VerifiedActor, ProviderError> {
        let Subject {
            actor,
            grant,
            bearer,
        } = subject;
        let retired: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM commit.accounts_uuid128_map WHERE old_uuid=$1)",
        )
        .bind(actor.uuid.as_str())
        .fetch_one(&self.pool)
        .await
        .map_err(|_| ProviderError::Unavailable)?;
        if retired {
            return Err(ProviderError::Rejected {
                code: "session_ended",
                message: "This account identity was migrated. Sign in to Commit again.".to_owned(),
            });
        }
        let stored = account_store::load(&self.pool, &actor.uuid)
            .await
            .map_err(|_| ProviderError::Unavailable)?;
        let lifecycle = account_store::lifecycle(&self.pool, &actor.uuid)
            .await
            .map_err(|_| ProviderError::Unavailable)?;
        if stored
            .as_ref()
            .is_some_and(account_store::StoredAccount::is_deleted)
            || lifecycle
                .as_ref()
                .is_some_and(|state| state.deleted_at.is_some())
        {
            return Err(ProviderError::Rejected {
                code: "account_deleted",
                message: "This Silicon Accounts account was deleted; it can no longer use Commit."
                    .to_owned(),
            });
        }
        let cutoff = lifecycle
            .as_ref()
            .and_then(|state| state.revoked_before)
            .or_else(|| stored.as_ref().and_then(|row| row.revoked_before));
        if let (Grant::Bearer { issued_at, .. }, Some(cutoff)) = (&grant, cutoff)
            && self
                .ended_by_cutoff(*issued_at, cutoff, bearer.as_ref())
                .await?
        {
            return Err(ProviderError::Rejected {
                code: "session_ended",
                message: "This sign-in to Commit ended. Sign in to Commit again.".to_owned(),
            });
        }
        let restoring_access = lifecycle.as_ref().and_then(|state| state.access_removed_at);
        if let Some(removed) = restoring_access {
            // Proofs issued by other apps cannot restore this account's grant to Commit.
            let Some(token) = &bearer else {
                return Err(ProviderError::Rejected {
                    code: "app_access_removed", message: "Commit's access was removed. Sign in to Commit again before using app proofs.".to_owned(),
                });
            };
            if !self
                .introspect_active(token.expose_secret(), Some(removed))
                .await?
            {
                return Err(ProviderError::Rejected {
                    code: "session_ended",
                    message: "This sign-in to Commit ended. Sign in to Commit again.".to_owned(),
                });
            }
        }

        let stale = restoring_access.is_some()
            || stored.as_ref().is_none_or(|stored| {
                stored.refreshed_at < OffsetDateTime::now_utc() - ACCOUNT_REFRESH_AFTER
                // Known only from lookups (someone named the account before it used
                // Commit): read its own view once, which alone carries its display name
                // and the email it shared with Commit.
                || (bearer.is_some() && stored.accounts_version == 0)
            });
        let mut custodian = stored.as_ref().and_then(|stored| stored.custodian.clone());
        let mut current = actor.clone();
        if stale {
            match self.refresh(&actor.uuid, bearer.as_ref()).await {
                Ok(fresh) if fresh.actor.actor_type == actor.actor_type => {
                    if fresh.status == AccountStatus::Deleted {
                        return Err(ProviderError::Rejected {
                            code: "account_deleted",
                            message: "This Silicon Accounts account was deleted; it can no longer use Commit.".to_owned(),
                        });
                    }
                    account_store::remember(&self.pool, &fresh)
                        .await
                        .map_err(|_| ProviderError::Unavailable)?;
                    // A webhook newer than a cached lookup wins in the store and here.
                    let remembered = account_store::load(&self.pool, &actor.uuid)
                        .await
                        .map_err(|_| ProviderError::Unavailable)?
                        .ok_or(ProviderError::Unavailable)?;
                    custodian = remembered.custodian;
                    current = remembered.actor;
                }
                Ok(_) => return Err(ProviderError::InvalidResponse),
                Err(
                    error @ (ProviderError::Rejected { .. }
                    | ProviderError::NotFound
                    | ProviderError::InvalidResponse),
                ) => return Err(error),
                Err(error) if restoring_access.is_some() => return Err(error),
                Err(error) if stored.is_none() => {
                    // Remember what the verified credential says; a later request refreshes it.
                    tracing::warn!(
                        ?error,
                        "account lookup failed; using the verified credential's identity"
                    );
                    let mut connection = self
                        .pool
                        .acquire()
                        .await
                        .map_err(|_| ProviderError::Unavailable)?;
                    account_store::ensure_actor(&mut connection, &actor)
                        .await
                        .map_err(|_| ProviderError::Unavailable)?;
                }
                Err(error) => {
                    tracing::warn!(
                        ?error,
                        "account refresh failed; using stored account details"
                    );
                    if let Some(stored) = &stored {
                        current = stored.actor.clone();
                    }
                    // A stale relationship is not authority when Accounts cannot refresh it.
                    custodian = None;
                }
            }
        } else if let Some(stored) = &stored {
            current = stored.actor.clone();
        }

        if let Some(removed) = restoring_access
            && !account_store::restore_access(&self.pool, &actor.uuid, removed)
                .await
                .map_err(|_| ProviderError::Unavailable)?
        {
            return Err(ProviderError::Unavailable);
        }

        let managed = if current.actor_type == ActorType::Carbon {
            for uuid in account_store::stale_managed(&self.pool, &current.uuid)
                .await
                .map_err(|_| ProviderError::Unavailable)?
            {
                if let Ok(fresh) = self.lookup(LookupKey::Uuid(uuid)).await {
                    account_store::remember(&self.pool, &fresh)
                        .await
                        .map_err(|_| ProviderError::Unavailable)?;
                }
            }
            account_store::managed_silicons(&self.pool, &current.uuid)
                .await
                .map_err(|_| ProviderError::Unavailable)?
        } else {
            std::collections::BTreeSet::new()
        };
        let custodian = if current.is_silicon() {
            custodian
        } else {
            None
        };
        Ok(VerifiedActor::new(current, grant)
            .with_custodian(custodian)
            .with_managed_silicons(managed))
    }

    /// Whether a bearer token predates the account's revocation cutoff (a sign-out other
    /// than Commit's own, removed access). `iat` has whole seconds, so a token from the
    /// cutoff's own second may have been issued just before it or just after it (a new
    /// sign-in): Silicon Accounts, which ended the earlier sign-ins, decides.
    async fn ended_by_cutoff(
        &self,
        issued_at: Option<i64>,
        cutoff: OffsetDateTime,
        bearer: Option<&SecretString>,
    ) -> Result<bool, ProviderError> {
        let cutoff_second = cutoff.unix_timestamp();
        match (issued_at, bearer) {
            (Some(issued_at), _) if issued_at > cutoff_second => Ok(false),
            (Some(issued_at), _) if issued_at == cutoff_second && cutoff.nanosecond() == 0 => {
                Ok(false)
            }
            (Some(issued_at), Some(token)) if issued_at == cutoff_second => Ok(!self
                .introspect_active(token.expose_secret(), Some(cutoff))
                .await?),
            _ => Ok(true),
        }
    }

    /// Fresh account details: the caller's own view through `userinfo` (which also
    /// carries the email it shared with Commit), else an app lookup.
    async fn refresh(
        &self,
        uuid: &AccountUuid,
        bearer: Option<&SecretString>,
    ) -> Result<ResolvedAccount, ProviderError> {
        if let Some(token) = bearer {
            let app = self
                .client
                .as_app(&self.app_id, self.app_secret.expose_secret());
            match app.userinfo(token.expose_secret()).await {
                Ok(info) if info.account.uuid == uuid.as_str() => {
                    let resolved = resolved_from_app_view(&info.account)?;
                    account_store::remember(&self.pool, &resolved)
                        .await
                        .map_err(|_| ProviderError::Unavailable)?;
                    account_store::remember_own_view(
                        &self.pool,
                        uuid,
                        info.account.email.as_deref(),
                        info.account.version,
                    )
                    .await
                    .map_err(|_| ProviderError::Unavailable)?;
                    return Ok(resolved);
                }
                Ok(_) => return Err(ProviderError::InvalidResponse),
                Err(error) if matches!(error.status(), Some(401 | 403 | 404)) => {
                    return Err(ProviderError::Rejected {
                        code: "session_ended",
                        message: "Silicon Accounts refused this sign-in. Sign in to Commit again."
                            .to_owned(),
                    });
                }
                Err(error) => {
                    tracing::debug!(
                        code = error.code(),
                        "userinfo unavailable; trying an account lookup"
                    );
                }
            }
        }
        self.lookup(LookupKey::Uuid(uuid.clone())).await
    }

    async fn lookup(&self, key: LookupKey) -> Result<ResolvedAccount, ProviderError> {
        let cache_key = key.cache_key();
        {
            let cache = self.lookups.lock().await;
            if let Some((account, at)) = cache.get(&cache_key)
                && at.elapsed()
                    < if account.is_ok() {
                        LOOKUP_TTL
                    } else {
                        Duration::from_secs(10)
                    }
            {
                return account.clone();
            }
        }
        let app = self
            .client
            .as_app(&self.app_id, self.app_secret.expose_secret());
        let summary = match match &key {
            LookupKey::Uuid(uuid) => app.lookup(uuid.as_str()).await,
            LookupKey::Id(id) => app.lookup_by_id(id).await,
        } {
            Ok(summary) => summary,
            Err(error) => {
                let error = lookup_error(&error);
                let mut cache = self.lookups.lock().await;
                prune(&mut cache, |(_, at)| at.elapsed() < LOOKUP_TTL);
                if error == ProviderError::NotFound {
                    cache.insert(cache_key, (Err(error.clone()), Instant::now()));
                }
                return Err(error);
            }
        };
        let account = resolved_from_summary(&summary)?;
        let mut cache = self.lookups.lock().await;
        prune(&mut cache, |(_, at)| at.elapsed() < LOOKUP_TTL);
        let now = Instant::now();
        cache.insert(
            LookupKey::Uuid(account.actor.uuid.clone()).cache_key(),
            (Ok(account.clone()), now),
        );
        if !account.actor.id.as_str().is_empty() {
            cache.insert(
                LookupKey::Id(account.actor.id.as_str().to_ascii_lowercase()).cache_key(),
                (Ok(account.clone()), now),
            );
        }
        Ok(account)
    }
}

/// How a caller named an account.
#[derive(Clone, Debug, Eq, PartialEq)]
enum LookupKey {
    Uuid(AccountUuid),
    Id(String),
}

impl LookupKey {
    fn for_id(id: &ActorId) -> Result<Self, ProviderError> {
        if id.prefixed_kind().is_some() {
            return Ok(Self::Id(id.as_str().to_ascii_lowercase()));
        }
        // Silicon Accounts uuids are 3 to 12 characters of a-z, A-Z and 0-9; anything else
        // names no account, so there is nothing to ask Accounts.
        let value = id.as_str();
        let legacy = (3..=12).contains(&value.len())
            && value.bytes().all(|byte| byte.is_ascii_alphanumeric());
        let canonical = value.len() == 36
            && uuid::Uuid::parse_str(value).is_ok_and(|id| id.to_string() == value);
        if !(legacy || canonical) {
            return Err(ProviderError::UnknownAccount {
                id: value.to_owned(),
            });
        }
        AccountUuid::new(value)
            .map(Self::Uuid)
            .map_err(|_| ProviderError::UnknownAccount {
                id: value.to_owned(),
            })
    }

    fn cache_key(&self) -> String {
        match self {
            Self::Uuid(uuid) => format!("uuid:{uuid}"),
            Self::Id(id) => format!("id:{id}"),
        }
    }
}

/// Checks that a resolved account can be named in a request.
pub(crate) fn usable(
    requested: &ActorId,
    account: ResolvedAccount,
    required_type: Option<ActorType>,
) -> Result<ResolvedAccount, ProviderError> {
    match account.status {
        AccountStatus::Deleted => {
            return Err(ProviderError::UnknownAccount {
                id: requested.as_str().to_owned(),
            });
        }
        AccountStatus::PendingCustodian => {
            return Err(ProviderError::UnusableAccount {
                id: requested.as_str().to_owned(),
                reason: "is a Silicon still waiting for its custodian to accept it".to_owned(),
            });
        }
        AccountStatus::Active | AccountStatus::Unclaimed => {}
    }
    if let Some(required) = required_type
        && required != account.actor.actor_type
    {
        let (is, takes) = match account.actor.actor_type {
            ActorType::Carbon => ("a Carbon", "Silicons (si: ids)"),
            ActorType::Silicon => ("a Silicon", "Carbons (c: ids)"),
        };
        return Err(ProviderError::UnusableAccount {
            id: requested.as_str().to_owned(),
            reason: format!("is {is}; this field takes {takes}"),
        });
    }
    Ok(account)
}

#[async_trait]
impl IdentityProvider for AccountsIdentity {
    async fn authenticate(
        &self,
        request: &AuthenticationRequest,
    ) -> Result<VerifiedActor, ProviderError> {
        let subject = match &request.credential {
            InboundCredential::Bearer(_) => {
                self.bearer_subject(request.credential.expose(), request.sensitive)
                    .await?
            }
            InboundCredential::Proof(_) => {
                self.proof_subject(
                    request.credential.expose(),
                    &request.scope,
                    request.sensitive,
                )
                .await?
            }
        };
        self.verified(subject).await
    }

    async fn resolve_accounts(
        &self,
        ids: &[ActorId],
        required_type: Option<ActorType>,
    ) -> Result<Vec<ResolvedAccount>, ProviderError> {
        let mut seen = HashSet::with_capacity(ids.len());
        let mut accounts = Vec::with_capacity(ids.len());
        for id in ids {
            let key = LookupKey::for_id(id)?;
            // c:/si: ids are case-insensitive; account uuids are exact.
            if !seen.insert(key.cache_key()) {
                continue;
            }
            let account = self.lookup(key).await.map_err(|error| match error {
                ProviderError::NotFound => ProviderError::UnknownAccount {
                    id: id.as_str().to_owned(),
                },
                other => other,
            })?;
            accounts.push(usable(id, account, required_type)?);
        }
        Ok(accounts)
    }
}
