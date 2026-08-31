//! HTTP authentication extraction and IAM request construction.

use axum::http::HeaderMap;
use secrecy::SecretString;
use uuid::Uuid;

use crate::{
    application::ports::{
        AuthenticationRequest, CapabilitySet, CredentialError, InboundCredential, OrganizationRole,
        TrustedIdentity,
    },
    config::AuthenticationMode,
    domain::{Actor, ActorId, ActorType, OrganizationId, PrincipalId, PublicOrganizationId},
    error::AppError,
};

/// Stable IAM OBO action names for each Commit operation.
pub mod action {
    /// List todos.
    pub const TODOS_LIST: &str = "commit.todos.list";
    /// Create a todo.
    pub const TODOS_CREATE: &str = "commit.todos.create";
    /// Read one todo.
    pub const TODOS_READ: &str = "commit.todos.read";
    /// Update one todo.
    pub const TODOS_UPDATE: &str = "commit.todos.update";
    /// Delete one todo.
    pub const TODOS_DELETE: &str = "commit.todos.delete";
    /// List todo notes.
    pub const TODO_NOTES_LIST: &str = "commit.todo_notes.list";
    /// Append a todo note.
    pub const TODO_NOTES_CREATE: &str = "commit.todo_notes.create";
    /// List projects.
    pub const PROJECTS_LIST: &str = "commit.projects.list";
    /// Create a project.
    pub const PROJECTS_CREATE: &str = "commit.projects.create";
    /// Read one project.
    pub const PROJECTS_READ: &str = "commit.projects.read";
    /// Update one project.
    pub const PROJECTS_UPDATE: &str = "commit.projects.update";
    /// Read a diary.
    pub const DIARY_READ: &str = "commit.project_diary.read";
    /// Replace a diary.
    pub const DIARY_UPDATE: &str = "commit.project_diary.update";
    /// List project tasks.
    pub const PROJECT_TASKS_LIST: &str = "commit.project_tasks.list";
    /// Create a project task.
    pub const PROJECT_TASKS_CREATE: &str = "commit.project_tasks.create";
    /// Update a project task.
    pub const PROJECT_TASKS_UPDATE: &str = "commit.project_tasks.update";
    /// Add a blocker.
    pub const PROJECT_BLOCKERS_CREATE: &str = "commit.project_blockers.create";
    /// Add a milestone update.
    pub const PROJECT_UPDATES_CREATE: &str = "commit.project_updates.create";
    /// Complete a project.
    pub const PROJECT_COMPLETION_CREATE: &str = "commit.project_completion.create";
    /// Generate a temporary Briefcase URL.
    pub const ATTACHMENTS_TEMPORARY_URL: &str = "commit.attachments.temporary_url";
}

/// Parses authentication headers under the configured runtime safety mode.
///
/// # Errors
///
/// Returns a normalized protocol or authentication error when organization or
/// credential headers are missing, duplicated, malformed, or mixed.
pub fn request(
    headers: &HeaderMap,
    mode: AuthenticationMode,
    action: &str,
    resource: Option<String>,
) -> Result<AuthenticationRequest, AppError> {
    let org_id = required_header(headers, "x-org-id")?
        .parse::<PublicOrganizationId>()
        .map_err(|_| invalid_header("x-org-id"))?;

    let credential = match mode {
        AuthenticationMode::Iam => iam_credential(headers)?,
        AuthenticationMode::TrustedHeaders => trusted_credential(headers, org_id.clone())?,
    };
    Ok(AuthenticationRequest {
        credential,
        org_id,
        action: action.to_owned(),
        resource,
    })
}

fn iam_credential(headers: &HeaderMap) -> Result<InboundCredential, AppError> {
    if TRUSTED_HEADERS
        .iter()
        .any(|name| headers.contains_key(*name))
    {
        return Err(AppError::BadRequest {
            code: "trusted_identity_headers_forbidden".into(),
        });
    }

    let bearer = unique_header(headers, http::header::AUTHORIZATION.as_str())?
        .map(|value| {
            let (scheme, token) = value.split_once(' ').ok_or(AppError::Unauthenticated)?;
            if !scheme.eq_ignore_ascii_case("bearer")
                || token.is_empty()
                || token.bytes().any(|byte| byte.is_ascii_whitespace())
            {
                return Err(AppError::Unauthenticated);
            }
            Ok::<SecretString, AppError>(SecretString::from(token.to_owned()))
        })
        .transpose()?;
    let app_id = optional_header(headers, "x-app-id")?;
    let proof = optional_header(headers, "x-iam-obo-access-proof")?.map(SecretString::from);

    InboundCredential::from_external_parts(bearer, app_id, proof).map_err(map_credential_error)
}

fn trusted_credential(
    headers: &HeaderMap,
    org_id: PublicOrganizationId,
) -> Result<InboundCredential, AppError> {
    if headers.contains_key(http::header::AUTHORIZATION)
        || headers.contains_key("x-app-id")
        || headers.contains_key("x-iam-obo-access-proof")
    {
        return Err(AppError::BadRequest {
            code: "mixed_authentication".into(),
        });
    }

    let organization_id = parse_uuid_header(headers, "x-test-organization-id")?;
    let membership_id = parse_uuid_header(headers, "x-test-membership-id")?;
    let principal_id = parse_uuid_header(headers, "x-test-principal-id")?;
    let actor_type = required_header(headers, "x-test-actor-type")?
        .parse::<ActorType>()
        .map_err(|_| invalid_header("x-test-actor-type"))?;
    let actor_id = required_header(headers, "x-test-actor-id")?
        .parse::<ActorId>()
        .map_err(|_| invalid_header("x-test-actor-id"))?;
    let organization_role = match required_header(headers, "x-test-org-role")? {
        "owner" => OrganizationRole::Owner,
        "admin" => OrganizationRole::Admin,
        "member" => OrganizationRole::Member,
        _ => return Err(invalid_header("x-test-org-role")),
    };
    let capability_names = optional_header(headers, "x-test-capabilities")?
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let capabilities = CapabilitySet::try_from_names(capability_names)
        .map_err(|_| invalid_header("x-test-capabilities"))?;
    if organization_id.is_nil() || membership_id.is_nil() || principal_id.is_nil() {
        return Err(AppError::BadRequest {
            code: "invalid_trusted_identity".into(),
        });
    }

    Ok(InboundCredential::trusted(TrustedIdentity {
        organization_id: OrganizationId::from_uuid(organization_id),
        org_id,
        membership_id,
        actor: Actor::new(PrincipalId::from_uuid(principal_id), actor_type, actor_id),
        organization_role,
        capabilities,
    }))
}

fn map_credential_error(error: CredentialError) -> AppError {
    match error {
        CredentialError::Missing | CredentialError::Malformed => AppError::Unauthenticated,
        CredentialError::Multiple => AppError::BadRequest {
            code: "mixed_authentication".into(),
        },
        CredentialError::IncompleteObo => AppError::BadRequest {
            code: "incomplete_obo_authentication".into(),
        },
    }
}

fn required_header<'a>(headers: &'a HeaderMap, name: &'static str) -> Result<&'a str, AppError> {
    unique_header(headers, name)?.ok_or_else(|| AppError::BadRequest {
        code: format!("{}_required", name.replace('-', "_")).into(),
    })
}

fn optional_header(headers: &HeaderMap, name: &'static str) -> Result<Option<String>, AppError> {
    unique_header(headers, name).map(|value| value.map(str::to_owned))
}

fn unique_header<'a>(
    headers: &'a HeaderMap,
    name: &'static str,
) -> Result<Option<&'a str>, AppError> {
    let mut values = headers.get_all(name).iter();
    let value = values.next();
    if values.next().is_some() {
        return Err(invalid_header(name));
    }
    value
        .map(|value| value.to_str().map_err(|_| invalid_header(name)))
        .transpose()
}

fn parse_uuid_header(headers: &HeaderMap, name: &'static str) -> Result<Uuid, AppError> {
    required_header(headers, name)?
        .parse()
        .map_err(|_| invalid_header(name))
}

fn invalid_header(name: &'static str) -> AppError {
    AppError::BadRequest {
        code: format!("invalid_{}", name.replace('-', "_")).into(),
    }
}

const TRUSTED_HEADERS: [&str; 7] = [
    "x-test-organization-id",
    "x-test-membership-id",
    "x-test-principal-id",
    "x-test-actor-type",
    "x-test-actor-id",
    "x-test-org-role",
    "x-test-capabilities",
];

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue};

    use super::request;
    use crate::config::AuthenticationMode;

    #[test]
    fn iam_mode_rejects_mixed_bearer_and_obo_credentials() {
        let mut headers = HeaderMap::new();
        headers.insert("x-org-id", HeaderValue::from_static("example"));
        headers.insert(
            http::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer opaque"),
        );
        headers.insert("x-app-id", HeaderValue::from_static("silicon-dm"));
        headers.insert("x-iam-obo-access-proof", HeaderValue::from_static("proof"));

        assert!(request(&headers, AuthenticationMode::Iam, "commit.test", None).is_err());
    }
}
