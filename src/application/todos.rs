//! Todo application workflows.
//!
//! A todo belongs to the account that created it (`assigned_by`) and is shared
//! with its assignee. It is visible to both their custodian circles and to
//! whoever can read its project. The owner changes its content; the owner and
//! the assignee move its status; custodians stand in for their Silicons.

use std::{borrow::Cow, sync::Arc, time::Duration};

use serde_json::{Map, Value, json};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::{
    application::{
        accounts::{account_field_error, ensure_reachable, remember},
        authorization,
        idempotency::{IdempotencyKey, MutationIdentity, MutationResponse},
        ports::{IdentityProvider, ResolvedAccount, VerifiedActor},
    },
    domain::{
        Actor, ActorId, AttachmentUrl, CollectionQuery, DomainLimits, LimitedText, NullablePatch,
        Page, PageCursor, ProjectId, ProjectLocator, RequiredText, Todo, TodoCreate, TodoId,
        TodoNote, TodoNoteCreate, TodoPage, TodoPatch, TodoQuery, TodoStatus, ValidatedTodoPatch,
        ValidationError,
    },
    error::AppError,
    infrastructure::postgres::{
        notifications as notification_store, projects as project_store,
        todos::{
            self as store, DeleteTarget, NewOutboxEvent, NewTodo, TodoActivityKind, TodoReplacement,
        },
    },
};

const CREATE_TODO_OPERATION: &str = "createTodo";
const UPDATE_TODO_OPERATION: &str = "updateTodo";
const ADD_TODO_NOTE_OPERATION: &str = "addTodoNote";

/// Complete todo and note use-case boundary.
#[derive(Clone)]
pub struct TodoService {
    pool: PgPool,
    identity_provider: Arc<dyn IdentityProvider>,
    limits: DomainLimits,
    idempotency_ttl: Duration,
    audit_retention: Duration,
    tombstone_retention: Duration,
}

impl TodoService {
    /// Creates a todo service from its database and Silicon Accounts dependencies.
    #[must_use]
    pub fn new(
        pool: PgPool,
        identity_provider: Arc<dyn IdentityProvider>,
        limits: DomainLimits,
        idempotency_ttl: Duration,
        audit_retention: Duration,
        tombstone_retention: Duration,
    ) -> Self {
        Self {
            pool,
            identity_provider,
            limits,
            idempotency_ttl,
            audit_retention,
            tombstone_retention,
        }
    }

    /// Lists active todos the caller can see, using stable keyset pagination.
    pub async fn list(
        &self,
        actor: &VerifiedActor,
        query: TodoQuery,
    ) -> Result<TodoPage, AppError> {
        let created_at = query.created_at_range().map_err(validation_error)?;
        store::list_todos(&self.pool, actor, &query, created_at).await
    }

    /// Returns one active todo the caller can see.
    pub async fn get(&self, actor: &VerifiedActor, todo_id: TodoId) -> Result<Todo, AppError> {
        store::get_visible_todo(&self.pool, todo_id, actor.uuid())
            .await?
            .ok_or(AppError::NotFound)
    }

    /// Creates a todo exactly once for one idempotency scope.
    pub async fn create(
        &self,
        actor: &VerifiedActor,
        request: TodoCreate,
        idempotency_key: IdempotencyKey,
        request_id: &str,
    ) -> Result<MutationResponse, AppError> {
        validate_request_id(request_id)?;
        let fingerprint_input = create_fingerprint_input(&request);
        let mutation = mutation_identity(
            CREATE_TODO_OPERATION,
            "/todos",
            idempotency_key,
            &fingerprint_input,
        )?;
        if let Some(response) = self.probe_replay(actor, &mutation).await? {
            return Ok(response);
        }

        let request = request.validate(&self.limits).map_err(validation_error)?;
        let assignee = self.resolve_assignee(actor, &request.assigned_to).await?;

        let mut transaction = self.pool.begin().await?;
        store::lock_idempotency_scope(transaction.as_mut(), actor, &mutation).await?;
        if let Some(response) = replay_on_connection(transaction.as_mut(), actor, &mutation).await?
        {
            transaction.commit().await?;
            return Ok(response);
        }

        remember(transaction.as_mut(), actor, std::slice::from_ref(&assignee)).await?;
        ensure_reachable(transaction.as_mut(), actor, &assignee.actor, "assigned_to").await?;
        let todo_id = TodoId::new();
        if let Some(project_id) = request.project_id {
            authorize_link(&mut transaction, actor, project_id).await?;
            share_project(&mut transaction, actor, project_id, &assignee.actor).await?;
        }
        store::insert_todo(
            transaction.as_mut(),
            &NewTodo {
                id: todo_id,
                title: &request.title,
                description: request.description.as_ref(),
                assigned_by: actor.uuid(),
                assigned_to: &assignee.actor.uuid,
                status: request.status,
                attachments: &request.attachments,
                project_id: request.project_id,
            },
        )
        .await?;

        let changes = json!({
            "fields": ["title", "description", "assigned_to", "status", "attachments"]
        });
        store::insert_activity(
            transaction.as_mut(),
            actor,
            todo_id,
            TodoActivityKind::Created,
            request_id,
            &changes,
            self.audit_retention,
        )
        .await?;
        store::insert_audit_event(
            transaction.as_mut(),
            actor,
            "todo.created",
            "todo",
            todo_id.into_uuid(),
            request_id,
            &changes,
            self.audit_retention,
        )
        .await?;

        let todo = store::lock_todo(transaction.as_mut(), todo_id)
            .await?
            .ok_or_else(|| {
                AppError::Internal(anyhow::anyhow!("inserted todo could not be read"))
            })?;
        let response = mutation_response(201, &todo)?;
        store::insert_idempotency_record(
            transaction.as_mut(),
            actor,
            todo_id,
            &mutation,
            &response,
            self.idempotency_ttl,
        )
        .await?;
        transaction.commit().await?;
        Ok(response)
    }

    /// Applies an authorized field-level todo patch exactly once.
    pub async fn update(
        &self,
        actor: &VerifiedActor,
        todo_id: TodoId,
        request: TodoPatch,
        idempotency_key: IdempotencyKey,
        request_id: &str,
    ) -> Result<MutationResponse, AppError> {
        validate_request_id(request_id)?;
        let path = format!("/todos/{todo_id}");
        let mutation = mutation_identity(UPDATE_TODO_OPERATION, path, idempotency_key, &request)?;
        if let Some(response) = self.probe_replay(actor, &mutation).await? {
            return Ok(response);
        }

        let patch = request.validate(&self.limits).map_err(validation_error)?;
        let authorization_snapshot = self.get(actor, todo_id).await?;
        authorize_patch(actor, &authorization_snapshot, &patch)?;
        let resolved_assignee = match patch.assigned_to.as_ref() {
            Some(assigned_to) => Some(self.resolve_assignee(actor, assigned_to).await?),
            None => None,
        };

        let mut transaction = self.pool.begin().await?;
        lock_related_project(&mut transaction, todo_id).await?;
        store::lock_idempotency_scope(transaction.as_mut(), actor, &mutation).await?;
        if let Some(response) = replay_on_connection(transaction.as_mut(), actor, &mutation).await?
        {
            transaction.commit().await?;
            return Ok(response);
        }

        let current = store::lock_todo(transaction.as_mut(), todo_id)
            .await?
            .ok_or(AppError::NotFound)?;
        if !store::can_see(transaction.as_mut(), todo_id, actor.uuid()).await? {
            return Err(AppError::NotFound);
        }
        authorize_patch(actor, &current, &patch)?;
        remember(transaction.as_mut(), actor, resolved_assignee.as_slice()).await?;

        let desired = DesiredTodo::from_patch(&current, patch, resolved_assignee);
        if desired.changed_fields.contains(&"assigned_to") {
            ensure_reachable(
                transaction.as_mut(),
                actor,
                &desired.assigned_to,
                "assigned_to",
            )
            .await?;
        }
        if desired.changed_fields.is_empty() {
            let response = mutation_response(200, &current)?;
            store::insert_idempotency_record(
                transaction.as_mut(),
                actor,
                todo_id,
                &mutation,
                &response,
                self.idempotency_ttl,
            )
            .await?;
            transaction.commit().await?;
            return Ok(response);
        }

        if desired.project_id != current.project_id {
            let linked: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM commit.project_tasks WHERE todo_id = $1)",
            )
            .bind(todo_id.into_uuid())
            .fetch_one(&mut *transaction)
            .await?;
            if linked {
                return Err(AppError::Conflict {
                    code: "project_task_link_is_immutable".into(),
                });
            }
            if let Some(id) = desired.project_id {
                authorize_link(&mut transaction, actor, id).await?;
            }
        }
        if let Some(project_id) = desired.project_id {
            share_project(&mut transaction, actor, project_id, &desired.assigned_to).await?;
        }
        store::update_todo(
            transaction.as_mut(),
            todo_id,
            &TodoReplacement {
                project_id: desired.project_id,
                title: &desired.title,
                description: desired.description.as_ref(),
                assigned_to: &desired.assigned_to.uuid,
                status: desired.status,
                attachments: &desired.attachments,
                replace_attachments: desired.replace_attachments,
            },
        )
        .await?;
        let updated = store::lock_todo(transaction.as_mut(), todo_id)
            .await?
            .ok_or_else(|| AppError::Internal(anyhow::anyhow!("updated todo could not be read")))?;

        let changes = json!({ "fields": desired.changed_fields });
        store::insert_activity(
            transaction.as_mut(),
            actor,
            todo_id,
            desired.activity_kind,
            request_id,
            &changes,
            self.audit_retention,
        )
        .await?;
        store::insert_audit_event(
            transaction.as_mut(),
            actor,
            "todo.updated",
            "todo",
            todo_id.into_uuid(),
            request_id,
            &changes,
            self.audit_retention,
        )
        .await?;
        if updated.should_notify_assigner() {
            let event_type = update_event_type(&desired.changed_fields);
            let resulting_status = (current.status != updated.status).then_some(updated.status);
            let mut notification_details =
                Map::from_iter([("changed_fields".to_owned(), json!(desired.changed_fields))]);
            if resulting_status.is_some() {
                notification_details.insert("previous_status".to_owned(), json!(current.status));
                notification_details.insert("status".to_owned(), json!(updated.status));
            }
            enqueue_notification(
                transaction.as_mut(),
                actor,
                &updated,
                event_type,
                request_id,
                Value::Object(notification_details),
                resulting_status,
            )
            .await?;
        }

        let response = mutation_response(200, &updated)?;
        store::insert_idempotency_record(
            transaction.as_mut(),
            actor,
            todo_id,
            &mutation,
            &response,
            self.idempotency_ttl,
        )
        .await?;
        transaction.commit().await?;
        Ok(response)
    }

    /// Soft-deletes one todo when the caller is its owner (or the owner's custodian).
    pub async fn delete(
        &self,
        actor: &VerifiedActor,
        todo_id: TodoId,
        request_id: &str,
    ) -> Result<(), AppError> {
        validate_request_id(request_id)?;
        let mut transaction = self.pool.begin().await?;
        lock_related_project(&mut transaction, todo_id).await?;
        let current = match store::lock_delete_target(transaction.as_mut(), todo_id).await? {
            DeleteTarget::Missing => return Err(AppError::NotFound),
            DeleteTarget::AlreadyDeleted(owner) => {
                if !actor.acts_for(&owner) {
                    return Err(AppError::NotFound);
                }
                transaction.commit().await?;
                return Ok(());
            }
            DeleteTarget::Active(todo) => todo,
        };
        if !store::can_see(transaction.as_mut(), todo_id, actor.uuid()).await? {
            return Err(AppError::NotFound);
        }
        if !authorization::can_delete_todo(actor, &current) {
            return Err(AppError::Denied {
                code: "not_todo_owner".into(),
                message:
                    "Only the todo's owner (who created it) or the owner's custodian can delete it."
                        .to_owned(),
            });
        }

        remember(transaction.as_mut(), actor, &[]).await?;
        let version = store::soft_delete_todo(
            transaction.as_mut(),
            todo_id,
            actor.uuid(),
            self.tombstone_retention,
        )
        .await?;
        let changes = json!({ "fields": ["deleted_at"], "version": version });
        store::insert_activity(
            transaction.as_mut(),
            actor,
            todo_id,
            TodoActivityKind::Deleted,
            request_id,
            &changes,
            self.audit_retention,
        )
        .await?;
        store::insert_audit_event(
            transaction.as_mut(),
            actor,
            "todo.deleted",
            "todo",
            todo_id.into_uuid(),
            request_id,
            &changes,
            self.audit_retention,
        )
        .await?;
        if current.should_notify_assigner() {
            enqueue_notification(
                transaction.as_mut(),
                actor,
                &current,
                "todo.deleted",
                request_id,
                json!({}),
                None,
            )
            .await?;
        }
        transaction.commit().await?;
        Ok(())
    }

    /// Lists append-only notes for one active todo the caller can see.
    pub async fn list_notes(
        &self,
        actor: &VerifiedActor,
        todo_id: TodoId,
        query: CollectionQuery,
    ) -> Result<Page<TodoNote>, AppError> {
        let limit = query.limit;
        let notes = store::list_notes(&self.pool, todo_id, actor.uuid(), query)
            .await?
            .ok_or(AppError::NotFound)?;
        Ok(Page::from_window(notes, limit, |note| {
            PageCursor::new(note.created_at, note.id.into_uuid())
        }))
    }

    /// Appends a todo note exactly once and notifies a delegating Silicon atomically.
    ///
    /// The note's author is always the caller: a custodian writing on its
    /// Silicon's todo writes as itself.
    pub async fn add_note(
        &self,
        actor: &VerifiedActor,
        todo_id: TodoId,
        request: TodoNoteCreate,
        idempotency_key: IdempotencyKey,
        request_id: &str,
    ) -> Result<MutationResponse, AppError> {
        validate_request_id(request_id)?;
        let path = format!("/todos/{todo_id}/notes");
        let fingerprint_input = note_fingerprint_input(&request);
        let mutation = mutation_identity(
            ADD_TODO_NOTE_OPERATION,
            path,
            idempotency_key,
            &fingerprint_input,
        )?;
        if let Some(response) = self.probe_replay(actor, &mutation).await? {
            return Ok(response);
        }
        let request = request.validate(&self.limits).map_err(validation_error)?;

        let mut transaction = self.pool.begin().await?;
        lock_related_project(&mut transaction, todo_id).await?;
        store::lock_idempotency_scope(transaction.as_mut(), actor, &mutation).await?;
        if let Some(response) = replay_on_connection(transaction.as_mut(), actor, &mutation).await?
        {
            transaction.commit().await?;
            return Ok(response);
        }
        let todo = store::lock_todo(transaction.as_mut(), todo_id)
            .await?
            .ok_or(AppError::NotFound)?;
        if !store::can_see(transaction.as_mut(), todo_id, actor.uuid()).await? {
            return Err(AppError::NotFound);
        }
        authorize_note(actor, &todo)?;
        remember(transaction.as_mut(), actor, &[]).await?;

        let note_id = crate::domain::TodoNoteId::new();
        let note = store::insert_note(
            transaction.as_mut(),
            todo_id,
            note_id,
            &actor.actor,
            &request.body,
        )
        .await?;
        let changes = json!({ "note_id": note_id, "fields": ["body"] });
        store::insert_activity(
            transaction.as_mut(),
            actor,
            todo_id,
            TodoActivityKind::NoteAdded,
            request_id,
            &changes,
            self.audit_retention,
        )
        .await?;
        store::insert_audit_event(
            transaction.as_mut(),
            actor,
            "todo.note_added",
            "todo_note",
            note_id.into_uuid(),
            request_id,
            &changes,
            self.audit_retention,
        )
        .await?;
        if todo.should_notify_assigner() {
            enqueue_notification(
                transaction.as_mut(),
                actor,
                &todo,
                "todo.note_added",
                request_id,
                json!({ "note_id": note_id }),
                None,
            )
            .await?;
        }

        let response = mutation_response(201, &note)?;
        store::insert_idempotency_record(
            transaction.as_mut(),
            actor,
            todo_id,
            &mutation,
            &response,
            self.idempotency_ttl,
        )
        .await?;
        transaction.commit().await?;
        Ok(response)
    }

    async fn probe_replay(
        &self,
        actor: &VerifiedActor,
        mutation: &MutationIdentity,
    ) -> Result<Option<MutationResponse>, AppError> {
        let mut transaction = self.pool.begin().await?;
        store::lock_idempotency_scope(transaction.as_mut(), actor, mutation).await?;
        let response = replay_on_connection(transaction.as_mut(), actor, mutation).await?;
        transaction.commit().await?;
        Ok(response)
    }

    /// Resolves an assignee named by `c:`/`si:` id or uuid. The caller itself needs no lookup.
    async fn resolve_assignee(
        &self,
        caller: &VerifiedActor,
        id: &ActorId,
    ) -> Result<ResolvedAccount, AppError> {
        if id.as_str() == caller.uuid().as_str()
            || (!caller.actor.id.as_str().is_empty()
                && id.as_str().eq_ignore_ascii_case(caller.actor.id.as_str()))
        {
            return Ok(ResolvedAccount::known(
                caller.actor.clone(),
                caller.custodian.clone(),
            ));
        }
        self.identity_provider
            .resolve_account(id, None)
            .await
            .map_err(|error| account_field_error("assigned_to", error))
    }
}

struct DesiredTodo {
    project_id: Option<ProjectId>,
    title: RequiredText,
    description: Option<LimitedText>,
    assigned_to: Actor,
    status: TodoStatus,
    attachments: Vec<AttachmentUrl>,
    replace_attachments: bool,
    changed_fields: Vec<&'static str>,
    activity_kind: TodoActivityKind,
}

impl DesiredTodo {
    fn from_patch(
        current: &Todo,
        patch: ValidatedTodoPatch,
        resolved_assignee: Option<ResolvedAccount>,
    ) -> Self {
        let replace_attachments = patch.attachments.is_some();
        let title = patch.title.unwrap_or_else(|| current.title.clone());
        let description = match patch.description {
            NullablePatch::Absent => current.description.clone(),
            NullablePatch::Null => None,
            NullablePatch::Value(description) => Some(description),
        };
        let assigned_to =
            resolved_assignee.map_or_else(|| current.assigned_to.clone(), |account| account.actor);
        let status = patch.status.unwrap_or(current.status);
        let attachments = patch
            .attachments
            .unwrap_or_else(|| current.attachments.clone());

        let mut changed_fields = Vec::with_capacity(5);
        if title != current.title {
            changed_fields.push("title");
        }
        if description != current.description {
            changed_fields.push("description");
        }
        if assigned_to.uuid != current.assigned_to.uuid {
            changed_fields.push("assigned_to");
        }
        if status != current.status {
            changed_fields.push("status");
        }
        if attachments != current.attachments {
            changed_fields.push("attachments");
        }
        let activity_kind = if changed_fields.contains(&"assigned_to") {
            TodoActivityKind::Reassigned
        } else if changed_fields.contains(&"status") {
            TodoActivityKind::StatusChanged
        } else {
            TodoActivityKind::Updated
        };

        let project_id = match patch.project_id {
            NullablePatch::Absent => current.project_id,
            NullablePatch::Null => None,
            NullablePatch::Value(id) => Some(id),
        };
        if project_id != current.project_id {
            changed_fields.push("project_id");
        }
        Self {
            project_id,
            title,
            description,
            assigned_to,
            status,
            attachments,
            replace_attachments,
            changed_fields,
            activity_kind,
        }
    }
}

async fn replay_on_connection(
    connection: &mut PgConnection,
    actor: &VerifiedActor,
    mutation: &MutationIdentity,
) -> Result<Option<MutationResponse>, AppError> {
    let Some(stored) = store::load_idempotency_record(connection, actor, mutation).await? else {
        return Ok(None);
    };
    if stored.request_fingerprint.as_slice() != mutation.fingerprint.as_bytes() {
        return Err(AppError::Conflict {
            code: Cow::Borrowed("idempotency_key_reused"),
        });
    }
    let id = mutation
        .resource_path
        .strip_prefix("/todos/")
        .and_then(|p| p.split('/').next())
        .and_then(|v| v.parse::<TodoId>().ok())
        .or_else(|| {
            stored
                .response
                .body
                .get("id")
                .and_then(Value::as_str)
                .and_then(|v| v.parse::<TodoId>().ok())
        });
    // A stored response is served again only while its live todo is still visible to the
    // caller; once the todo is deleted, the caller's own historical response stays replayable.
    if let Some(id) = id {
        let live: Option<bool> =
            sqlx::query_scalar("SELECT deleted_at IS NULL FROM commit.todos WHERE id = $1")
                .bind(id.into_uuid())
                .fetch_optional(&mut *connection)
                .await?;
        if live == Some(true) && !store::can_see(connection, id, actor.uuid()).await? {
            return Err(AppError::NotFound);
        }
    }
    Ok(Some(stored.response))
}

fn authorize_patch(
    actor: &VerifiedActor,
    todo: &Todo,
    patch: &ValidatedTodoPatch,
) -> Result<(), AppError> {
    if authorization::can_patch_todo(actor, todo, patch) {
        return Ok(());
    }
    Err(AppError::Denied {
        code: "todo_change_not_allowed".into(),
        message: "Only the todo's owner (or the owner's custodian) can change its title, description, assignee, attachments or project; the assignee (or its custodian) can also change its status.".to_owned(),
    })
}

fn authorize_note(actor: &VerifiedActor, todo: &Todo) -> Result<(), AppError> {
    if authorization::can_add_todo_note(actor, todo) {
        return Ok(());
    }
    Err(AppError::Denied {
        code: "todo_note_not_allowed".into(),
        message: "Only the todo's owner, its assignee, or their custodians can add notes."
            .to_owned(),
    })
}

/// Queues a webhook to the Silicon that delegated `todo`, when its rules ask for this change.
pub(crate) async fn enqueue_notification(
    connection: &mut PgConnection,
    actor: &VerifiedActor,
    todo: &Todo,
    event_type: &'static str,
    request_id: &str,
    details: Value,
    resulting_status: Option<TodoStatus>,
) -> Result<(), AppError> {
    let Some(routing) =
        notification_store::effective_routing_snapshot(connection, todo, resulting_status).await?
    else {
        return Ok(());
    };
    let event_id = Uuid::now_v7();
    let mut payload = json!({
        "event_id": event_id,
        "todo_id": todo.id,
        "event_type": event_type,
        "silicon": todo.assigned_by.public_ref(),
        "actor": actor.actor.public_ref(),
        "request_id": request_id,
        "details": details,
    });
    if let (Some(app), Value::Object(fields)) = (actor.via_app(), &mut payload) {
        fields.insert("via_app".to_owned(), Value::String(app.to_owned()));
    }
    store::insert_outbox_event(
        connection,
        &NewOutboxEvent {
            id: event_id,
            todo_id: todo.id,
            recipient_silicon: &todo.assigned_by.uuid,
            event_type,
            payload: &payload,
            routing: &routing,
        },
    )
    .await
}

fn update_event_type(changed_fields: &[&str]) -> &'static str {
    if changed_fields.contains(&"assigned_to") {
        "todo.reassigned"
    } else if changed_fields.contains(&"status") {
        "todo.status_changed"
    } else {
        "todo.updated"
    }
}

fn mutation_identity<T: serde::Serialize>(
    operation: &'static str,
    path: impl Into<String>,
    key: IdempotencyKey,
    input: &T,
) -> Result<MutationIdentity, AppError> {
    MutationIdentity::new(operation, path, key, input)
        .map_err(|error| AppError::Internal(error.into()))
}

fn mutation_response<T: serde::Serialize>(
    status: u16,
    value: &T,
) -> Result<MutationResponse, AppError> {
    serde_json::to_value(value)
        .map(|body| MutationResponse::created(status, body))
        .map_err(|error| AppError::Internal(error.into()))
}

fn create_fingerprint_input(request: &TodoCreate) -> Value {
    json!({
        "project_id": request.project_id,
        "title": request.title,
        "description": request.description,
        "assigned_to": request.assigned_to,
        "status": request.status,
        "attachments": request.attachments,
    })
}

fn note_fingerprint_input(request: &TodoNoteCreate) -> Value {
    json!({ "body": request.body })
}

fn validation_error(error: ValidationError) -> AppError {
    let mut details = Map::new();
    details.insert(
        error.field.to_owned(),
        Value::String(error.kind.to_string()),
    );
    AppError::Validation {
        details: Value::Object(details),
    }
}

fn validate_request_id(request_id: &str) -> Result<(), AppError> {
    if request_id.is_empty()
        || request_id.len() > 255
        || request_id.trim() != request_id
        || request_id.chars().any(char::is_control)
    {
        return Err(AppError::BadRequest {
            code: Cow::Borrowed("invalid_request_id"),
        });
    }
    Ok(())
}

/// Locks the todo's project (when it has one) so visibility cannot change mid-mutation.
async fn lock_related_project(
    connection: &mut PgConnection,
    todo_id: TodoId,
) -> Result<(), AppError> {
    let project: Option<Uuid> =
        sqlx::query_scalar("SELECT project_id FROM commit.todos WHERE id = $1")
            .bind(todo_id.into_uuid())
            .fetch_optional(&mut *connection)
            .await?
            .flatten();
    if let Some(id) = project {
        project_store::lock_project(connection, &ProjectLocator::Id(ProjectId::from_uuid(id)))
            .await?;
    }
    Ok(())
}

/// Linking work to a project needs the right to change that project.
async fn authorize_link(
    connection: &mut PgConnection,
    actor: &VerifiedActor,
    project_id: ProjectId,
) -> Result<(), AppError> {
    project_store::lock_project(connection, &ProjectLocator::Id(project_id))
        .await?
        .ok_or(AppError::NotFound)?;
    if !project_store::can_write(connection, actor.uuid(), project_id).await? {
        if project_store::can_read(connection, actor.uuid(), project_id).await? {
            return Err(AppError::Denied {
                code: "project_not_writable".into(),
                message: "Only the project's members (and the custodians of member Silicons) can add work to it.".to_owned(),
            });
        }
        return Err(AppError::NotFound);
    }
    Ok(())
}

/// Assigning project work shares the project with the assignee (the project row is locked).
async fn share_project(
    connection: &mut PgConnection,
    actor: &VerifiedActor,
    project_id: ProjectId,
    assignee: &Actor,
) -> Result<(), AppError> {
    project_store::add_member(connection, actor, project_id, &assignee.uuid).await
}

#[cfg(test)]
mod tests {
    use super::{update_event_type, validate_request_id};

    #[test]
    fn event_type_prefers_reassignment_then_status() {
        assert_eq!(
            update_event_type(&["status", "assigned_to"]),
            "todo.reassigned"
        );
        assert_eq!(update_event_type(&["status"]), "todo.status_changed");
        assert_eq!(update_event_type(&["title"]), "todo.updated");
    }

    #[test]
    fn request_id_validation_matches_storage_constraints() {
        assert!(validate_request_id("request-123").is_ok());
        assert!(validate_request_id("").is_err());
        assert!(validate_request_id(" leading").is_err());
        assert!(validate_request_id("line\nbreak").is_err());
    }
}
