//! Board access is always derived from the caller and its current Organizations teams.
use crate::{application::ports::VerifiedActor, error::AppError};
use sqlx::PgConnection;
use uuid::Uuid;

pub(crate) async fn check_board(
    db: &mut PgConnection,
    actor: &VerifiedActor,
    board: Option<Uuid>,
) -> Result<(), AppError> {
    if let Some(id) = board {
        require(db, actor, id, 1).await?;
    }
    Ok(())
}
pub(crate) async fn require(
    db: &mut PgConnection,
    actor: &VerifiedActor,
    board: Uuid,
    level: i32,
) -> Result<i32, AppError> {
    let role: i32 = sqlx::query_scalar("SELECT commit.board_role($1,$2,$3)")
        .bind(board)
        .bind(actor.uuid().as_str())
        .bind(&actor.teams)
        .fetch_one(db)
        .await?;
    if role == 0 {
        return Err(AppError::NotFound);
    }
    if role < level {
        return Err(AppError::Denied {
            code: "board_admin_required".into(),
            message: "A board administrator can change this setting.".into(),
        });
    }
    Ok(role)
}
