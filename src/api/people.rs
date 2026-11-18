//! Account-scoped invitation signals and the signed-in account'state Silicon Contacts.
use super::{
    AppState, auth,
    extract::{StrictJson, StrictPath, StrictQuery},
};
use crate::{
    application::{
        accounts::{account_field_error, authentication_error},
        ports::{InboundCredential, VerifiedActor},
    },
    domain::ActorId,
    error::AppError,
    infrastructure::clients::collaboration::Collaboration,
};
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, header},
    response::{IntoResponse, Response},
};
use reqwest::Method;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};
use uuid::Uuid;

type Data = Result<Json<Value>, AppError>;

fn gateway(state: &AppState) -> Result<&Collaboration, AppError> {
    state
        .collaboration
        .as_ref()
        .ok_or(AppError::ProviderUnavailable)
}
async fn caller(
    state: &AppState,
    headers: &HeaderMap,
    scope: &'static str,
) -> Result<(VerifiedActor, SecretString), AppError> {
    let req = auth::request(headers, scope, false)?;
    let actor = state
        .identity
        .authenticate(&req)
        .await
        .map_err(authentication_error)?;
    match req.credential {
        InboundCredential::Bearer(token) => Ok((actor, token)),
        InboundCredential::Proof(_) => Err(AppError::Denied {
            code: "commit_sign_in_required".into(),
            message: "Use your Commit access token for this operation.".into(),
        }),
    }
}
async fn connected(state: &AppState, actor: &VerifiedActor) -> Result<bool, AppError> {
    Ok(sqlx::query_scalar(
        "SELECT exists(SELECT 1 FROM commit.contact_connections WHERE account_uuid=$1)",
    )
    .bind(actor.uuid().as_str())
    .fetch_one(&state.pool)
    .await?)
}
async fn proof(
    state: &AppState,
    token: &SecretString,
    write: bool,
) -> Result<SecretString, AppError> {
    state
        .identity
        .contacts_proof(token.expose_secret(), write)
        .await
        .map_err(authentication_error)
}
async fn require_connection(state: &AppState, actor: &VerifiedActor) -> Result<(), AppError> {
    if connected(state, actor).await? {
        Ok(())
    } else {
        Err(AppError::Denied {
            code: "contacts_not_connected".into(),
            message: "Connect Silicon Contacts to use your address book in Commit.".into(),
        })
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Connection {
    enabled: bool,
}
pub(super) async fn connection(State(state): State<AppState>, headers: HeaderMap) -> Data {
    let (actor, _) = caller(&state, &headers, "commit.contacts.read").await?;
    Ok(Json(json!({"connected":connected(&state,&actor).await?})))
}
pub(super) async fn connect(
    State(state): State<AppState>,
    headers: HeaderMap,
    StrictJson(body): StrictJson<Connection>,
) -> Data {
    let (actor, access) = caller(&state, &headers, "commit.contacts.write").await?;
    if body.enabled {
        let proof_token = proof(&state, &access, false).await?;
        gateway(&state)?
            .contacts_json(proof_token.expose_secret(), Method::GET, &["me"], &[], None)
            .await?;
        sqlx::query("INSERT INTO commit.contact_connections VALUES($1) ON CONFLICT DO NOTHING")
            .bind(actor.uuid().as_str())
            .execute(&state.pool)
            .await?;
    } else {
        sqlx::query("DELETE FROM commit.contact_connections WHERE account_uuid=$1")
            .bind(actor.uuid().as_str())
            .execute(&state.pool)
            .await?;
    }
    Ok(Json(json!({"connected":body.enabled})))
}
#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub(super) struct Search {
    q: String,
    limit: Option<usize>,
    offset: usize,
}
pub(super) async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    StrictQuery(query): StrictQuery<Search>,
) -> Data {
    let (actor, access) = caller(&state, &headers, "commit.contacts.read").await?;
    if !connected(&state, &actor).await? {
        return Ok(Json(json!({"connected":false,"items":[],"total":0})));
    }
    let proof_token = proof(&state, &access, false).await?;
    let mut page = gateway(&state)?
        .contacts_json(
            proof_token.expose_secret(),
            Method::GET,
            &["contacts"],
            &[
                ("q", query.q),
                (
                    "limit",
                    query.limit.unwrap_or(100).clamp(1, 200).to_string(),
                ),
                ("offset", query.offset.to_string()),
            ],
            None,
        )
        .await?;
    page["connected"] = json!(true);
    Ok(Json(page))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ContactInput {
    id: String,
    name: String,
    trust: u8,
}
pub(super) async fn save(
    State(state): State<AppState>,
    headers: HeaderMap,
    StrictJson(body): StrictJson<ContactInput>,
) -> Data {
    let (actor, access) = caller(&state, &headers, "commit.contacts.write").await?;
    require_connection(&state, &actor).await?;
    if body.name.trim().is_empty() || body.name.len() > 200 || body.trust == 0 {
        return Err(AppError::Validation {
            details: json!({"contact":"A name and trust from 1 to 255 are required."}),
        });
    }
    let client = gateway(&state)?;
    let read_proof = proof(&state, &access, false).await?;
    let account = client
        .contacts_json(
            read_proof.expose_secret(),
            Method::GET,
            &["lookup"],
            &[("q", body.id)],
            None,
        )
        .await?;
    let uuid = account["uuid"].as_str().ok_or(AppError::BadGateway)?;
    let mut record = match client
        .contacts_json(
            read_proof.expose_secret(),
            Method::GET,
            &["contacts", uuid],
            &[],
            None,
        )
        .await
    {
        Ok(old) => old,
        Err(AppError::NotFound) => json!({}),
        Err(e) => return Err(e),
    };
    let fields = [
        "note",
        "photo_id",
        "phone",
        "email",
        "birthday_month",
        "birthday_day",
        "birthday_year",
    ];
    let obj = record.as_object_mut().ok_or(AppError::BadGateway)?;
    obj.retain(|key, _| fields.contains(&key.as_str()));
    obj.insert("id".into(), json!(uuid));
    obj.insert("name".into(), json!(body.name.trim()));
    obj.insert("trust".into(), json!(body.trust));
    let proof_token = proof(&state, &access, true).await?;
    Ok(Json(
        client
            .contacts_json(
                proof_token.expose_secret(),
                Method::POST,
                &["contacts"],
                &[],
                Some(&record),
            )
            .await?,
    ))
}
pub(super) async fn photo(
    State(state): State<AppState>,
    headers: HeaderMap,
    StrictPath(id): StrictPath<Uuid>,
) -> Result<Response, AppError> {
    let (actor, access) = caller(&state, &headers, "commit.contacts.read").await?;
    require_connection(&state, &actor).await?;
    let proof_token = proof(&state, &access, false).await?;
    let bytes = gateway(&state)?
        .contacts(
            proof_token.expose_secret(),
            Method::GET,
            &["photos", &id.to_string()],
            &[],
            None,
        )
        .await?;
    Ok(([(header::CONTENT_TYPE, "image/png")], bytes).into_response())
}
pub(super) async fn organizations(State(state): State<AppState>, headers: HeaderMap) -> Data {
    let (actor, access) = caller(&state, &headers, "commit.organizations.read").await?;
    let ctx = gateway(&state)?
        .organizations(&state.app_id, access.expose_secret(), actor.uuid().as_str())
        .await?;
    Ok(Json(
        json!({"organizations":ctx.organizations,"groups":ctx.groups,"share_targets":ctx.share_targets}),
    ))
}
#[derive(Clone, Default, Serialize, sqlx::FromRow)]
struct Person {
    uuid: String,
    id: String,
    kind: String,
    display_name: String,
    pfp_url: String,
    sent: i64,
    received: i64,
    last_invited_at: Option<i64>,
    trust: i32,
}
impl Person {
    fn contact(&mut self, contact: &Value) {
        let profile = &contact["account"];
        self.uuid = contact["uuid"].as_str().unwrap_or(&self.uuid).to_owned();
        self.id = profile["id"].as_str().unwrap_or(&self.id).to_owned();
        self.kind = profile["kind"].as_str().unwrap_or(&self.kind).to_owned();
        self.display_name = contact["name"]
            .as_str()
            .filter(|v| !v.is_empty())
            .or(profile["display_name"].as_str())
            .unwrap_or(&self.display_name)
            .to_owned();
        self.pfp_url = contact["photo_id"]
            .as_str()
            .and_then(|state| Uuid::parse_str(state).ok())
            .map_or_else(
                || {
                    profile["pfp_url"]
                        .as_str()
                        .filter(|state| !state.is_empty())
                        .unwrap_or(&self.pfp_url)
                        .to_owned()
                },
                |id| format!("/api/v1/contacts/photos/{id}"),
            );
        self.trust = contact["trust"]
            .as_i64()
            .and_then(|v| i32::try_from(v).ok())
            .unwrap_or(0);
    }
}
fn fuzzy(query: &str, value: &str) -> bool {
    let haystack = value.to_lowercase();
    let mut chars = haystack.chars();
    query
        .chars()
        .filter(|contact| !contact.is_whitespace())
        .all(|wanted| chars.by_ref().any(|contact| contact == wanted))
}
async fn ranked(
    state: &AppState,
    actor: &VerifiedActor,
    access: &SecretString,
) -> Result<Vec<Person>, AppError> {
    let owner = actor.uuid().as_str();
    let rows:Vec<Person>=sqlx::query_as(r"
        WITH invites(sender,recipient,at) AS (
            SELECT added_by_account,participant_account,added_at FROM commit.project_participants WHERE (added_by_account=$1 OR participant_account=$1)
            UNION ALL SELECT sender,recipient,created_at FROM commit.board_invitations WHERE (sender=$1 OR recipient=$1)
        ), peers AS (
            SELECT CASE WHEN sender=$1 THEN recipient ELSE sender END peer,
              count(*) FILTER (WHERE sender=$1) sent,count(*) FILTER (WHERE recipient=$1) received,
              floor(extract(epoch FROM max(at))*1000)::bigint last_invited_at
            FROM invites WHERE sender<>recipient GROUP BY 1)
        SELECT a.uuid,a.public_id AS id,a.kind::text AS kind,a.display_name,a.pfp_url,
          coalesce(p.sent,0)::bigint AS sent,coalesce(p.received,0)::bigint AS received,p.last_invited_at,0::integer AS trust
        FROM commit.accounts a LEFT JOIN peers p ON p.peer=a.uuid WHERE a.uuid<>$1 AND a.status='active'
          AND (p.peer IS NOT NULL OR commit.in_circle($1,a.uuid))").bind(owner).fetch_all(&state.pool).await?;
    let mut people: BTreeMap<_, _> = rows
        .into_iter()
        .map(|person| (person.uuid.clone(), person))
        .collect();
    if connected(state, actor).await? {
        let read_proof = proof(state, access, false).await?;
        let client = gateway(state)?;
        let relations = client
            .contacts_json(
                read_proof.expose_secret(),
                Method::GET,
                &["relationships"],
                &[],
                None,
            )
            .await?;
        let mut blocked: HashSet<String> = HashSet::new();
        for rel in relations["items"].as_array().ok_or(AppError::BadGateway)? {
            if rel["blocked"] == true
                && let Some(id) = rel["uuid"].as_str().or(rel["account"]["uuid"].as_str())
            {
                blocked.insert(id.to_owned());
            }
        }
        let mut offset = 0;
        loop {
            let page = client
                .contacts_json(
                    read_proof.expose_secret(),
                    Method::GET,
                    &["contacts"],
                    &[("offset", offset.to_string()), ("limit", "200".into())],
                    None,
                )
                .await?;
            let items = page["items"].as_array().ok_or(AppError::BadGateway)?;
            for contact in items {
                let uuid = contact["uuid"].as_str().ok_or(AppError::BadGateway)?;
                if contact["blocked"] == true || contact["account"]["status"] == "deleted" {
                    blocked.insert(uuid.to_owned());
                    continue;
                }
                if uuid != owner {
                    people.entry(uuid.to_owned()).or_default().contact(contact);
                }
            }
            offset += items.len();
            if offset
                >= usize::try_from(page["total"].as_u64().ok_or(AppError::BadGateway)?)
                    .map_err(|_| AppError::BadGateway)?
            {
                break;
            }
            if items.is_empty() {
                return Err(AppError::BadGateway);
            }
        }
        people.retain(|id, _| !blocked.contains(id));
    }
    let mut result: Vec<_> = people.into_values().collect();
    result.sort_by(|actor, b| {
        (2 * b.sent + b.received)
            .cmp(&(2 * actor.sent + actor.received))
            .then(b.last_invited_at.cmp(&actor.last_invited_at))
            .then(b.trust.cmp(&actor.trust))
            .then_with(|| actor.id.cmp(&b.id))
            .then_with(|| actor.uuid.cmp(&b.uuid))
    });
    Ok(result)
}
pub(super) async fn suggest(
    State(state): State<AppState>,
    headers: HeaderMap,
    StrictQuery(query): StrictQuery<Search>,
) -> Data {
    let (actor, access) = caller(&state, &headers, "commit.people.read").await?;
    let needle = query.q.trim().to_lowercase();
    let result: Vec<_> = ranked(&state, &actor, &access)
        .await?
        .into_iter()
        .filter(|person| {
            fuzzy(&needle, &person.display_name)
                || fuzzy(&needle, &person.id)
                || person.uuid == needle
        })
        .collect();
    let total = result.len();
    Ok(Json(
        json!({"items":result.into_iter().skip(query.offset).take(query.limit.unwrap_or(24).clamp(1,200)).collect::<Vec<_>>(),"total":total}),
    ))
}
#[derive(Deserialize)]
pub(super) struct Lookup {
    id: String,
}
pub(super) async fn resolve(
    State(state): State<AppState>,
    headers: HeaderMap,
    StrictQuery(query): StrictQuery<Lookup>,
) -> Data {
    let (actor, access) = caller(&state, &headers, "commit.people.read").await?;
    let id = ActorId::new(query.id.trim()).map_err(|_| AppError::Validation {
        details: json!({"id":"Enter an account ID or UUID."}),
    })?;
    let account = state
        .identity
        .resolve_account(&id, None)
        .await
        .map_err(|e| match e {
            crate::application::ports::ProviderError::UnknownAccount { .. } => AppError::NotFound,
            other => account_field_error("id", other),
        })?;
    let mut person = Person {
        uuid: account.actor.uuid.as_str().into(),
        id: account.actor.id.as_str().into(),
        kind: account.actor.actor_type.to_string(),
        display_name: account.display_name,
        pfp_url: account.pfp_url,
        ..Person::default()
    };
    if connected(&state, &actor).await? {
        let read_proof = proof(&state, &access, false).await?;
        let client = gateway(&state)?;
        match client
            .contacts_json(
                read_proof.expose_secret(),
                Method::GET,
                &["contacts", &person.uuid],
                &[],
                None,
            )
            .await
        {
            Ok(saved) => person.contact(&saved),
            Err(AppError::NotFound) => {
                let profile = client
                    .contacts_json(
                        read_proof.expose_secret(),
                        Method::GET,
                        &["lookup"],
                        &[("q", person.uuid.clone())],
                        None,
                    )
                    .await?;
                person.display_name = profile["display_name"]
                    .as_str()
                    .unwrap_or(&person.display_name)
                    .to_owned();
                person.pfp_url = profile["pfp_url"]
                    .as_str()
                    .unwrap_or(&person.pfp_url)
                    .to_owned();
            }
            Err(e) => return Err(e),
        }
    }
    Ok(Json(json!(person)))
}
