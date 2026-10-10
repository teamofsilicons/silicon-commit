//! Project assignments, initial nested work, claims, removal and history.
use super::{
    AppError, IdempotencyKey, MutationResponse, PgConnection, ProjectLocator, ProjectService,
    ProjectTaskCreate, ProjectTaskId, ResolvedAccount, Value, VerifiedActor, account_field_error,
    created_response, ensure_reachable, field_validation, json, locator_value, mutation_identity,
    postgres, remember, validate_request_id, validation_error,
};
use crate::domain::{ActorId, ProjectId, TodoId, ValidatedProjectTaskCreate};
use crate::infrastructure::postgres::todos as todo_store;
use uuid::Uuid;

type Seed = (
    ProjectTaskId,
    ValidatedProjectTaskCreate,
    Option<ResolvedAccount>,
);
impl ProjectService {
    pub(super) async fn prepare_seed_tasks(
        &self,
        actor: &VerifiedActor,
        tasks: &[crate::domain::project::ProjectSeedTask],
    ) -> Result<Vec<Seed>, AppError> {
        let mut pending = tasks
            .iter()
            .rev()
            .map(|task| (task, None, 1))
            .collect::<Vec<_>>();
        let mut output = Vec::new();
        while let Some((task, parent_task_id, depth)) = pending.pop() {
            if output.len() >= 1000 || depth > 16 {
                return Err(field_validation(
                    "tasks",
                    "at most 1000 initial tasks and 16 levels are supported",
                ));
            }
            let id = ProjectTaskId::new();
            let command = ProjectTaskCreate {
                parent_task_id,
                title: task.title.clone(),
                description: task.description.clone(),
                status: task.status,
                assigned_to: task.assigned_to.clone(),
            }
            .validate(&self.limits)
            .map_err(validation_error)?;
            let assignee = self
                .resolve_task_assignee(actor, task.assigned_to.as_ref())
                .await?;
            output.push((id, command, assignee));
            pending.extend(
                task.subtasks
                    .iter()
                    .rev()
                    .map(|child| (child, Some(id), depth + 1)),
            );
        }
        Ok(output)
    }

    /// Resolves a task assignee by `c:`/`si:` id or uuid; the caller needs no lookup.
    pub(super) async fn resolve_task_assignee(
        &self,
        actor: &VerifiedActor,
        id: Option<&ActorId>,
    ) -> Result<Option<ResolvedAccount>, AppError> {
        let Some(id) = id else {
            return Ok(None);
        };
        if id.as_str() == actor.uuid().as_str()
            || (!actor.actor.id.as_str().is_empty()
                && id.as_str().eq_ignore_ascii_case(actor.actor.id.as_str()))
        {
            return Ok(Some(ResolvedAccount::known(
                actor.actor.clone(),
                actor.custodian.clone(),
            )));
        }
        self.identity_provider
            .resolve_account(id, None)
            .await
            .map(Some)
            .map_err(|error| account_field_error("assigned_to", error))
    }

    /// Assigns (or, with `None`, unassigns) a task. Assignment creates or reuses the task's
    /// todo and shares the project with the assignee.
    pub(super) async fn assign_task(
        &self,
        tx: &mut PgConnection,
        actor: &VerifiedActor,
        project_id: ProjectId,
        task_id: ProjectTaskId,
        assignee: Option<&ResolvedAccount>,
    ) -> Result<(), AppError> {
        let task = postgres::fetch_task_by_id(tx, project_id, task_id)
            .await?
            .ok_or(AppError::NotFound)?;
        if let Some(account) = assignee {
            remember(tx, actor, std::slice::from_ref(account)).await?;
            ensure_reachable(tx, actor, &account.actor, "assigned_to").await?;
            // Assigning project work shares the project with its assignee.
            postgres::add_member(tx, actor, project_id, &account.actor.uuid).await?;
            let todo_id = if let Some(id) = task.todo_id {
                id
            } else {
                let id = TodoId::new();
                todo_store::insert_todo(
                    tx,
                    &todo_store::NewTodo {
                        id,
                        title: &task.title,
                        description: Some(&task.description),
                        assigned_by: actor.uuid(),
                        assigned_to: &account.actor.uuid,
                        status: task.status,
                        attachments: &[],
                        project_id: Some(project_id),
                    },
                )
                .await?;
                id
            };
            sqlx::query("UPDATE commit.project_tasks SET assigned_to_account = $3, todo_id = $4 WHERE project_id = $1 AND id = $2 AND deleted_at IS NULL")
                .bind(project_id.into_uuid())
                .bind(task_id.into_uuid())
                .bind(account.actor.uuid.as_str())
                .bind(todo_id.into_uuid())
                .execute(&mut *tx)
                .await?;
        } else {
            // Detach first: deleting the old todo must not delete the now-open task.
            sqlx::query("UPDATE commit.project_tasks SET assigned_to_account = NULL, todo_id = NULL WHERE project_id = $1 AND id = $2 AND deleted_at IS NULL")
                .bind(project_id.into_uuid())
                .bind(task_id.into_uuid())
                .execute(&mut *tx)
                .await?;
            if let Some(id) = task.todo_id {
                todo_store::soft_delete_todo(tx, id, actor.uuid(), self.audit_retention).await?;
            }
        }
        Ok(())
    }

    /// Atomically claims unassigned work; competing claimants cannot overwrite it.
    pub async fn claim_task(
        &self,
        actor: &VerifiedActor,
        locator: &ProjectLocator,
        task_id: ProjectTaskId,
        key: IdempotencyKey,
        request_id: &str,
    ) -> Result<MutationResponse, AppError> {
        validate_request_id(request_id)?;
        let identity = mutation_identity(
            "claimProjectTask",
            format!("/projects/{}/tasks/{task_id}/claim", locator_value(locator)),
            key,
            &json!({}),
        )?;
        let mut tx = self.pool.begin().await?;
        let project_id = self
            .authorize_locked_project(&mut tx, actor, locator)
            .await?
            .id;
        if let Some(response) = postgres::acquire_idempotency(&mut tx, actor, &identity).await? {
            tx.commit().await?;
            return Ok(response);
        }
        remember(&mut tx, actor, &[]).await?;
        let task = postgres::fetch_task_by_id(&mut tx, project_id, task_id)
            .await?
            .ok_or(AppError::NotFound)?;
        if task.assigned_to.is_some() {
            return Err(AppError::Conflict {
                code: "task_already_assigned".into(),
            });
        }
        let me = ResolvedAccount::known(actor.actor.clone(), actor.custodian.clone());
        self.assign_task(&mut tx, actor, project_id, task_id, Some(&me))
            .await?;
        postgres::insert_audit(
            &mut tx,
            actor,
            "project.task.claimed",
            "project_task",
            task_id.into_uuid(),
            request_id,
            json!({"project_id":project_id}),
            self.audit_retention,
        )
        .await?;
        let task = postgres::fetch_task_by_id(&mut tx, project_id, task_id)
            .await?
            .ok_or(AppError::NotFound)?;
        let response = created_response(&task, 200)?;
        postgres::save_idempotency(&mut tx, actor, &identity, &response, self.idempotency_ttl)
            .await?;
        tx.commit().await?;
        Ok(response)
    }

    /// Removes a task and its descendants, including their linked todos.
    pub async fn delete_task(
        &self,
        actor: &VerifiedActor,
        locator: &ProjectLocator,
        task_id: ProjectTaskId,
        request_id: &str,
    ) -> Result<(), AppError> {
        validate_request_id(request_id)?;
        let mut tx = self.pool.begin().await?;
        let project_id = self
            .authorize_locked_project(&mut tx, actor, locator)
            .await?
            .id;
        remember(&mut tx, actor, &[]).await?;
        // Delete children before parents so the mirrored todo-deletion cascade
        // never removes a child that this operation still needs to process.
        let tasks: Vec<(Uuid, Option<Uuid>)> = sqlx::query_as("WITH RECURSIVE subtree AS (SELECT id, todo_id, 0 AS depth FROM commit.project_tasks WHERE project_id = $1 AND id = $2 AND deleted_at IS NULL UNION ALL SELECT t.id, t.todo_id, s.depth + 1 FROM commit.project_tasks t JOIN subtree s ON t.parent_task_id = s.id WHERE t.project_id = $1 AND t.deleted_at IS NULL) SELECT id, todo_id FROM subtree ORDER BY depth DESC, id")
            .bind(project_id.into_uuid())
            .bind(task_id.into_uuid())
            .fetch_all(&mut *tx)
            .await?;
        if tasks.is_empty() {
            return Err(AppError::NotFound);
        }
        for (id, todo) in tasks {
            if let Some(todo) = todo {
                todo_store::soft_delete_todo(
                    &mut tx,
                    TodoId::from_uuid(todo),
                    actor.uuid(),
                    self.audit_retention,
                )
                .await?;
            }
            sqlx::query("UPDATE commit.project_tasks SET deleted_at = clock_timestamp() WHERE project_id = $1 AND id = $2 AND deleted_at IS NULL")
                .bind(project_id.into_uuid())
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        postgres::insert_audit(
            &mut tx,
            actor,
            "project.task.deleted",
            "project_task",
            task_id.into_uuid(),
            request_id,
            json!({"project_id":project_id}),
            self.audit_retention,
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Lists retained revision metadata, newest first, without large snapshots.
    pub async fn versions(
        &self,
        actor: &VerifiedActor,
        locator: &ProjectLocator,
        before: Option<i64>,
        limit: u16,
    ) -> Result<Value, AppError> {
        let project = self.get_project(actor, locator).await?;
        let rows: Vec<(i64, Value, String, String)> = sqlx::query_as("SELECT version, actor, action, to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') FROM commit.project_versions WHERE project_id = $1 AND ($2::bigint IS NULL OR version < $2) ORDER BY version DESC LIMIT $3")
            .bind(project.id.into_uuid())
            .bind(before)
            .bind(i64::from(limit) + 1)
            .fetch_all(&self.pool)
            .await?;
        let more = rows.len() > usize::from(limit);
        let items = rows
            .into_iter()
            .take(usize::from(limit))
            .map(|(version, actor, action, created_at)| {
                json!({"version": version, "actor": actor, "action": action, "created_at": created_at})
            })
            .collect::<Vec<_>>();
        Ok(
            json!({"next_before": if more {items.last().map(|v| v["version"].clone())} else {None}, "items": items}),
        )
    }

    /// Reads a retained snapshot under the project's current visibility policy.
    pub async fn version(
        &self,
        actor: &VerifiedActor,
        locator: &ProjectLocator,
        version: i64,
    ) -> Result<Value, AppError> {
        let project = self.get_project(actor, locator).await?;
        sqlx::query_scalar(
            "SELECT snapshot FROM commit.project_versions WHERE project_id = $1 AND version = $2",
        )
        .bind(project.id.into_uuid())
        .bind(version)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(AppError::NotFound)
    }
}
