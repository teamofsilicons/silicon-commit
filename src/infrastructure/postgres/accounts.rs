//! PostgreSQL persistence for Silicon Accounts identities, the Silicon allow-list
//! and the Accounts webhook inbox.

use std::collections::BTreeSet;

use serde_json::Value;
use sqlx::{FromRow, PgConnection, PgExecutor};
use time::OffsetDateTime;

use crate::{
    application::ports::{AccountStatus, ResolvedAccount},
    domain::{AccountUuid, Actor, ActorId, ActorRef, ActorType},
    error::AppError,
};

/// One row of `commit.accounts`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StoredAccount {
    pub(crate) actor: Actor,
    pub(crate) display_name: String,
    pub(crate) pfp_url: String,
    pub(crate) email: Option<String>,
    pub(crate) status: String,
    pub(crate) custodian: Option<AccountUuid>,
    pub(crate) revoked_before: Option<OffsetDateTime>,
    /// When the newest information stored was true at Silicon Accounts.
    pub(crate) refreshed_at: OffsetDateTime,
    /// Silicon Accounts' version of the account, from its own view (`userinfo`) or
    /// `account.updated`; 0 while Commit knows the account only from lookups.
    pub(crate) accounts_version: i64,
}

impl StoredAccount {
    /// True for a deleted account: it can never act again.
    pub(crate) fn is_deleted(&self) -> bool {
        self.status == "deleted"
    }
}

#[derive(FromRow)]
struct AccountRow {
    uuid: String,
    kind: ActorType,
    public_id: String,
    display_name: String,
    pfp_url: String,
    email: Option<String>,
    status: String,
    custodian_uuid: Option<String>,
    revoked_before: Option<OffsetDateTime>,
    refreshed_at: OffsetDateTime,
    accounts_version: i64,
}

impl AccountRow {
    fn into_stored(self) -> Result<StoredAccount, AppError> {
        Ok(StoredAccount {
            actor: Actor::new(
                account_uuid(self.uuid)?,
                self.kind,
                ActorId::from_persisted(self.public_id),
            ),
            display_name: self.display_name,
            pfp_url: self.pfp_url,
            email: self.email,
            status: self.status,
            custodian: self.custodian_uuid.map(account_uuid).transpose()?,
            revoked_before: self.revoked_before,
            refreshed_at: self.refreshed_at,
            accounts_version: self.accounts_version,
        })
    }
}

/// Converts a stored uuid back into the domain type.
pub(crate) fn account_uuid(value: String) -> Result<AccountUuid, AppError> {
    AccountUuid::new(value).map_err(|error| {
        AppError::Internal(anyhow::anyhow!("invalid persisted account uuid: {error}"))
    })
}

/// Builds a public reference from stored columns.
pub(crate) fn actor_ref(
    kind: ActorType,
    public_id: String,
    uuid: String,
) -> Result<ActorRef, AppError> {
    Ok(ActorRef::new(
        kind,
        ActorId::from_persisted(public_id),
        account_uuid(uuid)?,
    ))
}

/// Reads one account.
pub(crate) async fn load<'e, E>(
    executor: E,
    uuid: &AccountUuid,
) -> Result<Option<StoredAccount>, AppError>
where
    E: PgExecutor<'e>,
{
    sqlx::query_as::<_, AccountRow>(
        r"
        SELECT uuid, kind, public_id, display_name, pfp_url, email, status, custodian_uuid,
               revoked_before, refreshed_at, accounts_version
          FROM commit.accounts
         WHERE uuid = $1
        ",
    )
    .bind(uuid.as_str())
    .fetch_optional(executor)
    .await?
    .map(AccountRow::into_stored)
    .transpose()
}

/// The Silicons whose custodian is `custodian`, as known to Commit.
pub(crate) async fn managed_silicons<'e, E>(
    executor: E,
    custodian: &AccountUuid,
) -> Result<BTreeSet<AccountUuid>, AppError>
where
    E: PgExecutor<'e>,
{
    sqlx::query_scalar::<_, String>(
        r"
        SELECT uuid FROM commit.accounts
         WHERE custodian_uuid = $1 AND kind = 'silicon' AND status NOT IN ('deleted', 'unlinked')
        ",
    )
    .bind(custodian.as_str())
    .fetch_all(executor)
    .await?
    .into_iter()
    .map(account_uuid)
    .collect()
}

/// Makes sure an account row exists without overwriting fresher data.
///
/// Used for accounts known only from a verified token or proof. A row inserted
/// here is marked stale with `refreshed_at = 'epoch'`, older than any real answer,
/// so the next authentication refreshes it and any answer replaces it. The marker is
/// a finite time on purpose: `-infinity` cannot be decoded into `OffsetDateTime`, and
/// the table refuses it (`accounts_times_finite`).
pub(crate) async fn ensure_actor(
    connection: &mut PgConnection,
    actor: &Actor,
) -> Result<(), AppError> {
    sqlx::query(
        r"
        INSERT INTO commit.accounts (uuid, kind, public_id, display_name, refreshed_at)
        VALUES ($1, $2, $3, '', 'epoch'::timestamptz)
        ON CONFLICT (uuid) DO NOTHING
        ",
    )
    .bind(actor.uuid.as_str())
    .bind(actor.actor_type)
    .bind(actor.id.as_str())
    .execute(&mut *connection)
    .await?;
    let kind =
        sqlx::query_scalar::<_, ActorType>("SELECT kind FROM commit.accounts WHERE uuid = $1")
            .bind(actor.uuid.as_str())
            .fetch_one(&mut *connection)
            .await?;
    if kind != actor.actor_type {
        // An account's kind never changes; a contradiction means the provider data is wrong.
        return Err(AppError::BadGateway);
    }
    Ok(())
}

/// Stores what Silicon Accounts said about an account (lookups, `userinfo`).
///
/// The row takes the answer only when it is at least as new as what is stored
/// (`observed_at` against `refreshed_at`): a lookup answered from the cache before an
/// id change or a custodian transfer must not undo the webhook event that applied it.
/// Deleted accounts stay deleted; an account's kind never changes.
pub(crate) async fn remember<'e, E>(executor: E, account: &ResolvedAccount) -> Result<(), AppError>
where
    E: PgExecutor<'e>,
{
    let custodian = if account.actor.is_silicon() {
        account.custodian.as_ref().map(AccountUuid::as_str)
    } else {
        None
    };
    sqlx::query(
        r"
        INSERT INTO commit.accounts (uuid, kind, public_id, display_name, pfp_url, status, custodian_uuid, refreshed_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        ON CONFLICT (uuid) DO UPDATE
           SET public_id = EXCLUDED.public_id,
               display_name = CASE WHEN EXCLUDED.display_name = '' THEN commit.accounts.display_name
                                   ELSE EXCLUDED.display_name END,
               pfp_url = CASE WHEN EXCLUDED.pfp_url = '' THEN commit.accounts.pfp_url ELSE EXCLUDED.pfp_url END,
               status = EXCLUDED.status,
               custodian_uuid = EXCLUDED.custodian_uuid,
               refreshed_at = EXCLUDED.refreshed_at
         WHERE commit.accounts.kind = EXCLUDED.kind
           AND commit.accounts.status <> 'deleted'
           AND commit.accounts.refreshed_at <= EXCLUDED.refreshed_at
        ",
    )
    .bind(account.actor.uuid.as_str())
    .bind(account.actor.actor_type)
    .bind(account.actor.id.as_str())
    .bind(&account.display_name)
    .bind(&account.pfp_url)
    .bind(account.status.as_str())
    .bind(custodian)
    .bind(account.observed_at)
    .execute(executor)
    .await?;
    Ok(())
}

/// Records what only the account's own view (`userinfo`) carries: the email the Carbon
/// shared with Commit, and the account's version, which marks the row as read from that
/// view. An equal or newer version already stored (from `account.updated`) wins.
pub(crate) async fn remember_own_view<'e, E>(
    executor: E,
    uuid: &AccountUuid,
    email: Option<&str>,
    version: i64,
) -> Result<(), AppError>
where
    E: PgExecutor<'e>,
{
    sqlx::query(
        r"
        UPDATE commit.accounts SET email = $2, accounts_version = $3
         WHERE uuid = $1 AND status <> 'deleted' AND accounts_version <= $3
        ",
    )
    .bind(uuid.as_str())
    .bind(email)
    .bind(version)
    .execute(executor)
    .await?;
    Ok(())
}

/// Whether `from` may assign work to, or add to a project, the account `to`.
///
/// Carbons can be reached by anyone; a Silicon only by its custodian circle and
/// the accounts it (or its custodian) allowed.
pub(crate) async fn may_reach(
    connection: &mut PgConnection,
    from: &AccountUuid,
    to: &AccountUuid,
) -> Result<bool, AppError> {
    Ok(
        sqlx::query_scalar::<_, bool>("SELECT commit.may_reach($1, $2)")
            .bind(from.as_str())
            .bind(to.as_str())
            .fetch_one(connection)
            .await?,
    )
}

/// The accounts a Silicon allowed to reach it, oldest first.
pub(crate) async fn allowlist<'e, E>(
    executor: E,
    silicon: &AccountUuid,
) -> Result<Vec<AllowedAccount>, AppError>
where
    E: PgExecutor<'e>,
{
    sqlx::query_as::<_, AllowedRow>(
        r"
        SELECT allowed.allowed_account, account.kind, account.public_id,
               adder.kind AS added_by_kind, adder.public_id AS added_by_id, allowed.added_by_account,
               allowed.created_at
          FROM commit.silicon_allowed_accounts AS allowed
          JOIN commit.accounts AS account ON account.uuid = allowed.allowed_account
          JOIN commit.accounts AS adder ON adder.uuid = allowed.added_by_account
         WHERE allowed.silicon_account = $1
         ORDER BY allowed.created_at, allowed.allowed_account
        ",
    )
    .bind(silicon.as_str())
    .fetch_all(executor)
    .await?
    .into_iter()
    .map(AllowedRow::into_domain)
    .collect()
}

/// One allow-list entry.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct AllowedAccount {
    /// The allowed account.
    pub account: ActorRef,
    /// Who allowed it: the Silicon itself or its custodian.
    pub added_by: ActorRef,
    /// When.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

#[derive(FromRow)]
struct AllowedRow {
    allowed_account: String,
    kind: ActorType,
    public_id: String,
    added_by_kind: ActorType,
    added_by_id: String,
    added_by_account: String,
    created_at: OffsetDateTime,
}

impl AllowedRow {
    fn into_domain(self) -> Result<AllowedAccount, AppError> {
        Ok(AllowedAccount {
            account: actor_ref(self.kind, self.public_id, self.allowed_account)?,
            added_by: actor_ref(self.added_by_kind, self.added_by_id, self.added_by_account)?,
            created_at: self.created_at,
        })
    }
}

/// Adds an allow-list entry; returns false when it already existed.
pub(crate) async fn allow(
    connection: &mut PgConnection,
    silicon: &AccountUuid,
    allowed: &AccountUuid,
    added_by: &AccountUuid,
) -> Result<bool, AppError> {
    let inserted = sqlx::query(
        r"
        INSERT INTO commit.silicon_allowed_accounts (silicon_account, allowed_account, added_by_account)
        VALUES ($1, $2, $3)
        ON CONFLICT DO NOTHING
        ",
    )
    .bind(silicon.as_str())
    .bind(allowed.as_str())
    .bind(added_by.as_str())
    .execute(connection)
    .await?;
    Ok(inserted.rows_affected() == 1)
}

/// Removes an allow-list entry; returns false when there was none.
pub(crate) async fn disallow(
    connection: &mut PgConnection,
    silicon: &AccountUuid,
    allowed: &AccountUuid,
) -> Result<bool, AppError> {
    let removed = sqlx::query(
        "DELETE FROM commit.silicon_allowed_accounts WHERE silicon_account = $1 AND allowed_account = $2",
    )
    .bind(silicon.as_str())
    .bind(allowed.as_str())
    .execute(connection)
    .await?;
    Ok(removed.rows_affected() == 1)
}

/// Converts a lookup's status string, treating unknown future values as not active.
pub(crate) fn status_or_inactive(value: &str) -> AccountStatus {
    AccountStatus::parse(value).unwrap_or(AccountStatus::Deleted)
}

/// Dedupe record for an Accounts webhook delivery. Returns false for a duplicate.
pub(crate) async fn record_webhook_event(
    connection: &mut PgConnection,
    event_id: &str,
    event_type: &str,
    account: Option<&str>,
    occurred_at: Option<OffsetDateTime>,
    payload_sha256: &str,
    outcome: &str,
) -> Result<bool, AppError> {
    let inserted = sqlx::query(
        r"
        INSERT INTO commit.accounts_webhook_events (event_id, event_type, account_uuid, occurred_at, payload_sha256, outcome)
        VALUES ($1, $2, $3, $4, $5, $6)
        ON CONFLICT (event_id) DO NOTHING
        ",
    )
    .bind(event_id)
    .bind(event_type)
    .bind(account)
    .bind(occurred_at)
    .bind(payload_sha256)
    .bind(outcome)
    .execute(connection)
    .await?;
    Ok(inserted.rows_affected() == 1)
}

/// The body hash stored for an already-processed event id, if any.
pub(crate) async fn webhook_event_hash(
    connection: &mut PgConnection,
    event_id: &str,
) -> Result<Option<String>, AppError> {
    Ok(sqlx::query_scalar::<_, String>(
        "SELECT payload_sha256 FROM commit.accounts_webhook_events WHERE event_id = $1",
    )
    .bind(event_id)
    .fetch_optional(connection)
    .await?)
}

/// `account.id_changed`: the account's id is now `new_id` (unless fresher data already arrived).
pub(crate) async fn apply_id_change(
    connection: &mut PgConnection,
    uuid: &str,
    new_id: &str,
    occurred_at: OffsetDateTime,
) -> Result<bool, AppError> {
    let updated = sqlx::query(
        r"
        UPDATE commit.accounts SET public_id = $2, refreshed_at = $3
         WHERE uuid = $1 AND status <> 'deleted' AND refreshed_at < $3
        ",
    )
    .bind(uuid)
    .bind(new_id)
    .bind(occurred_at)
    .execute(connection)
    .await?;
    Ok(updated.rows_affected() == 1)
}

/// `account.updated`: the fields Commit may see, unless an equal or newer version is stored.
pub(crate) async fn apply_profile_update(
    connection: &mut PgConnection,
    uuid: &str,
    account: &Value,
    version: i64,
    occurred_at: OffsetDateTime,
) -> Result<bool, AppError> {
    let updated = sqlx::query(
        r"
        UPDATE commit.accounts
           SET public_id = CASE WHEN refreshed_at <= $4 THEN coalesce($2->>'id', public_id) ELSE public_id END,
               display_name = coalesce($2->>'display_name', display_name),
               pfp_url = coalesce($2->>'pfp_url', pfp_url),
               email = CASE WHEN $2 ? 'email' THEN nullif($2->>'email', '') ELSE email END,
               custodian_uuid = CASE WHEN refreshed_at <= $4 AND kind = 'silicon' AND jsonb_typeof($2->'custodian') = 'object'
                                     THEN coalesce($2->'custodian'->>'uuid', custodian_uuid)
                                     ELSE custodian_uuid END,
               accounts_version = $3,
               refreshed_at = greatest(refreshed_at, $4)
         WHERE uuid = $1 AND status <> 'deleted' AND accounts_version < $3
        ",
    )
    .bind(uuid)
    .bind(account)
    .bind(version)
    .bind(occurred_at)
    .execute(connection)
    .await?;
    Ok(updated.rows_affected() == 1)
}

/// `silicon.custodian_changed`: the Silicon's custodian is now `custodian`.
pub(crate) async fn apply_custodian_change(
    connection: &mut PgConnection,
    silicon: &str,
    custodian: &str,
    custodian_id: Option<&str>,
    occurred_at: OffsetDateTime,
) -> Result<bool, AppError> {
    if let Some(custodian_id) = custodian_id.filter(|id| !id.is_empty()) {
        // The new custodian may never have used Commit; remember it so circles resolve.
        // 'epoch' marks the row as never refreshed (see `ensure_actor`).
        sqlx::query(
            r"
            INSERT INTO commit.accounts (uuid, kind, public_id, refreshed_at)
            VALUES ($1, 'carbon', $2, 'epoch'::timestamptz)
            ON CONFLICT (uuid) DO NOTHING
            ",
        )
        .bind(custodian)
        .bind(custodian_id)
        .execute(&mut *connection)
        .await?;
    }
    let updated = sqlx::query(
        r"
        UPDATE commit.accounts SET custodian_uuid = $2, refreshed_at = greatest(refreshed_at, $3)
         WHERE uuid = $1 AND kind = 'silicon' AND status <> 'deleted' AND refreshed_at < $3
        ",
    )
    .bind(silicon)
    .bind(custodian)
    .bind(occurred_at)
    .execute(connection)
    .await?;
    Ok(updated.rows_affected() == 1)
}

/// Refuses every token of the account issued before `at` (sign-out, removed access).
pub(crate) async fn revoke_before(
    connection: &mut PgConnection,
    uuid: &str,
    at: OffsetDateTime,
) -> Result<bool, AppError> {
    let updated = sqlx::query(
        r"
        UPDATE commit.accounts
           SET revoked_before = greatest(coalesce(revoked_before, $2), $2)
         WHERE uuid = $1
        ",
    )
    .bind(uuid)
    .bind(at)
    .execute(connection)
    .await?;
    Ok(updated.rows_affected() == 1)
}

/// `account.deleted`: see `commit.forget_account` for exactly what is removed and what is kept.
pub(crate) async fn forget(
    connection: &mut PgConnection,
    uuid: &str,
    at: OffsetDateTime,
    request_id: &str,
    tombstone_seconds: i64,
    audit_seconds: i64,
) -> Result<Value, AppError> {
    Ok(
        sqlx::query_scalar::<_, Value>("SELECT commit.forget_account($1, $2, $3, $4, $5)")
            .bind(uuid)
            .bind(at)
            .bind(request_id)
            .bind(tombstone_seconds)
            .bind(audit_seconds)
            .fetch_one(connection)
            .await?,
    )
}
