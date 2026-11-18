//! Personal and team boards. Board settings are versioned; creation has actor durable retry key.
use super::{
    AppState,
    extract::{Idempotency, StrictJson, StrictPath, StrictQuery},
};
use crate::{
    application::{
        accounts::{account_field_error, ensure_reachable, remember},
        boards::require,
        ports::VerifiedActor,
    },
    domain::ActorId,
    error::AppError,
};
use axum::{Json, extract::State, http::HeaderMap};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgConnection;
use uuid::Uuid;

type Data = Result<Json<Value>, AppError>;
fn invalid(field: &str, message: &str) -> AppError {
    AppError::Validation {
        details: json!({(field):message}),
    }
}
fn name(text: &str) -> Result<&str, AppError> {
    let n = text.trim();
    if n.is_empty() || n.chars().count() > 120 || n.chars().any(char::is_control) {
        Err(invalid("name", "Enter a name of 1 to 120 characters."))
    } else {
        Ok(n)
    }
}
async fn seed_defaults(tx: &mut PgConnection, actor: &VerifiedActor) -> Result<(), AppError> {
    // The unique home key is also the creation marker. Renames and deleted starter categories survive revisits.
    sqlx::query("INSERT INTO commit.boards(id,name,owner_account,home_account) VALUES($1,'Personal',$2,$2) ON CONFLICT(home_account) DO NOTHING")
        .bind(Uuid::now_v7()).bind(actor.uuid().as_str()).execute(&mut *tx).await?;
    for (team, title) in &actor.team_names {
        let id:Option<Uuid>=sqlx::query_scalar("INSERT INTO commit.boards(id,name,home_team) VALUES($1,$2,$3) ON CONFLICT(home_team) DO NOTHING RETURNING id")
            .bind(Uuid::now_v7()).bind(title).bind(team).fetch_optional(&mut *tx).await?;
        if let Some(id) = id {
            add_category(tx, id, title).await?;
        }
    }
    Ok(())
}
async fn add_category(tx: &mut PgConnection, id: Uuid, name: &str) -> Result<(), AppError> {
    sqlx::query("INSERT INTO commit.board_categories(id,board_id,name) VALUES($1,$2,$3) ON CONFLICT DO NOTHING")
        .bind(Uuid::now_v7()).bind(id).bind(name).execute(tx).await?;
    Ok(())
}
pub(super) async fn list(State(state): State<AppState>, headers: HeaderMap) -> Data {
    let actor = state
        .authenticate(&headers, "commit.boards.read", None)
        .await?;
    let mut tx = state.pool.begin().await?;
    seed_defaults(&mut tx, &actor).await?;
    let items:Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(b)||jsonb_build_object('role',CASE WHEN commit.board_role(id,$1,$2)=2 THEN 'admin' ELSE 'member' END) FROM commit.boards b WHERE commit.board_role(id,$1,$2)>0 ORDER BY (home_account IS NULL),parent_id NULLS FIRST,lower(name),id")
        .bind(actor.uuid().as_str()).bind(&actor.teams).fetch_all(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(json!({"items":items})))
}
pub(super) async fn get(
    State(state): State<AppState>,
    headers: HeaderMap,
    StrictPath(id): StrictPath<Uuid>,
) -> Data {
    let actor = state
        .authenticate(&headers, "commit.boards.read", None)
        .await?;
    let mut tx = state.pool.begin().await?;
    let role = require(&mut tx, &actor, id, 1).await?;
    let value:Value=sqlx::query_scalar(r"SELECT to_jsonb(b)||jsonb_build_object('role',$2::text,
      'categories',(SELECT coalesce(jsonb_agg(to_jsonb(c) ORDER BY lower(c.name)),'[]') FROM commit.board_categories c WHERE c.board_id=b.id),
      'members',(SELECT coalesce(jsonb_agg(jsonb_build_object('subject',m.subject,'role',m.role,'id',a.public_id) ORDER BY m.subject),'[]')
       FROM commit.board_members m LEFT JOIN commit.accounts a ON a.uuid=m.subject WHERE m.board_id=b.id))
      FROM commit.boards b WHERE id=$1").bind(id).bind(if role==2{"admin"}else{"member"}).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(value))
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NewBoard {
    name: String,
    parent_id: Option<Uuid>,
}
pub(super) async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Idempotency(key): Idempotency,
    StrictJson(input): StrictJson<NewBoard>,
) -> Data {
    let actor = state
        .authenticate(&headers, "commit.boards.manage", None)
        .await?;
    let n = name(&input.name)?;
    let mut tx = state.pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(format!("{}:{}", actor.uuid(), key.as_str()))
        .execute(&mut *tx)
        .await?;
    if let Some(parent) = input.parent_id {
        require(&mut tx, &actor, parent, 2).await?;
    }
    let body = json!(input);
    if let Some((existing, id)) = sqlx::query_as::<_, (Value, Uuid)>(
        "SELECT input,result FROM commit.board_operations WHERE actor=$1 AND operation_key=$2",
    )
    .bind(actor.uuid().as_str())
    .bind(key.as_str())
    .fetch_optional(&mut *tx)
    .await?
    {
        if existing != body {
            return Err(AppError::Conflict {
                code: "idempotency_conflict".into(),
            });
        }
        require(&mut tx, &actor, id, 1).await?;
        return Ok(Json(json!({"id":id})));
    }
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO commit.boards(id,parent_id,name,owner_account) VALUES($1,$2,$3,$4)")
        .bind(id)
        .bind(input.parent_id)
        .bind(n)
        .bind(input.parent_id.is_none().then(|| actor.uuid().as_str()))
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO commit.board_operations VALUES($1,$2,$3,$4)")
        .bind(actor.uuid().as_str())
        .bind(key.as_str())
        .bind(body)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(json!({"id":id})))
}
#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Change {
    Settings {
        name: Option<String>,
        progress_mode: Option<String>,
    },
    Invite {
        subject: String,
        role: String,
    },
    Remove {
        subject: String,
    },
    CategoryAdd {
        name: String,
    },
    CategoryEdit {
        category_id: Uuid,
        name: String,
    },
    CategoryDelete {
        category_id: Uuid,
    },
}
#[derive(Deserialize)]
pub(super) struct BoardChange {
    revision: i64,
    #[serde(flatten)]
    change: Change,
}
pub(super) async fn update(
    State(state): State<AppState>,
    headers: HeaderMap,
    StrictPath(id): StrictPath<Uuid>,
    StrictJson(input): StrictJson<BoardChange>,
) -> Data {
    let actor = state
        .authenticate(&headers, "commit.boards.manage", None)
        .await?;
    let mut tx = state.pool.begin().await?;
    let revision: i64 =
        sqlx::query_scalar("SELECT revision FROM commit.boards WHERE id=$1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(AppError::NotFound)?;
    require(&mut tx, &actor, id, 2).await?;
    if revision != input.revision {
        return Err(AppError::Conflict {
            code: "board_changed".into(),
        });
    }
    match input.change {
        Change::Settings {
            name: title,
            progress_mode: mode,
        } => {
            if let Some(n) = &title {
                name(n)?;
            }
            if mode
                .as_deref()
                .is_some_and(|v| !matches!(v, "count" | "time" | "hidden"))
            {
                return Err(invalid("progress_mode", "Use count, time or hidden."));
            }
            sqlx::query("UPDATE commit.boards SET name=coalesce($2,name),progress_mode=coalesce($3,progress_mode) WHERE id=$1").bind(id).bind(title.as_deref().map(str::trim)).bind(mode).execute(&mut *tx).await?;
        }
        Change::Invite { subject, role } => {
            if !matches!(role.as_str(), "member" | "admin") {
                return Err(invalid("role", "Use member or admin."));
            }
            let team = subject.starts_with("team:");
            let subject = if team {
                if !actor.shareable_teams.contains(&subject) {
                    return Err(AppError::Forbidden);
                }
                subject
            } else {
                let lookup = ActorId::new(subject.trim())
                    .map_err(|_| invalid("subject", "Enter an account ID or a team target."))?;
                let person = state
                    .identity
                    .resolve_account(&lookup, None)
                    .await
                    .map_err(|e| account_field_error("subject", e))?;
                remember(&mut tx, &actor, std::slice::from_ref(&person)).await?;
                ensure_reachable(&mut tx, &actor, &person.actor, "subject").await?;
                person.actor.uuid.as_str().to_owned()
            };
            let existed:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM commit.board_members WHERE board_id=$1 AND subject=$2)").bind(id).bind(&subject).fetch_one(&mut *tx).await?;
            sqlx::query("INSERT INTO commit.board_members(board_id,subject,role) VALUES($1,$2,$3) ON CONFLICT(board_id,subject) DO UPDATE SET role=excluded.role").bind(id).bind(&subject).bind(role).execute(&mut *tx).await?;
            if !existed {
                if team {
                    if let Some(n) = actor.team_names.get(&subject) {
                        add_category(&mut tx, id, n).await?;
                    }
                } else if subject != actor.uuid().as_str() {
                    sqlx::query("INSERT INTO commit.board_invitations(id,board_id,sender,recipient) VALUES($1,$2,$3,$4)").bind(Uuid::now_v7()).bind(id).bind(actor.uuid().as_str()).bind(subject).execute(&mut *tx).await?;
                }
            }
        }
        Change::Remove { subject } => {
            sqlx::query("DELETE FROM commit.board_members WHERE board_id=$1 AND subject=$2")
                .bind(id)
                .bind(subject)
                .execute(&mut *tx)
                .await?;
        }
        Change::CategoryAdd { name: n } => {
            let names = crate::domain::categories(vec![n])?;
            add_category(&mut tx, id, &names[0]).await?;
        }
        Change::CategoryEdit {
            category_id,
            name: n,
        } => {
            let names = crate::domain::categories(vec![n])?;
            change_category(&mut tx, id, category_id, Some(&names[0])).await?;
        }
        Change::CategoryDelete { category_id } => {
            change_category(&mut tx, id, category_id, None).await?;
        }
    }
    sqlx::query("UPDATE commit.boards SET revision=revision+1 WHERE id=$1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(json!({"id":id,"revision":revision+1})))
}
async fn change_category(
    db: &mut PgConnection,
    board: Uuid,
    id: Uuid,
    new: Option<&str>,
) -> Result<(), AppError> {
    let previous: String =
        sqlx::query_scalar("SELECT name FROM commit.board_categories WHERE board_id=$1 AND id=$2")
            .bind(board)
            .bind(id)
            .fetch_optional(&mut *db)
            .await?
            .ok_or(AppError::NotFound)?;
    if let Some(n) = new {
        let conflict:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM commit.board_categories WHERE board_id=$1 AND id<>$2 AND lower(name)=lower($3))").bind(board).bind(id).bind(n).fetch_one(&mut *db).await?;
        if conflict {
            return Err(AppError::Conflict {
                code: "category_name_in_use".into(),
            });
        }
        sqlx::query("UPDATE commit.board_categories SET name=$2 WHERE id=$1")
            .bind(id)
            .bind(n)
            .execute(&mut *db)
            .await?;
    } else {
        sqlx::query("DELETE FROM commit.board_categories WHERE id=$1")
            .bind(id)
            .execute(&mut *db)
            .await?;
    }
    for query in [
        "UPDATE commit.todos SET categories=commit.rename_category(categories,$2,$3) WHERE board_id=$1 AND deleted_at IS NULL AND EXISTS(SELECT 1 FROM unnest(categories) c WHERE lower(c)=lower($2))",
        "UPDATE commit.projects SET categories=commit.rename_category(categories,$2,$3) WHERE board_id=$1 AND deleted_at IS NULL AND EXISTS(SELECT 1 FROM unnest(categories) c WHERE lower(c)=lower($2))",
    ] {
        sqlx::query(query)
            .bind(board)
            .bind(&previous)
            .bind(new)
            .execute(&mut *db)
            .await?;
    }
    Ok(())
}
#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct WorkQuery {
    board_id: Option<Uuid>,
    mine: bool,
    owner: Option<String>,
    assignee: Option<String>,
    category: Option<String>,
    status: Option<String>,
    kind: Option<String>,
    limit: Option<i32>,
    offset: i32,
}
pub(super) async fn work(
    State(state): State<AppState>,
    headers: HeaderMap,
    StrictQuery(filters): StrictQuery<WorkQuery>,
) -> Data {
    let actor = state
        .authenticate(&headers, "commit.work.read", None)
        .await?;
    let mut tx = state.pool.begin().await?;
    if let Some(id) = filters.board_id {
        require(&mut tx, &actor, id, 1).await?;
    }
    if filters
        .status
        .as_deref()
        .is_some_and(|v| !matches!(v, "open" | "active" | "completed" | "blocked" | "canceled"))
        || filters
            .kind
            .as_deref()
            .is_some_and(|v| !matches!(v, "project" | "task" | "todo"))
        || filters.offset < 0
    {
        return Err(invalid("filter", "Unknown work filter or negative offset."));
    }
    let result = sqlx::query_scalar::<_, Value>(include_str!("work.sql"))
        .bind(actor.uuid().as_str())
        .bind(&actor.teams)
        .bind(filters.board_id)
        .bind(filters.mine)
        .bind(filters.owner)
        .bind(filters.assignee)
        .bind(filters.category)
        .bind(filters.status)
        .bind(filters.kind)
        .bind(filters.limit.unwrap_or(60).clamp(1, 200))
        .bind(filters.offset)
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(result))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Links {
    todo_ids: Vec<crate::domain::TodoId>,
    project_ids: Vec<crate::domain::ProjectId>,
    #[serde(default)]
    remove: bool,
}
pub(super) async fn links(
    State(state): State<AppState>,
    headers: HeaderMap,
    StrictJson(input): StrictJson<Links>,
) -> Data {
    let actor = state
        .authenticate(&headers, "commit.todos.update", None)
        .await?;
    state
        .todos
        .link_projects(
            &actor,
            &input.todo_ids,
            &input.project_ids,
            input.remove,
            &super::required_request_id()?,
        )
        .await?;
    Ok(Json(json!({"updated":true})))
}
