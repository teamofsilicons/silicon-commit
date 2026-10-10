//! PostgreSQL persistence for project application workflows.
//!
//! This module is intentionally the only project module which knows the SQL
//! schema. Application code supplies validated commands and owns policy; rows
//! are converted back into domain values before crossing this boundary.
//! Reading a project is `commit.project_access(project, account)`; changing it
//! is `commit.project_writable(project, account)`.

use std::{borrow::Cow, time::Duration};

use serde_json::Value;
use sqlx::{AssertSqlSafe, FromRow, PgConnection, PgPool, types::Json};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{
    application::{
        idempotency::{MutationIdentity, MutationResponse},
        ports::{ResolvedAccount, VerifiedActor},
    },
    domain::{
        AccountUuid, Actor, ActorId, ActorType, BlockerStatus, CollectionQuery, Diary,
        DiaryVersion, LimitedText, PageCursor, Project, ProjectEntry, ProjectEntryId,
        ProjectEntryType, ProjectId, ProjectLocator, ProjectQuery, ProjectSlug, ProjectStatus,
        ProjectTask, ProjectTaskId, ProjectUid, RequiredText, ValidatedDiaryUpdate,
        ValidatedProjectCreate, ValidatedProjectEntryCreate, ValidatedProjectPatch,
        ValidatedProjectTaskCreate, ValidatedProjectTaskPatch,
    },
    error::AppError,
    infrastructure::postgres::accounts::account_uuid,
};

const PERSISTED_PROJECT_NAME_CHARS: usize = 200;
const PERSISTED_TITLE_CHARS: usize = 500;
const PERSISTED_DESCRIPTION_CHARS: usize = 100_000;

/// Project state read while holding its row lock for a mutation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LockedProject {
    /// Stable project identifier.
    pub(crate) id: ProjectId,
    /// Lifecycle state protected by the row lock.
    pub(crate) status: ProjectStatus,
    /// Current owner, who must remain a member.
    pub(crate) owner: AccountUuid,
    /// Whether the project is members-only.
    pub(crate) private: bool,
}

const PROJECT_SELECT: &str = r"
    SELECT project.id,
           project.name,
           project.slug,
           project.uid,
           project.status,
           project.description,
           project.attachments,
           project.private,
           (SELECT coalesce(max(v.version), 0) FROM commit.project_versions v WHERE v.project_id = project.id) AS version,
           (SELECT coalesce(jsonb_agg(jsonb_build_object('type', a.kind, 'id', a.public_id, 'uuid', a.uuid)
                                      ORDER BY c.first_contributed_at, a.public_id, a.uuid), '[]')
              FROM commit.project_collaborators c
              JOIN commit.accounts a ON a.uuid = c.account
             WHERE c.project_id = project.id) AS collaborators,
           owner.uuid AS owner_uuid,
           owner.kind AS owner_kind,
           owner.public_id AS owner_id,
           creator.uuid AS creator_uuid,
           creator.kind AS creator_kind,
           creator.public_id AS creator_id,
           project.created_at,
           project.updated_at,
           (SELECT coalesce(jsonb_agg(jsonb_build_object('type', a.kind, 'id', a.public_id, 'uuid', a.uuid)
                                      ORDER BY m.added_at, m.id), '[]')
              FROM commit.project_participants m
              JOIN commit.accounts a ON a.uuid = m.participant_account
             WHERE m.project_id = project.id AND m.removed_at IS NULL) AS members
      FROM commit.projects AS project
      JOIN commit.accounts AS owner ON owner.uuid = project.owner_account
      JOIN commit.accounts AS creator ON creator.uuid = project.created_by_account
";

/// Serializes one idempotency scope and returns its committed response, if any.
///
/// A replay is only returned while the caller can still read the project.
pub(crate) async fn acquire_idempotency(
    connection: &mut PgConnection,
    actor: &VerifiedActor,
    identity: &MutationIdentity,
) -> Result<Option<MutationResponse>, AppError> {
    sqlx::query(
        r"
        SELECT pg_advisory_xact_lock(
            hashtextextended(jsonb_build_array($1::text, $2::text, $3::text, $4::text)::text, 0)
        )
        ",
    )
    .bind(actor.uuid().as_str())
    .bind(identity.operation)
    .bind(&identity.resource_path)
    .bind(identity.key.as_str())
    .execute(&mut *connection)
    .await?;

    sqlx::query(
        r"
        DELETE FROM commit.idempotency_records
         WHERE actor_account = $1
           AND operation = $2
           AND resource_path = $3
           AND idempotency_key = $4
           AND expires_at <= transaction_timestamp()
        ",
    )
    .bind(actor.uuid().as_str())
    .bind(identity.operation)
    .bind(&identity.resource_path)
    .bind(identity.key.as_str())
    .execute(&mut *connection)
    .await?;

    let record = sqlx::query_as::<_, IdempotencyRow>(
        r"
        SELECT request_fingerprint, response_status, response_body
          FROM commit.idempotency_records
         WHERE actor_account = $1
           AND operation = $2
           AND resource_path = $3
           AND idempotency_key = $4
        ",
    )
    .bind(actor.uuid().as_str())
    .bind(identity.operation)
    .bind(&identity.resource_path)
    .bind(identity.key.as_str())
    .fetch_optional(&mut *connection)
    .await?;

    let response = replay_from_record(record, identity)?;
    if let Some(response) = &response {
        let raw = identity
            .resource_path
            .strip_prefix("/projects/")
            .and_then(|p| p.split('/').next());
        let locator = raw
            .and_then(|s| s.parse::<ProjectLocator>().ok())
            .or_else(|| {
                (identity.operation == "createProject")
                    .then(|| {
                        response
                            .body
                            .get("id")
                            .and_then(serde_json::Value::as_str)
                            .and_then(|s| s.parse::<ProjectLocator>().ok())
                    })
                    .flatten()
            });
        if let Some(locator) = locator {
            let project = lock_project(connection, &locator)
                .await?
                .ok_or(AppError::NotFound)?;
            if !can_read(connection, actor.uuid(), project.id).await? {
                return Err(AppError::NotFound);
            }
        }
    }
    Ok(response)
}

fn replay_from_record(
    record: Option<IdempotencyRow>,
    identity: &MutationIdentity,
) -> Result<Option<MutationResponse>, AppError> {
    let Some(record) = record else {
        return Ok(None);
    };
    if record.request_fingerprint.as_slice() != identity.fingerprint.as_bytes() {
        return Err(AppError::Conflict {
            code: Cow::Borrowed("idempotency_key_reused"),
        });
    }

    let status = u16::try_from(record.response_status).map_err(|error| {
        AppError::Internal(anyhow::anyhow!(
            "stored idempotency response status is invalid: {error}"
        ))
    })?;
    Ok(Some(MutationResponse::replayed(
        status,
        record.response_body.0,
    )))
}

/// Commits a successful response into the caller's already-locked replay scope.
pub(crate) async fn save_idempotency(
    connection: &mut PgConnection,
    actor: &VerifiedActor,
    identity: &MutationIdentity,
    response: &MutationResponse,
    ttl: Duration,
) -> Result<(), AppError> {
    let status = i16::try_from(response.status).map_err(|error| {
        AppError::Internal(anyhow::anyhow!(
            "idempotency response status is invalid: {error}"
        ))
    })?;
    let ttl_millis = i64::try_from(ttl.as_millis()).map_err(|error| {
        AppError::Internal(anyhow::anyhow!(
            "idempotency retention duration is too large: {error}"
        ))
    })?;
    if ttl_millis < 1 {
        return Err(AppError::Internal(anyhow::anyhow!(
            "idempotency retention duration must be positive"
        )));
    }

    sqlx::query(
        r"
        INSERT INTO commit.idempotency_records (
            id, actor_account, operation, resource_path, idempotency_key,
            request_fingerprint, response_status, response_body, expires_at
        )
        VALUES (
            $1, $2, $3, $4, $5, $6, $7, $8,
            transaction_timestamp() + ($9 * interval '1 millisecond')
        )
        ",
    )
    .bind(Uuid::now_v7())
    .bind(actor.uuid().as_str())
    .bind(identity.operation)
    .bind(&identity.resource_path)
    .bind(identity.key.as_str())
    .bind(identity.fingerprint.as_bytes().as_slice())
    .bind(status)
    .bind(Json(response.body.clone()))
    .bind(ttl_millis)
    .execute(connection)
    .await?;

    Ok(())
}

/// Writes one minimal project audit event in the current transaction.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn insert_audit(
    connection: &mut PgConnection,
    actor: &VerifiedActor,
    action: &'static str,
    resource_type: &'static str,
    resource_id: Uuid,
    request_id: &str,
    change_summary: Value,
    audit_retention: Duration,
) -> Result<(), AppError> {
    super::todos::insert_audit_event(
        connection,
        actor,
        action,
        resource_type,
        resource_id,
        request_id,
        &change_summary,
        audit_retention,
    )
    .await
}

/// Loads a stable, keyset-ordered page of readable projects plus one look-ahead project.
pub(crate) async fn list_projects(
    pool: &PgPool,
    actor: &VerifiedActor,
    query: &ProjectQuery,
) -> Result<Vec<Project>, AppError> {
    let statement = format!(
        r"
        {PROJECT_SELECT}
         WHERE project.deleted_at IS NULL
           AND commit.project_access(project.id, $6)
           AND ($1::commit.project_status IS NULL OR project.status = $1)
           AND (
                $2::text IS NULL
                OR EXISTS (
                    SELECT 1
                      FROM commit.project_participants AS filter_member
                      JOIN commit.accounts AS filter_account
                        ON filter_account.uuid = filter_member.participant_account
                     WHERE filter_member.project_id = project.id
                       AND filter_member.removed_at IS NULL
                       AND filter_account.kind = 'silicon'
                       AND (lower(filter_account.public_id) = lower($2) OR filter_account.uuid = $2)
                )
           )
           AND ($3::timestamptz IS NULL OR (project.created_at, project.id) < ($3, $4))
         ORDER BY project.created_at DESC, project.id DESC
         LIMIT $5
        ",
    );
    let cursor_created_at = query.cursor.map(crate::domain::PageCursor::created_at);
    let cursor_id = query.cursor.map(crate::domain::PageCursor::id);
    let member_id = query.silicon_id.as_ref().map(ActorId::as_str);
    let fetch_limit = i64::from(query.limit.get()) + 1;

    let rows = sqlx::query_as::<_, ProjectRow>(AssertSqlSafe(statement))
        .bind(query.status)
        .bind(member_id)
        .bind(cursor_created_at)
        .bind(cursor_id)
        .bind(fetch_limit)
        .bind(actor.uuid().as_str())
        .fetch_all(pool)
        .await?;

    rows.into_iter().map(ProjectRow::into_domain).collect()
}

/// Loads one project by exact UUID or exact stable UID, when the viewer can read it.
pub(crate) async fn get_project(
    pool: &PgPool,
    locator: &ProjectLocator,
    viewer: &AccountUuid,
) -> Result<Option<Project>, AppError> {
    let mut connection = pool.acquire().await?;
    let Some(project_id) = find_project_id(&mut connection, locator).await? else {
        return Ok(None);
    };
    if !can_read(&mut connection, viewer, project_id).await? {
        return Ok(None);
    }
    fetch_project_by_id(&mut connection, project_id).await
}

/// Resolves a project locator without locking it. Deleted projects are not found.
pub(crate) async fn find_project_id(
    connection: &mut PgConnection,
    locator: &ProjectLocator,
) -> Result<Option<ProjectId>, AppError> {
    let id = match locator {
        ProjectLocator::Id(project_id) => {
            sqlx::query_scalar::<_, Uuid>(
                "SELECT id FROM commit.projects WHERE id = $1 AND deleted_at IS NULL",
            )
            .bind(project_id.into_uuid())
            .fetch_optional(connection)
            .await?
        }
        ProjectLocator::Uid(uid) => {
            sqlx::query_scalar::<_, Uuid>(
                r"
                SELECT id FROM commit.projects
                 WHERE (uid = $1 OR legacy_uid = $1) AND deleted_at IS NULL
                 ORDER BY (uid = $1) DESC
                 LIMIT 1
                ",
            )
            .bind(uid.as_str())
            .fetch_optional(connection)
            .await?
        }
    };

    Ok(id.map(ProjectId::from_uuid))
}

/// Resolves and row-locks a project's lifecycle and ownership state.
pub(crate) async fn lock_project(
    connection: &mut PgConnection,
    locator: &ProjectLocator,
) -> Result<Option<LockedProject>, AppError> {
    let Some(project_id) = find_project_id(connection, locator).await? else {
        return Ok(None);
    };
    sqlx::query_as::<_, LockedProjectRow>(
        r"
        SELECT id, status, owner_account, private
          FROM commit.projects
         WHERE id = $1 AND deleted_at IS NULL
         FOR UPDATE
        ",
    )
    .bind(project_id.into_uuid())
    .fetch_optional(connection)
    .await?
    .map(LockedProjectRow::into_locked)
    .transpose()
}

#[derive(FromRow)]
struct LockedProjectRow {
    id: Uuid,
    status: ProjectStatus,
    owner_account: String,
    private: bool,
}

impl LockedProjectRow {
    fn into_locked(self) -> Result<LockedProject, AppError> {
        Ok(LockedProject {
            id: ProjectId::from_uuid(self.id),
            status: self.status,
            owner: account_uuid(self.owner_account)?,
            private: self.private,
        })
    }
}

/// Chooses a collision-free millisecond creation time for the documented UID.
pub(crate) async fn next_project_created_at(
    connection: &mut PgConnection,
    creator: &Actor,
    slug: &ProjectSlug,
) -> Result<OffsetDateTime, AppError> {
    sqlx::query(
        r"
        SELECT pg_advisory_xact_lock(
            hashtextextended(jsonb_build_array('project_uid', $1::text, $2::text)::text, 0)
        )
        ",
    )
    .bind(creator.uuid.as_str())
    .bind(slug.as_str())
    .execute(&mut *connection)
    .await?;

    let timestamp = sqlx::query_scalar::<_, OffsetDateTime>(
        r"
        SELECT greatest(
                   clock_timestamp(),
                   coalesce(max(created_at) + interval '1 millisecond', '-infinity'::timestamptz)
               )
          FROM commit.projects
         WHERE created_by_account = $1
           AND slug = $2
        ",
    )
    .bind(creator.uuid.as_str())
    .bind(slug.as_str())
    .fetch_one(connection)
    .await?;

    Ok(timestamp)
}

/// Inserts a project and its active member set.
pub(crate) async fn insert_project(
    connection: &mut PgConnection,
    actor: &VerifiedActor,
    project_id: ProjectId,
    command: &ValidatedProjectCreate,
    uid: &ProjectUid,
    created_at: OffsetDateTime,
    members: &[ResolvedAccount],
) -> Result<Project, AppError> {
    sqlx::query(
        r"
        INSERT INTO commit.projects (
            id, name, slug, uid, status, created_by_account, owner_account,
            created_at, updated_at, description, attachments, private
        )
        VALUES ($1, $2, $3, $4, $5, $6, $6, $7, $7, $8, $9, $10)
        ",
    )
    .bind(project_id.into_uuid())
    .bind(command.name.as_str())
    .bind(command.slug.as_str())
    .bind(uid.as_str())
    .bind(ProjectStatus::YetToStart)
    .bind(actor.uuid().as_str())
    .bind(created_at)
    .bind(&command.details.description)
    .bind(Json(&command.details.attachments))
    .bind(command.details.private)
    .execute(&mut *connection)
    .await?;

    insert_missing_members(connection, actor, project_id, members).await?;
    fetch_project_by_id(connection, project_id)
        .await?
        .ok_or_else(|| {
            AppError::Internal(anyhow::anyhow!(
                "newly inserted project could not be reloaded"
            ))
        })
}

/// Applies metadata and temporal member replacement.
pub(crate) async fn update_project(
    connection: &mut PgConnection,
    actor: &VerifiedActor,
    locked_project: &LockedProject,
    command: &ValidatedProjectPatch,
    members: Option<&[ResolvedAccount]>,
) -> Result<Project, AppError> {
    if locked_project.status == ProjectStatus::Completed && command.status.is_some() {
        return Err(AppError::Conflict {
            code: Cow::Borrowed("project_already_completed"),
        });
    }
    if members.is_some_and(|members| {
        !members
            .iter()
            .any(|member| member.actor.uuid == locked_project.owner)
    }) {
        return Err(crate::domain::ValidationError::invalid(
            "participants",
            "must retain the project owner",
        )
        .into());
    }

    let project_id = locked_project.id;
    sqlx::query(
        r"
        UPDATE commit.projects
           SET name = coalesce($2, name),
               status = coalesce($3, status),
               description = coalesce($4, description),
               attachments = coalesce($5, attachments),
               private = coalesce($6, private),
               updated_at = GREATEST(updated_at, clock_timestamp())
         WHERE id = $1
        ",
    )
    .bind(project_id.into_uuid())
    .bind(command.name.as_ref().map(RequiredText::as_str))
    .bind(command.status)
    .bind(&command.description)
    .bind(command.attachments.as_ref().map(Json))
    .bind(command.private)
    .execute(&mut *connection)
    .await?;

    if let Some(members) = members {
        let accounts = members
            .iter()
            .map(|member| member.actor.uuid.as_str().to_owned())
            .collect::<Vec<_>>();
        sqlx::query(
            r"
            UPDATE commit.project_participants
               SET removed_by_account = $2,
                   removed_at = GREATEST(added_at, clock_timestamp())
             WHERE project_id = $1
               AND removed_at IS NULL
               AND NOT (participant_account = ANY($3))
            ",
        )
        .bind(project_id.into_uuid())
        .bind(actor.uuid().as_str())
        .bind(&accounts)
        .execute(&mut *connection)
        .await?;

        insert_missing_members(connection, actor, project_id, members).await?;
    }

    fetch_project_by_id(connection, project_id)
        .await?
        .ok_or_else(|| AppError::Internal(anyhow::anyhow!("updated project disappeared")))
}

/// Adds every listed account that is not already an active member.
pub(crate) async fn insert_missing_members(
    connection: &mut PgConnection,
    actor: &VerifiedActor,
    project_id: ProjectId,
    members: &[ResolvedAccount],
) -> Result<(), AppError> {
    for member in members {
        add_member(connection, actor, project_id, &member.actor.uuid).await?;
    }
    Ok(())
}

/// Adds one active member unless it already is one.
pub(crate) async fn add_member(
    connection: &mut PgConnection,
    actor: &VerifiedActor,
    project_id: ProjectId,
    member: &AccountUuid,
) -> Result<(), AppError> {
    sqlx::query(
        r"
        INSERT INTO commit.project_participants (id, project_id, participant_account, added_by_account)
        SELECT $1, $2, $3, $4
         WHERE NOT EXISTS (
             SELECT 1
               FROM commit.project_participants
              WHERE project_id = $2
                AND participant_account = $3
                AND removed_at IS NULL
         )
        ",
    )
    .bind(Uuid::now_v7())
    .bind(project_id.into_uuid())
    .bind(member.as_str())
    .bind(actor.uuid().as_str())
    .execute(&mut *connection)
    .await?;
    Ok(())
}

/// Reloads a project aggregate inside an existing transaction.
pub(crate) async fn fetch_project_by_id(
    connection: &mut PgConnection,
    project_id: ProjectId,
) -> Result<Option<Project>, AppError> {
    let statement =
        format!("{PROJECT_SELECT} WHERE project.id = $1 AND project.deleted_at IS NULL");
    let row = sqlx::query_as::<_, ProjectRow>(AssertSqlSafe(statement))
        .bind(project_id.into_uuid())
        .fetch_optional(connection)
        .await?;

    row.map(ProjectRow::into_domain).transpose()
}

/// Reads a project's current diary.
pub(crate) async fn get_diary(
    pool: &PgPool,
    project_id: ProjectId,
) -> Result<Option<Diary>, AppError> {
    let row = sqlx::query_as::<_, DiaryRow>(DIARY_SELECT)
        .bind(project_id.into_uuid())
        .fetch_optional(pool)
        .await?;
    row.map(DiaryRow::into_domain).transpose()
}

/// Row-locks and reads the diary for optimistic replacement.
pub(crate) async fn lock_diary(
    connection: &mut PgConnection,
    project_id: ProjectId,
) -> Result<Option<Diary>, AppError> {
    let statement = format!("{DIARY_SELECT} FOR UPDATE OF diary");
    let row = sqlx::query_as::<_, DiaryRow>(AssertSqlSafe(statement))
        .bind(project_id.into_uuid())
        .fetch_optional(connection)
        .await?;
    row.map(DiaryRow::into_domain).transpose()
}

/// Replaces a locked diary at the expected version and reloads it.
pub(crate) async fn replace_diary(
    connection: &mut PgConnection,
    actor: &VerifiedActor,
    project_id: ProjectId,
    expected_version: DiaryVersion,
    command: &ValidatedDiaryUpdate,
) -> Result<Diary, AppError> {
    let result = sqlx::query(
        r"
        UPDATE commit.project_diaries
           SET markdown = $3,
               updated_by_account = $2,
               updated_at = GREATEST(updated_at, clock_timestamp())
         WHERE project_id = $1
           AND version = $4
        ",
    )
    .bind(project_id.into_uuid())
    .bind(actor.uuid().as_str())
    .bind(&command.markdown)
    .bind(expected_version.get())
    .execute(&mut *connection)
    .await?;
    if result.rows_affected() != 1 {
        return Err(AppError::Conflict {
            code: Cow::Borrowed("diary_version_mismatch"),
        });
    }

    touch_project(connection, project_id).await?;
    let row = sqlx::query_as::<_, DiaryRow>(DIARY_SELECT)
        .bind(project_id.into_uuid())
        .fetch_optional(connection)
        .await?;
    row.map(DiaryRow::into_domain)
        .transpose()?
        .ok_or_else(|| AppError::Internal(anyhow::anyhow!("updated project diary disappeared")))
}

/// Lists all tasks and subtasks in deterministic creation order.
pub(crate) async fn list_tasks(
    pool: &PgPool,
    project_id: ProjectId,
    query: CollectionQuery,
) -> Result<Vec<ProjectTask>, AppError> {
    let statement = format!(
        r"
        {PROJECT_TASK_SELECT_BASE}
           AND ($2::timestamptz IS NULL OR (task.created_at, task.id) < ($2, $3))
         ORDER BY task.created_at DESC, task.id DESC
         LIMIT $4
        "
    );
    let rows = sqlx::query_as::<_, ProjectTaskRow>(AssertSqlSafe(statement))
        .bind(project_id.into_uuid())
        .bind(query.cursor.map(PageCursor::created_at))
        .bind(query.cursor.map(PageCursor::id))
        .bind(i64::from(query.limit.get()) + 1)
        .fetch_all(pool)
        .await?;
    rows.into_iter().map(ProjectTaskRow::into_domain).collect()
}

/// Checks a requested parent without revealing tasks from another project.
pub(crate) async fn parent_task_exists(
    connection: &mut PgConnection,
    project_id: ProjectId,
    parent_task_id: ProjectTaskId,
) -> Result<bool, AppError> {
    sqlx::query_scalar::<_, bool>(
        r"
        SELECT EXISTS (
            SELECT 1 FROM commit.project_tasks
             WHERE project_id = $1 AND id = $2 AND deleted_at IS NULL
        )
        ",
    )
    .bind(project_id.into_uuid())
    .bind(parent_task_id.into_uuid())
    .fetch_one(connection)
    .await
    .map_err(AppError::from)
}

/// Inserts one project-local task or subtask.
pub(crate) async fn insert_task(
    connection: &mut PgConnection,
    actor: &VerifiedActor,
    project_id: ProjectId,
    task_id: ProjectTaskId,
    command: &ValidatedProjectTaskCreate,
) -> Result<ProjectTask, AppError> {
    sqlx::query(
        r"
        INSERT INTO commit.project_tasks (
            id, project_id, parent_task_id, title, description, status, created_by_account
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        ",
    )
    .bind(task_id.into_uuid())
    .bind(project_id.into_uuid())
    .bind(command.parent_task_id.map(ProjectTaskId::into_uuid))
    .bind(command.title.as_str())
    .bind(command.description.as_str())
    .bind(command.status)
    .bind(actor.uuid().as_str())
    .execute(&mut *connection)
    .await?;

    touch_project(connection, project_id).await?;
    fetch_task_by_id(connection, project_id, task_id)
        .await?
        .ok_or_else(|| {
            AppError::Internal(anyhow::anyhow!("newly inserted project task disappeared"))
        })
}

/// Applies a non-idempotent task patch under the project mutation lock.
pub(crate) async fn update_task(
    connection: &mut PgConnection,
    project_id: ProjectId,
    task_id: ProjectTaskId,
    command: &ValidatedProjectTaskPatch,
) -> Result<Option<ProjectTask>, AppError> {
    let result = sqlx::query(
        r"
        UPDATE commit.project_tasks
           SET title = coalesce($3, title),
               description = coalesce($4, description),
               status = coalesce($5, status),
               updated_at = GREATEST(updated_at, clock_timestamp())
         WHERE project_id = $1
           AND id = $2 AND deleted_at IS NULL
        ",
    )
    .bind(project_id.into_uuid())
    .bind(task_id.into_uuid())
    .bind(command.title.as_ref().map(RequiredText::as_str))
    .bind(command.description.as_ref().map(LimitedText::as_str))
    .bind(command.status)
    .execute(&mut *connection)
    .await?;
    if result.rows_affected() == 0 {
        return Ok(None);
    }

    touch_project(connection, project_id).await?;
    fetch_task_by_id(connection, project_id, task_id).await
}

/// Reads one live task of a project.
pub(crate) async fn fetch_task_by_id(
    connection: &mut PgConnection,
    project_id: ProjectId,
    task_id: ProjectTaskId,
) -> Result<Option<ProjectTask>, AppError> {
    let statement = format!("{PROJECT_TASK_SELECT_BASE} AND task.id = $2");
    let row = sqlx::query_as::<_, ProjectTaskRow>(AssertSqlSafe(statement))
        .bind(project_id.into_uuid())
        .bind(task_id.into_uuid())
        .fetch_optional(connection)
        .await?;
    row.map(ProjectTaskRow::into_domain).transpose()
}

/// Reports whether the immutable completion statement already exists.
pub(crate) async fn completion_exists(
    connection: &mut PgConnection,
    project_id: ProjectId,
) -> Result<bool, AppError> {
    sqlx::query_scalar::<_, bool>(
        r"
        SELECT EXISTS (
            SELECT 1 FROM commit.project_entries
             WHERE project_id = $1 AND entry_type = 'completion'
        )
        ",
    )
    .bind(project_id.into_uuid())
    .fetch_one(connection)
    .await
    .map_err(AppError::from)
}

/// Appends a blocker, update, or completion entry.
pub(crate) async fn insert_entry(
    connection: &mut PgConnection,
    actor: &VerifiedActor,
    project_id: ProjectId,
    entry_id: ProjectEntryId,
    command: &ValidatedProjectEntryCreate,
) -> Result<ProjectEntry, AppError> {
    sqlx::query(
        r"
        INSERT INTO commit.project_entries (
            id, project_id, entry_type, title, description, blocker_status, created_by_account
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        ",
    )
    .bind(entry_id.into_uuid())
    .bind(project_id.into_uuid())
    .bind(command.entry_type)
    .bind(command.title.as_str())
    .bind(command.description.as_str())
    .bind(command.status)
    .bind(actor.uuid().as_str())
    .execute(&mut *connection)
    .await?;

    if command.entry_type == ProjectEntryType::Completion {
        sqlx::query(
            r"
            UPDATE commit.projects
               SET status = 'completed',
                   updated_at = GREATEST(updated_at, clock_timestamp())
             WHERE id = $1
            ",
        )
        .bind(project_id.into_uuid())
        .execute(&mut *connection)
        .await?;
    } else {
        touch_project(connection, project_id).await?;
    }
    fetch_entry_by_id(connection, project_id, entry_id)
        .await?
        .ok_or_else(|| {
            AppError::Internal(anyhow::anyhow!("newly inserted project entry disappeared"))
        })
}

const ENTRY_SELECT: &str = r"
    SELECT entry.id,
           entry.project_id,
           entry.entry_type,
           entry.title,
           entry.description,
           entry.blocker_status,
           author.uuid AS author_uuid,
           author.kind AS author_kind,
           author.public_id AS author_id,
           entry.created_at
      FROM commit.project_entries AS entry
      JOIN commit.accounts AS author ON author.uuid = entry.created_by_account
     WHERE entry.project_id = $1
";

/// Lists project activity in stable reverse creation order.
pub(crate) async fn list_entries(
    pool: &PgPool,
    project_id: ProjectId,
    query: CollectionQuery,
) -> Result<Vec<ProjectEntry>, AppError> {
    let statement = format!(
        r"
        {ENTRY_SELECT}
           AND ($2::timestamptz IS NULL OR (entry.created_at, entry.id) < ($2, $3))
         ORDER BY entry.created_at DESC, entry.id DESC
         LIMIT $4
        "
    );
    let rows = sqlx::query_as::<_, ProjectEntryRow>(AssertSqlSafe(statement))
        .bind(project_id.into_uuid())
        .bind(query.cursor.map(PageCursor::created_at))
        .bind(query.cursor.map(PageCursor::id))
        .bind(i64::from(query.limit.get()) + 1)
        .fetch_all(pool)
        .await?;
    rows.into_iter().map(ProjectEntryRow::into_domain).collect()
}

async fn fetch_entry_by_id(
    connection: &mut PgConnection,
    project_id: ProjectId,
    entry_id: ProjectEntryId,
) -> Result<Option<ProjectEntry>, AppError> {
    let statement = format!("{ENTRY_SELECT} AND entry.id = $2");
    let row = sqlx::query_as::<_, ProjectEntryRow>(AssertSqlSafe(statement))
        .bind(project_id.into_uuid())
        .bind(entry_id.into_uuid())
        .fetch_optional(connection)
        .await?;
    row.map(ProjectEntryRow::into_domain).transpose()
}

async fn touch_project(
    connection: &mut PgConnection,
    project_id: ProjectId,
) -> Result<(), AppError> {
    sqlx::query(
        r"
        UPDATE commit.projects
           SET updated_at = GREATEST(updated_at, clock_timestamp())
         WHERE id = $1
        ",
    )
    .bind(project_id.into_uuid())
    .execute(connection)
    .await?;
    Ok(())
}

const DIARY_SELECT: &str = r"
    SELECT diary.project_id,
           diary.markdown,
           diary.version,
           editor.uuid AS editor_uuid,
           editor.kind AS editor_kind,
           editor.public_id AS editor_id,
           diary.updated_at
      FROM commit.project_diaries AS diary
      JOIN commit.accounts AS editor ON editor.uuid = diary.updated_by_account
     WHERE diary.project_id = $1
";

const PROJECT_TASK_SELECT_BASE: &str = r"
    SELECT task.id,
           task.project_id,
           task.parent_task_id,
           task.title,
           task.description,
           task.status,
           (SELECT jsonb_build_object('type', a.kind, 'id', a.public_id, 'uuid', a.uuid)
              FROM commit.accounts a WHERE a.uuid = task.assigned_to_account) AS assigned_to,
           task.todo_id,
           author.uuid AS author_uuid,
           author.kind AS author_kind,
           author.public_id AS author_id,
           task.created_at
      FROM commit.project_tasks AS task
      JOIN commit.accounts AS author ON author.uuid = task.created_by_account
     WHERE task.project_id = $1
       AND task.deleted_at IS NULL
";

#[derive(Debug, FromRow)]
struct IdempotencyRow {
    request_fingerprint: Vec<u8>,
    response_status: i16,
    response_body: Json<Value>,
}

#[derive(Debug, FromRow)]
struct ProjectRow {
    id: Uuid,
    name: String,
    slug: String,
    uid: String,
    status: ProjectStatus,
    owner_uuid: String,
    owner_kind: ActorType,
    owner_id: String,
    creator_uuid: String,
    creator_kind: ActorType,
    creator_id: String,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
    members: Json<Vec<crate::domain::ActorRef>>,
    description: String,
    attachments: Json<Vec<crate::domain::AttachmentUrl>>,
    private: bool,
    version: i64,
    collaborators: Json<Vec<crate::domain::ActorRef>>,
}

impl ProjectRow {
    fn into_domain(self) -> Result<Project, AppError> {
        let members = self
            .members
            .0
            .into_iter()
            .map(|member| Actor::new(member.uuid, member.actor_type, member.id))
            .collect::<Vec<_>>();
        let carbon_ids = members
            .iter()
            .filter(|member| !member.is_silicon())
            .map(|member| member.id.clone())
            .collect();
        Ok(Project {
            details: crate::domain::project::ProjectDetails {
                description: self.description,
                attachments: self.attachments.0,
                private: self.private,
                carbon_ids,
            },
            version: self.version,
            collaborators: self.collaborators.0,
            id: ProjectId::from_uuid(self.id),
            name: RequiredText::new("name", self.name, PERSISTED_PROJECT_NAME_CHARS)
                .map_err(|error| invalid_row(error.to_string()))?,
            slug: self
                .slug
                .parse::<ProjectSlug>()
                .map_err(|error| invalid_row(error.to_string()))?,
            uid: self
                .uid
                .parse::<ProjectUid>()
                .map_err(|error| invalid_row(error.to_string()))?,
            status: self.status,
            members,
            owner: actor_from_row(self.owner_uuid, self.owner_kind, self.owner_id)?,
            created_by: actor_from_row(self.creator_uuid, self.creator_kind, self.creator_id)?,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

#[derive(Debug, FromRow)]
struct DiaryRow {
    project_id: Uuid,
    markdown: String,
    version: i64,
    editor_uuid: String,
    editor_kind: ActorType,
    editor_id: String,
    updated_at: OffsetDateTime,
}

impl DiaryRow {
    fn into_domain(self) -> Result<Diary, AppError> {
        Ok(Diary {
            project_id: ProjectId::from_uuid(self.project_id),
            markdown: self.markdown,
            version: DiaryVersion::new(self.version)
                .map_err(|error| invalid_row(error.to_string()))?,
            updated_by: actor_from_row(self.editor_uuid, self.editor_kind, self.editor_id)?,
            updated_at: self.updated_at,
        })
    }
}

#[derive(Debug, FromRow)]
struct ProjectTaskRow {
    id: Uuid,
    project_id: Uuid,
    parent_task_id: Option<Uuid>,
    title: String,
    description: String,
    status: crate::domain::TodoStatus,
    author_uuid: String,
    author_kind: ActorType,
    author_id: String,
    created_at: OffsetDateTime,
    assigned_to: Option<Json<crate::domain::ActorRef>>,
    todo_id: Option<Uuid>,
}

impl ProjectTaskRow {
    fn into_domain(self) -> Result<ProjectTask, AppError> {
        Ok(ProjectTask {
            assigned_to: self.assigned_to.map(|a| a.0),
            todo_id: self.todo_id.map(crate::domain::TodoId::from_uuid),
            id: ProjectTaskId::from_uuid(self.id),
            project_id: ProjectId::from_uuid(self.project_id),
            parent_task_id: self.parent_task_id.map(ProjectTaskId::from_uuid),
            title: RequiredText::new("title", self.title, PERSISTED_TITLE_CHARS)
                .map_err(|error| invalid_row(error.to_string()))?,
            description: LimitedText::new(
                "description",
                self.description,
                PERSISTED_DESCRIPTION_CHARS,
            )
            .map_err(|error| invalid_row(error.to_string()))?,
            status: self.status,
            created_by: actor_from_row(self.author_uuid, self.author_kind, self.author_id)?,
            created_at: self.created_at,
        })
    }
}

#[derive(Debug, FromRow)]
struct ProjectEntryRow {
    id: Uuid,
    project_id: Uuid,
    entry_type: ProjectEntryType,
    title: String,
    description: String,
    blocker_status: Option<BlockerStatus>,
    author_uuid: String,
    author_kind: ActorType,
    author_id: String,
    created_at: OffsetDateTime,
}

impl ProjectEntryRow {
    fn into_domain(self) -> Result<ProjectEntry, AppError> {
        let valid_status = matches!(
            (self.entry_type, self.blocker_status),
            (ProjectEntryType::Blocker, Some(_))
                | (
                    ProjectEntryType::Update | ProjectEntryType::Completion,
                    None
                )
        );
        if !valid_status {
            return Err(invalid_row("project entry blocker status is inconsistent"));
        }

        Ok(ProjectEntry {
            id: ProjectEntryId::from_uuid(self.id),
            project_id: ProjectId::from_uuid(self.project_id),
            entry_type: self.entry_type,
            title: RequiredText::new("title", self.title, PERSISTED_TITLE_CHARS)
                .map_err(|error| invalid_row(error.to_string()))?,
            description: LimitedText::new(
                "description",
                self.description,
                PERSISTED_DESCRIPTION_CHARS,
            )
            .map_err(|error| invalid_row(error.to_string()))?,
            status: self.blocker_status,
            created_by: actor_from_row(self.author_uuid, self.author_kind, self.author_id)?,
            created_at: self.created_at,
        })
    }
}

fn actor_from_row(
    uuid: String,
    actor_type: ActorType,
    public_id: String,
) -> Result<Actor, AppError> {
    Ok(Actor::new(
        account_uuid(uuid)?,
        actor_type,
        ActorId::from_persisted(public_id),
    ))
}

fn invalid_row(message: impl Into<String>) -> AppError {
    AppError::Internal(anyhow::anyhow!(
        "invalid persisted project data: {}",
        message.into()
    ))
}

/// Whether the account can read the project (members, member Silicons' custodians,
/// and the owner's circle unless the project is private).
pub(crate) async fn can_read(
    connection: &mut PgConnection,
    account: &AccountUuid,
    project_id: ProjectId,
) -> Result<bool, AppError> {
    Ok(sqlx::query_scalar("SELECT commit.project_access($1, $2)")
        .bind(project_id.into_uuid())
        .bind(account.as_str())
        .fetch_one(connection)
        .await?)
}

/// Whether the account can change the project (members and member Silicons' custodians).
pub(crate) async fn can_write(
    connection: &mut PgConnection,
    account: &AccountUuid,
    project_id: ProjectId,
) -> Result<bool, AppError> {
    Ok(sqlx::query_scalar("SELECT commit.project_writable($1, $2)")
        .bind(project_id.into_uuid())
        .bind(account.as_str())
        .fetch_one(connection)
        .await?)
}

#[cfg(test)]
mod tests {
    use super::actor_from_row;
    use crate::domain::ActorType;

    #[test]
    fn persisted_actor_conversion_rejects_an_empty_uuid_but_keeps_a_deleted_id() {
        assert!(actor_from_row(String::new(), ActorType::Silicon, "si:x".to_owned()).is_err());
        assert!(actor_from_row("K1E".to_owned(), ActorType::Carbon, String::new()).is_ok());
    }
}
