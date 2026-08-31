//! PostgreSQL persistence for todo use cases.

use std::time::Duration;

use anyhow::anyhow;
use serde_json::Value;
use sqlx::{AssertSqlSafe, FromRow, PgConnection, PgPool, Postgres, QueryBuilder};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{
    application::{
        idempotency::{MutationIdentity, MutationResponse},
        ports::{ActiveMember, VerifiedActor},
    },
    domain::{
        Actor, ActorId, ActorType, CollectionQuery, CreatedAtRange, LimitedText, OrganizationId,
        Page, PageCursor, PermanentAttachmentUrl, PrincipalId, PublicOrganizationId, RequiredText,
        Todo, TodoId, TodoNote, TodoNoteId, TodoPage, TodoQuery, TodoStatus, TodoView,
    },
    error::AppError,
};

const STORED_TITLE_CHARS: usize = 500;
const STORED_DESCRIPTION_CHARS: usize = 100_000;
const STORED_NOTE_CHARS: usize = 100_000;
const ADVISORY_LOCK_SEED: i64 = 7_621_913_449_043_511_527;

const TODO_PROJECTION: &str = r#"
    SELECT
        todo.id,
        todo.organization_id,
        organization.org_id,
        todo.title,
        todo.description,
        todo.assigned_by_principal_id,
        assigned_by.actor_type AS assigned_by_actor_type,
        assigned_by.actor_id AS assigned_by_actor_id,
        todo.assigned_to_principal_id,
        assigned_to.actor_type AS assigned_to_actor_type,
        assigned_to.actor_id AS assigned_to_actor_id,
        todo.status,
        ARRAY(
            SELECT attachment.permanent_url
            FROM commit.todo_attachments AS attachment
            WHERE attachment.organization_id = todo.organization_id
              AND attachment.todo_id = todo.id
            ORDER BY attachment.position
        ) AS attachments,
        todo.created_at,
        todo.updated_at
    FROM commit.todos AS todo
    INNER JOIN commit.organization_projection AS organization
        ON organization.organization_id = todo.organization_id
    INNER JOIN commit.actor_projection AS assigned_by
        ON assigned_by.organization_id = todo.organization_id
       AND assigned_by.principal_id = todo.assigned_by_principal_id
    INNER JOIN commit.actor_projection AS assigned_to
        ON assigned_to.organization_id = todo.organization_id
       AND assigned_to.principal_id = todo.assigned_to_principal_id
"#;

/// Complete values needed to insert a todo aggregate.
pub(crate) struct NewTodo<'a> {
    pub(crate) id: TodoId,
    pub(crate) organization_id: OrganizationId,
    pub(crate) title: &'a RequiredText,
    pub(crate) description: Option<&'a LimitedText>,
    pub(crate) assigned_by_principal_id: PrincipalId,
    pub(crate) assigned_to_principal_id: PrincipalId,
    pub(crate) status: TodoStatus,
    pub(crate) attachments: &'a [PermanentAttachmentUrl],
}

/// Full desired todo state after an authorized patch.
pub(crate) struct TodoReplacement<'a> {
    pub(crate) title: &'a RequiredText,
    pub(crate) description: Option<&'a LimitedText>,
    pub(crate) assigned_to_principal_id: PrincipalId,
    pub(crate) status: TodoStatus,
    pub(crate) attachments: &'a [PermanentAttachmentUrl],
    pub(crate) replace_attachments: bool,
}

/// Durable response read under an idempotency advisory lock.
pub(crate) struct StoredMutation {
    pub(crate) request_fingerprint: Vec<u8>,
    pub(crate) response: MutationResponse,
}

/// State of a tenant-qualified todo selected for DELETE.
pub(crate) enum DeleteTarget {
    Missing,
    AlreadyDeleted(PrincipalId),
    Active(Box<Todo>),
}

/// Internal todo activity categories supported by the schema.
#[derive(Clone, Copy)]
pub(crate) enum TodoActivityKind {
    Created,
    Updated,
    StatusChanged,
    Reassigned,
    NoteAdded,
    Deleted,
}

impl TodoActivityKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Updated => "updated",
            Self::StatusChanged => "status_changed",
            Self::Reassigned => "reassigned",
            Self::NoteAdded => "note_added",
            Self::Deleted => "deleted",
        }
    }
}

/// Lists todos with stable descending keyset pagination.
pub(crate) async fn list_todos(
    pool: &PgPool,
    actor: &VerifiedActor,
    query: &TodoQuery,
    created_at: CreatedAtRange,
) -> Result<TodoPage, AppError> {
    let mut builder = QueryBuilder::<Postgres>::new(TODO_PROJECTION);
    builder
        .push(" WHERE todo.organization_id = ")
        .push_bind(actor.organization_id.into_uuid())
        .push(" AND todo.deleted_at IS NULL");

    match query.view {
        TodoView::AssignedToMe => {
            builder
                .push(" AND todo.assigned_to_principal_id = ")
                .push_bind(actor.actor.principal_id.into_uuid());
        }
        TodoView::DelegatedByMe => {
            builder
                .push(" AND todo.assigned_by_principal_id = ")
                .push_bind(actor.actor.principal_id.into_uuid())
                .push(" AND todo.assigned_to_principal_id <> todo.assigned_by_principal_id");
        }
        TodoView::All => {}
    }

    if let Some(status) = query.status {
        builder.push(" AND todo.status = ").push_bind(status);
    }
    if let Some(assigned_to) = &query.assigned_to {
        builder
            .push(" AND assigned_to.actor_id = ")
            .push_bind(assigned_to.as_str());
    }
    if let Some(assigned_by) = &query.assigned_by {
        builder
            .push(" AND assigned_by.actor_id = ")
            .push_bind(assigned_by.as_str());
    }
    if let Some(from) = created_at.from {
        builder.push(" AND todo.created_at >= ").push_bind(from);
    }
    if let Some(to) = created_at.to {
        builder.push(" AND todo.created_at <= ").push_bind(to);
    }
    if let Some(cursor) = query.cursor {
        builder
            .push(" AND (todo.created_at, todo.id) < (")
            .push_bind(cursor.created_at())
            .push(", ")
            .push_bind(cursor.id())
            .push(")");
    }

    let requested_limit = usize::from(query.limit.get());
    let database_limit = i64::from(query.limit.get()) + 1;
    builder
        .push(" ORDER BY todo.created_at DESC, todo.id DESC LIMIT ")
        .push_bind(database_limit);

    let records = builder
        .build_query_as::<TodoRecord>()
        .fetch_all(pool)
        .await?;
    let has_more = records.len() > requested_limit;
    let items = records
        .into_iter()
        .take(requested_limit)
        .map(TodoRecord::into_domain)
        .collect::<Result<Vec<_>, _>>()?;
    let next_cursor = if has_more {
        items
            .last()
            .map(|todo| PageCursor::new(todo.created_at, todo.id.into_uuid()))
    } else {
        None
    };

    Ok(Page::new(items, next_cursor))
}

/// Reads one active todo in the caller's organization.
pub(crate) async fn get_todo(
    pool: &PgPool,
    organization_id: OrganizationId,
    todo_id: TodoId,
) -> Result<Option<Todo>, AppError> {
    let sql = format!(
        "{TODO_PROJECTION} WHERE todo.organization_id = $1 AND todo.id = $2 AND todo.deleted_at IS NULL"
    );
    let record = sqlx::query_as::<_, TodoRecord>(AssertSqlSafe(sql))
        .bind(organization_id.into_uuid())
        .bind(todo_id.into_uuid())
        .fetch_optional(pool)
        .await?;
    record.map(TodoRecord::into_domain).transpose()
}

/// Locks and reads one active todo for a mutation transaction.
pub(crate) async fn lock_todo(
    connection: &mut PgConnection,
    organization_id: OrganizationId,
    todo_id: TodoId,
) -> Result<Option<Todo>, AppError> {
    let locked = sqlx::query_scalar::<_, Uuid>(
        r#"
        SELECT id
        FROM commit.todos
        WHERE organization_id = $1
          AND id = $2
          AND deleted_at IS NULL
        FOR UPDATE
        "#,
    )
    .bind(organization_id.into_uuid())
    .bind(todo_id.into_uuid())
    .fetch_optional(&mut *connection)
    .await?;
    if locked.is_none() {
        return Ok(None);
    }
    get_todo_on_connection(connection, organization_id, todo_id).await
}

/// Locks a todo for scoped, internally soft DELETE semantics.
pub(crate) async fn lock_delete_target(
    connection: &mut PgConnection,
    organization_id: OrganizationId,
    todo_id: TodoId,
) -> Result<DeleteTarget, AppError> {
    let target = sqlx::query_as::<_, (Option<OffsetDateTime>, Uuid)>(
        r#"
        SELECT deleted_at, assigned_by_principal_id
        FROM commit.todos
        WHERE organization_id = $1 AND id = $2
        FOR UPDATE
        "#,
    )
    .bind(organization_id.into_uuid())
    .bind(todo_id.into_uuid())
    .fetch_optional(&mut *connection)
    .await?;

    match target {
        None => Ok(DeleteTarget::Missing),
        Some((Some(_), assigned_by_principal_id)) => Ok(DeleteTarget::AlreadyDeleted(
            PrincipalId::from_uuid(assigned_by_principal_id),
        )),
        Some((None, _)) => get_todo_on_connection(connection, organization_id, todo_id)
            .await?
            .map_or(Ok(DeleteTarget::Missing), |todo| {
                Ok(DeleteTarget::Active(Box::new(todo)))
            }),
    }
}

async fn get_todo_on_connection(
    connection: &mut PgConnection,
    organization_id: OrganizationId,
    todo_id: TodoId,
) -> Result<Option<Todo>, AppError> {
    let sql = format!(
        "{TODO_PROJECTION} WHERE todo.organization_id = $1 AND todo.id = $2 AND todo.deleted_at IS NULL"
    );
    let record = sqlx::query_as::<_, TodoRecord>(AssertSqlSafe(sql))
        .bind(organization_id.into_uuid())
        .bind(todo_id.into_uuid())
        .fetch_optional(&mut *connection)
        .await?;
    record.map(TodoRecord::into_domain).transpose()
}

/// Lists append-only notes from the same snapshot that establishes parent visibility.
pub(crate) async fn list_notes(
    pool: &PgPool,
    organization_id: OrganizationId,
    todo_id: TodoId,
    query: CollectionQuery,
) -> Result<Option<Vec<TodoNote>>, AppError> {
    let cursor_created_at = query.cursor.map(PageCursor::created_at);
    let cursor_id = query.cursor.map(PageCursor::id);
    let records = sqlx::query_as::<_, TodoNoteJoinRecord>(
        r#"
        SELECT
            todo.id AS parent_todo_id,
            note.id,
            note.body,
            note.author_principal_id,
            author.actor_type AS author_actor_type,
            author.actor_id AS author_actor_id,
            note.created_at
        FROM commit.todos AS todo
        LEFT JOIN LATERAL (
            SELECT candidate.id,
                   candidate.body,
                   candidate.author_principal_id,
                   candidate.created_at
            FROM commit.todo_notes AS candidate
            WHERE candidate.organization_id = todo.organization_id
              AND candidate.todo_id = todo.id
              AND (
                  $3::timestamptz IS NULL
                  OR (candidate.created_at, candidate.id) < ($3, $4)
              )
            ORDER BY candidate.created_at DESC, candidate.id DESC
            LIMIT $5
        ) AS note ON TRUE
        LEFT JOIN commit.actor_projection AS author
            ON author.organization_id = todo.organization_id
           AND author.principal_id = note.author_principal_id
        WHERE todo.organization_id = $1
          AND todo.id = $2
          AND todo.deleted_at IS NULL
        ORDER BY note.created_at DESC NULLS LAST, note.id DESC NULLS LAST
        "#,
    )
    .bind(organization_id.into_uuid())
    .bind(todo_id.into_uuid())
    .bind(cursor_created_at)
    .bind(cursor_id)
    .bind(i64::from(query.limit.get()) + 1)
    .fetch_all(pool)
    .await?;

    if records.is_empty() {
        return Ok(None);
    }

    records
        .into_iter()
        .filter_map(TodoNoteJoinRecord::into_domain)
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

/// Persists or verifies the caller's immutable IAM identity projection.
pub(crate) async fn upsert_verified_actor(
    connection: &mut PgConnection,
    actor: &VerifiedActor,
) -> Result<(), AppError> {
    upsert_actor_projection(
        connection,
        actor.organization_id,
        &actor.org_id,
        actor.membership_id,
        &actor.actor,
    )
    .await
}

/// Persists or verifies an online-resolved assignee identity projection.
pub(crate) async fn upsert_active_member(
    connection: &mut PgConnection,
    member: &ActiveMember,
) -> Result<(), AppError> {
    upsert_actor_projection(
        connection,
        member.organization_id,
        &member.org_id,
        member.membership_id,
        &member.actor,
    )
    .await
}

async fn upsert_actor_projection(
    connection: &mut PgConnection,
    organization_id: OrganizationId,
    org_id: &PublicOrganizationId,
    membership_id: Uuid,
    actor: &Actor,
) -> Result<(), AppError> {
    super::identity_projection::persist_identity(
        connection,
        organization_id,
        org_id,
        membership_id,
        actor,
    )
    .await
}

/// Inserts a todo and its ordered attachment set.
pub(crate) async fn insert_todo(
    connection: &mut PgConnection,
    todo: &NewTodo<'_>,
) -> Result<(), AppError> {
    sqlx::query(
        r#"
        INSERT INTO commit.todos (
            id,
            organization_id,
            title,
            description,
            assigned_by_principal_id,
            assigned_to_principal_id,
            status
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        "#,
    )
    .bind(todo.id.into_uuid())
    .bind(todo.organization_id.into_uuid())
    .bind(todo.title.as_str())
    .bind(todo.description.map(LimitedText::as_str))
    .bind(todo.assigned_by_principal_id.into_uuid())
    .bind(todo.assigned_to_principal_id.into_uuid())
    .bind(todo.status)
    .execute(&mut *connection)
    .await?;
    insert_attachments(connection, todo.organization_id, todo.id, todo.attachments).await
}

/// Applies one complete, already-authorized desired todo state.
pub(crate) async fn update_todo(
    connection: &mut PgConnection,
    organization_id: OrganizationId,
    todo_id: TodoId,
    replacement: &TodoReplacement<'_>,
) -> Result<(), AppError> {
    let result = sqlx::query(
        r#"
        UPDATE commit.todos
        SET title = $3,
            description = $4,
            assigned_to_principal_id = $5,
            status = $6
        WHERE organization_id = $1
          AND id = $2
          AND deleted_at IS NULL
        "#,
    )
    .bind(organization_id.into_uuid())
    .bind(todo_id.into_uuid())
    .bind(replacement.title.as_str())
    .bind(replacement.description.map(LimitedText::as_str))
    .bind(replacement.assigned_to_principal_id.into_uuid())
    .bind(replacement.status)
    .execute(&mut *connection)
    .await?;
    if result.rows_affected() != 1 {
        return Err(AppError::NotFound);
    }

    if replacement.replace_attachments {
        sqlx::query(
            "DELETE FROM commit.todo_attachments WHERE organization_id = $1 AND todo_id = $2",
        )
        .bind(organization_id.into_uuid())
        .bind(todo_id.into_uuid())
        .execute(&mut *connection)
        .await?;
        insert_attachments(
            connection,
            organization_id,
            todo_id,
            replacement.attachments,
        )
        .await?;
    }
    Ok(())
}

async fn insert_attachments(
    connection: &mut PgConnection,
    organization_id: OrganizationId,
    todo_id: TodoId,
    attachments: &[PermanentAttachmentUrl],
) -> Result<(), AppError> {
    if attachments.is_empty() {
        return Ok(());
    }

    let positioned = attachments
        .iter()
        .enumerate()
        .map(|(position, attachment)| {
            i16::try_from(position)
                .map(|position| (position, attachment.as_str()))
                .map_err(|error| AppError::Internal(error.into()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut builder = QueryBuilder::<Postgres>::new(
        "INSERT INTO commit.todo_attachments (organization_id, todo_id, position, permanent_url) ",
    );
    builder.push_values(positioned, |mut row, (position, permanent_url)| {
        row.push_bind(organization_id.into_uuid())
            .push_bind(todo_id.into_uuid())
            .push_bind(position)
            .push_bind(permanent_url);
    });
    builder.build().execute(&mut *connection).await?;
    Ok(())
}

/// Soft-deletes the already-locked active todo and returns its new version.
pub(crate) async fn soft_delete_todo(
    connection: &mut PgConnection,
    organization_id: OrganizationId,
    todo_id: TodoId,
    deleted_by: PrincipalId,
    tombstone_retention: Duration,
) -> Result<i64, AppError> {
    let retention_seconds = i64::try_from(tombstone_retention.as_secs())
        .map_err(|error| AppError::Internal(error.into()))?;
    sqlx::query_scalar::<_, i64>(
        r#"
        UPDATE commit.todos
        SET deleted_at = GREATEST(updated_at, clock_timestamp()),
            content_retain_until = GREATEST(updated_at, clock_timestamp())
                + make_interval(secs => $4::double precision),
            deleted_by_principal_id = $3
        WHERE organization_id = $1
          AND id = $2
          AND deleted_at IS NULL
        RETURNING version
        "#,
    )
    .bind(organization_id.into_uuid())
    .bind(todo_id.into_uuid())
    .bind(deleted_by.into_uuid())
    .bind(retention_seconds)
    .fetch_optional(&mut *connection)
    .await?
    .ok_or(AppError::NotFound)
}

/// Appends one note and returns its immutable public projection.
pub(crate) async fn insert_note(
    connection: &mut PgConnection,
    organization_id: OrganizationId,
    todo_id: TodoId,
    note_id: TodoNoteId,
    author: &Actor,
    body: &RequiredText,
) -> Result<TodoNote, AppError> {
    let created_at = sqlx::query_scalar::<_, OffsetDateTime>(
        r#"
        INSERT INTO commit.todo_notes (
            id,
            organization_id,
            todo_id,
            author_principal_id,
            body
        )
        VALUES ($1, $2, $3, $4, $5)
        RETURNING created_at
        "#,
    )
    .bind(note_id.into_uuid())
    .bind(organization_id.into_uuid())
    .bind(todo_id.into_uuid())
    .bind(author.principal_id.into_uuid())
    .bind(body.as_str())
    .fetch_one(&mut *connection)
    .await?;
    Ok(TodoNote {
        id: note_id,
        todo_id,
        body: body.clone(),
        author: author.clone(),
        created_at,
    })
}

/// Acquires the transaction-scoped lock for one exact idempotency namespace.
pub(crate) async fn lock_idempotency_scope(
    connection: &mut PgConnection,
    actor: &VerifiedActor,
    mutation: &MutationIdentity,
) -> Result<(), AppError> {
    let scope = advisory_lock_scope(actor, mutation);
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, $2))")
        .bind(scope)
        .bind(ADVISORY_LOCK_SEED)
        .execute(&mut *connection)
        .await?;
    Ok(())
}

fn advisory_lock_scope(actor: &VerifiedActor, mutation: &MutationIdentity) -> String {
    let values = [
        actor.organization_id.to_string(),
        actor.actor.principal_id.to_string(),
        mutation.operation.to_owned(),
        mutation.resource_path.clone(),
        mutation.key.as_str().to_owned(),
    ];
    values.iter().fold(String::new(), |mut output, value| {
        use std::fmt::Write as _;
        let _ = write!(output, "{}:{value}", value.len());
        output
    })
}

/// Removes an expired scope record and reads a still-active response.
///
/// The caller must hold [`lock_idempotency_scope`] in the same transaction.
pub(crate) async fn load_idempotency_record(
    connection: &mut PgConnection,
    actor: &VerifiedActor,
    mutation: &MutationIdentity,
) -> Result<Option<StoredMutation>, AppError> {
    sqlx::query(
        r#"
        DELETE FROM commit.idempotency_records
        WHERE organization_id = $1
          AND actor_principal_id = $2
          AND operation = $3
          AND resource_path = $4
          AND idempotency_key = $5
          AND expires_at <= transaction_timestamp()
        "#,
    )
    .bind(actor.organization_id.into_uuid())
    .bind(actor.actor.principal_id.into_uuid())
    .bind(mutation.operation)
    .bind(&mutation.resource_path)
    .bind(mutation.key.as_str())
    .execute(&mut *connection)
    .await?;

    let record = sqlx::query_as::<_, StoredMutationRecord>(
        r#"
        SELECT request_fingerprint, response_status, response_body
        FROM commit.idempotency_records
        WHERE organization_id = $1
          AND actor_principal_id = $2
          AND operation = $3
          AND resource_path = $4
          AND idempotency_key = $5
          AND expires_at > transaction_timestamp()
        "#,
    )
    .bind(actor.organization_id.into_uuid())
    .bind(actor.actor.principal_id.into_uuid())
    .bind(mutation.operation)
    .bind(&mutation.resource_path)
    .bind(mutation.key.as_str())
    .fetch_optional(&mut *connection)
    .await?;
    record.map(StoredMutationRecord::into_stored).transpose()
}

/// Stores the complete response in the same transaction as its domain mutation.
pub(crate) async fn insert_idempotency_record(
    connection: &mut PgConnection,
    actor: &VerifiedActor,
    todo_id: TodoId,
    mutation: &MutationIdentity,
    response: &MutationResponse,
    ttl: Duration,
) -> Result<(), AppError> {
    let ttl_seconds =
        i64::try_from(ttl.as_secs()).map_err(|error| AppError::Internal(error.into()))?;
    if ttl_seconds < 1 {
        return Err(AppError::Internal(anyhow!(
            "idempotency TTL must be at least one second"
        )));
    }
    let status =
        i16::try_from(response.status).map_err(|error| AppError::Internal(error.into()))?;
    sqlx::query(
        r#"
        INSERT INTO commit.idempotency_records (
            id,
            organization_id,
            todo_id,
            actor_principal_id,
            operation,
            resource_path,
            idempotency_key,
            request_fingerprint,
            response_status,
            response_body,
            expires_at
        )
        VALUES (
            $1,
            $2,
            $3,
            $4,
            $5,
            $6,
            $7,
            $8,
            $9,
            $10,
            transaction_timestamp() + ($11::double precision * interval '1 second')
        )
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(actor.organization_id.into_uuid())
    .bind(todo_id.into_uuid())
    .bind(actor.actor.principal_id.into_uuid())
    .bind(mutation.operation)
    .bind(&mutation.resource_path)
    .bind(mutation.key.as_str())
    .bind(mutation.fingerprint.as_bytes().as_slice())
    .bind(status)
    .bind(&response.body)
    .bind(ttl_seconds)
    .execute(&mut *connection)
    .await?;
    Ok(())
}

/// Appends internal todo history in the domain transaction.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn insert_activity(
    connection: &mut PgConnection,
    organization_id: OrganizationId,
    todo_id: TodoId,
    kind: TodoActivityKind,
    actor_principal_id: PrincipalId,
    request_id: &str,
    changes: &Value,
    audit_retention: std::time::Duration,
) -> Result<(), AppError> {
    let retention_seconds = i64::try_from(audit_retention.as_secs()).map_err(|error| {
        AppError::Internal(anyhow::anyhow!(
            "activity retention duration is too large: {error}"
        ))
    })?;
    if retention_seconds == 0 {
        return Err(AppError::Internal(anyhow::anyhow!(
            "activity retention duration must be positive"
        )));
    }
    sqlx::query(
        r#"
        INSERT INTO commit.todo_activity (
            id,
            organization_id,
            todo_id,
            activity_type,
            actor_principal_id,
            request_id,
            changes,
            retain_until
        )
        VALUES (
            $1, $2, $3, $4::commit.todo_activity_type, $5, $6, $7,
            transaction_timestamp() + make_interval(secs => $8::double precision)
        )
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(organization_id.into_uuid())
    .bind(todo_id.into_uuid())
    .bind(kind.as_str())
    .bind(actor_principal_id.into_uuid())
    .bind(request_id)
    .bind(changes)
    .bind(retention_seconds)
    .execute(&mut *connection)
    .await?;
    Ok(())
}

/// Appends one minimal mutation audit event.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn insert_audit_event(
    connection: &mut PgConnection,
    organization_id: OrganizationId,
    actor_principal_id: PrincipalId,
    action: &'static str,
    resource_type: &'static str,
    resource_id: Uuid,
    request_id: &str,
    change_summary: &Value,
    audit_retention: std::time::Duration,
) -> Result<(), AppError> {
    let retention_seconds = i64::try_from(audit_retention.as_secs()).map_err(|error| {
        AppError::Internal(anyhow::anyhow!(
            "audit retention duration is too large: {error}"
        ))
    })?;
    if retention_seconds == 0 {
        return Err(AppError::Internal(anyhow::anyhow!(
            "audit retention duration must be positive"
        )));
    }
    sqlx::query(
        r#"
        INSERT INTO commit.audit_events (
            id,
            organization_id,
            actor_principal_id,
            action,
            resource_type,
            resource_id,
            request_id,
            change_summary,
            retain_until
        )
        VALUES (
            $1, $2, $3, $4, $5, $6, $7, $8,
            transaction_timestamp() + make_interval(secs => $9::double precision)
        )
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(organization_id.into_uuid())
    .bind(actor_principal_id.into_uuid())
    .bind(action)
    .bind(resource_type)
    .bind(resource_id)
    .bind(request_id)
    .bind(change_summary)
    .bind(retention_seconds)
    .execute(&mut *connection)
    .await?;
    Ok(())
}

/// Enqueues one delegated-Silicon Hook event in the domain transaction.
pub(crate) async fn insert_outbox_event(
    connection: &mut PgConnection,
    event_id: Uuid,
    organization_id: OrganizationId,
    todo_id: TodoId,
    recipient_silicon_principal_id: PrincipalId,
    event_type: &'static str,
    payload: &Value,
) -> Result<(), AppError> {
    sqlx::query(
        r#"
        INSERT INTO commit.outbox_events (
            id,
            organization_id,
            todo_id,
            recipient_silicon_principal_id,
            event_type,
            payload_version,
            payload
        )
        VALUES ($1, $2, $3, $4, $5, 1, $6)
        "#,
    )
    .bind(event_id)
    .bind(organization_id.into_uuid())
    .bind(todo_id.into_uuid())
    .bind(recipient_silicon_principal_id.into_uuid())
    .bind(event_type)
    .bind(payload)
    .execute(&mut *connection)
    .await?;
    Ok(())
}

#[derive(FromRow)]
struct TodoRecord {
    id: Uuid,
    organization_id: Uuid,
    org_id: String,
    title: String,
    description: Option<String>,
    assigned_by_principal_id: Uuid,
    assigned_by_actor_type: ActorType,
    assigned_by_actor_id: String,
    assigned_to_principal_id: Uuid,
    assigned_to_actor_type: ActorType,
    assigned_to_actor_id: String,
    status: TodoStatus,
    attachments: Vec<String>,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
}

impl TodoRecord {
    fn into_domain(self) -> Result<Todo, AppError> {
        let title = RequiredText::new("title", self.title, STORED_TITLE_CHARS)
            .map_err(corrupt_persisted_data)?;
        let description = self
            .description
            .map(|description| {
                LimitedText::new("description", description, STORED_DESCRIPTION_CHARS)
            })
            .transpose()
            .map_err(corrupt_persisted_data)?;
        let attachments = self
            .attachments
            .into_iter()
            .map(|attachment| {
                PermanentAttachmentUrl::from_persisted(attachment).map_err(corrupt_persisted_data)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Todo {
            id: TodoId::from_uuid(self.id),
            organization_id: OrganizationId::from_uuid(self.organization_id),
            org_id: PublicOrganizationId::new(self.org_id).map_err(corrupt_persisted_data)?,
            title,
            description,
            assigned_to: Actor::new(
                PrincipalId::from_uuid(self.assigned_to_principal_id),
                self.assigned_to_actor_type,
                ActorId::new(self.assigned_to_actor_id).map_err(corrupt_persisted_data)?,
            ),
            assigned_by: Actor::new(
                PrincipalId::from_uuid(self.assigned_by_principal_id),
                self.assigned_by_actor_type,
                ActorId::new(self.assigned_by_actor_id).map_err(corrupt_persisted_data)?,
            ),
            status: self.status,
            attachments,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

#[derive(FromRow)]
struct TodoNoteJoinRecord {
    parent_todo_id: Uuid,
    id: Option<Uuid>,
    body: Option<String>,
    author_principal_id: Option<Uuid>,
    author_actor_type: Option<ActorType>,
    author_actor_id: Option<String>,
    created_at: Option<OffsetDateTime>,
}

impl TodoNoteJoinRecord {
    fn into_domain(self) -> Option<Result<TodoNote, AppError>> {
        let id = self.id?;
        Some(self.note_from_present_row(id))
    }

    fn note_from_present_row(self, id: Uuid) -> Result<TodoNote, AppError> {
        let body = self
            .body
            .ok_or_else(|| corrupt_persisted_data("note body is null"))?;
        let author_principal_id = self
            .author_principal_id
            .ok_or_else(|| corrupt_persisted_data("note author principal is null"))?;
        let author_actor_type = self
            .author_actor_type
            .ok_or_else(|| corrupt_persisted_data("note author type is null"))?;
        let author_actor_id = self
            .author_actor_id
            .ok_or_else(|| corrupt_persisted_data("note author public ID is null"))?;
        let created_at = self
            .created_at
            .ok_or_else(|| corrupt_persisted_data("note creation time is null"))?;
        Ok(TodoNote {
            id: TodoNoteId::from_uuid(id),
            todo_id: TodoId::from_uuid(self.parent_todo_id),
            body: RequiredText::new("body", body, STORED_NOTE_CHARS)
                .map_err(corrupt_persisted_data)?,
            author: Actor::new(
                PrincipalId::from_uuid(author_principal_id),
                author_actor_type,
                ActorId::new(author_actor_id).map_err(corrupt_persisted_data)?,
            ),
            created_at,
        })
    }
}

#[derive(FromRow)]
struct StoredMutationRecord {
    request_fingerprint: Vec<u8>,
    response_status: i16,
    response_body: Value,
}

impl StoredMutationRecord {
    fn into_stored(self) -> Result<StoredMutation, AppError> {
        let status = u16::try_from(self.response_status).map_err(corrupt_persisted_data)?;
        Ok(StoredMutation {
            request_fingerprint: self.request_fingerprint,
            response: MutationResponse::replayed(status, self.response_body),
        })
    }
}

fn corrupt_persisted_data(error: impl std::fmt::Display) -> AppError {
    AppError::Internal(anyhow!("invalid persisted todo data: {error}"))
}

#[cfg(test)]
mod tests {
    use secrecy::SecretString;
    use uuid::Uuid;

    use super::advisory_lock_scope;
    use crate::{
        application::{
            idempotency::{IdempotencyKey, MutationIdentity},
            ports::{CapabilitySet, InboundCredential, OrganizationRole, VerifiedActor},
        },
        domain::{Actor, ActorId, ActorType, OrganizationId, PrincipalId, PublicOrganizationId},
    };

    fn verified_actor(principal_id: Uuid) -> Option<VerifiedActor> {
        let actor_id = ActorId::new("silicon-test").ok()?;
        let org_id = PublicOrganizationId::new("test-org").ok()?;
        let actor = Actor::new(
            PrincipalId::from_uuid(principal_id),
            ActorType::Silicon,
            actor_id,
        );
        let grant = InboundCredential::Bearer(SecretString::from("test-token".to_owned()));
        Some(VerifiedActor::new(
            OrganizationId::from_uuid(Uuid::from_u128(1)),
            org_id,
            Uuid::from_u128(2),
            actor,
            OrganizationRole::Member,
            CapabilitySet::default(),
            grant,
        ))
    }

    fn mutation(path: &str, key: &str) -> Option<MutationIdentity> {
        MutationIdentity::new(
            "updateTodo",
            path,
            IdempotencyKey::new(key).ok()?,
            &serde_json::json!({ "status": "blocked" }),
        )
        .ok()
    }

    #[test]
    fn advisory_scope_is_length_framed_and_bound_to_every_identity_dimension() {
        let Some(actor) = verified_actor(Uuid::from_u128(3)) else {
            return;
        };
        let Some(other_actor) = verified_actor(Uuid::from_u128(4)) else {
            return;
        };
        let Some(first) = mutation("/todos/one", "request-one") else {
            return;
        };
        let Some(other_path) = mutation("/todos/two", "request-one") else {
            return;
        };
        let Some(other_key) = mutation("/todos/one", "request-two") else {
            return;
        };

        let scope = advisory_lock_scope(&actor, &first);
        assert_ne!(scope, advisory_lock_scope(&other_actor, &first));
        assert_ne!(scope, advisory_lock_scope(&actor, &other_path));
        assert_ne!(scope, advisory_lock_scope(&actor, &other_key));
        assert!(scope.contains("10:updateTodo"));
    }
}
