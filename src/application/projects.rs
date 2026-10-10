//! Project application workflows.
//!
//! [`ProjectService`] is the use-case boundary used by the HTTP layer. It
//! validates domain commands, resolves members through Silicon Accounts,
//! applies authorization against locked project state, and commits each
//! mutation with its audit and (where contracted) replay record.
//!
//! Members (explicit shares) and the custodians of member Silicons read and
//! change a project; a project that is not private is also readable by its
//! owner's custodian circle. Adding a Silicon from outside the caller's circle
//! needs that Silicon's allow-list.

mod collaboration;

use std::{borrow::Cow, collections::HashSet, sync::Arc, time::Duration};

use serde_json::{Map, Value, json};
use sqlx::{PgConnection, PgPool};

use super::{
    accounts::{account_field_error, ensure_reachable, remember},
    idempotency::{IdempotencyKey, MutationIdentity, MutationResponse},
    ports::{IdentityProvider, ResolvedAccount, VerifiedActor},
};
use crate::{
    domain::{
        ActorId, ActorType, BlockerCreate, CollectionQuery, Diary, DiaryUpdate,
        ExpectedDiaryVersion, Page, PageCursor, Project, ProjectCompletionCreate, ProjectCreate,
        ProjectEntry, ProjectEntryId, ProjectEntryType, ProjectLocator, ProjectPage, ProjectPatch,
        ProjectQuery, ProjectTask, ProjectTaskCreate, ProjectTaskId, ProjectTaskPatch, ProjectUid,
        ProjectUpdateCreate, ValidatedProjectEntryCreate, ValidationError,
    },
    error::AppError,
    infrastructure::postgres::projects as postgres,
};

/// Project use cases backed by PostgreSQL and Silicon Accounts.
#[derive(Clone)]
pub struct ProjectService {
    pool: PgPool,
    identity_provider: Arc<dyn IdentityProvider>,
    limits: crate::domain::DomainLimits,
    idempotency_ttl: Duration,
    audit_retention: Duration,
}

impl ProjectService {
    /// Creates the project use-case boundary.
    #[must_use]
    pub fn new(
        pool: PgPool,
        identity_provider: Arc<dyn IdentityProvider>,
        limits: crate::domain::DomainLimits,
        idempotency_ttl: Duration,
        audit_retention: Duration,
    ) -> Self {
        Self {
            pool,
            identity_provider,
            limits,
            idempotency_ttl,
            audit_retention,
        }
    }

    /// Lists the projects the caller can read, in stable keyset order.
    ///
    /// # Errors
    ///
    /// Returns an internal error when the read fails.
    pub async fn list_projects(
        &self,
        actor: &VerifiedActor,
        query: ProjectQuery,
    ) -> Result<ProjectPage, AppError> {
        let limit = usize::from(query.limit.get());
        let mut projects = postgres::list_projects(&self.pool, actor, &query).await?;
        let has_next_page = projects.len() > limit;
        if has_next_page {
            projects.truncate(limit);
        }
        let next_cursor = if has_next_page {
            projects.last().map(|project| {
                crate::domain::PageCursor::new(project.created_at, project.id.into_uuid())
            })
        } else {
            None
        };

        Ok(ProjectPage::new(projects, next_cursor))
    }

    /// Gets one readable project by UUID or exact UID.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::NotFound`] when the project does not exist or the
    /// caller cannot read it, or an internal error when persistence fails.
    pub async fn get_project(
        &self,
        actor: &VerifiedActor,
        locator: &ProjectLocator,
    ) -> Result<Project, AppError> {
        postgres::get_project(&self.pool, locator, actor.uuid())
            .await?
            .ok_or(AppError::NotFound)
    }

    /// Creates a Silicon-owned project exactly once for an idempotency scope.
    ///
    /// # Errors
    ///
    /// Returns a validation, authorization, provider, replay-conflict, or
    /// persistence error when the corresponding check cannot be satisfied.
    pub async fn create_project(
        &self,
        actor: &VerifiedActor,
        request: ProjectCreate,
        idempotency_key: IdempotencyKey,
        request_id: &str,
    ) -> Result<MutationResponse, AppError> {
        validate_request_id(request_id)?;
        let identity = mutation_identity(
            "createProject",
            "/projects",
            idempotency_key,
            &project_create_fingerprint(&request),
        )?;
        if let Some(response) = self.probe_replay(actor, &identity).await? {
            return Ok(response);
        }

        let command = request
            .validate(&self.limits, &actor.actor)
            .map_err(validation_error)?;
        let participants = self
            .resolve_participants(actor, &command.silicon_ids, &command.details.carbon_ids)
            .await?;
        let seeds = self.prepare_seed_tasks(actor, &command.tasks).await?;

        let mut transaction = self.pool.begin().await?;
        if let Some(response) =
            postgres::acquire_idempotency(&mut transaction, actor, &identity).await?
        {
            transaction.commit().await?;
            return Ok(response);
        }
        remember(&mut transaction, actor, &participants).await?;
        for participant in &participants {
            ensure_reachable(&mut transaction, actor, &participant.actor, "silicon_ids").await?;
        }

        let created_at =
            postgres::next_project_created_at(&mut transaction, &actor.actor, &command.slug)
                .await?;
        let uid = ProjectUid::new(&command.slug, &actor.actor.id, created_at);
        let project_id = crate::domain::ProjectId::new();
        let mut project = postgres::insert_project(
            &mut transaction,
            actor,
            project_id,
            &command,
            &uid,
            created_at,
            &participants,
        )
        .await?;
        for (task_id, command, assignee) in seeds {
            postgres::insert_task(&mut transaction, actor, project_id, task_id, &command).await?;
            if let Some(assignee) = assignee {
                self.assign_task(
                    &mut transaction,
                    actor,
                    project_id,
                    task_id,
                    Some(&assignee),
                )
                .await?;
            }
        }

        postgres::insert_audit(
            &mut transaction,
            actor,
            "project.created",
            "project",
            project_id.into_uuid(),
            request_id,
            json!({ "participant_count": participants.len() }),
            self.audit_retention,
        )
        .await?;
        project = postgres::fetch_project_by_id(&mut transaction, project.id)
            .await?
            .ok_or(AppError::NotFound)?;
        let response = created_response(&project, 201)?;
        postgres::save_idempotency(
            &mut transaction,
            actor,
            &identity,
            &response,
            self.idempotency_ttl,
        )
        .await?;
        transaction.commit().await?;

        Ok(response)
    }

    /// Replaces supplied metadata and/or the current participant set.
    ///
    /// # Errors
    ///
    /// Returns not found or forbidden for an inaccessible project, validation
    /// or replay conflict for invalid input, and provider/persistence failures.
    pub async fn update_project(
        &self,
        actor: &VerifiedActor,
        locator: &ProjectLocator,
        request: ProjectPatch,
        idempotency_key: IdempotencyKey,
        request_id: &str,
    ) -> Result<MutationResponse, AppError> {
        validate_request_id(request_id)?;
        let path = format!("/projects/{}", locator_value(locator));
        let identity = mutation_identity(
            "updateProject",
            path,
            idempotency_key,
            &project_patch_fingerprint(&request),
        )?;
        if let Some(response) = self.probe_replay(actor, &identity).await? {
            return Ok(response);
        }

        let command = request.validate(&self.limits).map_err(validation_error)?;
        let snapshot = self.get_project(actor, locator).await?;
        ensure_project_remains_terminal(snapshot.status, &command)?;
        let (participants, newly_resolved) =
            if command.silicon_ids.is_some() || command.carbon_ids.is_some() {
                let mut newly_resolved = Vec::new();
                let mut members = Vec::new();
                for (requested, kind) in [
                    (command.silicon_ids.as_ref(), ActorType::Silicon),
                    (command.carbon_ids.as_ref(), ActorType::Carbon),
                ] {
                    if let Some(ids) = requested {
                        let resolved = self.resolve_members(actor, ids, kind).await?;
                        newly_resolved.extend(resolved.iter().cloned());
                        members.extend(resolved);
                    } else {
                        members.extend(
                            snapshot
                                .members
                                .iter()
                                .filter(|member| member.actor_type == kind)
                                .map(|member| ResolvedAccount::known(member.clone(), None)),
                        );
                    }
                }
                ensure_unique_members(&members)?;
                if !members
                    .iter()
                    .any(|member| member.actor.uuid == snapshot.owner.uuid)
                {
                    return Err(AppError::Validation {
                        details: json!({ "participants": format!(
                            "must keep the project owner {} as a member",
                            snapshot.owner.id
                        ) }),
                    });
                }
                (Some(members), newly_resolved)
            } else {
                (None, Vec::new())
            };

        let mut transaction = self.pool.begin().await?;
        if let Some(response) =
            postgres::acquire_idempotency(&mut transaction, actor, &identity).await?
        {
            transaction.commit().await?;
            return Ok(response);
        }
        let locked_project = self
            .authorize_locked_project(&mut transaction, actor, locator)
            .await?;
        ensure_project_remains_terminal(locked_project.status, &command)?;
        remember(&mut transaction, actor, &newly_resolved).await?;
        for member in &newly_resolved {
            if !snapshot.has_member(&member.actor.uuid) {
                ensure_reachable(&mut transaction, actor, &member.actor, "silicon_ids").await?;
            }
        }

        let _project = postgres::update_project(
            &mut transaction,
            actor,
            &locked_project,
            &command,
            participants.as_deref(),
        )
        .await?;
        let project_id = locked_project.id;
        postgres::insert_audit(
            &mut transaction,
            actor,
            "project.updated",
            "project",
            project_id.into_uuid(),
            request_id,
            json!({
                "name_changed": command.name.is_some(),
                "status_changed": command.status.is_some(),
                "visibility_changed": command.private.is_some_and(|private| private != locked_project.private),
                "participants_replaced": participants.is_some(),
                "participant_count": participants.as_ref().map(Vec::len),
            }),
            self.audit_retention,
        )
        .await?;
        let project = postgres::fetch_project_by_id(&mut transaction, project_id)
            .await?
            .ok_or(AppError::NotFound)?;
        let response = created_response(&project, 200)?;
        postgres::save_idempotency(
            &mut transaction,
            actor,
            &identity,
            &response,
            self.idempotency_ttl,
        )
        .await?;
        transaction.commit().await?;

        Ok(response)
    }

    /// Gets the complete current Markdown diary.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::NotFound`] for an unknown or unreadable project, or an
    /// internal error when the required diary cannot be loaded.
    pub async fn get_diary(
        &self,
        actor: &VerifiedActor,
        locator: &ProjectLocator,
    ) -> Result<Diary, AppError> {
        let project_id = self.get_project(actor, locator).await?.id;
        postgres::get_diary(&self.pool, project_id)
            .await?
            .ok_or_else(|| {
                AppError::Internal(anyhow::anyhow!("project exists without its required diary"))
            })
    }

    /// Replaces the whole diary only when `expected_version` is current.
    ///
    /// # Errors
    ///
    /// Returns not found/forbidden for inaccessible projects, conflict for a
    /// stale version, validation for an oversized diary, or persistence errors.
    pub async fn replace_diary(
        &self,
        actor: &VerifiedActor,
        locator: &ProjectLocator,
        request: DiaryUpdate,
        expected_version: ExpectedDiaryVersion,
        request_id: &str,
    ) -> Result<Diary, AppError> {
        validate_request_id(request_id)?;
        let command = request.validate().map_err(validation_error)?;
        let mut transaction = self.pool.begin().await?;
        let project_id = self
            .authorize_locked_project(&mut transaction, actor, locator)
            .await?
            .id;
        remember(&mut transaction, actor, &[]).await?;
        let current = postgres::lock_diary(&mut transaction, project_id)
            .await?
            .ok_or_else(|| {
                AppError::Internal(anyhow::anyhow!("project exists without its required diary"))
            })?;
        if !expected_version.matches(current.version) {
            return Err(AppError::Conflict {
                code: Cow::Borrowed("diary_version_mismatch"),
            });
        }

        let diary = postgres::replace_diary(
            &mut transaction,
            actor,
            project_id,
            current.version,
            &command,
        )
        .await?;
        postgres::insert_audit(
            &mut transaction,
            actor,
            "project.diary.updated",
            "project_diary",
            project_id.into_uuid(),
            request_id,
            json!({
                "from_version": current.version.get(),
                "to_version": diary.version.get(),
                "word_count": command.word_count,
            }),
            self.audit_retention,
        )
        .await?;
        transaction.commit().await?;

        Ok(diary)
    }

    /// Lists all tasks and subtasks of a project the caller can read.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::NotFound`] for an unknown project or an internal
    /// error when persistence cannot serve the task list.
    pub async fn list_tasks(
        &self,
        actor: &VerifiedActor,
        locator: &ProjectLocator,
        query: CollectionQuery,
    ) -> Result<Page<ProjectTask>, AppError> {
        let limit = query.limit;
        let project_id = self.get_project(actor, locator).await?.id;
        let tasks = postgres::list_tasks(&self.pool, project_id, query).await?;
        Ok(Page::from_window(tasks, limit, |task| {
            PageCursor::new(task.created_at, task.id.into_uuid())
        }))
    }

    /// Lists blockers, updates, and completion entries of a project the caller can read.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::NotFound`] for an unknown project or an internal
    /// error when persistence cannot serve the activity list.
    pub async fn list_entries(
        &self,
        actor: &VerifiedActor,
        locator: &ProjectLocator,
        query: CollectionQuery,
    ) -> Result<Page<ProjectEntry>, AppError> {
        let limit = query.limit;
        let project_id = self.get_project(actor, locator).await?.id;
        let entries = postgres::list_entries(&self.pool, project_id, query).await?;
        Ok(Page::from_window(entries, limit, |entry| {
            PageCursor::new(entry.created_at, entry.id.into_uuid())
        }))
    }

    /// Creates one project task or project-local subtask exactly once.
    ///
    /// # Errors
    ///
    /// Returns authorization, validation, parent-locality, replay-conflict, or
    /// persistence errors when the operation cannot commit.
    pub async fn create_task(
        &self,
        actor: &VerifiedActor,
        locator: &ProjectLocator,
        request: ProjectTaskCreate,
        idempotency_key: IdempotencyKey,
        request_id: &str,
    ) -> Result<MutationResponse, AppError> {
        validate_request_id(request_id)?;
        let path = format!("/projects/{}/tasks", locator_value(locator));
        let identity = mutation_identity(
            "createProjectTask",
            path,
            idempotency_key,
            &project_task_create_fingerprint(&request),
        )?;
        if let Some(response) = self.probe_replay(actor, &identity).await? {
            return Ok(response);
        }
        let command = request.validate(&self.limits).map_err(validation_error)?;

        let assignee = self
            .resolve_task_assignee(actor, command.assigned_to.as_ref())
            .await?;
        let mut transaction = self.pool.begin().await?;
        if let Some(response) =
            postgres::acquire_idempotency(&mut transaction, actor, &identity).await?
        {
            transaction.commit().await?;
            return Ok(response);
        }
        let project_id = self
            .authorize_locked_project(&mut transaction, actor, locator)
            .await?
            .id;
        remember(&mut transaction, actor, &[]).await?;
        if let Some(parent_task_id) = command.parent_task_id
            && !postgres::parent_task_exists(&mut transaction, project_id, parent_task_id).await?
        {
            return Err(field_validation(
                "parent_task_id",
                "must identify a task in the same project",
            ));
        }

        let task_id = ProjectTaskId::new();
        postgres::insert_task(&mut transaction, actor, project_id, task_id, &command).await?;
        if let Some(assignee) = &assignee {
            self.assign_task(&mut transaction, actor, project_id, task_id, Some(assignee))
                .await?;
        }
        let task = postgres::fetch_task_by_id(&mut transaction, project_id, task_id)
            .await?
            .ok_or(AppError::NotFound)?;
        let response = created_response(&task, 201)?;
        postgres::insert_audit(
            &mut transaction,
            actor,
            "project.task.created",
            "project_task",
            task_id.into_uuid(),
            request_id,
            json!({
                "project_id": project_id,
                "has_parent": command.parent_task_id.is_some(),
            }),
            self.audit_retention,
        )
        .await?;
        postgres::save_idempotency(
            &mut transaction,
            actor,
            &identity,
            &response,
            self.idempotency_ttl,
        )
        .await?;
        transaction.commit().await?;

        Ok(response)
    }

    /// Applies the contract's deliberately non-idempotent project-task patch.
    ///
    /// # Errors
    ///
    /// Returns not found/forbidden for inaccessible resources, validation for
    /// an invalid patch, or a persistence error when the transaction fails.
    pub async fn update_task(
        &self,
        actor: &VerifiedActor,
        locator: &ProjectLocator,
        task_id: ProjectTaskId,
        request: ProjectTaskPatch,
        request_id: &str,
    ) -> Result<ProjectTask, AppError> {
        validate_request_id(request_id)?;
        let command = request.validate(&self.limits).map_err(validation_error)?;
        let assignment = match &command.assigned_to {
            crate::domain::NullablePatch::Value(id) => {
                Some(self.resolve_task_assignee(actor, Some(id)).await?)
            }
            crate::domain::NullablePatch::Null => Some(None),
            crate::domain::NullablePatch::Absent => None,
        };
        let mut transaction = self.pool.begin().await?;
        let project_id = self
            .authorize_locked_project(&mut transaction, actor, locator)
            .await?
            .id;
        let previous = postgres::fetch_task_by_id(&mut transaction, project_id, task_id)
            .await?
            .ok_or(AppError::NotFound)?;
        remember(&mut transaction, actor, &[]).await?;
        if let Some(assignee) = &assignment {
            self.assign_task(
                &mut transaction,
                actor,
                project_id,
                task_id,
                assignee.as_ref(),
            )
            .await?;
        }
        let task = postgres::update_task(&mut transaction, project_id, task_id, &command)
            .await?
            .ok_or(AppError::NotFound)?;
        if let Some(todo_id) = task.todo_id {
            let todo = crate::infrastructure::postgres::todos::lock_todo(&mut transaction, todo_id)
                .await?
                .ok_or(AppError::NotFound)?;
            if todo.should_notify_assigner() {
                let status_changed = previous.status != task.status;
                let event = if previous.assigned_to != task.assigned_to {
                    "todo.reassigned"
                } else if status_changed {
                    "todo.status_changed"
                } else {
                    "todo.updated"
                };
                crate::application::todos::enqueue_notification(
                    &mut transaction,
                    actor,
                    &todo,
                    event,
                    request_id,
                    json!({"project_id":project_id,"task_id":task_id}),
                    status_changed.then_some(task.status),
                )
                .await?;
            }
        }
        postgres::insert_audit(
            &mut transaction,
            actor,
            "project.task.updated",
            "project_task",
            task_id.into_uuid(),
            request_id,
            json!({
                "project_id": project_id,
                "title_changed": command.title.is_some(),
                "description_changed": command.description.is_some(),
                "status_changed": previous.status != task.status,
                "assignment_changed": previous.assigned_to != task.assigned_to,
            }),
            self.audit_retention,
        )
        .await?;
        transaction.commit().await?;

        Ok(task)
    }

    /// Appends a blocker entry exactly once.
    ///
    /// # Errors
    ///
    /// Returns authorization, validation, replay-conflict, or persistence
    /// errors when the blocker cannot be appended.
    pub async fn create_blocker(
        &self,
        actor: &VerifiedActor,
        locator: &ProjectLocator,
        request: BlockerCreate,
        idempotency_key: IdempotencyKey,
        request_id: &str,
    ) -> Result<MutationResponse, AppError> {
        let fingerprint = blocker_fingerprint(&request);
        let (identity, replay) = self
            .prepare_entry_identity(
                actor,
                locator,
                idempotency_key,
                request_id,
                "createBlocker",
                "blockers",
                &fingerprint,
            )
            .await?;
        if let Some(response) = replay {
            return Ok(response);
        }
        let command = request.validate(&self.limits).map_err(validation_error)?;
        self.create_entry(
            actor,
            locator,
            command,
            identity,
            request_id,
            "project.blocker.created",
        )
        .await
    }

    /// Appends a milestone update exactly once.
    ///
    /// # Errors
    ///
    /// Returns authorization, validation, replay-conflict, or persistence
    /// errors when the update cannot be appended.
    pub async fn create_update(
        &self,
        actor: &VerifiedActor,
        locator: &ProjectLocator,
        request: ProjectUpdateCreate,
        idempotency_key: IdempotencyKey,
        request_id: &str,
    ) -> Result<MutationResponse, AppError> {
        let fingerprint = project_update_fingerprint(&request);
        let (identity, replay) = self
            .prepare_entry_identity(
                actor,
                locator,
                idempotency_key,
                request_id,
                "createProjectUpdate",
                "updates",
                &fingerprint,
            )
            .await?;
        if let Some(response) = replay {
            return Ok(response);
        }
        let command = request.validate(&self.limits).map_err(validation_error)?;
        self.create_entry(
            actor,
            locator,
            command,
            identity,
            request_id,
            "project.update.created",
        )
        .await
    }

    /// Atomically appends the one completion statement and completes the project.
    ///
    /// # Errors
    ///
    /// Returns conflict when a completion already exists, or authorization,
    /// validation, replay-conflict, and persistence errors as applicable.
    pub async fn complete_project(
        &self,
        actor: &VerifiedActor,
        locator: &ProjectLocator,
        request: ProjectCompletionCreate,
        idempotency_key: IdempotencyKey,
        request_id: &str,
    ) -> Result<MutationResponse, AppError> {
        let fingerprint = completion_fingerprint(&request);
        let (identity, replay) = self
            .prepare_entry_identity(
                actor,
                locator,
                idempotency_key,
                request_id,
                "completeProject",
                "completion",
                &fingerprint,
            )
            .await?;
        if let Some(response) = replay {
            return Ok(response);
        }
        let command = request.validate(&self.limits).map_err(validation_error)?;
        self.create_entry(
            actor,
            locator,
            command,
            identity,
            request_id,
            "project.completed",
        )
        .await
    }

    async fn create_entry(
        &self,
        actor: &VerifiedActor,
        locator: &ProjectLocator,
        command: ValidatedProjectEntryCreate,
        identity: MutationIdentity,
        request_id: &str,
        audit_action: &'static str,
    ) -> Result<MutationResponse, AppError> {
        let mut transaction = self.pool.begin().await?;
        if let Some(response) =
            postgres::acquire_idempotency(&mut transaction, actor, &identity).await?
        {
            transaction.commit().await?;
            return Ok(response);
        }
        let locked_project = self
            .authorize_locked_project(&mut transaction, actor, locator)
            .await?;
        let project_id = locked_project.id;
        remember(&mut transaction, actor, &[]).await?;
        if command.entry_type == ProjectEntryType::Completion
            && (locked_project.status == crate::domain::ProjectStatus::Completed
                || postgres::completion_exists(&mut transaction, project_id).await?)
        {
            return Err(AppError::Conflict {
                code: Cow::Borrowed("project_already_completed"),
            });
        }

        let entry_id = ProjectEntryId::new();
        let entry =
            postgres::insert_entry(&mut transaction, actor, project_id, entry_id, &command).await?;
        let response = created_response(&entry, 201)?;
        postgres::insert_audit(
            &mut transaction,
            actor,
            audit_action,
            "project_entry",
            entry_id.into_uuid(),
            request_id,
            json!({
                "project_id": project_id,
                "entry_type": command.entry_type,
            }),
            self.audit_retention,
        )
        .await?;
        postgres::save_idempotency(
            &mut transaction,
            actor,
            &identity,
            &response,
            self.idempotency_ttl,
        )
        .await?;
        transaction.commit().await?;

        Ok(response)
    }

    #[allow(clippy::too_many_arguments)]
    async fn prepare_entry_identity(
        &self,
        actor: &VerifiedActor,
        locator: &ProjectLocator,
        idempotency_key: IdempotencyKey,
        request_id: &str,
        operation: &'static str,
        path_suffix: &'static str,
        fingerprint: &Value,
    ) -> Result<(MutationIdentity, Option<MutationResponse>), AppError> {
        validate_request_id(request_id)?;
        let path = format!("/projects/{}/{}", locator_value(locator), path_suffix);
        let identity = mutation_identity(operation, path, idempotency_key, fingerprint)?;
        let replay = self.probe_replay(actor, &identity).await?;
        Ok((identity, replay))
    }

    /// Locks a project the caller may change. A readable project the caller may
    /// not change answers 403; an invisible one 404.
    pub(super) async fn authorize_locked_project(
        &self,
        connection: &mut PgConnection,
        actor: &VerifiedActor,
        locator: &ProjectLocator,
    ) -> Result<postgres::LockedProject, AppError> {
        let project = postgres::lock_project(connection, locator)
            .await?
            .ok_or(AppError::NotFound)?;
        if !postgres::can_write(connection, actor.uuid(), project.id).await? {
            if postgres::can_read(connection, actor.uuid(), project.id).await? {
                return Err(AppError::Denied {
                    code: "project_not_writable".into(),
                    message: "Only the project's members (and the custodians of member Silicons) can change it. Ask a member to add you.".to_owned(),
                });
            }
            return Err(AppError::NotFound);
        }
        Ok(project)
    }

    async fn probe_replay(
        &self,
        actor: &VerifiedActor,
        identity: &MutationIdentity,
    ) -> Result<Option<MutationResponse>, AppError> {
        let mut transaction = self.pool.begin().await?;
        let response = postgres::acquire_idempotency(&mut transaction, actor, identity).await?;
        transaction.commit().await?;
        Ok(response)
    }

    /// Resolves the Silicon and Carbon members a creator names (the caller is added by validation).
    async fn resolve_participants(
        &self,
        actor: &VerifiedActor,
        silicon_ids: &[ActorId],
        carbon_ids: &[ActorId],
    ) -> Result<Vec<ResolvedAccount>, AppError> {
        let mut members = self
            .resolve_members(actor, silicon_ids, ActorType::Silicon)
            .await?;
        members.extend(
            self.resolve_members(actor, carbon_ids, ActorType::Carbon)
                .await?,
        );
        ensure_unique_members(&members)?;
        Ok(members)
    }

    /// Resolves member ids of one kind; the caller itself needs no lookup.
    pub(super) async fn resolve_members(
        &self,
        actor: &VerifiedActor,
        ids: &[ActorId],
        kind: ActorType,
    ) -> Result<Vec<ResolvedAccount>, AppError> {
        let field = match kind {
            ActorType::Silicon => "silicon_ids",
            ActorType::Carbon => "carbon_ids",
        };
        let is_caller = |id: &ActorId| {
            actor.actor.actor_type == kind
                && (id.as_str() == actor.uuid().as_str()
                    || (!actor.actor.id.as_str().is_empty()
                        && id.as_str().eq_ignore_ascii_case(actor.actor.id.as_str())))
        };
        let lookup_ids = ids
            .iter()
            .filter(|id| !is_caller(id))
            .cloned()
            .collect::<Vec<_>>();
        let mut resolved = if lookup_ids.is_empty() {
            Vec::new()
        } else {
            self.identity_provider
                .resolve_accounts(&lookup_ids, Some(kind))
                .await
                .map_err(|error| account_field_error(field, error))?
        };
        if ids.iter().any(is_caller) {
            resolved.insert(
                0,
                ResolvedAccount::known(actor.actor.clone(), actor.custodian.clone()),
            );
        }
        Ok(resolved)
    }
}

/// Two ids (for example an old and a new id, or an id and a uuid) may name the same account.
fn ensure_unique_members(members: &[ResolvedAccount]) -> Result<(), AppError> {
    let mut seen = HashSet::with_capacity(members.len());
    for member in members {
        if !seen.insert(member.actor.uuid.clone()) {
            return Err(AppError::Validation {
                details: json!({ "participants": format!(
                    "{} is listed more than once (two ids or a uuid name the same account)",
                    member.actor.id
                ) }),
            });
        }
    }
    Ok(())
}

fn ensure_project_remains_terminal(
    current_status: crate::domain::ProjectStatus,
    command: &crate::domain::ValidatedProjectPatch,
) -> Result<(), AppError> {
    if current_status == crate::domain::ProjectStatus::Completed && command.status.is_some() {
        return Err(AppError::Conflict {
            code: Cow::Borrowed("project_already_completed"),
        });
    }

    Ok(())
}

fn mutation_identity(
    operation: &'static str,
    resource_path: impl Into<String>,
    key: IdempotencyKey,
    fingerprint_input: &Value,
) -> Result<MutationIdentity, AppError> {
    MutationIdentity::new(operation, resource_path, key, fingerprint_input).map_err(|error| {
        AppError::Internal(anyhow::anyhow!(
            "project request fingerprint serialization failed: {error}"
        ))
    })
}

fn created_response<T: serde::Serialize>(
    resource: &T,
    status: u16,
) -> Result<MutationResponse, AppError> {
    let body = serde_json::to_value(resource).map_err(|error| {
        AppError::Internal(anyhow::anyhow!(
            "project response serialization failed: {error}"
        ))
    })?;
    Ok(MutationResponse::created(status, body))
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

fn validation_error(error: ValidationError) -> AppError {
    let ValidationError { field, kind } = error;
    let mut details = Map::new();
    details.insert(field.to_owned(), Value::String(kind.to_string()));
    AppError::Validation {
        details: Value::Object(details),
    }
}

fn field_validation(field: &'static str, message: &'static str) -> AppError {
    let mut details = Map::new();
    details.insert(field.to_owned(), Value::String(message.to_owned()));
    AppError::Validation {
        details: Value::Object(details),
    }
}

fn locator_value(locator: &ProjectLocator) -> String {
    match locator {
        ProjectLocator::Id(id) => id.to_string(),
        ProjectLocator::Uid(uid) => uid.as_str().to_owned(),
    }
}

fn project_create_fingerprint(request: &ProjectCreate) -> Value {
    serde_json::to_value(request).unwrap_or(Value::Null)
}

fn project_patch_fingerprint(request: &ProjectPatch) -> Value {
    serde_json::to_value(request).unwrap_or(Value::Null)
}

fn project_task_create_fingerprint(request: &ProjectTaskCreate) -> Value {
    json!({
        "assigned_to": request.assigned_to,
        "parent_task_id": request.parent_task_id,
        "title": request.title,
        "description": request.description,
        "status": request.status,
    })
}

fn blocker_fingerprint(request: &BlockerCreate) -> Value {
    json!({
        "title": request.title,
        "description": request.description,
        "status": request.status,
    })
}

fn project_update_fingerprint(request: &ProjectUpdateCreate) -> Value {
    json!({
        "title": request.title,
        "description": request.description,
    })
}

fn completion_fingerprint(request: &ProjectCompletionCreate) -> Value {
    json!({
        "title": request.title,
        "description": request.description,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{project_patch_fingerprint, validate_request_id};
    use crate::domain::ProjectPatch;

    #[test]
    fn patch_fingerprint_distinguishes_absent_from_explicit_fields() {
        let absent = project_patch_fingerprint(&ProjectPatch::default());
        let present = project_patch_fingerprint(&ProjectPatch {
            name: Some("Launch".to_owned()),
            ..ProjectPatch::default()
        });
        assert_eq!(absent, json!({}));
        assert_ne!(absent, present);
    }

    #[test]
    fn audit_request_ids_must_be_bounded_visible_values() {
        assert!(validate_request_id("request-123").is_ok());
        assert!(validate_request_id("").is_err());
        assert!(validate_request_id(" request-123").is_err());
        assert!(validate_request_id("request\n123").is_err());
    }
}
