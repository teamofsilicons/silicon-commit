//! Project application workflows.
//!
//! [`ProjectService`] is the use-case boundary used by the HTTP layer. It
//! validates domain commands, resolves current Silicon participants through
//! IAM, applies authorization against locked project state, and commits each
//! mutation with its audit and (where contracted) replay record.

use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use serde_json::{Map, Value, json};
use sqlx::{PgConnection, PgPool};

use super::{
    idempotency::{IdempotencyKey, MutationIdentity, MutationResponse},
    ports::{ActiveMember, IdentityProvider, ProviderError, VerifiedActor},
};
use crate::{
    domain::{
        ActorId, ActorType, BlockerCreate, CollectionQuery, Diary, DiaryUpdate,
        ExpectedDiaryVersion, Page, PageCursor, Project, ProjectCompletionCreate, ProjectCreate,
        ProjectEntryId, ProjectEntryType, ProjectLocator, ProjectPage, ProjectPatch, ProjectQuery,
        ProjectTask, ProjectTaskCreate, ProjectTaskId, ProjectTaskPatch, ProjectUid,
        ProjectUpdateCreate, ValidatedProjectEntryCreate, ValidationError,
    },
    error::AppError,
    infrastructure::postgres::projects as postgres,
};

/// Project use cases backed by PostgreSQL and Silicon IAM.
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

    /// Lists organization-visible projects in stable keyset order.
    ///
    /// # Errors
    ///
    /// Returns an internal error when the organization-qualified read fails.
    pub async fn list_projects(
        &self,
        actor: &VerifiedActor,
        query: ProjectQuery,
    ) -> Result<ProjectPage, AppError> {
        let limit = usize::from(query.limit.get());
        let mut projects =
            postgres::list_projects(&self.pool, actor.organization_id, &query).await?;
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

    /// Gets one organization-visible project by UUID or exact UID.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::NotFound`] when the locator is absent in the actor's
    /// organization, or an internal error when persistence fails.
    pub async fn get_project(
        &self,
        actor: &VerifiedActor,
        locator: &ProjectLocator,
    ) -> Result<Project, AppError> {
        postgres::get_project(&self.pool, actor.organization_id, locator)
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
        if !actor.actor.is_silicon() {
            return Err(AppError::Forbidden);
        }

        let command = request
            .validate(&self.limits, &actor.actor)
            .map_err(validation_error)?;
        let participants = self.resolve_silicons(actor, &command.silicon_ids).await?;

        let mut transaction = self.pool.begin().await?;
        if let Some(response) =
            postgres::acquire_idempotency(&mut transaction, actor, &identity).await?
        {
            transaction.commit().await?;
            return Ok(response);
        }
        postgres::upsert_verified_actor(&mut transaction, actor).await?;
        for participant in &participants {
            postgres::upsert_active_member(&mut transaction, participant).await?;
        }

        let created_at = postgres::next_project_created_at(
            &mut transaction,
            actor.organization_id,
            &actor.actor,
            &command.slug,
        )
        .await?;
        let uid = ProjectUid::new(&command.slug, &actor.actor.id, created_at);
        let project_id = crate::domain::ProjectId::new();
        let project = postgres::insert_project(
            &mut transaction,
            actor,
            project_id,
            &command,
            &uid,
            created_at,
            &participants,
        )
        .await?;
        let response = created_response(&project, 201)?;

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
        let authorization_snapshot = self.authorize_existing_project(actor, locator).await?;
        command
            .ensure_creator_participates(&authorization_snapshot.created_by.id)
            .map_err(validation_error)?;
        ensure_project_remains_terminal(authorization_snapshot.status, &command)?;
        let participants = match command.silicon_ids.as_ref() {
            Some(ids) => Some(self.resolve_silicons(actor, ids).await?),
            None => None,
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
        postgres::upsert_verified_actor(&mut transaction, actor).await?;
        if let Some(participants) = &participants {
            for participant in participants {
                postgres::upsert_active_member(&mut transaction, participant).await?;
            }
        }

        let project = postgres::update_project(
            &mut transaction,
            actor,
            locked_project,
            &command,
            participants.as_deref(),
        )
        .await?;
        let project_id = locked_project.id;
        let response = created_response(&project, 200)?;
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
                "participants_replaced": participants.is_some(),
                "participant_count": participants.as_ref().map(Vec::len),
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

    /// Gets the complete current Markdown diary.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::NotFound`] for an unknown organization-qualified
    /// project, or an internal error when the required diary cannot be loaded.
    pub async fn get_diary(
        &self,
        actor: &VerifiedActor,
        locator: &ProjectLocator,
    ) -> Result<Diary, AppError> {
        let mut connection = self.pool.acquire().await?;
        let project_id = postgres::find_project_id(&mut connection, actor.organization_id, locator)
            .await?
            .ok_or(AppError::NotFound)?;
        drop(connection);
        postgres::get_diary(&self.pool, actor.organization_id, project_id)
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
        postgres::upsert_verified_actor(&mut transaction, actor).await?;
        let current = postgres::lock_diary(&mut transaction, actor.organization_id, project_id)
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

    /// Lists all tasks and subtasks for an organization-visible project.
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
        let mut connection = self.pool.acquire().await?;
        let project_id = postgres::find_project_id(&mut connection, actor.organization_id, locator)
            .await?
            .ok_or(AppError::NotFound)?;
        drop(connection);
        let tasks =
            postgres::list_tasks(&self.pool, actor.organization_id, project_id, query).await?;
        Ok(Page::from_window(tasks, limit, |task| {
            PageCursor::new(task.created_at, task.id.into_uuid())
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
        postgres::upsert_verified_actor(&mut transaction, actor).await?;
        if let Some(parent_task_id) = command.parent_task_id
            && !postgres::parent_task_exists(
                &mut transaction,
                actor.organization_id,
                project_id,
                parent_task_id,
            )
            .await?
        {
            return Err(field_validation(
                "parent_task_id",
                "must identify a task in the same project",
            ));
        }

        let task_id = ProjectTaskId::new();
        let task =
            postgres::insert_task(&mut transaction, actor, project_id, task_id, &command).await?;
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
        let mut transaction = self.pool.begin().await?;
        let project_id = self
            .authorize_locked_project(&mut transaction, actor, locator)
            .await?
            .id;
        postgres::upsert_verified_actor(&mut transaction, actor).await?;
        let task = postgres::update_task(&mut transaction, actor, project_id, task_id, &command)
            .await?
            .ok_or(AppError::NotFound)?;
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
                "status_changed": command.status.is_some(),
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
        postgres::upsert_verified_actor(&mut transaction, actor).await?;
        if command.entry_type == ProjectEntryType::Completion
            && (locked_project.status == crate::domain::ProjectStatus::Completed
                || postgres::completion_exists(&mut transaction, actor.organization_id, project_id)
                    .await?)
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

    async fn authorize_existing_project(
        &self,
        actor: &VerifiedActor,
        locator: &ProjectLocator,
    ) -> Result<Project, AppError> {
        let project = postgres::get_project(&self.pool, actor.organization_id, locator)
            .await?
            .ok_or(AppError::NotFound)?;
        let participates = project.has_participant(&actor.actor);
        if !has_project_authority(actor, participates) {
            return Err(AppError::Forbidden);
        }
        Ok(project)
    }

    async fn authorize_locked_project(
        &self,
        connection: &mut PgConnection,
        actor: &VerifiedActor,
        locator: &ProjectLocator,
    ) -> Result<postgres::LockedProject, AppError> {
        let project = postgres::lock_project(connection, actor.organization_id, locator)
            .await?
            .ok_or(AppError::NotFound)?;
        let participates = postgres::is_active_participant(
            connection,
            actor.organization_id,
            project.id,
            &actor.actor,
        )
        .await?;
        if !has_project_authority(actor, participates) {
            return Err(AppError::Forbidden);
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

    async fn resolve_silicons(
        &self,
        actor: &VerifiedActor,
        silicon_ids: &[ActorId],
    ) -> Result<Vec<ActiveMember>, AppError> {
        let lookup_ids = silicon_ids
            .iter()
            .filter(|silicon_id| {
                actor.actor.actor_type != ActorType::Silicon || &actor.actor.id != *silicon_id
            })
            .cloned()
            .collect::<Vec<_>>();
        let resolved = if lookup_ids.is_empty() {
            Vec::new()
        } else {
            self.identity_provider
                .resolve_active_members(&actor.org_id, &lookup_ids, Some(ActorType::Silicon))
                .await
                .map_err(directory_error)?
        };
        if resolved.len() != lookup_ids.len() {
            return Err(AppError::BadGateway);
        }

        let expected_ids = lookup_ids.iter().cloned().collect::<HashSet<_>>();
        let mut resolved_by_id = HashMap::with_capacity(resolved.len());
        for member in resolved {
            if !expected_ids.contains(&member.actor.id)
                || !valid_resolved_silicon(actor, &member.actor.id, &member)
                || resolved_by_id
                    .insert(member.actor.id.clone(), member)
                    .is_some()
            {
                return Err(AppError::BadGateway);
            }
        }
        if resolved_by_id.len() != lookup_ids.len() {
            return Err(AppError::BadGateway);
        }

        let mut participants = Vec::with_capacity(silicon_ids.len());
        let mut principal_ids = HashSet::with_capacity(silicon_ids.len());
        let mut membership_ids = HashSet::with_capacity(silicon_ids.len());

        for silicon_id in silicon_ids {
            let member =
                if actor.actor.actor_type == ActorType::Silicon && actor.actor.id == *silicon_id {
                    ActiveMember {
                        organization_id: actor.organization_id,
                        org_id: actor.org_id.clone(),
                        membership_id: actor.membership_id,
                        actor: actor.actor.clone(),
                    }
                } else {
                    resolved_by_id
                        .remove(silicon_id)
                        .ok_or(AppError::BadGateway)?
                };

            if !valid_resolved_silicon(actor, silicon_id, &member) {
                return Err(AppError::BadGateway);
            }
            if !principal_ids.insert(member.actor.principal_id)
                || !membership_ids.insert(member.membership_id)
            {
                return Err(AppError::BadGateway);
            }
            participants.push(member);
        }

        Ok(participants)
    }
}

fn valid_resolved_silicon(
    caller: &VerifiedActor,
    expected_actor_id: &ActorId,
    member: &ActiveMember,
) -> bool {
    member.organization_id == caller.organization_id
        && member.org_id == caller.org_id
        && member.actor.actor_type == ActorType::Silicon
        && member.actor.id == *expected_actor_id
        && !member.organization_id.as_uuid().is_nil()
        && !member.membership_id.is_nil()
        && !member.actor.principal_id.as_uuid().is_nil()
}

fn has_project_authority(actor: &VerifiedActor, participates: bool) -> bool {
    actor.manages_projects() || (actor.actor.is_silicon() && participates)
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

fn directory_error(error: ProviderError) -> AppError {
    match error {
        ProviderError::NotFound => field_validation(
            "silicon_ids",
            "must identify current Silicons in the organization",
        ),
        ProviderError::RateLimited { retry_after } => AppError::RateLimited {
            retry_after_seconds: retry_after.map_or(1, duration_ceiling_seconds),
        },
        ProviderError::InvalidResponse | ProviderError::Conflict => AppError::BadGateway,
        ProviderError::Unauthenticated | ProviderError::Forbidden | ProviderError::Unavailable => {
            AppError::ProviderUnavailable
        }
    }
}

fn duration_ceiling_seconds(duration: Duration) -> u64 {
    duration
        .as_secs()
        .saturating_add(u64::from(duration.subsec_nanos() != 0))
        .max(1)
}

fn locator_value(locator: &ProjectLocator) -> String {
    match locator {
        ProjectLocator::Id(id) => id.to_string(),
        ProjectLocator::Uid(uid) => uid.as_str().to_owned(),
    }
}

fn project_create_fingerprint(request: &ProjectCreate) -> Value {
    json!({
        "name": request.name,
        "silicon_ids": request.silicon_ids,
    })
}

fn project_patch_fingerprint(request: &ProjectPatch) -> Value {
    let mut object = Map::new();
    if let Some(name) = &request.name {
        object.insert("name".to_owned(), json!(name));
    }
    if let Some(status) = request.status {
        object.insert("status".to_owned(), json!(status));
    }
    if let Some(silicon_ids) = &request.silicon_ids {
        object.insert("silicon_ids".to_owned(), json!(silicon_ids));
    }
    Value::Object(object)
}

fn project_task_create_fingerprint(request: &ProjectTaskCreate) -> Value {
    json!({
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
    use secrecy::SecretString;
    use serde_json::json;
    use uuid::Uuid;

    use super::{project_patch_fingerprint, valid_resolved_silicon, validate_request_id};
    use crate::{
        application::ports::{
            ActiveMember, CapabilitySet, InboundCredential, OrganizationRole,
            PROJECT_MANAGE_CAPABILITY, VerifiedActor,
        },
        domain::{
            Actor, ActorId, ActorType, OrganizationId, PrincipalId, ProjectPatch,
            PublicOrganizationId,
        },
    };

    fn verified_actor(capability: bool) -> Option<VerifiedActor> {
        let actor_id = ActorId::new("silicon:one").ok()?;
        let org_id = PublicOrganizationId::new("org-one").ok()?;
        let capabilities = if capability {
            CapabilitySet::try_from_names([PROJECT_MANAGE_CAPABILITY]).ok()?
        } else {
            CapabilitySet::default()
        };
        Some(VerifiedActor::new(
            OrganizationId::from_uuid(Uuid::from_u128(1)),
            org_id,
            Uuid::from_u128(2),
            Actor::new(
                PrincipalId::from_uuid(Uuid::from_u128(3)),
                ActorType::Silicon,
                actor_id,
            ),
            OrganizationRole::Member,
            capabilities,
            InboundCredential::Bearer(SecretString::from("opaque-token".to_owned())),
        ))
    }

    #[test]
    fn project_authority_requires_participation_or_explicit_capability() {
        let Some(participant) = verified_actor(false) else {
            return;
        };
        let Some(manager) = verified_actor(true) else {
            return;
        };
        assert!(super::has_project_authority(&participant, true));
        assert!(!super::has_project_authority(&participant, false));
        assert!(super::has_project_authority(&manager, false));
    }

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

    #[test]
    fn resolved_participants_reject_nil_provider_identifiers() {
        let Some(caller) = verified_actor(false) else {
            return;
        };
        let actor_id = ActorId::new("silicon:two");
        let Ok(actor_id) = actor_id else {
            return;
        };
        let valid_member = ActiveMember {
            organization_id: caller.organization_id,
            org_id: caller.org_id.clone(),
            membership_id: Uuid::from_u128(4),
            actor: Actor::new(
                PrincipalId::from_uuid(Uuid::from_u128(5)),
                ActorType::Silicon,
                actor_id.clone(),
            ),
        };
        assert!(valid_resolved_silicon(&caller, &actor_id, &valid_member));

        let nil_membership = ActiveMember {
            membership_id: Uuid::nil(),
            ..valid_member.clone()
        };
        assert!(!valid_resolved_silicon(&caller, &actor_id, &nil_membership));

        let nil_principal = ActiveMember {
            actor: Actor::new(
                PrincipalId::from_uuid(Uuid::nil()),
                ActorType::Silicon,
                actor_id.clone(),
            ),
            ..valid_member
        };
        assert!(!valid_resolved_silicon(&caller, &actor_id, &nil_principal));
    }
}
