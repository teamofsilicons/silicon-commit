//! Transactional testing-environment generation and capacity checks.
use crate::{error::AppError, request_context};
use sqlx::PgConnection;

/// Serializes sandbox writes with cleanup, rotation and deletion.
pub async fn guard(connection: &mut PgConnection) -> Result<(), AppError> {
    let Some(scope) = request_context::testing_scope() else {
        return Ok(());
    };
    let found: Option<uuid::Uuid> = sqlx::query_scalar("SELECT environment_id FROM commit.testing_environments WHERE environment_id=$1 AND version=$2 AND status='active' FOR UPDATE")
        .bind(scope.id).bind(scope.version).fetch_optional(connection).await?;
    if found.is_none() {
        return Err(AppError::Unauthenticated);
    }
    Ok(())
}
/// Enforces the environment-wide limit after replay lookup inside its locked transaction.
pub async fn capacity(connection: &mut PgConnection, projects: bool) -> Result<(), AppError> {
    let Some(scope) = request_context::testing_scope() else {
        return Ok(());
    };
    let sql = if projects {
        "SELECT count(*) FROM commit.projects WHERE organization_id IN (SELECT storage_organization_id FROM commit.testing_organizations WHERE environment_id=$1)"
    } else {
        "SELECT count(*) FROM commit.todos WHERE organization_id IN (SELECT storage_organization_id FROM commit.testing_organizations WHERE environment_id=$1)"
    };
    let count: i64 = sqlx::query_scalar(sql)
        .bind(scope.id)
        .fetch_one(connection)
        .await?;
    if count >= if projects { 10 } else { 100 } {
        return Err(AppError::Conflict {
            code: if projects {
                "test_project_limit"
            } else {
                "test_todo_limit"
            }
            .into(),
        });
    }
    Ok(())
}
