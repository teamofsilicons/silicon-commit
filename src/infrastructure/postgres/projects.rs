//! PostgreSQL persistence for project application workflows.
//!
//! This module is intentionally the only project module which knows the SQL
//! schema. Application code supplies validated commands and owns policy; rows
//! are converted back into domain values before crossing this boundary.

use std::{borrow::Cow, str::FromStr as _, time::Duration};

use serde_json::Value;
use sqlx::{AssertSqlSafe, FromRow, PgConnection, PgPool, types::Json};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{
    application::{
        idempotency::{MutationIdentity, MutationResponse},
        ports::{ActiveMember, VerifiedActor},
    },
    domain::{
        Actor, ActorId, ActorType, BlockerStatus, CollectionQuery, Diary, DiaryVersion,
        LimitedText, OrganizationId, PageCursor, Project, ProjectEntry, ProjectEntryId,
        ProjectEntryType, ProjectId, ProjectLocator, ProjectQuery, ProjectSlug, ProjectStatus,
        ProjectTask, ProjectTaskId, ProjectUid, PublicOrganizationId, RequiredText,
        ValidatedDiaryUpdate, ValidatedProjectCreate, ValidatedProjectEntryCreate,
        ValidatedProjectPatch, ValidatedProjectTaskCreate, ValidatedProjectTaskPatch,
    },
    error::AppError,
};

const PERSISTED_PROJECT_NAME_CHARS: usize = 200;
const PERSISTED_TITLE_CHARS: usize = 500;
const PERSISTED_DESCRIPTION_CHARS: usize = 100_000;

/// Project state read while holding its row lock for a mutation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct LockedProject {
    /// Stable project identifier.
    pub(crate) id: ProjectId,
    /// Lifecycle state protected by the row lock.
    pub(crate) status: ProjectStatus,
    /// Immutable creator used to defend participant replacement.
    pub(crate) creator_principal_id: crate::domain::PrincipalId,
}

const PROJECT_SELECT: &str = r"
    SELECT project.id,
           project.organization_id,
           organization.org_id,
           project.name,
           project.slug,
           project.uid,
           project.status,
           creator.principal_id AS creator_principal_id,
           creator.actor_type AS creator_actor_type,
           creator.actor_id AS creator_actor_id,
           project.created_at,
           project.updated_at,
           array_agg(participant_actor.principal_id
                     ORDER BY participant.added_at, participant.id)
               AS participant_principal_ids,
           array_agg(participant_actor.actor_type::text
                     ORDER BY participant.added_at, participant.id)
               AS participant_actor_types,
           array_agg(participant_actor.actor_id
                     ORDER BY participant.added_at, participant.id)
               AS participant_actor_ids
      FROM commit.projects AS project
      JOIN commit.organization_projection AS organization
        ON organization.organization_id = project.organization_id
      JOIN commit.actor_projection AS creator
        ON creator.organization_id = project.organization_id
       AND creator.principal_id = project.created_by_principal_id
      JOIN commit.project_participants AS participant
        ON participant.organization_id = project.organization_id
       AND participant.project_id = project.id
       AND participant.removed_at IS NULL
      JOIN commit.actor_projection AS participant_actor
        ON participant_actor.organization_id = participant.organization_id
       AND participant_actor.principal_id = participant.silicon_principal_id
";

const PROJECT_GROUP_BY: &str = r"
    GROUP BY project.id,
             project.organization_id,
             organization.org_id,
             project.name,
             project.slug,
             project.uid,
             project.status,
             creator.principal_id,
             creator.actor_type,
             creator.actor_id,
             project.created_at,
             project.updated_at
";

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

/// Persists or verifies one freshly resolved participant identity projection.
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

/// Serializes one idempotency scope and returns its committed response, if any.
pub(crate) async fn acquire_idempotency(
    connection: &mut PgConnection,
    actor: &VerifiedActor,
    identity: &MutationIdentity,
) -> Result<Option<MutationResponse>, AppError> {
    sqlx::query(
        r"
        SELECT pg_advisory_xact_lock(
            hashtextextended(
                jsonb_build_array(
                    $1::uuid,
                    $2::uuid,
                    $3::text,
                    $4::text,
                    $5::text
                )::text,
                0
            )
        )
        ",
    )
    .bind(actor.organization_id.into_uuid())
    .bind(actor.actor.principal_id.into_uuid())
    .bind(identity.operation)
    .bind(&identity.resource_path)
    .bind(identity.key.as_str())
    .execute(&mut *connection)
    .await?;

    sqlx::query(
        r"
        DELETE FROM commit.idempotency_records
         WHERE organization_id = $1
           AND actor_principal_id = $2
           AND operation = $3
           AND resource_path = $4
           AND idempotency_key = $5
           AND expires_at <= transaction_timestamp()
        ",
    )
    .bind(actor.organization_id.into_uuid())
    .bind(actor.actor.principal_id.into_uuid())
    .bind(identity.operation)
    .bind(&identity.resource_path)
    .bind(identity.key.as_str())
    .execute(&mut *connection)
    .await?;

    let record = sqlx::query_as::<_, IdempotencyRow>(
        r"
        SELECT request_fingerprint, response_status, response_body
          FROM commit.idempotency_records
         WHERE organization_id = $1
           AND actor_principal_id = $2
           AND operation = $3
           AND resource_path = $4
           AND idempotency_key = $5
        ",
    )
    .bind(actor.organization_id.into_uuid())
    .bind(actor.actor.principal_id.into_uuid())
    .bind(identity.operation)
    .bind(&identity.resource_path)
    .bind(identity.key.as_str())
    .fetch_optional(connection)
    .await?;

    replay_from_record(record, identity)
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
            id,
            organization_id,
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
            $1, $2, $3, $4, $5, $6, $7, $8, $9,
            transaction_timestamp() + ($10 * interval '1 millisecond')
        )
        ",
    )
    .bind(Uuid::now_v7())
    .bind(actor.organization_id.into_uuid())
    .bind(actor.actor.principal_id.into_uuid())
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
        r"
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
        ",
    )
    .bind(Uuid::now_v7())
    .bind(actor.organization_id.into_uuid())
    .bind(actor.actor.principal_id.into_uuid())
    .bind(action)
    .bind(resource_type)
    .bind(resource_id)
    .bind(request_id)
    .bind(Json(change_summary))
    .bind(retention_seconds)
    .execute(connection)
    .await?;

    Ok(())
}

/// Loads a stable, keyset-ordered page plus one look-ahead project.
pub(crate) async fn list_projects(
    pool: &PgPool,
    organization_id: OrganizationId,
    query: &ProjectQuery,
) -> Result<Vec<Project>, AppError> {
    let statement = format!(
        r"
        {PROJECT_SELECT}
         WHERE project.organization_id = $1
           AND ($2::commit.project_status IS NULL OR project.status = $2)
           AND (
                $3::text IS NULL
                OR EXISTS (
                    SELECT 1
                      FROM commit.project_participants AS filter_participant
                      JOIN commit.actor_projection AS filter_actor
                        ON filter_actor.organization_id = filter_participant.organization_id
                       AND filter_actor.principal_id = filter_participant.silicon_principal_id
                     WHERE filter_participant.organization_id = project.organization_id
                       AND filter_participant.project_id = project.id
                       AND filter_participant.removed_at IS NULL
                       AND filter_actor.actor_type = 'silicon'
                       AND filter_actor.actor_id = $3
                )
           )
           AND (
                $4::timestamptz IS NULL
                OR (project.created_at, project.id) < ($4, $5)
           )
        {PROJECT_GROUP_BY}
         ORDER BY project.created_at DESC, project.id DESC
         LIMIT $6
        ",
    );
    let cursor_created_at = query.cursor.map(crate::domain::PageCursor::created_at);
    let cursor_id = query.cursor.map(crate::domain::PageCursor::id);
    let participant_id = query.silicon_id.as_ref().map(ActorId::as_str);
    let fetch_limit = i64::from(query.limit.get()) + 1;

    let rows = sqlx::query_as::<_, ProjectRow>(AssertSqlSafe(statement))
        .bind(organization_id.into_uuid())
        .bind(query.status)
        .bind(participant_id)
        .bind(cursor_created_at)
        .bind(cursor_id)
        .bind(fetch_limit)
        .fetch_all(pool)
        .await?;

    rows.into_iter().map(ProjectRow::into_domain).collect()
}

/// Loads one project by exact UUID or exact stable UID.
pub(crate) async fn get_project(
    pool: &PgPool,
    organization_id: OrganizationId,
    locator: &ProjectLocator,
) -> Result<Option<Project>, AppError> {
    let mut connection = pool.acquire().await?;
    let Some(project_id) = find_project_id(&mut connection, organization_id, locator).await? else {
        return Ok(None);
    };
    fetch_project_by_id(&mut connection, organization_id, project_id).await
}

/// Resolves a project locator without locking it.
pub(crate) async fn find_project_id(
    connection: &mut PgConnection,
    organization_id: OrganizationId,
    locator: &ProjectLocator,
) -> Result<Option<ProjectId>, AppError> {
    let id = match locator {
        ProjectLocator::Id(project_id) => {
            sqlx::query_scalar::<_, Uuid>(
                r"
                SELECT id
                  FROM commit.projects
                 WHERE organization_id = $1
                   AND id = $2
                ",
            )
            .bind(organization_id.into_uuid())
            .bind(project_id.into_uuid())
            .fetch_optional(connection)
            .await?
        }
        ProjectLocator::Uid(uid) => {
            sqlx::query_scalar::<_, Uuid>(
                r"
                SELECT id
                  FROM commit.projects
                 WHERE organization_id = $1
                   AND uid = $2
                ",
            )
            .bind(organization_id.into_uuid())
            .bind(uid.as_str())
            .fetch_optional(connection)
            .await?
        }
    };

    Ok(id.map(ProjectId::from_uuid))
}

/// Resolves and row-locks a project's lifecycle and creator state.
pub(crate) async fn lock_project(
    connection: &mut PgConnection,
    organization_id: OrganizationId,
    locator: &ProjectLocator,
) -> Result<Option<LockedProject>, AppError> {
    let row = match locator {
        ProjectLocator::Id(project_id) => {
            sqlx::query_as::<_, LockedProjectRow>(
                r"
                SELECT id, status, created_by_principal_id
                  FROM commit.projects
                 WHERE organization_id = $1
                   AND id = $2
                 FOR UPDATE
                ",
            )
            .bind(organization_id.into_uuid())
            .bind(project_id.into_uuid())
            .fetch_optional(connection)
            .await?
        }
        ProjectLocator::Uid(uid) => {
            sqlx::query_as::<_, LockedProjectRow>(
                r"
                SELECT id, status, created_by_principal_id
                  FROM commit.projects
                 WHERE organization_id = $1
                   AND uid = $2
                 FOR UPDATE
                ",
            )
            .bind(organization_id.into_uuid())
            .bind(uid.as_str())
            .fetch_optional(connection)
            .await?
        }
    };

    Ok(row.map(LockedProjectRow::into_locked))
}

#[derive(FromRow)]
struct LockedProjectRow {
    id: Uuid,
    status: ProjectStatus,
    created_by_principal_id: Uuid,
}

impl LockedProjectRow {
    fn into_locked(self) -> LockedProject {
        LockedProject {
            id: ProjectId::from_uuid(self.id),
            status: self.status,
            creator_principal_id: crate::domain::PrincipalId::from_uuid(
                self.created_by_principal_id,
            ),
        }
    }
}

/// Reports whether the principal is a current project participant.
pub(crate) async fn is_active_participant(
    connection: &mut PgConnection,
    organization_id: OrganizationId,
    project_id: ProjectId,
    actor: &Actor,
) -> Result<bool, AppError> {
    let participates = sqlx::query_scalar::<_, bool>(
        r"
        SELECT EXISTS (
            SELECT 1
              FROM commit.project_participants
             WHERE organization_id = $1
               AND project_id = $2
               AND silicon_principal_id = $3
               AND removed_at IS NULL
        )
        ",
    )
    .bind(organization_id.into_uuid())
    .bind(project_id.into_uuid())
    .bind(actor.principal_id.into_uuid())
    .fetch_one(connection)
    .await?;

    Ok(participates)
}

/// Chooses a collision-free millisecond creation time for the documented UID.
pub(crate) async fn next_project_created_at(
    connection: &mut PgConnection,
    organization_id: OrganizationId,
    creator: &Actor,
    slug: &ProjectSlug,
) -> Result<OffsetDateTime, AppError> {
    sqlx::query(
        r"
        SELECT pg_advisory_xact_lock(
            hashtextextended(
                jsonb_build_array(
                    'project_uid',
                    $1::uuid,
                    $2::uuid,
                    $3::text
                )::text,
                0
            )
        )
        ",
    )
    .bind(organization_id.into_uuid())
    .bind(creator.principal_id.into_uuid())
    .bind(slug.as_str())
    .execute(&mut *connection)
    .await?;

    let timestamp = sqlx::query_scalar::<_, OffsetDateTime>(
        r"
        SELECT greatest(
                   clock_timestamp(),
                   coalesce(
                       max(created_at) + interval '1 millisecond',
                       '-infinity'::timestamptz
                   )
               )
          FROM commit.projects
         WHERE organization_id = $1
           AND created_by_principal_id = $2
           AND slug = $3
        ",
    )
    .bind(organization_id.into_uuid())
    .bind(creator.principal_id.into_uuid())
    .bind(slug.as_str())
    .fetch_one(connection)
    .await?;

    Ok(timestamp)
}

/// Inserts a project and its active participant set.
pub(crate) async fn insert_project(
    connection: &mut PgConnection,
    actor: &VerifiedActor,
    project_id: ProjectId,
    command: &ValidatedProjectCreate,
    uid: &ProjectUid,
    created_at: OffsetDateTime,
    participants: &[ActiveMember],
) -> Result<Project, AppError> {
    sqlx::query(
        r"
        INSERT INTO commit.projects (
            id,
            organization_id,
            name,
            slug,
            uid,
            status,
            created_by_principal_id,
            created_at,
            updated_at
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $8)
        ",
    )
    .bind(project_id.into_uuid())
    .bind(actor.organization_id.into_uuid())
    .bind(command.name.as_str())
    .bind(command.slug.as_str())
    .bind(uid.as_str())
    .bind(ProjectStatus::YetToStart)
    .bind(actor.actor.principal_id.into_uuid())
    .bind(created_at)
    .execute(&mut *connection)
    .await?;

    insert_missing_participants(connection, actor, project_id, participants).await?;
    fetch_project_by_id(connection, actor.organization_id, project_id)
        .await?
        .ok_or_else(|| {
            AppError::Internal(anyhow::anyhow!(
                "newly inserted project could not be reloaded"
            ))
        })
}

/// Applies metadata and temporal participant replacement.
pub(crate) async fn update_project(
    connection: &mut PgConnection,
    actor: &VerifiedActor,
    locked_project: LockedProject,
    command: &ValidatedProjectPatch,
    participants: Option<&[ActiveMember]>,
) -> Result<Project, AppError> {
    if locked_project.status == ProjectStatus::Completed && command.status.is_some() {
        return Err(AppError::Conflict {
            code: Cow::Borrowed("project_already_completed"),
        });
    }
    if participants.is_some_and(|members| {
        !members
            .iter()
            .any(|member| member.actor.principal_id == locked_project.creator_principal_id)
    }) {
        return Err(crate::domain::ValidationError::invalid(
            "silicon_ids",
            "must include the project creator",
        )
        .into());
    }

    let project_id = locked_project.id;
    sqlx::query(
        r"
        UPDATE commit.projects
           SET name = coalesce($3, name),
               status = coalesce($4, status),
               updated_at = GREATEST(updated_at, clock_timestamp())
         WHERE organization_id = $1
           AND id = $2
        ",
    )
    .bind(actor.organization_id.into_uuid())
    .bind(project_id.into_uuid())
    .bind(command.name.as_ref().map(RequiredText::as_str))
    .bind(command.status)
    .execute(&mut *connection)
    .await?;

    if let Some(participants) = participants {
        let principal_ids = participants
            .iter()
            .map(|member| member.actor.principal_id.into_uuid())
            .collect::<Vec<_>>();
        sqlx::query(
            r"
            UPDATE commit.project_participants
               SET removed_by_principal_id = $3,
                   removed_at = GREATEST(added_at, clock_timestamp())
             WHERE organization_id = $1
               AND project_id = $2
               AND removed_at IS NULL
               AND NOT (silicon_principal_id = ANY($4))
            ",
        )
        .bind(actor.organization_id.into_uuid())
        .bind(project_id.into_uuid())
        .bind(actor.actor.principal_id.into_uuid())
        .bind(&principal_ids)
        .execute(&mut *connection)
        .await?;

        insert_missing_participants(connection, actor, project_id, participants).await?;
    }

    fetch_project_by_id(connection, actor.organization_id, project_id)
        .await?
        .ok_or_else(|| AppError::Internal(anyhow::anyhow!("updated project disappeared")))
}

async fn insert_missing_participants(
    connection: &mut PgConnection,
    actor: &VerifiedActor,
    project_id: ProjectId,
    participants: &[ActiveMember],
) -> Result<(), AppError> {
    for participant in participants {
        sqlx::query(
            r"
            INSERT INTO commit.project_participants (
                id,
                organization_id,
                project_id,
                silicon_principal_id,
                added_by_principal_id
            )
            SELECT $1, $2, $3, $4, $5
             WHERE NOT EXISTS (
                 SELECT 1
                   FROM commit.project_participants
                  WHERE organization_id = $2
                    AND project_id = $3
                    AND silicon_principal_id = $4
                    AND removed_at IS NULL
             )
            ",
        )
        .bind(Uuid::now_v7())
        .bind(actor.organization_id.into_uuid())
        .bind(project_id.into_uuid())
        .bind(participant.actor.principal_id.into_uuid())
        .bind(actor.actor.principal_id.into_uuid())
        .execute(&mut *connection)
        .await?;
    }

    Ok(())
}

/// Reloads a project aggregate inside an existing transaction.
pub(crate) async fn fetch_project_by_id(
    connection: &mut PgConnection,
    organization_id: OrganizationId,
    project_id: ProjectId,
) -> Result<Option<Project>, AppError> {
    let statement = format!(
        r"
        {PROJECT_SELECT}
         WHERE project.organization_id = $1
           AND project.id = $2
        {PROJECT_GROUP_BY}
        ",
    );
    let row = sqlx::query_as::<_, ProjectRow>(AssertSqlSafe(statement))
        .bind(organization_id.into_uuid())
        .bind(project_id.into_uuid())
        .fetch_optional(connection)
        .await?;

    row.map(ProjectRow::into_domain).transpose()
}

/// Reads a project's current diary.
pub(crate) async fn get_diary(
    pool: &PgPool,
    organization_id: OrganizationId,
    project_id: ProjectId,
) -> Result<Option<Diary>, AppError> {
    let row = sqlx::query_as::<_, DiaryRow>(DIARY_SELECT)
        .bind(organization_id.into_uuid())
        .bind(project_id.into_uuid())
        .fetch_optional(pool)
        .await?;
    row.map(DiaryRow::into_domain).transpose()
}

/// Row-locks and reads the diary for optimistic replacement.
pub(crate) async fn lock_diary(
    connection: &mut PgConnection,
    organization_id: OrganizationId,
    project_id: ProjectId,
) -> Result<Option<Diary>, AppError> {
    let statement = format!("{DIARY_SELECT} FOR UPDATE OF diary");
    let row = sqlx::query_as::<_, DiaryRow>(AssertSqlSafe(statement))
        .bind(organization_id.into_uuid())
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
           SET markdown = $4,
               updated_by_principal_id = $3,
               updated_at = GREATEST(updated_at, clock_timestamp())
         WHERE organization_id = $1
           AND project_id = $2
           AND version = $5
        ",
    )
    .bind(actor.organization_id.into_uuid())
    .bind(project_id.into_uuid())
    .bind(actor.actor.principal_id.into_uuid())
    .bind(&command.markdown)
    .bind(expected_version.get())
    .execute(&mut *connection)
    .await?;
    if result.rows_affected() != 1 {
        return Err(AppError::Conflict {
            code: Cow::Borrowed("diary_version_mismatch"),
        });
    }

    touch_project(connection, actor.organization_id, project_id).await?;
    let row = sqlx::query_as::<_, DiaryRow>(DIARY_SELECT)
        .bind(actor.organization_id.into_uuid())
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
    organization_id: OrganizationId,
    project_id: ProjectId,
    query: CollectionQuery,
) -> Result<Vec<ProjectTask>, AppError> {
    let statement = format!(
        r"
        {PROJECT_TASK_SELECT_BASE}
           AND (
               $3::timestamptz IS NULL
               OR (task.created_at, task.id) < ($3, $4)
           )
         ORDER BY task.created_at DESC, task.id DESC
         LIMIT $5
        "
    );
    let rows = sqlx::query_as::<_, ProjectTaskRow>(AssertSqlSafe(statement))
        .bind(organization_id.into_uuid())
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
    organization_id: OrganizationId,
    project_id: ProjectId,
    parent_task_id: ProjectTaskId,
) -> Result<bool, AppError> {
    sqlx::query_scalar::<_, bool>(
        r"
        SELECT EXISTS (
            SELECT 1
              FROM commit.project_tasks
             WHERE organization_id = $1
               AND project_id = $2
               AND id = $3
        )
        ",
    )
    .bind(organization_id.into_uuid())
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
            id,
            organization_id,
            project_id,
            parent_task_id,
            title,
            description,
            status,
            created_by_principal_id
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        ",
    )
    .bind(task_id.into_uuid())
    .bind(actor.organization_id.into_uuid())
    .bind(project_id.into_uuid())
    .bind(command.parent_task_id.map(ProjectTaskId::into_uuid))
    .bind(command.title.as_str())
    .bind(command.description.as_str())
    .bind(command.status)
    .bind(actor.actor.principal_id.into_uuid())
    .execute(&mut *connection)
    .await?;

    touch_project(connection, actor.organization_id, project_id).await?;
    fetch_task_by_id(connection, actor.organization_id, project_id, task_id)
        .await?
        .ok_or_else(|| {
            AppError::Internal(anyhow::anyhow!("newly inserted project task disappeared"))
        })
}

/// Applies a non-idempotent task patch under the project mutation lock.
pub(crate) async fn update_task(
    connection: &mut PgConnection,
    actor: &VerifiedActor,
    project_id: ProjectId,
    task_id: ProjectTaskId,
    command: &ValidatedProjectTaskPatch,
) -> Result<Option<ProjectTask>, AppError> {
    let result = sqlx::query(
        r"
        UPDATE commit.project_tasks
           SET title = coalesce($4, title),
               description = coalesce($5, description),
               status = coalesce($6, status),
               updated_at = GREATEST(updated_at, clock_timestamp())
         WHERE organization_id = $1
           AND project_id = $2
           AND id = $3
        ",
    )
    .bind(actor.organization_id.into_uuid())
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

    touch_project(connection, actor.organization_id, project_id).await?;
    fetch_task_by_id(connection, actor.organization_id, project_id, task_id).await
}

async fn fetch_task_by_id(
    connection: &mut PgConnection,
    organization_id: OrganizationId,
    project_id: ProjectId,
    task_id: ProjectTaskId,
) -> Result<Option<ProjectTask>, AppError> {
    let statement = format!("{PROJECT_TASK_SELECT_BASE} AND task.id = $3");
    let row = sqlx::query_as::<_, ProjectTaskRow>(AssertSqlSafe(statement))
        .bind(organization_id.into_uuid())
        .bind(project_id.into_uuid())
        .bind(task_id.into_uuid())
        .fetch_optional(connection)
        .await?;
    row.map(ProjectTaskRow::into_domain).transpose()
}

/// Reports whether the immutable completion statement already exists.
pub(crate) async fn completion_exists(
    connection: &mut PgConnection,
    organization_id: OrganizationId,
    project_id: ProjectId,
) -> Result<bool, AppError> {
    sqlx::query_scalar::<_, bool>(
        r"
        SELECT EXISTS (
            SELECT 1
              FROM commit.project_entries
             WHERE organization_id = $1
               AND project_id = $2
               AND entry_type = 'completion'
        )
        ",
    )
    .bind(organization_id.into_uuid())
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
            id,
            organization_id,
            project_id,
            entry_type,
            title,
            description,
            blocker_status,
            created_by_principal_id
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        ",
    )
    .bind(entry_id.into_uuid())
    .bind(actor.organization_id.into_uuid())
    .bind(project_id.into_uuid())
    .bind(command.entry_type)
    .bind(command.title.as_str())
    .bind(command.description.as_str())
    .bind(command.status)
    .bind(actor.actor.principal_id.into_uuid())
    .execute(&mut *connection)
    .await?;

    if command.entry_type == ProjectEntryType::Completion {
        sqlx::query(
            r"
            UPDATE commit.projects
               SET status = 'completed',
                   updated_at = GREATEST(updated_at, clock_timestamp())
             WHERE organization_id = $1
               AND id = $2
            ",
        )
        .bind(actor.organization_id.into_uuid())
        .bind(project_id.into_uuid())
        .execute(&mut *connection)
        .await?;
    } else {
        touch_project(connection, actor.organization_id, project_id).await?;
    }

    fetch_entry_by_id(connection, actor.organization_id, project_id, entry_id)
        .await?
        .ok_or_else(|| {
            AppError::Internal(anyhow::anyhow!("newly inserted project entry disappeared"))
        })
}

/// Lists project activity in stable reverse creation order within one tenant.
pub(crate) async fn list_entries(
    pool: &PgPool,
    organization_id: OrganizationId,
    project_id: ProjectId,
    query: CollectionQuery,
) -> Result<Vec<ProjectEntry>, AppError> {
    let rows = sqlx::query_as::<_, ProjectEntryRow>(
        r"
        SELECT entry.id,
               entry.project_id,
               entry.entry_type,
               entry.title,
               entry.description,
               entry.blocker_status,
               author.principal_id AS author_principal_id,
               author.actor_type AS author_actor_type,
               author.actor_id AS author_actor_id,
               entry.created_at
          FROM commit.project_entries AS entry
          JOIN commit.actor_projection AS author
            ON author.organization_id = entry.organization_id
           AND author.principal_id = entry.created_by_principal_id
         WHERE entry.organization_id = $1
           AND entry.project_id = $2
           AND ($3::timestamptz IS NULL OR (entry.created_at, entry.id) < ($3, $4))
         ORDER BY entry.created_at DESC, entry.id DESC
         LIMIT $5
        ",
    )
    .bind(organization_id.into_uuid())
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
    organization_id: OrganizationId,
    project_id: ProjectId,
    entry_id: ProjectEntryId,
) -> Result<Option<ProjectEntry>, AppError> {
    let row = sqlx::query_as::<_, ProjectEntryRow>(
        r"
        SELECT entry.id,
               entry.project_id,
               entry.entry_type,
               entry.title,
               entry.description,
               entry.blocker_status,
               author.principal_id AS author_principal_id,
               author.actor_type AS author_actor_type,
               author.actor_id AS author_actor_id,
               entry.created_at
          FROM commit.project_entries AS entry
          JOIN commit.actor_projection AS author
            ON author.organization_id = entry.organization_id
           AND author.principal_id = entry.created_by_principal_id
         WHERE entry.organization_id = $1
           AND entry.project_id = $2
           AND entry.id = $3
        ",
    )
    .bind(organization_id.into_uuid())
    .bind(project_id.into_uuid())
    .bind(entry_id.into_uuid())
    .fetch_optional(connection)
    .await?;
    row.map(ProjectEntryRow::into_domain).transpose()
}

async fn touch_project(
    connection: &mut PgConnection,
    organization_id: OrganizationId,
    project_id: ProjectId,
) -> Result<(), AppError> {
    sqlx::query(
        r"
        UPDATE commit.projects
           SET updated_at = GREATEST(updated_at, clock_timestamp())
         WHERE organization_id = $1
           AND id = $2
        ",
    )
    .bind(organization_id.into_uuid())
    .bind(project_id.into_uuid())
    .execute(connection)
    .await?;
    Ok(())
}

const DIARY_SELECT: &str = r"
    SELECT diary.project_id,
           diary.markdown,
           diary.version,
           editor.principal_id AS editor_principal_id,
           editor.actor_type AS editor_actor_type,
           editor.actor_id AS editor_actor_id,
           diary.updated_at
      FROM commit.project_diaries AS diary
      JOIN commit.actor_projection AS editor
        ON editor.organization_id = diary.organization_id
       AND editor.principal_id = diary.updated_by_principal_id
     WHERE diary.organization_id = $1
       AND diary.project_id = $2
";

const PROJECT_TASK_SELECT_BASE: &str = r"
    SELECT task.id,
           task.project_id,
           task.parent_task_id,
           task.title,
           task.description,
           task.status,
           author.principal_id AS author_principal_id,
           author.actor_type AS author_actor_type,
           author.actor_id AS author_actor_id,
           task.created_at
      FROM commit.project_tasks AS task
      JOIN commit.actor_projection AS author
        ON author.organization_id = task.organization_id
       AND author.principal_id = task.created_by_principal_id
     WHERE task.organization_id = $1
       AND task.project_id = $2
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
    organization_id: Uuid,
    org_id: String,
    name: String,
    slug: String,
    uid: String,
    status: ProjectStatus,
    creator_principal_id: Uuid,
    creator_actor_type: ActorType,
    creator_actor_id: String,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
    participant_principal_ids: Vec<Uuid>,
    participant_actor_types: Vec<String>,
    participant_actor_ids: Vec<String>,
}

impl ProjectRow {
    fn into_domain(self) -> Result<Project, AppError> {
        if self.participant_principal_ids.len() != self.participant_actor_types.len()
            || self.participant_principal_ids.len() != self.participant_actor_ids.len()
        {
            return Err(invalid_row(
                "project participant arrays have different lengths",
            ));
        }

        let silicons = self
            .participant_principal_ids
            .into_iter()
            .zip(self.participant_actor_types)
            .zip(self.participant_actor_ids)
            .map(|((principal_id, actor_type), actor_id)| {
                let actor_type = ActorType::from_str(&actor_type)
                    .map_err(|error| invalid_row(error.to_string()))?;
                if actor_type != ActorType::Silicon {
                    return Err(invalid_row("project participant is not a Silicon"));
                }
                actor_from_row(principal_id, actor_type, actor_id)
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Project {
            id: ProjectId::from_uuid(self.id),
            organization_id: OrganizationId::from_uuid(self.organization_id),
            org_id: PublicOrganizationId::new(self.org_id)
                .map_err(|error| invalid_row(error.to_string()))?,
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
            silicons,
            created_by: actor_from_row(
                self.creator_principal_id,
                self.creator_actor_type,
                self.creator_actor_id,
            )?,
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
    editor_principal_id: Uuid,
    editor_actor_type: ActorType,
    editor_actor_id: String,
    updated_at: OffsetDateTime,
}

impl DiaryRow {
    fn into_domain(self) -> Result<Diary, AppError> {
        Ok(Diary {
            project_id: ProjectId::from_uuid(self.project_id),
            markdown: self.markdown,
            version: DiaryVersion::new(self.version)
                .map_err(|error| invalid_row(error.to_string()))?,
            updated_by: actor_from_row(
                self.editor_principal_id,
                self.editor_actor_type,
                self.editor_actor_id,
            )?,
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
    author_principal_id: Uuid,
    author_actor_type: ActorType,
    author_actor_id: String,
    created_at: OffsetDateTime,
}

impl ProjectTaskRow {
    fn into_domain(self) -> Result<ProjectTask, AppError> {
        Ok(ProjectTask {
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
            created_by: actor_from_row(
                self.author_principal_id,
                self.author_actor_type,
                self.author_actor_id,
            )?,
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
    author_principal_id: Uuid,
    author_actor_type: ActorType,
    author_actor_id: String,
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
            created_by: actor_from_row(
                self.author_principal_id,
                self.author_actor_type,
                self.author_actor_id,
            )?,
            created_at: self.created_at,
        })
    }
}

fn actor_from_row(
    principal_id: Uuid,
    actor_type: ActorType,
    actor_id: String,
) -> Result<Actor, AppError> {
    let actor_id = ActorId::new(actor_id).map_err(|error| invalid_row(error.to_string()))?;
    Ok(Actor::new(principal_id.into(), actor_type, actor_id))
}

fn invalid_row(message: impl Into<String>) -> AppError {
    AppError::Internal(anyhow::anyhow!(
        "invalid persisted project data: {}",
        message.into()
    ))
}

#[cfg(test)]
mod tests {
    use super::actor_from_row;
    use crate::domain::ActorType;
    use uuid::Uuid;

    #[test]
    fn persisted_actor_conversion_rejects_an_empty_public_id() {
        assert!(actor_from_row(Uuid::nil(), ActorType::Silicon, String::new()).is_err());
    }
}
