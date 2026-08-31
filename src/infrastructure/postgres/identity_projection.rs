//! Insert-once IAM identity projection persistence.

use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::{
    application::ports::VerifiedActor,
    domain::{Actor, ActorType, OrganizationId, PublicOrganizationId},
    error::AppError,
};

/// Fails closed when a verified IAM identity contradicts either direction of
/// an already persisted organization or actor mapping.
///
/// An absent projection is valid for a genuinely new tenant and remains
/// insert-on-first-write. Existing history is never read through a remapped
/// public handle or reused internal identifier.
pub(super) async fn assert_consistent(
    pool: &PgPool,
    actor: &VerifiedActor,
) -> Result<(), AppError> {
    let contradiction = sqlx::query_scalar::<_, bool>(
        r#"
        SELECT EXISTS (
                   SELECT 1
                   FROM commit.organization_projection AS organization
                   WHERE (
                           organization.organization_id = $1
                           OR organization.org_id = $2
                       )
                     AND NOT (
                         organization.organization_id = $1
                         AND organization.org_id = $2
                     )
               )
               OR EXISTS (
                   SELECT 1
                   FROM commit.actor_projection AS projection
                   WHERE projection.organization_id = $1
                     AND (
                         projection.principal_id = $3
                         OR projection.membership_id = $4
                         OR (
                             projection.actor_type = $5
                             AND projection.actor_id = $6
                         )
                     )
                     AND NOT (
                         projection.principal_id = $3
                         AND projection.membership_id = $4
                         AND projection.actor_type = $5
                         AND projection.actor_id = $6
                     )
               )
        "#,
    )
    .bind(actor.organization_id.into_uuid())
    .bind(actor.org_id.as_str())
    .bind(actor.actor.principal_id.into_uuid())
    .bind(actor.membership_id)
    .bind(actor.actor.actor_type)
    .bind(actor.actor.id.as_str())
    .fetch_one(pool)
    .await?;

    if contradiction {
        return Err(AppError::BadGateway);
    }
    Ok(())
}

/// Persists one observed identity mapping without taking a tenant-wide update
/// lock on every request, then fails closed if an existing mapping differs.
pub(super) async fn persist_identity(
    connection: &mut PgConnection,
    organization_id: OrganizationId,
    org_id: &PublicOrganizationId,
    membership_id: Uuid,
    actor: &Actor,
) -> Result<(), AppError> {
    let existing_org_id = sqlx::query_scalar::<_, String>(
        r#"
        SELECT org_id
        FROM commit.organization_projection
        WHERE organization_id = $1
        "#,
    )
    .bind(organization_id.into_uuid())
    .fetch_optional(&mut *connection)
    .await?;
    if let Some(existing_org_id) = existing_org_id {
        if existing_org_id != org_id.as_str() {
            return Err(AppError::BadGateway);
        }
    } else {
        sqlx::query(
            r#"
            INSERT INTO commit.organization_projection (organization_id, org_id)
            VALUES ($1, $2)
            ON CONFLICT DO NOTHING
            "#,
        )
        .bind(organization_id.into_uuid())
        .bind(org_id.as_str())
        .execute(&mut *connection)
        .await?;

        let organization_matches = sqlx::query_scalar::<_, bool>(
            r#"
            SELECT EXISTS (
                SELECT 1
                FROM commit.organization_projection
                WHERE organization_id = $1
                  AND org_id = $2
            )
            "#,
        )
        .bind(organization_id.into_uuid())
        .bind(org_id.as_str())
        .fetch_one(&mut *connection)
        .await?;
        if !organization_matches {
            return Err(AppError::BadGateway);
        }
    }

    let existing_actor = sqlx::query_as::<_, (Uuid, ActorType, String)>(
        r#"
        SELECT membership_id, actor_type, actor_id
        FROM commit.actor_projection
        WHERE organization_id = $1
          AND principal_id = $2
        "#,
    )
    .bind(organization_id.into_uuid())
    .bind(actor.principal_id.into_uuid())
    .fetch_optional(&mut *connection)
    .await?;
    if let Some((existing_membership_id, existing_actor_type, existing_actor_id)) = existing_actor {
        if existing_membership_id != membership_id
            || existing_actor_type != actor.actor_type
            || existing_actor_id != actor.id.as_str()
        {
            return Err(AppError::BadGateway);
        }
        return Ok(());
    }

    sqlx::query(
        r#"
        INSERT INTO commit.actor_projection (
            organization_id,
            principal_id,
            membership_id,
            actor_type,
            actor_id
        )
        VALUES ($1, $2, $3, $4, $5)
        ON CONFLICT DO NOTHING
        "#,
    )
    .bind(organization_id.into_uuid())
    .bind(actor.principal_id.into_uuid())
    .bind(membership_id)
    .bind(actor.actor_type)
    .bind(actor.id.as_str())
    .execute(&mut *connection)
    .await?;

    let actor_matches = sqlx::query_scalar::<_, bool>(
        r#"
        SELECT EXISTS (
            SELECT 1
            FROM commit.actor_projection
            WHERE organization_id = $1
              AND principal_id = $2
              AND membership_id = $3
              AND actor_type = $4
              AND actor_id = $5
        )
        "#,
    )
    .bind(organization_id.into_uuid())
    .bind(actor.principal_id.into_uuid())
    .bind(membership_id)
    .bind(actor.actor_type)
    .bind(actor.id.as_str())
    .fetch_one(&mut *connection)
    .await?;
    if !actor_matches {
        return Err(AppError::BadGateway);
    }

    Ok(())
}
