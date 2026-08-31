//! Todo application workflows.

use std::{borrow::Cow, sync::Arc, time::Duration};

use serde_json::{Map, Value, json};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::{
    application::{
        idempotency::{IdempotencyKey, MutationIdentity, MutationResponse},
        ports::{ActiveMember, IdentityProvider, InboundCredential, ProviderError, VerifiedActor},
    },
    domain::{
        Actor, ActorId, AttachmentUrlPolicy, CollectionQuery, DomainLimits, LimitedText,
        NullablePatch, Page, PageCursor, PermanentAttachmentUrl, RequiredText, Todo, TodoCreate,
        TodoId, TodoNote, TodoNoteCreate, TodoPage, TodoPatch, TodoQuery, TodoStatus,
        ValidatedTodoPatch, ValidationError,
    },
    error::AppError,
    infrastructure::postgres::todos::{
        self as store, DeleteTarget, NewTodo, TodoActivityKind, TodoReplacement,
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
    attachment_policy: AttachmentUrlPolicy,
    idempotency_ttl: Duration,
    audit_retention: Duration,
    tombstone_retention: Duration,
}

impl TodoService {
    /// Creates a todo service from its database and online IAM dependencies.
    #[must_use]
    pub fn new(
        pool: PgPool,
        identity_provider: Arc<dyn IdentityProvider>,
        limits: DomainLimits,
        attachment_policy: AttachmentUrlPolicy,
        idempotency_ttl: Duration,
        audit_retention: Duration,
        tombstone_retention: Duration,
    ) -> Self {
        Self {
            pool,
            identity_provider,
            limits,
            attachment_policy,
            idempotency_ttl,
            audit_retention,
            tombstone_retention,
        }
    }

    /// Lists active organization-visible todos using stable keyset pagination.
    pub async fn list(
        &self,
        actor: &VerifiedActor,
        query: TodoQuery,
    ) -> Result<TodoPage, AppError> {
        let created_at = query.created_at_range().map_err(validation_error)?;
        store::list_todos(&self.pool, actor, &query, created_at).await
    }

    /// Returns one active organization-visible todo.
    pub async fn get(&self, actor: &VerifiedActor, todo_id: TodoId) -> Result<Todo, AppError> {
        store::get_todo(&self.pool, actor.organization_id, todo_id)
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

        let request = request
            .validate(&self.limits, &self.attachment_policy)
            .map_err(validation_error)?;
        let assignee = self.resolve_assignee(actor, &request.assigned_to).await?;

        let mut transaction = self.pool.begin().await?;
        store::lock_idempotency_scope(transaction.as_mut(), actor, &mutation).await?;
        if let Some(response) = replay_on_connection(transaction.as_mut(), actor, &mutation).await?
        {
            transaction.commit().await?;
            return Ok(response);
        }

        store::upsert_verified_actor(transaction.as_mut(), actor).await?;
        store::upsert_active_member(transaction.as_mut(), &assignee).await?;
        let todo_id = TodoId::new();
        let new_todo = NewTodo {
            id: todo_id,
            organization_id: actor.organization_id,
            title: &request.title,
            description: request.description.as_ref(),
            assigned_by_principal_id: actor.actor.principal_id,
            assigned_to_principal_id: assignee.actor.principal_id,
            status: request.status,
            attachments: &request.attachments,
        };
        store::insert_todo(transaction.as_mut(), &new_todo).await?;

        let changes = json!({
            "fields": ["title", "description", "assigned_to", "status", "attachments"]
        });
        store::insert_activity(
            transaction.as_mut(),
            actor.organization_id,
            todo_id,
            TodoActivityKind::Created,
            actor.actor.principal_id,
            request_id,
            &changes,
            self.audit_retention,
        )
        .await?;
        store::insert_audit_event(
            transaction.as_mut(),
            actor.organization_id,
            actor.actor.principal_id,
            "todo.created",
            "todo",
            todo_id.into_uuid(),
            request_id,
            &changes,
            self.audit_retention,
        )
        .await?;

        let todo = store::lock_todo(transaction.as_mut(), actor.organization_id, todo_id)
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

        let patch = request
            .validate(&self.limits, &self.attachment_policy)
            .map_err(validation_error)?;
        let authorization_snapshot = store::get_todo(&self.pool, actor.organization_id, todo_id)
            .await?
            .ok_or(AppError::NotFound)?;
        authorize_patch(actor, &authorization_snapshot, &patch)?;
        let resolved_assignee = match patch.assigned_to.as_ref() {
            Some(assigned_to) => Some(self.resolve_assignee(actor, assigned_to).await?),
            None => None,
        };

        let mut transaction = self.pool.begin().await?;
        store::lock_idempotency_scope(transaction.as_mut(), actor, &mutation).await?;
        if let Some(response) = replay_on_connection(transaction.as_mut(), actor, &mutation).await?
        {
            transaction.commit().await?;
            return Ok(response);
        }

        let current = store::lock_todo(transaction.as_mut(), actor.organization_id, todo_id)
            .await?
            .ok_or(AppError::NotFound)?;
        authorize_patch(actor, &current, &patch)?;
        store::upsert_verified_actor(transaction.as_mut(), actor).await?;
        if let Some(assignee) = &resolved_assignee {
            store::upsert_active_member(transaction.as_mut(), assignee).await?;
        }

        let desired = DesiredTodo::from_patch(&current, patch, resolved_assignee);
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

        let replacement = TodoReplacement {
            title: &desired.title,
            description: desired.description.as_ref(),
            assigned_to_principal_id: desired.assigned_to.principal_id,
            status: desired.status,
            attachments: &desired.attachments,
            replace_attachments: desired.replace_attachments,
        };
        store::update_todo(
            transaction.as_mut(),
            actor.organization_id,
            todo_id,
            &replacement,
        )
        .await?;
        let updated = store::lock_todo(transaction.as_mut(), actor.organization_id, todo_id)
            .await?
            .ok_or_else(|| AppError::Internal(anyhow::anyhow!("updated todo could not be read")))?;

        let changes = json!({ "fields": desired.changed_fields });
        store::insert_activity(
            transaction.as_mut(),
            actor.organization_id,
            todo_id,
            desired.activity_kind,
            actor.actor.principal_id,
            request_id,
            &changes,
            self.audit_retention,
        )
        .await?;
        store::insert_audit_event(
            transaction.as_mut(),
            actor.organization_id,
            actor.actor.principal_id,
            "todo.updated",
            "todo",
            todo_id.into_uuid(),
            request_id,
            &changes,
            self.audit_retention,
        )
        .await?;
        if current.should_notify_assigner() || updated.should_notify_assigner() {
            let event_type = update_event_type(&desired.changed_fields);
            enqueue_notification(
                transaction.as_mut(),
                actor,
                &updated,
                event_type,
                request_id,
                json!({ "changed_fields": desired.changed_fields }),
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

    /// Soft-deletes one todo when the caller is its assigner or an explicit manager.
    pub async fn delete(
        &self,
        actor: &VerifiedActor,
        todo_id: TodoId,
        request_id: &str,
    ) -> Result<(), AppError> {
        validate_request_id(request_id)?;
        let mut transaction = self.pool.begin().await?;
        let current =
            match store::lock_delete_target(transaction.as_mut(), actor.organization_id, todo_id)
                .await?
            {
                DeleteTarget::Missing => return Err(AppError::NotFound),
                DeleteTarget::AlreadyDeleted(assigned_by_principal_id) => {
                    if actor.actor.principal_id != assigned_by_principal_id
                        && !actor.manages_todos()
                    {
                        return Err(AppError::Forbidden);
                    }
                    transaction.commit().await?;
                    return Ok(());
                }
                DeleteTarget::Active(todo) => todo,
            };
        if actor.actor.principal_id != current.assigned_by.principal_id && !actor.manages_todos() {
            return Err(AppError::Forbidden);
        }

        store::upsert_verified_actor(transaction.as_mut(), actor).await?;
        let version = store::soft_delete_todo(
            transaction.as_mut(),
            actor.organization_id,
            todo_id,
            actor.actor.principal_id,
            self.tombstone_retention,
        )
        .await?;
        let changes = json!({ "fields": ["deleted_at"], "version": version });
        store::insert_activity(
            transaction.as_mut(),
            actor.organization_id,
            todo_id,
            TodoActivityKind::Deleted,
            actor.actor.principal_id,
            request_id,
            &changes,
            self.audit_retention,
        )
        .await?;
        store::insert_audit_event(
            transaction.as_mut(),
            actor.organization_id,
            actor.actor.principal_id,
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
            )
            .await?;
        }
        transaction.commit().await?;
        Ok(())
    }

    /// Lists append-only notes for one active organization-visible todo.
    pub async fn list_notes(
        &self,
        actor: &VerifiedActor,
        todo_id: TodoId,
        query: CollectionQuery,
    ) -> Result<Page<TodoNote>, AppError> {
        let limit = query.limit;
        let notes = store::list_notes(&self.pool, actor.organization_id, todo_id, query)
            .await?
            .ok_or(AppError::NotFound)?;
        Ok(Page::from_window(notes, limit, |note| {
            PageCursor::new(note.created_at, note.id.into_uuid())
        }))
    }

    /// Appends a todo note exactly once and notifies a delegating Silicon atomically.
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
        store::lock_idempotency_scope(transaction.as_mut(), actor, &mutation).await?;
        if let Some(response) = replay_on_connection(transaction.as_mut(), actor, &mutation).await?
        {
            transaction.commit().await?;
            return Ok(response);
        }
        let todo = store::lock_todo(transaction.as_mut(), actor.organization_id, todo_id)
            .await?
            .ok_or(AppError::NotFound)?;
        authorize_note(actor, &todo)?;
        store::upsert_verified_actor(transaction.as_mut(), actor).await?;

        let note_id = crate::domain::TodoNoteId::new();
        let note = store::insert_note(
            transaction.as_mut(),
            actor.organization_id,
            todo_id,
            note_id,
            &actor.actor,
            &request.body,
        )
        .await?;
        let changes = json!({ "note_id": note_id, "fields": ["body"] });
        store::insert_activity(
            transaction.as_mut(),
            actor.organization_id,
            todo_id,
            TodoActivityKind::NoteAdded,
            actor.actor.principal_id,
            request_id,
            &changes,
            self.audit_retention,
        )
        .await?;
        store::insert_audit_event(
            transaction.as_mut(),
            actor.organization_id,
            actor.actor.principal_id,
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

    async fn resolve_assignee(
        &self,
        caller: &VerifiedActor,
        actor_id: &ActorId,
    ) -> Result<ActiveMember, AppError> {
        let resolved = self
            .identity_provider
            .resolve_active_member(&caller.org_id, actor_id, None)
            .await;
        let member = match resolved {
            Ok(member) => member,
            Err(ProviderError::NotFound)
                if caller.actor.id == *actor_id
                    && matches!(caller.grant(), InboundCredential::Trusted(_)) =>
            {
                // The non-production trusted-header provider has no configured
                // directory by default. Its authenticated caller is still a
                // known member, but only after the provider had an opportunity
                // to reject a configured public-ID collision.
                ActiveMember {
                    organization_id: caller.organization_id,
                    org_id: caller.org_id.clone(),
                    membership_id: caller.membership_id,
                    actor: caller.actor.clone(),
                }
            }
            Err(ProviderError::NotFound) if caller.actor.id == *actor_id => {
                return Err(AppError::BadGateway);
            }
            Err(error) => return Err(map_assignee_provider_error(error)),
        };

        validate_resolved_assignee(caller, actor_id, member)
    }
}

fn validate_resolved_assignee(
    caller: &VerifiedActor,
    requested_actor_id: &ActorId,
    member: ActiveMember,
) -> Result<ActiveMember, AppError> {
    if member.organization_id != caller.organization_id
        || member.org_id != caller.org_id
        || member.actor.id != *requested_actor_id
        || member.organization_id.as_uuid().is_nil()
        || member.membership_id.is_nil()
        || member.actor.principal_id.as_uuid().is_nil()
    {
        return Err(AppError::BadGateway);
    }

    if caller.actor.id == *requested_actor_id
        && (member.membership_id != caller.membership_id || member.actor != caller.actor)
    {
        // The caller itself is one known match. A different member returned for
        // the same untyped public ID proves that the request is ambiguous.
        return Err(AppError::BadGateway);
    }

    Ok(member)
}

struct DesiredTodo {
    title: RequiredText,
    description: Option<LimitedText>,
    assigned_to: Actor,
    status: TodoStatus,
    attachments: Vec<PermanentAttachmentUrl>,
    replace_attachments: bool,
    changed_fields: Vec<&'static str>,
    activity_kind: TodoActivityKind,
}

impl DesiredTodo {
    fn from_patch(
        current: &Todo,
        patch: ValidatedTodoPatch,
        resolved_assignee: Option<ActiveMember>,
    ) -> Self {
        let replace_attachments = patch.attachments.is_some();
        let title = patch.title.unwrap_or_else(|| current.title.clone());
        let description = match patch.description {
            NullablePatch::Absent => current.description.clone(),
            NullablePatch::Null => None,
            NullablePatch::Value(description) => Some(description),
        };
        let assigned_to =
            resolved_assignee.map_or_else(|| current.assigned_to.clone(), |member| member.actor);
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
        if assigned_to.principal_id != current.assigned_to.principal_id {
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

        Self {
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
    Ok(Some(stored.response))
}

fn authorize_patch(
    actor: &VerifiedActor,
    todo: &Todo,
    patch: &ValidatedTodoPatch,
) -> Result<(), AppError> {
    let changes_content = patch.title.is_some()
        || !patch.description.is_absent()
        || patch.assigned_to.is_some()
        || patch.attachments.is_some();
    let changes_status = patch.status.is_some();
    let is_assigner = actor.actor.principal_id == todo.assigned_by.principal_id;
    let is_assignee = actor.actor.principal_id == todo.assigned_to.principal_id;
    let manages_todos = actor.manages_todos();

    if (changes_content && !(is_assigner || manages_todos))
        || (changes_status && !(is_assigner || is_assignee || manages_todos))
    {
        return Err(AppError::Forbidden);
    }
    Ok(())
}

fn authorize_note(actor: &VerifiedActor, todo: &Todo) -> Result<(), AppError> {
    let is_assigner = actor.actor.principal_id == todo.assigned_by.principal_id;
    let is_assignee = actor.actor.principal_id == todo.assigned_to.principal_id;
    if is_assigner || is_assignee || actor.manages_todos() {
        Ok(())
    } else {
        Err(AppError::Forbidden)
    }
}

async fn enqueue_notification(
    connection: &mut PgConnection,
    actor: &VerifiedActor,
    todo: &Todo,
    event_type: &'static str,
    request_id: &str,
    details: Value,
) -> Result<(), AppError> {
    let event_id = Uuid::now_v7();
    let payload = json!({
        "event_id": event_id,
        "todo_id": todo.id,
        "org_id": todo.org_id,
        "event_type": event_type,
        "actor": actor.actor.public_ref(),
        "request_id": request_id,
        "details": details,
    });
    store::insert_outbox_event(
        connection,
        event_id,
        actor.organization_id,
        todo.id,
        todo.assigned_by.principal_id,
        event_type,
        &payload,
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

fn map_assignee_provider_error(error: ProviderError) -> AppError {
    match error {
        ProviderError::NotFound => AppError::Validation {
            details: json!({
                "assigned_to": "Must identify a current Carbon or Silicon in this organization."
            }),
        },
        ProviderError::RateLimited { retry_after } => AppError::RateLimited {
            retry_after_seconds: retry_after.map_or(1, |duration| duration.as_secs().max(1)),
        },
        ProviderError::InvalidResponse => AppError::BadGateway,
        ProviderError::Unavailable | ProviderError::Unauthenticated | ProviderError::Forbidden => {
            AppError::ProviderUnavailable
        }
        ProviderError::Conflict => AppError::Conflict {
            code: Cow::Borrowed("assignee_resolution_conflict"),
        },
    }
}

#[cfg(test)]
mod tests {
    use secrecy::SecretString;
    use uuid::Uuid;

    use super::{
        authorize_note, authorize_patch, update_event_type, validate_request_id,
        validate_resolved_assignee,
    };
    use crate::{
        application::ports::{
            ActiveMember, CapabilitySet, InboundCredential, OrganizationRole,
            TODO_MANAGE_CAPABILITY, VerifiedActor,
        },
        domain::{
            Actor, ActorId, ActorType, OrganizationId, PrincipalId, PublicOrganizationId, Todo,
            TodoId, TodoStatus, ValidatedTodoPatch,
        },
        error::AppError,
    };

    fn actor(principal: u128, public_id: &str) -> Option<Actor> {
        Some(Actor::new(
            PrincipalId::from_uuid(Uuid::from_u128(principal)),
            ActorType::Carbon,
            ActorId::new(public_id).ok()?,
        ))
    }

    fn verified(principal: u128, capabilities: CapabilitySet) -> Option<VerifiedActor> {
        let actor = actor(principal, &format!("actor-{principal}"))?;
        Some(VerifiedActor::new(
            OrganizationId::from_uuid(Uuid::from_u128(100)),
            PublicOrganizationId::new("test-org").ok()?,
            Uuid::from_u128(principal.saturating_add(1_000)),
            actor,
            OrganizationRole::Member,
            capabilities,
            InboundCredential::Bearer(SecretString::from("test-token".to_owned())),
        ))
    }

    fn todo() -> Option<Todo> {
        Some(Todo {
            id: TodoId::new(),
            organization_id: OrganizationId::from_uuid(Uuid::from_u128(100)),
            org_id: PublicOrganizationId::new("test-org").ok()?,
            title: crate::domain::RequiredText::new("title", "Ship", 500).ok()?,
            description: None,
            assigned_by: actor(1, "assigner")?,
            assigned_to: actor(2, "assignee")?,
            status: TodoStatus::YetToDo,
            attachments: Vec::new(),
            created_at: time::OffsetDateTime::UNIX_EPOCH,
            updated_at: time::OffsetDateTime::UNIX_EPOCH,
        })
    }

    #[test]
    fn assignee_may_change_status_but_not_content() {
        let Some(todo) = todo() else {
            return;
        };
        let Some(assignee) = verified(2, CapabilitySet::default()) else {
            return;
        };
        let status_patch = ValidatedTodoPatch {
            status: Some(TodoStatus::Blocked),
            ..ValidatedTodoPatch::default()
        };
        assert!(authorize_patch(&assignee, &todo, &status_patch).is_ok());

        let title = crate::domain::RequiredText::new("title", "Changed", 500);
        let Ok(title) = title else {
            return;
        };
        let content_patch = ValidatedTodoPatch {
            title: Some(title),
            ..ValidatedTodoPatch::default()
        };
        assert!(matches!(
            authorize_patch(&assignee, &todo, &content_patch),
            Err(AppError::Forbidden)
        ));
    }

    #[test]
    fn explicit_manage_capability_authorizes_every_patch_field() {
        let Some(todo) = todo() else {
            return;
        };
        let capabilities = CapabilitySet::try_from_names([TODO_MANAGE_CAPABILITY]);
        let Ok(capabilities) = capabilities else {
            return;
        };
        let Some(manager) = verified(3, capabilities) else {
            return;
        };
        let title = crate::domain::RequiredText::new("title", "Changed", 500);
        let Ok(title) = title else {
            return;
        };
        let patch = ValidatedTodoPatch {
            title: Some(title),
            status: Some(TodoStatus::Completed),
            ..ValidatedTodoPatch::default()
        };
        assert!(authorize_patch(&manager, &todo, &patch).is_ok());
    }

    #[test]
    fn notes_are_limited_to_involved_actors_or_explicit_managers() {
        let Some(todo) = todo() else {
            return;
        };
        let Some(assignee) = verified(2, CapabilitySet::default()) else {
            return;
        };
        let Some(bystander) = verified(3, CapabilitySet::default()) else {
            return;
        };
        let capabilities = CapabilitySet::try_from_names([TODO_MANAGE_CAPABILITY]);
        let Ok(capabilities) = capabilities else {
            return;
        };
        let Some(manager) = verified(4, capabilities) else {
            return;
        };

        assert!(authorize_note(&assignee, &todo).is_ok());
        assert!(matches!(
            authorize_note(&bystander, &todo),
            Err(AppError::Forbidden)
        ));
        assert!(authorize_note(&manager, &todo).is_ok());
    }

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

    #[test]
    fn self_assignment_rejects_a_different_principal_with_the_same_public_id() {
        let Some(caller) = verified(1, CapabilitySet::default()) else {
            return;
        };
        let collision = ActiveMember {
            organization_id: caller.organization_id,
            org_id: caller.org_id.clone(),
            membership_id: Uuid::from_u128(9_999),
            actor: Actor::new(
                PrincipalId::from_uuid(Uuid::from_u128(8_888)),
                ActorType::Silicon,
                caller.actor.id.clone(),
            ),
        };

        assert!(matches!(
            validate_resolved_assignee(&caller, &caller.actor.id, collision),
            Err(AppError::BadGateway)
        ));
    }
}
