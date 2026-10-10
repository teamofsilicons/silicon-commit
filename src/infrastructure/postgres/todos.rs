//! PostgreSQL persistence for todo use cases.
//!
//! Rows are keyed by their global ids and by Silicon Accounts uuids. Who may
//! see a todo is decided by `commit.todo_access(todo, account)`: the circles of
//! its owner and assignee, plus whoever can read its project.

use std::time::Duration;

use anyhow::anyhow;
use serde_json::Value;
use sqlx::{AssertSqlSafe, FromRow, PgConnection, PgPool, Postgres, QueryBuilder};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{
    application::{
        idempotency::{MutationIdentity, MutationResponse},
        ports::{VerifiedActor, WebhookRoutingSnapshot},
    },
    domain::{
        AccountUuid, Actor, ActorId, ActorType, AttachmentUrl, CollectionQuery, CreatedAtRange,
        LimitedText, Page, PageCursor, RequiredText, Todo, TodoId, TodoNote, TodoNoteId, TodoPage,
        TodoQuery, TodoStatus, TodoView,
    },
    error::AppError,
    infrastructure::postgres::accounts::account_uuid,
};

const STORED_TITLE_CHARS: usize = 500;
const STORED_DESCRIPTION_CHARS: usize = 100_000;
const STORED_NOTE_CHARS: usize = 100_000;
const ADVISORY_LOCK_SEED: i64 = 7_621_913_449_043_511_527;
/// Version of the JSON delivered to Silicon webhooks (3: accounts, no organization).
pub(crate) const OUTBOX_PAYLOAD_VERSION: i16 = 3;

const TODO_PROJECTION: &str = r#"
    SELECT
        todo.id,
        todo.project_id,
        todo.title,
        todo.description,
        assigned_by.uuid AS assigned_by_uuid,
        assigned_by.kind AS assigned_by_kind,
        assigned_by.public_id AS assigned_by_id,
        assigned_to.uuid AS assigned_to_uuid,
        assigned_to.kind AS assigned_to_kind,
        assigned_to.public_id AS assigned_to_id,
        todo.status,
        ARRAY(
            SELECT attachment.url
            FROM commit.todo_attachments AS attachment
            WHERE attachment.todo_id = todo.id
            ORDER BY attachment.position
        ) AS attachments,
        todo.created_at,
        todo.updated_at
    FROM commit.todos AS todo
    INNER JOIN commit.accounts AS assigned_by ON assigned_by.uuid = todo.assigned_by_account
    INNER JOIN commit.accounts AS assigned_to ON assigned_to.uuid = todo.assigned_to_account
"#;

/// Complete values needed to insert a todo aggregate.
pub(crate) struct NewTodo<'a> {
    pub(crate) id: TodoId,
    pub(crate) title: &'a RequiredText,
    pub(crate) description: Option<&'a LimitedText>,
    pub(crate) assigned_by: &'a AccountUuid,
    pub(crate) assigned_to: &'a AccountUuid,
    pub(crate) status: TodoStatus,
    pub(crate) attachments: &'a [AttachmentUrl],
    pub(crate) project_id: Option<crate::domain::ProjectId>,
}

/// Full desired todo state after an authorized patch.
pub(crate) struct TodoReplacement<'a> {
    pub(crate) title: &'a RequiredText,
    pub(crate) description: Option<&'a LimitedText>,
    pub(crate) assigned_to: &'a AccountUuid,
    pub(crate) status: TodoStatus,
    pub(crate) attachments: &'a [AttachmentUrl],
    pub(crate) replace_attachments: bool,
    pub(crate) project_id: Option<crate::domain::ProjectId>,
}

/// Complete immutable webhook event selected within a todo mutation transaction.
pub(crate) struct NewOutboxEvent<'a> {
    pub(crate) id: Uuid,
    pub(crate) todo_id: TodoId,
    pub(crate) recipient_silicon: &'a AccountUuid,
    pub(crate) event_type: &'static str,
    pub(crate) payload: &'a Value,
    pub(crate) routing: &'a WebhookRoutingSnapshot,
}

/// Durable response read under an idempotency advisory lock.
pub(crate) struct StoredMutation {
    pub(crate) request_fingerprint: Vec<u8>,
    pub(crate) response: MutationResponse,
}

/// State of a todo selected for DELETE.
pub(crate) enum DeleteTarget {
    Missing,
    AlreadyDeleted(AccountUuid),
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

/// Lists todos visible to the caller with stable descending keyset pagination.
pub(crate) async fn list_todos(
    pool: &PgPool,
    actor: &VerifiedActor,
    query: &TodoQuery,
    created_at: CreatedAtRange,
) -> Result<TodoPage, AppError> {
    let me = actor.uuid().as_str();
    let mut builder = QueryBuilder::<Postgres>::new(TODO_PROJECTION);
    builder
        .push(" WHERE todo.deleted_at IS NULL AND commit.todo_access(todo.id, ")
        .push_bind(me)
        .push(")");
    if let Some(project_id) = query.project_id {
        builder
            .push(" AND todo.project_id = ")
            .push_bind(project_id.into_uuid());
    }

    match query.view {
        TodoView::AssignedToMe => {
            builder
                .push(" AND todo.assigned_to_account = ")
                .push_bind(me);
        }
        TodoView::DelegatedByMe => {
            builder
                .push(" AND todo.assigned_by_account = ")
                .push_bind(me)
                .push(" AND todo.assigned_to_account <> todo.assigned_by_account");
        }
        TodoView::All => {}
    }

    if let Some(status) = query.status {
        builder.push(" AND todo.status = ").push_bind(status);
    }
    if let Some(assigned_to) = &query.assigned_to {
        push_account_filter(&mut builder, "assigned_to", assigned_to);
    }
    if let Some(assigned_by) = &query.assigned_by {
        push_account_filter(&mut builder, "assigned_by", assigned_by);
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

/// Filters by an account named by `c:`/`si:` id (current id) or by uuid.
fn push_account_filter(builder: &mut QueryBuilder<Postgres>, alias: &'static str, value: &ActorId) {
    if value.prefixed_kind().is_some() {
        builder
            .push(format_args!(" AND lower({alias}.public_id) = "))
            .push_bind(value.as_str().to_ascii_lowercase());
    } else {
        builder
            .push(format_args!(" AND {alias}.uuid = "))
            .push_bind(value.as_str().to_owned());
    }
}

/// Reads one active todo when the viewer may see it.
pub(crate) async fn get_visible_todo(
    pool: &PgPool,
    todo_id: TodoId,
    viewer: &AccountUuid,
) -> Result<Option<Todo>, AppError> {
    let sql = format!(
        "{TODO_PROJECTION} WHERE todo.id = $1 AND todo.deleted_at IS NULL AND commit.todo_access(todo.id, $2)"
    );
    let record = sqlx::query_as::<_, TodoRecord>(AssertSqlSafe(sql))
        .bind(todo_id.into_uuid())
        .bind(viewer.as_str())
        .fetch_optional(pool)
        .await?;
    record.map(TodoRecord::into_domain).transpose()
}

/// Whether the viewer may see the active todo.
pub(crate) async fn can_see(
    connection: &mut PgConnection,
    todo_id: TodoId,
    viewer: &AccountUuid,
) -> Result<bool, AppError> {
    Ok(
        sqlx::query_scalar::<_, bool>("SELECT commit.todo_access($1, $2)")
            .bind(todo_id.into_uuid())
            .bind(viewer.as_str())
            .fetch_one(connection)
            .await?,
    )
}

/// Locks and reads one active todo for a mutation transaction.
pub(crate) async fn lock_todo(
    connection: &mut PgConnection,
    todo_id: TodoId,
) -> Result<Option<Todo>, AppError> {
    let locked = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM commit.todos WHERE id = $1 AND deleted_at IS NULL FOR UPDATE",
    )
    .bind(todo_id.into_uuid())
    .fetch_optional(&mut *connection)
    .await?;
    if locked.is_none() {
        return Ok(None);
    }
    get_todo_on_connection(connection, todo_id).await
}

/// Locks a todo for internally soft DELETE semantics.
pub(crate) async fn lock_delete_target(
    connection: &mut PgConnection,
    todo_id: TodoId,
) -> Result<DeleteTarget, AppError> {
    let target = sqlx::query_as::<_, (Option<OffsetDateTime>, String)>(
        "SELECT deleted_at, assigned_by_account FROM commit.todos WHERE id = $1 FOR UPDATE",
    )
    .bind(todo_id.into_uuid())
    .fetch_optional(&mut *connection)
    .await?;

    match target {
        None => Ok(DeleteTarget::Missing),
        Some((Some(_), owner)) => Ok(DeleteTarget::AlreadyDeleted(account_uuid(owner)?)),
        Some((None, _)) => get_todo_on_connection(connection, todo_id)
            .await?
            .map_or(Ok(DeleteTarget::Missing), |todo| {
                Ok(DeleteTarget::Active(Box::new(todo)))
            }),
    }
}

async fn get_todo_on_connection(
    connection: &mut PgConnection,
    todo_id: TodoId,
) -> Result<Option<Todo>, AppError> {
    let sql = format!("{TODO_PROJECTION} WHERE todo.id = $1 AND todo.deleted_at IS NULL");
    let record = sqlx::query_as::<_, TodoRecord>(AssertSqlSafe(sql))
        .bind(todo_id.into_uuid())
        .fetch_optional(&mut *connection)
        .await?;
    record.map(TodoRecord::into_domain).transpose()
}

/// Lists append-only notes from the same snapshot that establishes parent visibility.
pub(crate) async fn list_notes(
    pool: &PgPool,
    todo_id: TodoId,
    viewer: &AccountUuid,
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
            author.uuid AS author_uuid,
            author.kind AS author_kind,
            author.public_id AS author_id,
            note.created_at
        FROM commit.todos AS todo
        LEFT JOIN LATERAL (
            SELECT candidate.id, candidate.body, candidate.author_account, candidate.created_at
            FROM commit.todo_notes AS candidate
            WHERE candidate.todo_id = todo.id
              AND ($3::timestamptz IS NULL OR (candidate.created_at, candidate.id) < ($3, $4))
            ORDER BY candidate.created_at DESC, candidate.id DESC
            LIMIT $5
        ) AS note ON TRUE
        LEFT JOIN commit.accounts AS author ON author.uuid = note.author_account
        WHERE todo.id = $1
          AND todo.deleted_at IS NULL
          AND commit.todo_access(todo.id, $2)
        ORDER BY note.created_at DESC NULLS LAST, note.id DESC NULLS LAST
        "#,
    )
    .bind(todo_id.into_uuid())
    .bind(viewer.as_str())
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

/// Inserts a todo and its ordered attachment set.
pub(crate) async fn insert_todo(
    connection: &mut PgConnection,
    todo: &NewTodo<'_>,
) -> Result<(), AppError> {
    sqlx::query(
        r#"
        INSERT INTO commit.todos (
            id, title, description, assigned_by_account, assigned_to_account, status, project_id
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        "#,
    )
    .bind(todo.id.into_uuid())
    .bind(todo.title.as_str())
    .bind(todo.description.map(LimitedText::as_str))
    .bind(todo.assigned_by.as_str())
    .bind(todo.assigned_to.as_str())
    .bind(todo.status)
    .bind(todo.project_id.map(crate::domain::ProjectId::into_uuid))
    .execute(&mut *connection)
    .await?;
    insert_attachments(connection, todo.id, todo.attachments).await
}

/// Applies one complete, already-authorized desired todo state.
pub(crate) async fn update_todo(
    connection: &mut PgConnection,
    todo_id: TodoId,
    replacement: &TodoReplacement<'_>,
) -> Result<(), AppError> {
    let result = sqlx::query(
        r#"
        UPDATE commit.todos
        SET title = $2,
            description = $3,
            assigned_to_account = $4,
            status = $5,
            project_id = $6
        WHERE id = $1
          AND deleted_at IS NULL
        "#,
    )
    .bind(todo_id.into_uuid())
    .bind(replacement.title.as_str())
    .bind(replacement.description.map(LimitedText::as_str))
    .bind(replacement.assigned_to.as_str())
    .bind(replacement.status)
    .bind(
        replacement
            .project_id
            .map(crate::domain::ProjectId::into_uuid),
    )
    .execute(&mut *connection)
    .await?;
    if result.rows_affected() != 1 {
        return Err(AppError::NotFound);
    }

    if replacement.replace_attachments {
        sqlx::query("DELETE FROM commit.todo_attachments WHERE todo_id = $1")
            .bind(todo_id.into_uuid())
            .execute(&mut *connection)
            .await?;
        insert_attachments(connection, todo_id, replacement.attachments).await?;
    }
    Ok(())
}

async fn insert_attachments(
    connection: &mut PgConnection,
    todo_id: TodoId,
    attachments: &[AttachmentUrl],
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
        "INSERT INTO commit.todo_attachments (todo_id, position, url) ",
    );
    builder.push_values(positioned, |mut row, (position, attachment_url)| {
        row.push_bind(todo_id.into_uuid())
            .push_bind(position)
            .push_bind(attachment_url);
    });
    builder.build().execute(&mut *connection).await?;
    Ok(())
}

/// Soft-deletes the already-locked active todo and returns its new version.
pub(crate) async fn soft_delete_todo(
    connection: &mut PgConnection,
    todo_id: TodoId,
    deleted_by: &AccountUuid,
    tombstone_retention: Duration,
) -> Result<i64, AppError> {
    let retention_seconds = i64::try_from(tombstone_retention.as_secs())
        .map_err(|error| AppError::Internal(error.into()))?;
    sqlx::query_scalar::<_, i64>(
        r#"
        UPDATE commit.todos
        SET deleted_at = GREATEST(updated_at, clock_timestamp()),
            content_retain_until = GREATEST(updated_at, clock_timestamp())
                + make_interval(secs => $3::double precision),
            deleted_by_account = $2
        WHERE id = $1
          AND deleted_at IS NULL
        RETURNING version
        "#,
    )
    .bind(todo_id.into_uuid())
    .bind(deleted_by.as_str())
    .bind(retention_seconds)
    .fetch_optional(&mut *connection)
    .await?
    .ok_or(AppError::NotFound)
}

/// Appends one note and returns its immutable public projection.
pub(crate) async fn insert_note(
    connection: &mut PgConnection,
    todo_id: TodoId,
    note_id: TodoNoteId,
    author: &Actor,
    body: &RequiredText,
) -> Result<TodoNote, AppError> {
    let created_at = sqlx::query_scalar::<_, OffsetDateTime>(
        r#"
        INSERT INTO commit.todo_notes (id, todo_id, author_account, body)
        VALUES ($1, $2, $3, $4)
        RETURNING created_at
        "#,
    )
    .bind(note_id.into_uuid())
    .bind(todo_id.into_uuid())
    .bind(author.uuid.as_str())
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
        actor.uuid().as_str().to_owned(),
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
        WHERE actor_account = $1
          AND operation = $2
          AND resource_path = $3
          AND idempotency_key = $4
          AND expires_at <= transaction_timestamp()
        "#,
    )
    .bind(actor.uuid().as_str())
    .bind(mutation.operation)
    .bind(&mutation.resource_path)
    .bind(mutation.key.as_str())
    .execute(&mut *connection)
    .await?;

    let record = sqlx::query_as::<_, StoredMutationRecord>(
        r#"
        SELECT request_fingerprint, response_status, response_body
        FROM commit.idempotency_records
        WHERE actor_account = $1
          AND operation = $2
          AND resource_path = $3
          AND idempotency_key = $4
          AND expires_at > transaction_timestamp()
        "#,
    )
    .bind(actor.uuid().as_str())
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
            id, todo_id, actor_account, operation, resource_path, idempotency_key,
            request_fingerprint, response_status, response_body, expires_at
        )
        VALUES (
            $1, $2, $3, $4, $5, $6, $7, $8, $9,
            transaction_timestamp() + ($10::double precision * interval '1 second')
        )
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(todo_id.into_uuid())
    .bind(actor.uuid().as_str())
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

/// Adds `via_app` to change details when another app acts for the account.
pub(crate) fn annotate(actor: &VerifiedActor, details: &Value) -> Value {
    match (actor.via_app(), details) {
        (Some(app), Value::Object(fields)) => {
            let mut fields = fields.clone();
            fields.insert("via_app".to_owned(), Value::String(app.to_owned()));
            Value::Object(fields)
        }
        _ => details.clone(),
    }
}

/// Appends internal todo history in the domain transaction.
pub(crate) async fn insert_activity(
    connection: &mut PgConnection,
    actor: &VerifiedActor,
    todo_id: TodoId,
    kind: TodoActivityKind,
    request_id: &str,
    changes: &Value,
    audit_retention: Duration,
) -> Result<(), AppError> {
    let retention_seconds = positive_seconds(audit_retention, "activity")?;
    sqlx::query(
        r#"
        INSERT INTO commit.todo_activity (
            id, todo_id, activity_type, actor_account, request_id, changes, retain_until
        )
        VALUES (
            $1, $2, $3::commit.todo_activity_type, $4, $5, $6,
            transaction_timestamp() + make_interval(secs => $7::double precision)
        )
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(todo_id.into_uuid())
    .bind(kind.as_str())
    .bind(actor.uuid().as_str())
    .bind(request_id)
    .bind(annotate(actor, changes))
    .bind(retention_seconds)
    .execute(&mut *connection)
    .await?;
    Ok(())
}

/// Appends one minimal mutation audit event naming the acting account.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn insert_audit_event(
    connection: &mut PgConnection,
    actor: &VerifiedActor,
    action: &'static str,
    resource_type: &'static str,
    resource_id: Uuid,
    request_id: &str,
    change_summary: &Value,
    audit_retention: Duration,
) -> Result<(), AppError> {
    let retention_seconds = positive_seconds(audit_retention, "audit")?;
    sqlx::query(
        r#"
        INSERT INTO commit.audit_events (
            id, actor_account, action, resource_type, resource_id, request_id, change_summary, retain_until
        )
        VALUES (
            $1, $2, $3, $4, $5, $6, $7,
            transaction_timestamp() + make_interval(secs => $8::double precision)
        )
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(actor.uuid().as_str())
    .bind(action)
    .bind(resource_type)
    .bind(resource_id)
    .bind(request_id)
    .bind(annotate(actor, change_summary))
    .bind(retention_seconds)
    .execute(&mut *connection)
    .await?;
    Ok(())
}

fn positive_seconds(duration: Duration, what: &str) -> Result<i64, AppError> {
    let seconds = i64::try_from(duration.as_secs()).map_err(|error| {
        AppError::Internal(anyhow!("{what} retention duration is too large: {error}"))
    })?;
    if seconds == 0 {
        return Err(AppError::Internal(anyhow!(
            "{what} retention duration must be positive"
        )));
    }
    Ok(seconds)
}

/// Enqueues one delegated-Silicon webhook event in the domain transaction.
pub(crate) async fn insert_outbox_event(
    connection: &mut PgConnection,
    event: &NewOutboxEvent<'_>,
) -> Result<(), AppError> {
    sqlx::query(
        r#"
        INSERT INTO commit.outbox_events (
            id, todo_id, recipient_silicon_account, event_type, payload_version, payload,
            webhook_url, destination_version, subscription_level, subscription_scope, subscription_version
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
        "#,
    )
    .bind(event.id)
    .bind(event.todo_id.into_uuid())
    .bind(event.recipient_silicon.as_str())
    .bind(event.event_type)
    .bind(OUTBOX_PAYLOAD_VERSION)
    .bind(event.payload)
    .bind(event.routing.webhook_url().as_str())
    .bind(event.routing.destination_version().get())
    .bind(event.routing.subscription_level())
    .bind(event.routing.subscription_scope())
    .bind(event.routing.subscription_version().get())
    .execute(&mut *connection)
    .await?;
    Ok(())
}

#[derive(FromRow)]
struct TodoRecord {
    id: Uuid,
    title: String,
    description: Option<String>,
    assigned_by_uuid: String,
    assigned_by_kind: ActorType,
    assigned_by_id: String,
    assigned_to_uuid: String,
    assigned_to_kind: ActorType,
    assigned_to_id: String,
    status: TodoStatus,
    attachments: Vec<String>,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
    project_id: Option<Uuid>,
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
                AttachmentUrl::from_persisted(attachment).map_err(corrupt_persisted_data)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Todo {
            project_id: self.project_id.map(crate::domain::ProjectId::from_uuid),
            id: TodoId::from_uuid(self.id),
            title,
            description,
            assigned_to: Actor::new(
                account_uuid(self.assigned_to_uuid)?,
                self.assigned_to_kind,
                ActorId::from_persisted(self.assigned_to_id),
            ),
            assigned_by: Actor::new(
                account_uuid(self.assigned_by_uuid)?,
                self.assigned_by_kind,
                ActorId::from_persisted(self.assigned_by_id),
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
    author_uuid: Option<String>,
    author_kind: Option<ActorType>,
    author_id: Option<String>,
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
        let author_uuid = self
            .author_uuid
            .ok_or_else(|| corrupt_persisted_data("note author account is missing"))?;
        let author_kind = self
            .author_kind
            .ok_or_else(|| corrupt_persisted_data("note author kind is null"))?;
        let created_at = self
            .created_at
            .ok_or_else(|| corrupt_persisted_data("note creation time is null"))?;
        Ok(TodoNote {
            id: TodoNoteId::from_uuid(id),
            todo_id: TodoId::from_uuid(self.parent_todo_id),
            body: RequiredText::new("body", body, STORED_NOTE_CHARS)
                .map_err(corrupt_persisted_data)?,
            author: Actor::new(
                account_uuid(author_uuid)?,
                author_kind,
                ActorId::from_persisted(self.author_id.unwrap_or_default()),
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
    use super::{advisory_lock_scope, annotate};
    use crate::{
        application::{
            idempotency::{IdempotencyKey, MutationIdentity},
            ports::{Grant, VerifiedActor},
        },
        domain::{AccountUuid, Actor, ActorId, ActorType},
    };

    fn verified_actor(uuid: &str, grant: Grant) -> Option<VerifiedActor> {
        let actor = Actor::new(
            AccountUuid::new(uuid).ok()?,
            ActorType::Silicon,
            ActorId::new("si:test").ok()?,
        );
        Some(VerifiedActor::new(actor, grant))
    }

    fn bearer() -> Grant {
        Grant::Bearer {
            issued_at: None,
            family: None,
        }
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
        let (Some(actor), Some(other_actor)) = (
            verified_actor("aaa", bearer()),
            verified_actor("aaA", bearer()),
        ) else {
            panic!("fixture accounts are valid");
        };
        let (Some(first), Some(other_path), Some(other_key)) = (
            mutation("/todos/one", "request-one"),
            mutation("/todos/two", "request-one"),
            mutation("/todos/one", "request-two"),
        ) else {
            panic!("fixture mutations are valid");
        };

        let scope = advisory_lock_scope(&actor, &first);
        assert_ne!(scope, advisory_lock_scope(&other_actor, &first));
        assert_ne!(scope, advisory_lock_scope(&actor, &other_path));
        assert_ne!(scope, advisory_lock_scope(&actor, &other_key));
        assert!(scope.contains("10:updateTodo"));
    }

    #[test]
    fn proof_callers_are_recorded_on_audit_details() {
        let Some(actor) = verified_actor(
            "aaa",
            Grant::Proof {
                issuing_app: "interface".to_owned(),
                proof_id: "p1".to_owned(),
                scopes: vec!["commit.todos.create".to_owned()],
            },
        ) else {
            panic!("fixture account is valid");
        };
        let details = annotate(&actor, &serde_json::json!({ "fields": ["title"] }));
        assert_eq!(details["via_app"], "interface");
        let Some(direct) = verified_actor("aaa", bearer()) else {
            panic!("fixture account is valid");
        };
        assert!(
            annotate(&direct, &serde_json::json!({}))
                .get("via_app")
                .is_none()
        );
    }
}
