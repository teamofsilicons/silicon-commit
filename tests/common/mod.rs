//! Shared fixtures for PostgreSQL-backed tests: accounts, custodian circles and a
//! Silicon Accounts directory double.

#![allow(dead_code, clippy::missing_errors_doc, missing_docs)]

use std::{
    collections::{BTreeSet, HashMap},
    env,
    num::NonZeroU32,
    sync::Mutex,
    time::Duration,
};

use anyhow::{Context as _, bail};
use async_trait::async_trait;
use secrecy::SecretString;
use sqlx::PgPool;
use uuid::Uuid;

use silicon_commit::{
    application::{
        idempotency::{IdempotencyKey, MutationResponse},
        ports::{
            AuthenticationRequest, Grant, IdentityProvider, ProviderError, ResolvedAccount,
            VerifiedActor,
        },
    },
    config::DatabaseSettings,
    domain::{AccountUuid, Actor, ActorId, ActorType},
    error::AppError,
    infrastructure::postgres,
};

pub const IDEMPOTENCY_TTL: Duration = Duration::from_hours(24);
pub const AUDIT_RETENTION: Duration = Duration::from_hours(7 * 365 * 24);
pub const TOMBSTONE_RETENTION: Duration = Duration::from_hours(1_080);

/// Connects to `COMMIT_TEST_DATABASE_URL` after applying every migration (twice,
/// proving the ledger is stable). `None` when the variable is unset.
pub async fn test_pool() -> anyhow::Result<Option<PgPool>> {
    let Ok(database_url) = env::var("COMMIT_TEST_DATABASE_URL") else {
        eprintln!("skipping PostgreSQL integration test: COMMIT_TEST_DATABASE_URL is not set");
        return Ok(None);
    };
    let Some(max_connections) = NonZeroU32::new(8) else {
        bail!("the fixed integration-test pool size must be non-zero");
    };
    let settings = DatabaseSettings {
        url: SecretString::from(database_url),
        max_connections,
        min_connections: 0,
        acquire_timeout: Duration::from_secs(5),
        statement_timeout: Duration::from_secs(30),
    };
    let migration_pool = postgres::connect_migrator(&settings, "commit-integration-migrator")
        .await
        .context("connect migrator to COMMIT_TEST_DATABASE_URL")?;
    let migration_owner = sqlx::query_scalar::<_, String>("SELECT current_user::text")
        .fetch_one(&migration_pool)
        .await
        .context("read integration migration owner")?;
    postgres::migrate(&migration_pool, &migration_owner)
        .await
        .context("apply Commit migrations to the test database")?;
    postgres::migrate(&migration_pool, &migration_owner)
        .await
        .context("reapply Commit migrations using the stable public ledger")?;
    migration_pool.close().await;

    let pool = postgres::connect(&settings, "commit-integration-runtime")
        .await
        .context("connect to COMMIT_TEST_DATABASE_URL")?;
    Ok(Some(pool))
}

/// A unique, Accounts-shaped uuid (short, case-sensitive, alphanumeric).
pub fn new_uuid() -> String {
    let raw = Uuid::new_v4().simple().to_string();
    // Mix case so tests prove uuids are compared exactly.
    raw.chars()
        .take(12)
        .enumerate()
        .map(|(index, character)| {
            if index % 2 == 0 {
                character.to_ascii_uppercase()
            } else {
                character
            }
        })
        .collect()
}

fn suffix() -> String {
    Uuid::new_v4().simple().to_string()[..10].to_owned()
}

/// Creates accounts directly in `commit.accounts`, as Silicon Accounts would have described them.
pub struct World {
    pub pool: PgPool,
}

impl World {
    pub fn new(pool: &PgPool) -> Self {
        Self { pool: pool.clone() }
    }

    async fn insert(
        &self,
        kind: ActorType,
        label: &str,
        custodian: Option<&AccountUuid>,
    ) -> anyhow::Result<Actor> {
        let uuid = AccountUuid::new(new_uuid())?;
        let prefix = match kind {
            ActorType::Carbon => "c",
            ActorType::Silicon => "si",
        };
        let id = ActorId::new(format!("{prefix}:{label}-{}", suffix()))?;
        sqlx::query(
            r"
            INSERT INTO commit.accounts (uuid, kind, public_id, display_name, custodian_uuid, refreshed_at)
            VALUES ($1, $2, $3, $4, $5, clock_timestamp())
            ",
        )
        .bind(uuid.as_str())
        .bind(kind)
        .bind(id.as_str())
        .bind(label)
        .bind(custodian.map(AccountUuid::as_str))
        .execute(&self.pool)
        .await?;
        Ok(Actor::new(uuid, kind, id))
    }

    /// A new Carbon signed in with its own access token.
    pub async fn carbon(&self, label: &str) -> anyhow::Result<VerifiedActor> {
        let actor = self.insert(ActorType::Carbon, label, None).await?;
        Ok(VerifiedActor::new(actor, bearer()))
    }

    /// A new Silicon whose custodian is `custodian`.
    pub async fn silicon(
        &self,
        label: &str,
        custodian: &VerifiedActor,
    ) -> anyhow::Result<VerifiedActor> {
        let actor = self
            .insert(ActorType::Silicon, label, Some(custodian.uuid()))
            .await?;
        Ok(VerifiedActor::new(actor, bearer()).with_custodian(Some(custodian.uuid().clone())))
    }

    /// The same Carbon after Commit learned about the Silicons it is custodian of.
    pub async fn refreshed(&self, carbon: &VerifiedActor) -> anyhow::Result<VerifiedActor> {
        let managed = sqlx::query_scalar::<_, String>(
            "SELECT uuid FROM commit.accounts WHERE custodian_uuid = $1 AND kind = 'silicon'",
        )
        .bind(carbon.uuid().as_str())
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(AccountUuid::new)
        .collect::<Result<BTreeSet<_>, _>>()?;
        Ok(carbon.clone().with_managed_silicons(managed))
    }
}

/// A bearer grant without revocation data.
pub fn bearer() -> Grant {
    Grant::Bearer {
        issued_at: None,
        family: None,
    }
}

/// The same account acting through another app's proof.
pub fn via(actor: &VerifiedActor, app: &str, scopes: &[&str]) -> VerifiedActor {
    let mut proxied = actor.clone();
    proxied.grant = Grant::Proof {
        issuing_app: app.to_owned(),
        proof_id: format!("proof-{}", suffix()),
        scopes: scopes.iter().map(|scope| (*scope).to_owned()).collect(),
    };
    proxied
}

/// An in-memory Silicon Accounts directory.
#[derive(Default)]
pub struct Directory {
    accounts: Mutex<HashMap<String, ResolvedAccount>>,
}

impl Directory {
    pub fn with(accounts: &[&VerifiedActor]) -> Self {
        let directory = Self::default();
        for account in accounts {
            directory.add(account);
        }
        directory
    }

    pub fn add(&self, account: &VerifiedActor) {
        let resolved = ResolvedAccount::known(account.actor.clone(), account.custodian.clone());
        if let Ok(mut accounts) = self.accounts.lock() {
            accounts.insert(
                account.actor.id.as_str().to_ascii_lowercase(),
                resolved.clone(),
            );
            accounts.insert(account.uuid().as_str().to_owned(), resolved);
        }
    }
}

#[async_trait]
impl IdentityProvider for Directory {
    async fn authenticate(
        &self,
        _request: &AuthenticationRequest,
    ) -> Result<VerifiedActor, ProviderError> {
        Err(ProviderError::Unavailable)
    }

    async fn resolve_accounts(
        &self,
        ids: &[ActorId],
        required_type: Option<ActorType>,
    ) -> Result<Vec<ResolvedAccount>, ProviderError> {
        let accounts = self
            .accounts
            .lock()
            .map_err(|_| ProviderError::Unavailable)?;
        let mut seen = BTreeSet::new();
        let mut resolved = Vec::new();
        for id in ids {
            let key = if id.prefixed_kind().is_some() {
                id.as_str().to_ascii_lowercase()
            } else {
                id.as_str().to_owned()
            };
            if !seen.insert(key.clone()) {
                continue;
            }
            let account =
                accounts
                    .get(&key)
                    .cloned()
                    .ok_or_else(|| ProviderError::UnknownAccount {
                        id: id.as_str().to_owned(),
                    })?;
            if required_type.is_some_and(|kind| kind != account.actor.actor_type) {
                return Err(ProviderError::UnusableAccount {
                    id: id.as_str().to_owned(),
                    reason: "is the wrong kind of account for this field".to_owned(),
                });
            }
            resolved.push(account);
        }
        Ok(resolved)
    }
}

pub fn unique_key(prefix: &str) -> anyhow::Result<IdempotencyKey> {
    IdempotencyKey::new(format!("{prefix}-{}", Uuid::new_v4().simple())).map_err(Into::into)
}

pub fn response_uuid(response: &MutationResponse, field: &str) -> anyhow::Result<Uuid> {
    response
        .body
        .get(field)
        .and_then(serde_json::Value::as_str)
        .with_context(|| format!("mutation response is missing string field {field}"))?
        .parse()
        .with_context(|| format!("mutation response field {field} is not a UUID"))
}

pub fn assert_conflict(
    result: Result<MutationResponse, AppError>,
    expected_code: &str,
) -> anyhow::Result<()> {
    match result {
        Err(AppError::Conflict { code }) if code == expected_code => Ok(()),
        other => bail!("expected conflict code {expected_code}, received {other:?}"),
    }
}

/// Asserts a precise 403 with this code.
pub fn assert_denied<T: std::fmt::Debug>(
    result: Result<T, AppError>,
    expected_code: &str,
) -> anyhow::Result<()> {
    match result {
        Err(AppError::Denied { code, .. }) if code == expected_code => Ok(()),
        other => bail!("expected a 403 {expected_code}, received {other:?}"),
    }
}

pub fn assert_not_found<T: std::fmt::Debug>(result: Result<T, AppError>) -> anyhow::Result<()> {
    match result {
        Err(AppError::NotFound) => Ok(()),
        other => bail!("expected 404, received {other:?}"),
    }
}

pub fn assert_database_code<T>(
    result: Result<T, sqlx::Error>,
    expected_code: &str,
) -> anyhow::Result<()> {
    match result {
        Err(error)
            if error
                .as_database_error()
                .and_then(sqlx::error::DatabaseError::code)
                .as_deref()
                == Some(expected_code) =>
        {
            Ok(())
        }
        Err(error) => bail!("expected database code {expected_code}, received {error:?}"),
        Ok(_) => bail!("expected database code {expected_code}, but the statement succeeded"),
    }
}

/// Creates an isolated database, runs `check` against it and drops it again.
pub async fn with_isolated_database<F, Fut>(prefix: &str, check: F) -> anyhow::Result<()>
where
    F: FnOnce(sqlx::postgres::PgConnectOptions) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<()>>,
{
    use std::str::FromStr as _;
    let Ok(database_url) = env::var("COMMIT_TEST_DATABASE_URL") else {
        eprintln!("skipping PostgreSQL migration test: COMMIT_TEST_DATABASE_URL is not set");
        return Ok(());
    };
    let options = sqlx::postgres::PgConnectOptions::from_str(&database_url)?;
    let admin = PgPool::connect_with(options.clone()).await?;
    let database = format!("{prefix}_{}", Uuid::new_v4().simple());
    // The identifier contains only the fixed prefix and generated hex digits.
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {database}")))
        .execute(&admin)
        .await
        .context("create isolated database; the test role needs CREATEDB")?;
    let outcome = check(options.database(&database)).await;
    let cleanup = sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE {database} WITH (FORCE)"
    )))
    .execute(&admin)
    .await;
    admin.close().await;
    outcome?;
    cleanup.context("remove isolated database")?;
    Ok(())
}
