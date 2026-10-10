//! `commit-migrate link-identities`: re-points IAM-era rows to Silicon Accounts.
//!
//! Migration 0033 gave every IAM-era principal a placeholder account
//! (`iam:<organization>:<principal>`) and pointed all its rows at it. This
//! operator command reads a mapping file (`iam_principal_id,accounts_uuid`) and,
//! in one transaction:
//!
//! 1. records the mapping in `commit_private.identity_links`;
//! 2. moves every row of a re-mapped principal from its previous account
//!    (placeholder or earlier link) to the new one. Rows are matched through
//!    their retained IAM columns, and only while they still point at the
//!    previous account, so work changed after the cutover is never clobbered;
//! 3. where an account may own only one row (email preference, Silicon
//!    notification settings, an active membership, an idempotency key…), the
//!    newest legacy row wins and the others stay on their placeholders;
//! 4. reports unmatched mapping rows, principals left unlinked, rows kept on
//!    placeholders, and formerly organization-wide projects whose former
//!    readers are outside the new owner's circle.
//!
//! The command is idempotent: running it again with the same file changes
//! nothing; running it with another file (before the cutover) re-points again.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
};

use anyhow::{Context as _, bail};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use sqlx::{FromRow, PgConnection, PgPool};
use uuid::Uuid;

use crate::domain::ActorType;

/// One line of the mapping file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MappingRow {
    /// 1-based line number in the file.
    pub line: usize,
    /// IAM principal UUID, or the IAM-era `c:`/`si:` public id.
    pub iam_principal: String,
    /// Silicon Accounts uuid to link to; `None` unlinks (rows go back to the placeholder).
    pub accounts_uuid: Option<String>,
    /// Optional IAM organization handle (`org_id`) that restricts the match.
    pub org_id: Option<String>,
}

/// Account details used when a mapped account is new to Commit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KnownAccount {
    /// Carbon or Silicon.
    pub kind: ActorType,
    /// Current public id.
    pub public_id: String,
    /// A Silicon's custodian uuid.
    pub custodian: Option<String>,
}

/// Parses a mapping file: CSV with a header naming `iam_principal_id` and
/// `accounts_uuid` (and optionally `org_id`), in any column order.
///
/// # Errors
///
/// Returns a precise error naming the line for any malformed line.
pub fn parse_mapping(text: &str) -> anyhow::Result<Vec<MappingRow>> {
    let mut lines = text
        .lines()
        .enumerate()
        .map(|(index, line)| (index + 1, line.trim()))
        .filter(|(_, line)| !line.is_empty() && !line.starts_with('#'));
    let Some((_, header)) = lines.next() else {
        bail!("the mapping file is empty; it needs a header line `iam_principal_id,accounts_uuid`");
    };
    let columns = header.split(',').map(str::trim).collect::<Vec<_>>();
    let position = |name: &str| columns.iter().position(|column| *column == name);
    let (Some(principal_column), Some(uuid_column)) =
        (position("iam_principal_id"), position("accounts_uuid"))
    else {
        bail!(
            "the header `{header}` must name the columns iam_principal_id and accounts_uuid (org_id is optional)"
        );
    };
    let org_column = position("org_id");
    let mut rows = Vec::new();
    for (line, text) in lines {
        let fields = text.split(',').map(str::trim).collect::<Vec<_>>();
        if fields.len() != columns.len() {
            bail!(
                "line {line}: expected {} comma-separated values like the header, found {}",
                columns.len(),
                fields.len()
            );
        }
        let iam_principal = fields[principal_column].to_owned();
        if iam_principal.is_empty() {
            bail!("line {line}: iam_principal_id is empty");
        }
        let accounts_uuid = Some(fields[uuid_column].to_owned()).filter(|value| !value.is_empty());
        if let Some(uuid) = &accounts_uuid
            && (uuid.starts_with("iam:")
                || uuid.len() > crate::domain::MAX_ACCOUNT_UUID_BYTES
                || uuid
                    .chars()
                    .any(|c| c.is_whitespace() || c.is_control() || c == ':'))
        {
            bail!(
                "line {line}: `{uuid}` is not a Silicon Accounts uuid (a short case-sensitive value such as zQo; c:/si: ids are not uuids)"
            );
        }
        let org_id = org_column
            .map(|column| fields[column].to_owned())
            .filter(|value| !value.is_empty());
        rows.push(MappingRow {
            line,
            iam_principal,
            accounts_uuid,
            org_id,
        });
    }
    Ok(rows)
}

/// What one run did (or, in a dry run, would do).
#[derive(Clone, Debug, Default, Serialize)]
pub struct LinkReport {
    /// SHA-256 of the mapping file, recorded as the link source.
    pub mapping_sha256: String,
    /// True when nothing was committed.
    pub dry_run: bool,
    /// IAM principals whose link changed.
    pub links_changed: usize,
    /// Rows moved, per `table.column`.
    pub rows_repointed: BTreeMap<String, u64>,
    /// Rows left on their placeholder because the account already owns such a row.
    pub kept_on_placeholder: BTreeMap<String, u64>,
    /// Mapping lines that matched no IAM principal.
    pub unmatched_mapping_lines: Vec<Value>,
    /// IAM principals that stay unlinked, with how many rows they still own.
    pub unlinked_principals: Vec<Value>,
    /// Formerly organization-wide projects and the linked accounts that read them
    /// before but are outside the new owner's circle now (share them explicitly if needed).
    pub public_projects_needing_shares: Vec<Value>,
    /// Accounts created in Commit for mapped uuids it had not seen.
    pub accounts_created: Vec<String>,
}

/// An account column that follows an IAM-era principal column.
struct Column {
    table: &'static str,
    account: &'static str,
    principal: &'static str,
    unique: Uniqueness,
}

/// How an account column is constrained.
enum Uniqueness {
    /// Any number of rows per account.
    None,
    /// Unique per account (and these columns), optionally only among rows matching a filter.
    Per {
        keys: &'static [&'static str],
        filter: Option<&'static str>,
        newest_first: &'static str,
    },
}

const COLUMNS: &[Column] = &[
    Column {
        table: "todos",
        account: "assigned_by_account",
        principal: "assigned_by_principal_id",
        unique: Uniqueness::None,
    },
    Column {
        table: "todos",
        account: "assigned_to_account",
        principal: "assigned_to_principal_id",
        unique: Uniqueness::None,
    },
    Column {
        table: "todos",
        account: "deleted_by_account",
        principal: "deleted_by_principal_id",
        unique: Uniqueness::None,
    },
    Column {
        table: "todo_notes",
        account: "author_account",
        principal: "author_principal_id",
        unique: Uniqueness::None,
    },
    Column {
        table: "todo_activity",
        account: "actor_account",
        principal: "actor_principal_id",
        unique: Uniqueness::None,
    },
    Column {
        table: "audit_events",
        account: "actor_account",
        principal: "actor_principal_id",
        unique: Uniqueness::None,
    },
    Column {
        table: "outbox_events",
        account: "recipient_silicon_account",
        principal: "recipient_silicon_principal_id",
        unique: Uniqueness::None,
    },
    Column {
        table: "projects",
        account: "created_by_account",
        principal: "created_by_principal_id",
        unique: Uniqueness::None,
    },
    Column {
        table: "projects",
        account: "owner_account",
        principal: "created_by_principal_id",
        unique: Uniqueness::None,
    },
    Column {
        table: "project_participants",
        account: "added_by_account",
        principal: "added_by_principal_id",
        unique: Uniqueness::None,
    },
    Column {
        table: "project_participants",
        account: "removed_by_account",
        principal: "removed_by_principal_id",
        unique: Uniqueness::None,
    },
    Column {
        table: "project_participants",
        account: "participant_account",
        principal: "silicon_principal_id",
        unique: Uniqueness::Per {
            keys: &["project_id"],
            filter: Some("removed_at IS NULL"),
            newest_first: "added_at ASC",
        },
    },
    Column {
        table: "project_diaries",
        account: "updated_by_account",
        principal: "updated_by_principal_id",
        unique: Uniqueness::None,
    },
    Column {
        table: "project_tasks",
        account: "created_by_account",
        principal: "created_by_principal_id",
        unique: Uniqueness::None,
    },
    Column {
        table: "project_tasks",
        account: "assigned_to_account",
        principal: "assigned_to_principal_id",
        unique: Uniqueness::None,
    },
    Column {
        table: "project_entries",
        account: "created_by_account",
        principal: "created_by_principal_id",
        unique: Uniqueness::None,
    },
    Column {
        table: "project_collaborators",
        account: "account",
        principal: "principal_id",
        unique: Uniqueness::Per {
            keys: &["project_id"],
            filter: None,
            newest_first: "first_contributed_at ASC",
        },
    },
    Column {
        table: "idempotency_records",
        account: "actor_account",
        principal: "actor_principal_id",
        unique: Uniqueness::Per {
            keys: &["operation", "resource_path", "idempotency_key"],
            filter: None,
            newest_first: "created_at DESC",
        },
    },
    Column {
        table: "silicon_notification_settings",
        account: "silicon_account",
        principal: "silicon_principal_id",
        unique: Uniqueness::Per {
            keys: &[],
            filter: None,
            newest_first: "updated_at DESC",
        },
    },
    Column {
        table: "email_preferences",
        account: "account",
        principal: "principal_id",
        unique: Uniqueness::Per {
            keys: &[],
            filter: None,
            newest_first: "updated_at DESC",
        },
    },
    Column {
        table: "email_jobs",
        account: "account",
        principal: "principal_id",
        unique: Uniqueness::Per {
            keys: &["report_key"],
            filter: Some("report_key IS NOT NULL"),
            newest_first: "created_at DESC",
        },
    },
];

/// Tables whose user triggers (immutability, versioning, mirrors) must not fire while rows move.
const TABLES: &[&str] = &[
    "todos",
    "todo_notes",
    "todo_activity",
    "audit_events",
    "outbox_events",
    "projects",
    "project_participants",
    "project_diaries",
    "project_tasks",
    "project_entries",
    "project_collaborators",
    "idempotency_records",
    "silicon_notification_settings",
    "todo_notification_subscriptions",
    "email_preferences",
    "email_jobs",
];

#[derive(FromRow)]
struct LinkRow {
    organization_id: Uuid,
    iam_principal_id: Uuid,
    environment_id: Option<Uuid>,
    /// `commit.actor_type` as text: the migrator's connection does not search the commit schema.
    actor_type: String,
    iam_public_id: String,
    org_id: String,
    placeholder_uuid: String,
    accounts_uuid: Option<String>,
}

fn kind(value: &str) -> anyhow::Result<ActorType> {
    value
        .parse()
        .map_err(|_| anyhow::anyhow!("unexpected account kind `{value}` in the database"))
}

/// SHA-256 of the mapping file, hex encoded.
#[must_use]
pub fn mapping_digest(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

/// Applies a mapping in one transaction; with `dry_run`, reports and rolls back.
///
/// `accounts` carries current details of mapped uuids when Silicon Accounts
/// was reachable; without it, new accounts start from the IAM-era id and are
/// refreshed from Accounts the first time they are seen by the API.
///
/// # Errors
///
/// Returns a precise error (and changes nothing) for a principal mapped to two
/// accounts, a Carbon mapped to a Silicon account (or the reverse), or a
/// database failure.
pub async fn apply(
    pool: &PgPool,
    rows: &[MappingRow],
    mapping_sha256: &str,
    accounts: &BTreeMap<String, KnownAccount>,
    dry_run: bool,
) -> anyhow::Result<LinkReport> {
    let mut report = LinkReport {
        mapping_sha256: mapping_sha256.to_owned(),
        dry_run,
        ..LinkReport::default()
    };
    let mut transaction = pool.begin().await?;
    sqlx::query("SET CONSTRAINTS ALL DEFERRED")
        .execute(&mut *transaction)
        .await?;

    let links = sqlx::query_as::<_, LinkRow>(
        "SELECT organization_id, iam_principal_id, environment_id, actor_type::text AS actor_type, iam_public_id, org_id, placeholder_uuid, accounts_uuid FROM commit_private.identity_links",
    )
    .fetch_all(&mut *transaction)
    .await?;

    let mut desired: BTreeMap<(Uuid, Uuid), (Option<String>, usize)> = BTreeMap::new();
    for row in rows {
        let as_uuid = Uuid::parse_str(&row.iam_principal).ok();
        let matches = links
            .iter()
            .filter(|link| match as_uuid {
                Some(principal) => link.iam_principal_id == principal,
                // A c:/si: id names production principals only. Sandboxes of former testing
                // environments reused production ids; link those by principal UUID if ever needed.
                None => {
                    link.environment_id.is_none()
                        && link.iam_public_id.eq_ignore_ascii_case(&row.iam_principal)
                }
            })
            .filter(|link| row.org_id.as_ref().is_none_or(|org| &link.org_id == org))
            .collect::<Vec<_>>();
        if matches.is_empty() {
            report.unmatched_mapping_lines.push(json!({
                "line": row.line,
                "iam_principal_id": row.iam_principal,
                "org_id": row.org_id,
                "reason": "no IAM-era principal has this principal UUID, or no production principal has this c:/si: id",
            }));
            continue;
        }
        for link in matches {
            let key = (link.organization_id, link.iam_principal_id);
            if let Some((previous, line)) = desired.get(&key)
                && previous != &row.accounts_uuid
            {
                bail!(
                    "lines {line} and {} map the IAM principal {} ({} in {}) to different accounts; keep one",
                    row.line,
                    link.iam_principal_id,
                    link.iam_public_id,
                    link.org_id
                );
            }
            desired.insert(key, (row.accounts_uuid.clone(), row.line));
        }
    }

    let mut changes = Vec::new();
    for link in &links {
        let Some((target, line)) = desired.get(&(link.organization_id, link.iam_principal_id))
        else {
            continue;
        };
        if let Some(target) = target {
            ensure_account(&mut transaction, link, target, *line, accounts, &mut report).await?;
        }
        let old = link
            .accounts_uuid
            .clone()
            .unwrap_or_else(|| link.placeholder_uuid.clone());
        let new = target
            .clone()
            .unwrap_or_else(|| link.placeholder_uuid.clone());
        if old != new {
            changes.push((link, old, new, target.clone()));
        }
    }
    report.links_changed = changes.len();

    sqlx::query(
        "CREATE TEMP TABLE link_changes (organization_id uuid, iam_principal_id uuid, old_account text, placeholder text, new_account text) ON COMMIT DROP",
    )
    .execute(&mut *transaction)
    .await?;
    for (link, old, new, target) in &changes {
        sqlx::query("INSERT INTO link_changes VALUES ($1, $2, $3, $4, $5)")
            .bind(link.organization_id)
            .bind(link.iam_principal_id)
            .bind(old)
            .bind(&link.placeholder_uuid)
            .bind(new)
            .execute(&mut *transaction)
            .await?;
        sqlx::query(
            r"
            UPDATE commit_private.identity_links
               SET accounts_uuid = $3,
                   linked_at = CASE WHEN $3::text IS NULL THEN NULL ELSE clock_timestamp() END,
                   source = $4
             WHERE organization_id = $1 AND iam_principal_id = $2
            ",
        )
        .bind(link.organization_id)
        .bind(link.iam_principal_id)
        .bind(target)
        .bind(format!("link-identities sha256:{mapping_sha256}"))
        .execute(&mut *transaction)
        .await?;
    }

    for table in TABLES {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "ALTER TABLE commit.{table} DISABLE TRIGGER USER"
        )))
        .execute(&mut *transaction)
        .await?;
    }
    repoint(&mut transaction, &mut report).await?;
    // Check the deferred foreign keys now: triggers can only be re-enabled once no
    // constraint events are pending.
    sqlx::query("SET CONSTRAINTS ALL IMMEDIATE")
        .execute(&mut *transaction)
        .await
        .context("the re-pointed rows do not satisfy Commit's foreign keys")?;
    for table in TABLES {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "ALTER TABLE commit.{table} ENABLE TRIGGER USER"
        )))
        .execute(&mut *transaction)
        .await?;
    }
    // The invariant triggers were off while rows moved; re-check the one a link can break.
    let ownerless = sqlx::query_scalar::<_, String>(
        r"
        SELECT project.uid FROM commit.projects AS project
         WHERE project.deleted_at IS NULL
           AND NOT EXISTS (
               SELECT 1 FROM commit.project_participants AS member
                WHERE member.project_id = project.id
                  AND member.participant_account = project.owner_account
                  AND member.removed_at IS NULL)
         ORDER BY project.uid
        ",
    )
    .fetch_all(&mut *transaction)
    .await?;
    if !ownerless.is_empty() {
        bail!(
            "this mapping would leave the owner of {} outside the project's members (two IAM principals of one project mapped to the same account?); nothing was changed",
            ownerless.join(", ")
        );
    }

    report.unlinked_principals = unlinked(&mut transaction).await?;
    report.public_projects_needing_shares = public_project_gaps(&mut transaction).await?;
    if dry_run {
        transaction.rollback().await?;
    } else {
        transaction.commit().await?;
    }
    Ok(report)
}

async fn ensure_account(
    connection: &mut PgConnection,
    link: &LinkRow,
    target: &str,
    line: usize,
    accounts: &BTreeMap<String, KnownAccount>,
    report: &mut LinkReport,
) -> anyhow::Result<()> {
    let existing =
        sqlx::query_scalar::<_, String>("SELECT kind::text FROM commit.accounts WHERE uuid = $1")
            .bind(target)
            .fetch_optional(&mut *connection)
            .await?
            .map(|value| kind(&value))
            .transpose()?;
    let known = accounts.get(target);
    let link_kind = kind(&link.actor_type)?;
    let kind = existing
        .or_else(|| known.map(|account| account.kind))
        .unwrap_or(link_kind);
    if kind != link_kind {
        bail!(
            "line {line}: {} ({}) was a {} in IAM, but the Silicon Accounts account {target} is a {}; a Carbon links to a Carbon and a Silicon to a Silicon",
            link.iam_public_id,
            link.org_id,
            link.actor_type,
            kind
        );
    }
    if existing.is_none()
        && !report
            .accounts_created
            .iter()
            .any(|created| created == target)
    {
        let public_id = known.map_or_else(
            || link.iam_public_id.clone(),
            |account| account.public_id.clone(),
        );
        let custodian = known
            .and_then(|account| account.custodian.clone())
            .filter(|_| kind == ActorType::Silicon);
        sqlx::query(
            r"
            INSERT INTO commit.accounts (uuid, kind, public_id, custodian_uuid, refreshed_at)
            VALUES ($1, $2::text::commit.actor_type, $3, $4,
                    CASE WHEN $5 THEN clock_timestamp() ELSE '-infinity'::timestamptz END)
            ",
        )
        .bind(target)
        .bind(kind.as_str())
        .bind(public_id)
        .bind(custodian)
        .bind(known.is_some())
        .execute(&mut *connection)
        .await?;
        report.accounts_created.push(target.to_owned());
    }
    Ok(())
}

fn qualified(alias: &str, expression: &str) -> String {
    format!("{alias}.{expression}")
}

async fn repoint(connection: &mut PgConnection, report: &mut LinkReport) -> anyhow::Result<()> {
    // Phase 1: rows that moved with an earlier link go back to their placeholder.
    for column in COLUMNS {
        let moved = sqlx::query(sqlx::AssertSqlSafe(format!(
            r"
            UPDATE commit.{table} AS t
               SET {account} = c.placeholder
              FROM link_changes AS c
             WHERE t.organization_id = c.organization_id
               AND t.{principal} = c.iam_principal_id
               AND t.{account} = c.old_account
               AND c.old_account <> c.placeholder
            ",
            table = column.table,
            account = column.account,
            principal = column.principal,
        )))
        .execute(&mut *connection)
        .await?
        .rows_affected();
        *report
            .rows_repointed
            .entry(format!("{}.{}", column.table, column.account))
            .or_default() += moved;
    }
    // Phase 2: rows on a placeholder move to the newly linked account.
    for column in COLUMNS {
        let key = format!("{}.{}", column.table, column.account);
        let (moved, kept) = match &column.unique {
            Uniqueness::None => (link_plain(connection, column, None).await?, 0),
            Uniqueness::Per {
                keys,
                filter,
                newest_first,
            } => {
                let unconstrained = match filter {
                    Some(filter) => {
                        link_plain(
                            connection,
                            column,
                            Some(&format!("NOT ({})", qualified("t", filter))),
                        )
                        .await?
                    }
                    None => 0,
                };
                let (moved, kept) =
                    link_ranked(connection, column, keys, *filter, newest_first).await?;
                (moved + unconstrained, kept)
            }
        };
        *report.rows_repointed.entry(key.clone()).or_default() += moved;
        if kept > 0 {
            *report.kept_on_placeholder.entry(key).or_default() += kept;
        }
    }
    // A per-todo subscription always belongs to the todo's owner.
    let followed = sqlx::query(
        r"
        UPDATE commit.todo_notification_subscriptions AS s
           SET silicon_account = t.assigned_by_account
          FROM commit.todos AS t
         WHERE s.todo_id = t.id AND s.silicon_account <> t.assigned_by_account
        ",
    )
    .execute(&mut *connection)
    .await?
    .rows_affected();
    *report
        .rows_repointed
        .entry("todo_notification_subscriptions.silicon_account".to_owned())
        .or_default() += followed;
    report.rows_repointed.retain(|_, count| *count > 0);
    Ok(())
}

async fn link_plain(
    connection: &mut PgConnection,
    column: &Column,
    extra: Option<&str>,
) -> anyhow::Result<u64> {
    Ok(sqlx::query(sqlx::AssertSqlSafe(format!(
        r"
        UPDATE commit.{table} AS t
           SET {account} = c.new_account
          FROM link_changes AS c
         WHERE t.organization_id = c.organization_id
           AND t.{principal} = c.iam_principal_id
           AND t.{account} = c.placeholder
           AND c.new_account <> c.placeholder
           {extra}
        ",
        table = column.table,
        account = column.account,
        principal = column.principal,
        extra = extra
            .map(|condition| format!("AND {condition}"))
            .unwrap_or_default(),
    )))
    .execute(connection)
    .await?
    .rows_affected())
}

async fn link_ranked(
    connection: &mut PgConnection,
    column: &Column,
    keys: &[&str],
    filter: Option<&str>,
    order: &str,
) -> anyhow::Result<(u64, u64)> {
    let partition = keys.iter().fold(String::new(), |mut text, key| {
        let _ = write!(text, ", t.{key}");
        text
    });
    let same_keys = keys.iter().fold(String::new(), |mut text, key| {
        let _ = write!(text, " AND other.{key} = t.{key}");
        text
    });
    let candidate_filter = filter
        .map(|filter| format!(" AND {}", qualified("t", filter)))
        .unwrap_or_default();
    let other_filter = filter
        .map(|filter| format!(" AND {}", qualified("other", filter)))
        .unwrap_or_default();
    let candidates = format!(
        r"
        SELECT t.ctid AS row_ref, c.new_account AS target,
               row_number() OVER (PARTITION BY c.new_account{partition} ORDER BY {order_by}) AS rank
          FROM commit.{table} AS t
          JOIN link_changes AS c
            ON t.organization_id = c.organization_id AND t.{principal} = c.iam_principal_id
         WHERE t.{account} = c.placeholder AND c.new_account <> c.placeholder{candidate_filter}
        ",
        table = column.table,
        account = column.account,
        principal = column.principal,
        order_by = qualified("t", order),
    );
    let total = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(format!(
        "SELECT count(*) FROM ({candidates}) AS candidates"
    )))
    .fetch_one(&mut *connection)
    .await?;
    let moved = sqlx::query(sqlx::AssertSqlSafe(format!(
        r"
        WITH candidates AS ({candidates})
        UPDATE commit.{table} AS t
           SET {account} = candidates.target
          FROM candidates
         WHERE t.ctid = candidates.row_ref
           AND candidates.rank = 1
           AND NOT EXISTS (
               SELECT 1 FROM commit.{table} AS other
                WHERE other.{account} = candidates.target{same_keys}{other_filter}
           )
        ",
        table = column.table,
        account = column.account,
    )))
    .execute(&mut *connection)
    .await?
    .rows_affected();
    let total = u64::try_from(total).unwrap_or_default();
    Ok((moved, total.saturating_sub(moved)))
}

async fn unlinked(connection: &mut PgConnection) -> anyhow::Result<Vec<Value>> {
    let rows = sqlx::query_as::<_, (String, String, String, Uuid, String, i64, i64, i64)>(
        r"
        SELECT link.org_id, link.iam_public_id, link.actor_type::text, link.iam_principal_id, link.placeholder_uuid,
               (SELECT count(*) FROM commit.todos t
                 WHERE t.assigned_by_account = link.placeholder_uuid OR t.assigned_to_account = link.placeholder_uuid),
               (SELECT count(*) FROM commit.projects p WHERE p.owner_account = link.placeholder_uuid),
               (SELECT count(*) FROM commit.project_participants m
                 WHERE m.participant_account = link.placeholder_uuid AND m.removed_at IS NULL)
          FROM commit_private.identity_links AS link
         WHERE link.accounts_uuid IS NULL
         ORDER BY link.environment_id NULLS FIRST, link.org_id, link.iam_public_id
        ",
    )
    .fetch_all(connection)
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(org_id, public_id, kind, principal, placeholder, todos, projects, memberships)| {
                json!({
                    "org_id": org_id,
                    "iam_public_id": public_id,
                    "kind": kind,
                    "iam_principal_id": principal,
                    "placeholder": placeholder,
                    "todos": todos,
                    "projects_owned": projects,
                    "active_memberships": memberships,
                })
            },
        )
        .collect())
}

async fn public_project_gaps(connection: &mut PgConnection) -> anyhow::Result<Vec<Value>> {
    let rows = sqlx::query_as::<_, (Uuid, String, String, String, Vec<String>)>(
        r"
        SELECT project.id, project.uid, owner.uuid, owner.public_id,
               array_agg(DISTINCT reader.public_id ORDER BY reader.public_id)
          FROM commit.projects AS project
          JOIN commit.accounts AS owner ON owner.uuid = project.owner_account
          JOIN commit_private.identity_links AS link
            ON link.organization_id = project.organization_id AND link.accounts_uuid IS NOT NULL
          JOIN commit.accounts AS reader ON reader.uuid = link.accounts_uuid
         WHERE project.organization_id IS NOT NULL
           AND NOT project.private
           AND project.deleted_at IS NULL
           AND NOT commit.project_access(project.id, reader.uuid)
         GROUP BY project.id, project.uid, owner.uuid, owner.public_id
         ORDER BY project.uid
        ",
    )
    .fetch_all(connection)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, uid, owner_uuid, owner_id, readers)| {
            json!({
                "project_id": id,
                "uid": uid,
                "owner": { "uuid": owner_uuid, "id": owner_id },
                "former_readers_without_access": readers,
                "how_to_share": "Add them as members (silicon_ids/carbon_ids) with PATCH /api/v1/projects/{id}, or leave them out.",
            })
        })
        .collect())
}

/// The IAM-era public ids that need a mapping line, with suggested accounts
/// (`suggested_uuid`) when Silicon Accounts knows an account with that id today.
///
/// # Errors
///
/// Returns a database error.
pub async fn plan(
    pool: &PgPool,
) -> anyhow::Result<Vec<(Uuid, String, String, ActorType, Option<String>)>> {
    sqlx::query_as::<_, (Uuid, String, String, String, Option<String>)>(
        r"
        SELECT iam_principal_id, iam_public_id, org_id, actor_type::text, accounts_uuid
          FROM commit_private.identity_links
         WHERE environment_id IS NULL
         ORDER BY org_id, iam_public_id
        ",
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|(principal, id, org, actor_type, linked)| {
        Ok((principal, id, org, kind(&actor_type)?, linked))
    })
    .collect()
}

/// Distinct IAM-era public ids of production principals (for suggestion lookups).
#[must_use]
pub fn distinct_ids(
    plan: &[(Uuid, String, String, ActorType, Option<String>)],
) -> BTreeSet<String> {
    plan.iter().map(|(_, id, _, _, _)| id.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::parse_mapping;

    #[test]
    fn mapping_files_name_their_columns_and_lines() {
        let rows = parse_mapping(
            "# Commit cutover\niam_principal_id,accounts_uuid,org_id\n018f268d-715a-7b72-8f0f-41f16f9af553,zQo,tos\nc:saket,,\n",
        );
        let Ok(rows) = rows else {
            panic!("valid mapping");
        };
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].line, 3);
        assert_eq!(rows[0].accounts_uuid.as_deref(), Some("zQo"));
        assert_eq!(rows[0].org_id.as_deref(), Some("tos"));
        assert_eq!(rows[1].iam_principal, "c:saket");
        assert_eq!(rows[1].accounts_uuid, None);
    }

    #[test]
    fn mapping_files_refuse_ids_where_uuids_belong_and_bad_shapes() {
        assert!(parse_mapping("").is_err());
        assert!(parse_mapping("principal,uuid\nx,y\n").is_err());
        let wrong = parse_mapping("iam_principal_id,accounts_uuid\nc:saket,c:saket\n");
        assert!(wrong.is_err_and(|error| error.to_string().contains("line 2")));
        assert!(parse_mapping("iam_principal_id,accounts_uuid\nc:saket\n").is_err());
        assert!(parse_mapping("iam_principal_id,accounts_uuid\nc:saket,iam:x:y\n").is_err());
    }
}
