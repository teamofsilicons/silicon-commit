//! Online, fail-closed Silicon IAM adapter using the official client.

use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use async_trait::async_trait;
use secrecy::{ExposeSecret as _, SecretString};
use serde::{Deserialize, Serialize};
use silicon_iam_client::{Client, Credential, EnvironmentKey, Paging, models};
use time::OffsetDateTime;
use uuid::Uuid;

use super::ClientBuildError;
use crate::{
    application::ports::{
        ActiveMember, AuthenticationRequest, CapabilitySet, ChildProofRequest, DelegatedOboProof,
        IdentityProvider, InboundCredential, OrganizationRole, ProviderError, TrustedIdentity,
        VerifiedActor,
    },
    config::IamSettings,
    domain::{
        actor::{Actor, ActorType},
        ids::{ActorId, OrganizationId, PrincipalId, PublicOrganizationId},
    },
    request_context::{self, IamTestingCredentials},
};

const MAX_DIRECTORY_PAGES: usize = 100;
const DIRECTORY_PAGE_SIZE: u16 = 100;

/// Authenticated official IAM client for introspection, scoped directory reads, and OBO.
#[derive(Clone, Debug)]
pub struct IamClient {
    client: Client,
    app_id: String,
    audience: String,
    max_response_bytes: usize,
}

impl IamClient {
    /// Builds a redirect-free official client from validated integration settings.
    ///
    /// The SDK uses the request timeout for connecting as well as reading, and
    /// enforces a 4 MiB wire-body limit. Commit additionally limits decoded
    /// response size using its configured provider limit.
    ///
    /// # Errors
    /// Returns a redacted construction error for invalid credentials or URLs.
    pub fn new(
        settings: &IamSettings,
        _connect_timeout: Duration,
        request_timeout: Duration,
        max_response_bytes: usize,
    ) -> Result<Self, ClientBuildError> {
        let app_id = settings
            .app_id
            .as_deref()
            .ok_or(ClientBuildError::InvalidCredential)?;
        let secret = settings
            .app_secret
            .as_ref()
            .ok_or(ClientBuildError::InvalidCredential)?;
        if app_id.trim().is_empty()
            || settings.audience.trim().is_empty()
            || max_response_bytes == 0
            || secret.expose_secret().is_empty()
            || secret
                .expose_secret()
                .bytes()
                .any(|byte| byte.is_ascii_control())
        {
            return Err(ClientBuildError::InvalidCredential);
        }
        let mut origin = settings.base_url.clone();
        if origin.path().trim_end_matches('/') == "/api/v1" {
            origin.set_path("/");
        }
        let client = Client::builder(origin.as_str())
            .map_err(|_| ClientBuildError::InvalidEndpoint)?
            .credential(Credential::application(app_id, secret.expose_secret()))
            .timeout(request_timeout)
            .user_agent(concat!("silicon-commit/", env!("CARGO_PKG_VERSION")))
            .auto_update(false)
            .telemetry(false)
            .build()
            .map_err(|_| ClientBuildError::HttpClient)?;
        Ok(Self {
            client,
            app_id: app_id.to_owned(),
            audience: settings.audience.clone(),
            max_response_bytes,
        })
    }

    fn request_credentials(&self) -> Result<Option<IamTestingCredentials>, ProviderError> {
        if let Some(credentials) = request_context::current_iam_testing_credentials() {
            if credentials.app_id != self.app_id {
                return Err(ProviderError::Unauthenticated);
            }
            return Ok(Some(credentials));
        }
        if request_context::current_environment_key().is_some()
            || request_context::testing_scope().is_some()
        {
            return Err(ProviderError::Unauthenticated);
        }
        Ok(None)
    }

    fn request_client(&self) -> Result<Client, ProviderError> {
        let Some(credentials) = self.request_credentials()? else {
            return Ok(self.client.clone());
        };
        let client = self.client.with_credential(Credential::application(
            &credentials.app_id,
            credentials.app_secret.expose_secret(),
        ));
        if credentials.environment_key.expose_secret().is_empty() {
            client
                .with_testing_application(
                    &credentials.app_id,
                    credentials.app_secret.expose_secret(),
                )
                .map_err(|_| ProviderError::Unauthenticated)
        } else {
            let environment = EnvironmentKey::new(credentials.environment_key.expose_secret())
                .map_err(|_| ProviderError::Unauthenticated)?;
            Ok(client.with_environment(environment))
        }
    }

    fn directory_client(&self) -> Result<Client, ProviderError> {
        let token =
            request_context::current_iam_bearer_token().ok_or(ProviderError::Unauthenticated)?;
        Ok(self
            .request_client()?
            .with_credential(Credential::bearer(token.expose_secret())))
    }

    fn bounded<T: Serialize>(&self, value: T) -> Result<T, ProviderError> {
        let size = serde_json::to_vec(&value)
            .map_err(|_| ProviderError::InvalidResponse)?
            .len();
        if size > self.max_response_bytes {
            return Err(ProviderError::InvalidResponse);
        }
        Ok(value)
    }

    fn projection<T: for<'de> Deserialize<'de>>(
        &self,
        value: serde_json::Value,
    ) -> Result<T, ProviderError> {
        serde_json::from_value(self.bounded(value)?).map_err(|_| ProviderError::InvalidResponse)
    }

    async fn authenticate_bearer(
        &self,
        token: &SecretString,
        request: &AuthenticationRequest,
    ) -> Result<VerifiedActor, ProviderError> {
        request_context::set_iam_bearer_token(Some(token.clone()));
        let client = self.request_client()?;
        let introspection = self.bounded(
            client
                .oauth()
                .introspect(
                    &models::TokenIntrospectionRequest {
                        token: token.expose_secret().to_owned(),
                        token_type_hint: None,
                    },
                    Some(request.org_id.as_str()),
                )
                .await
                .map_err(map_sdk_error)
                .map_err(map_authentication_provider_error)?,
        )?;
        if !introspection.active {
            return Err(ProviderError::Unauthenticated);
        }
        if introspection
            .expires_at
            .is_none_or(|expiry| expiry <= OffsetDateTime::now_utc().unix_timestamp())
            || introspection.org_id.as_deref() != Some(request.org_id.as_str())
            || introspection.audience.as_deref() != Some(self.audience.as_str())
        {
            return Err(ProviderError::Unauthenticated);
        }
        // A live snapshot binds disclosure to this exact token and organization.
        // Never replace absent authority with a broader administrative directory read.
        let snapshot = introspection
            .authorization
            .ok_or(ProviderError::InvalidResponse)?;
        if introspection.principal_id != Some(snapshot.principal_id)
            || introspection.membership_id.as_deref() != Some(snapshot.membership_id.as_str())
        {
            return Err(ProviderError::Unauthenticated);
        }
        let top_kind = match introspection.actor_type {
            Some(models::TokenIntrospectionActorType::Carbon) => ActorType::Carbon,
            Some(models::TokenIntrospectionActorType::Silicon) => ActorType::Silicon,
            None => return Err(ProviderError::Forbidden),
            Some(_) => return Err(ProviderError::InvalidResponse),
        };
        let actor = self.verified_snapshot(snapshot, request)?;
        if actor.actor.actor_type != top_kind {
            return Err(ProviderError::Unauthenticated);
        }
        Ok(actor)
    }

    fn verified_snapshot(
        &self,
        snapshot: models::ApplicationAuthorization,
        request: &AuthenticationRequest,
    ) -> Result<VerifiedActor, ProviderError> {
        if snapshot.audience != self.audience || snapshot.org_id != request.org_id.as_str() {
            return Err(ProviderError::Unauthenticated);
        }
        let credentials = self.request_credentials()?;
        if credentials.is_none() && snapshot.testing_environment_id.is_some()
            || credentials
                .as_ref()
                .is_some_and(|c| c.environment_key.expose_secret().is_empty())
                && snapshot.testing_environment_id
                    != request_context::testing_scope().map(|scope| scope.id)
        {
            return Err(ProviderError::Unauthenticated);
        }
        if snapshot.principal_id.is_nil() || snapshot.organization_id.is_nil() {
            return Err(ProviderError::InvalidResponse);
        }
        let actor_type = match snapshot.actor_type {
            Some(models::ApplicationAuthorizationActorType::Carbon) => ActorType::Carbon,
            Some(models::ApplicationAuthorizationActorType::Silicon) => ActorType::Silicon,
            None => return Err(ProviderError::Forbidden),
            Some(_) => return Err(ProviderError::InvalidResponse),
        };
        let actor_id = ActorId::new(snapshot.public_id.ok_or(ProviderError::Forbidden)?)
            .map_err(|_| ProviderError::InvalidResponse)?;
        validate_membership(&snapshot.membership_id, &actor_id, &request.org_id)?;
        let role = parse_organization_role(
            snapshot
                .org_role
                .as_deref()
                .ok_or(ProviderError::Forbidden)?,
        )?;
        Ok(VerifiedActor::new(
            OrganizationId::from_uuid(snapshot.organization_id),
            request.org_id.clone(),
            snapshot.membership_id,
            Actor::new(
                PrincipalId::from_uuid(snapshot.principal_id),
                actor_type,
                actor_id,
            ),
            role,
            CapabilitySet::default(),
            request.credential.clone(),
        )
        .with_tags(
            snapshot
                .tags
                .unwrap_or_default()
                .into_iter()
                .flat_map(|tag| [tag.name, tag.id.to_string()])
                .collect(),
        ))
    }

    async fn authenticate_obo(
        &self,
        issuer_app_id: &str,
        proof: &SecretString,
        request: &AuthenticationRequest,
    ) -> Result<VerifiedActor, ProviderError> {
        let binding =
            request_context::current_obo_request_binding().ok_or(ProviderError::Unauthenticated)?;
        let client = self.request_client()?;
        // IAM consumes verification strictly once. SDK verify intentionally sends no
        // retry/idempotency key and checks the actual method, path and raw-body digest.
        let verification = self.bounded(
            client
                .obo()
                .verify(&models::OboVerifyRequest {
                    access_proof: proof.expose_secret().to_owned(),
                    request: binding.clone(),
                })
                .await
                .map_err(map_sdk_error)
                .map_err(map_obo_provider_error)?,
        )?;
        let now = OffsetDateTime::now_utc();
        if verification.proof_id.is_nil()
            || verification.expires_at > now + time::Duration::seconds(60)
            || verification.consumed_at > now + time::Duration::seconds(1)
        {
            return Err(ProviderError::InvalidResponse);
        }
        if verification.valid != serde_json::Value::Bool(true)
            || verification.expires_at <= now
            || verification.issuer_app_id != issuer_app_id
            || verification.audience != self.audience
            || verification.org_id != request.org_id.as_str()
            || verification.endpoint.path != binding.path
        {
            return Err(ProviderError::Unauthenticated);
        }
        let represented = verification.actor;
        let actor = self.verified_snapshot(verification.authorization, request)?;
        let kind = match represented.type_field {
            models::ActorRefType::Carbon => ActorType::Carbon,
            models::ActorRefType::Silicon => ActorType::Silicon,
            _ => return Err(ProviderError::InvalidResponse),
        };
        if represented.principal_id != *actor.actor.principal_id.as_uuid()
            || represented.public_id != actor.actor.id.as_str()
            || kind != actor.actor.actor_type
        {
            return Err(ProviderError::Unauthenticated);
        }
        request_context::set_verified_iam_member(ActiveMember {
            organization_id: actor.organization_id,
            org_id: actor.org_id.clone(),
            membership_id: actor.membership_id.clone(),
            actor: actor.actor.clone(),
        });
        Ok(actor)
    }

    async fn active_members_by_actor(
        &self,
        org_id: &PublicOrganizationId,
        actor_ids: &[ActorId],
        required_type: Option<ActorType>,
    ) -> Result<Vec<ActiveMember>, ProviderError> {
        let mut requested_order = Vec::with_capacity(actor_ids.len());
        let mut requested_ids = HashSet::with_capacity(actor_ids.len());
        for id in actor_ids {
            if requested_ids.insert(id.clone()) {
                requested_order.push(id.clone());
            }
        }
        if requested_order.is_empty() {
            return Ok(Vec::new());
        }
        if let Some(member) = request_context::current_verified_iam_member() {
            if member.org_id == *org_id
                && requested_order.len() == 1
                && requested_order[0] == member.actor.id
                && required_type.is_none_or(|kind| kind == member.actor.actor_type)
            {
                return Ok(vec![member]);
            }
            // A consumed OBO proof discloses its represented member, not a
            // reusable directory credential. Never elevate it to application
            // or administrative authority to resolve additional assignees.
            if request_context::current_iam_bearer_token().is_none() {
                return Err(ProviderError::Forbidden);
            }
        }
        let client = self.directory_client()?;
        let organization: OrganizationResponse = self.projection(
            client
                .application_reads()
                .organization(org_id.as_str())
                .await
                .map_err(map_sdk_error)?,
        )?;
        let organization_id = organization.internal_id(org_id)?;
        let requested_memberships: HashSet<_> = requested_order
            .iter()
            .map(|actor| format!("{}[{}]", actor.as_str(), org_id.as_str()))
            .collect();
        let mut cursor = None::<String>;
        let mut seen_cursors = HashSet::new();
        let mut matched = HashMap::with_capacity(requested_order.len());
        for _ in 0..MAX_DIRECTORY_PAGES {
            let mut paging = Paging::new().limit(DIRECTORY_PAGE_SIZE);
            if let Some(cursor) = &cursor {
                paging = paging.after(cursor);
            }
            // Scoped reads preserve omission. A management Membership model would
            // incorrectly require unrelated profile, hierarchy, and tag disclosure.
            let page: MembershipPage = self.projection(
                client
                    .application_reads()
                    .members(org_id.as_str(), &paging)
                    .await
                    .map_err(map_sdk_error)?,
            )?;
            if page.items.len() > usize::from(DIRECTORY_PAGE_SIZE) {
                return Err(ProviderError::InvalidResponse);
            }
            let next_cursor = page.next_cursor()?;
            for member in page.items {
                if !requested_memberships.contains(&member.id) {
                    continue;
                }
                if member.status.as_deref() == Some("removed") {
                    continue;
                }
                let member = member.into_active(org_id, organization_id)?;
                if required_type.is_some_and(|kind| kind != member.actor.actor_type) {
                    continue;
                }
                if matched.insert(member.actor.id.clone(), member).is_some() {
                    return Err(ProviderError::InvalidResponse);
                }
            }
            let Some(next_cursor) = next_cursor else {
                if matched.len() != requested_order.len() {
                    return Err(ProviderError::NotFound);
                }
                return requested_order
                    .into_iter()
                    .map(|actor| matched.remove(&actor).ok_or(ProviderError::InvalidResponse))
                    .collect();
            };
            if !seen_cursors.insert(next_cursor.clone()) {
                return Err(ProviderError::InvalidResponse);
            }
            cursor = Some(next_cursor);
        }
        Err(ProviderError::InvalidResponse)
    }
}

#[async_trait]
impl IdentityProvider for IamClient {
    async fn authenticate(
        &self,
        request: &AuthenticationRequest,
    ) -> Result<VerifiedActor, ProviderError> {
        match &request.credential {
            InboundCredential::Bearer(token) => self.authenticate_bearer(token, request).await,
            InboundCredential::Obo { app_id, proof } => {
                self.authenticate_obo(app_id, proof, request).await
            }
            InboundCredential::Trusted(_) => Err(ProviderError::Unauthenticated),
        }
    }
    async fn resolve_active_members(
        &self,
        org_id: &PublicOrganizationId,
        actor_ids: &[ActorId],
        required_type: Option<ActorType>,
    ) -> Result<Vec<ActiveMember>, ProviderError> {
        self.active_members_by_actor(org_id, actor_ids, required_type)
            .await
    }
    async fn exchange_child_proof(
        &self,
        _actor: &VerifiedActor,
        _request: &ChildProofRequest,
    ) -> Result<DelegatedOboProof, ProviderError> {
        // This retired Briefcase interface has no exact downstream request binding.
        // No Commit operation calls it; it cannot safely issue a current IAM proof.
        Err(ProviderError::Forbidden)
    }
}

#[derive(Debug, Deserialize)]
struct OrganizationResponse {
    id: Uuid,
    org_id: String,
    #[serde(default, deserialize_with = "disclosed_status")]
    status: Option<String>,
}

fn disclosed_status<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    String::deserialize(deserializer).map(Some)
}

impl OrganizationResponse {
    fn internal_id(&self, org_id: &PublicOrganizationId) -> Result<OrganizationId, ProviderError> {
        if self.id.is_nil() || self.org_id != org_id.as_str() {
            return Err(ProviderError::InvalidResponse);
        }
        if let Some(status) = &self.status {
            require_active_directory_status(status, "disabled")?;
        }
        Ok(OrganizationId::from_uuid(self.id))
    }
}

#[derive(Debug, Deserialize)]
struct MembershipResponse {
    id: String,
    org_id: Option<String>,
    principal: Option<DirectoryActor>,
    #[serde(default, deserialize_with = "disclosed_status")]
    status: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DirectoryActor {
    principal_id: Uuid,
    #[serde(rename = "type")]
    actor_type: String,
    public_id: String,
}

impl MembershipResponse {
    fn into_active(
        self,
        org_id: &PublicOrganizationId,
        organization_id: OrganizationId,
    ) -> Result<ActiveMember, ProviderError> {
        if self.org_id.as_deref().ok_or(ProviderError::Forbidden)? != org_id.as_str() {
            return Err(ProviderError::InvalidResponse);
        }
        require_active_directory_status(
            self.status.as_deref().ok_or(ProviderError::Forbidden)?,
            "removed",
        )?;
        let principal = self.principal.ok_or(ProviderError::Forbidden)?;
        if principal.principal_id.is_nil() {
            return Err(ProviderError::InvalidResponse);
        }
        let actor_type = principal
            .actor_type
            .parse()
            .map_err(|_| ProviderError::InvalidResponse)?;
        let actor_id =
            ActorId::new(principal.public_id).map_err(|_| ProviderError::InvalidResponse)?;
        validate_membership(&self.id, &actor_id, org_id)?;
        Ok(ActiveMember {
            organization_id,
            org_id: org_id.clone(),
            membership_id: self.id,
            actor: Actor::new(
                PrincipalId::from_uuid(principal.principal_id),
                actor_type,
                actor_id,
            ),
        })
    }
}

#[derive(Debug, Deserialize)]
struct MembershipPage {
    items: Vec<MembershipResponse>,
    page: PageInfo,
}
#[derive(Debug, Deserialize)]
struct PageInfo {
    next_cursor: Option<String>,
    has_more: bool,
}
impl MembershipPage {
    fn next_cursor(&self) -> Result<Option<String>, ProviderError> {
        match (self.page.has_more, self.page.next_cursor.as_deref()) {
            (false, None) => Ok(None),
            (true, Some(cursor))
                if !cursor.is_empty()
                    && cursor.len() <= 2_048
                    && !cursor.chars().any(char::is_control) =>
            {
                Ok(Some(cursor.to_owned()))
            }
            _ => Err(ProviderError::InvalidResponse),
        }
    }
}

fn validate_membership(
    membership: &str,
    actor: &ActorId,
    org: &PublicOrganizationId,
) -> Result<(), ProviderError> {
    if membership != format!("{}[{}]", actor.as_str(), org.as_str()) {
        return Err(ProviderError::InvalidResponse);
    }
    Ok(())
}
fn require_active_directory_status(
    status: &str,
    inactive_status: &str,
) -> Result<(), ProviderError> {
    match status {
        "active" => Ok(()),
        status if status == inactive_status => Err(ProviderError::NotFound),
        _ => Err(ProviderError::InvalidResponse),
    }
}
fn parse_organization_role(value: &str) -> Result<OrganizationRole, ProviderError> {
    match value {
        "owner" => Ok(OrganizationRole::Owner),
        "admin" => Ok(OrganizationRole::Admin),
        "member" => Ok(OrganizationRole::Member),
        _ => Err(ProviderError::InvalidResponse),
    }
}
fn map_sdk_error(error: silicon_iam_client::Error) -> ProviderError {
    use silicon_iam_client::Error;
    match error {
        Error::Api(error) if error.code == "invalid_client" => ProviderError::Unavailable,
        Error::Api(error) => match error.status {
            401 => ProviderError::Unauthenticated,
            403 => ProviderError::Forbidden,
            404 | 410 => ProviderError::NotFound,
            409 => ProviderError::Conflict,
            _ => ProviderError::Unavailable,
        },
        Error::RateLimited { retry_after, .. } => ProviderError::RateLimited {
            retry_after: Some(retry_after),
        },
        Error::Decode(_)
        | Error::ResponseTooLarge { .. }
        | Error::ApiVersionUnsupported { .. }
        | Error::UnstructuredResponse { .. } => ProviderError::InvalidResponse,
        _ => ProviderError::Unavailable,
    }
}
fn map_obo_provider_error(error: ProviderError) -> ProviderError {
    match error {
        ProviderError::NotFound | ProviderError::Conflict => ProviderError::Unauthenticated,
        error => error,
    }
}

fn map_authentication_provider_error(error: ProviderError) -> ProviderError {
    match error {
        ProviderError::NotFound | ProviderError::Conflict => ProviderError::Unauthenticated,
        ProviderError::Forbidden => ProviderError::Unavailable,
        error => error,
    }
}

/// Explicit non-production identity provider. Its optional directory is fixed
/// at construction so trusted headers cannot invent assignment targets.
#[derive(Clone, Debug, Default)]
pub struct TrustedHeaderIdentityProvider {
    directory: Vec<ActiveMember>,
}

impl TrustedHeaderIdentityProvider {
    /// Creates a trusted provider with a fixed test/development member directory.
    #[must_use]
    pub fn new(directory: Vec<ActiveMember>) -> Self {
        Self { directory }
    }
}

#[async_trait]
impl IdentityProvider for TrustedHeaderIdentityProvider {
    async fn authenticate(
        &self,
        request: &AuthenticationRequest,
    ) -> Result<VerifiedActor, ProviderError> {
        let InboundCredential::Trusted(TrustedIdentity {
            organization_id,
            org_id,
            membership_id,
            actor,
            organization_role,
            capabilities,
        }) = &request.credential
        else {
            return Err(ProviderError::Unauthenticated);
        };
        if org_id != &request.org_id {
            return Err(ProviderError::Unauthenticated);
        }
        Ok(VerifiedActor::new(
            *organization_id,
            org_id.clone(),
            membership_id.clone(),
            actor.clone(),
            *organization_role,
            capabilities.clone(),
            request.credential.clone(),
        ))
    }

    async fn resolve_active_members(
        &self,
        org_id: &PublicOrganizationId,
        actor_ids: &[ActorId],
        required_type: Option<ActorType>,
    ) -> Result<Vec<ActiveMember>, ProviderError> {
        let mut requested_order = Vec::with_capacity(actor_ids.len());
        let mut requested_ids = HashSet::with_capacity(actor_ids.len());
        for actor_id in actor_ids {
            if requested_ids.insert(actor_id.clone()) {
                requested_order.push(actor_id.clone());
            }
        }
        if requested_order.is_empty() {
            return Ok(Vec::new());
        }

        let mut matched = HashMap::with_capacity(requested_order.len());
        for member in &self.directory {
            if &member.org_id == org_id
                && requested_ids.contains(&member.actor.id)
                && required_type.is_none_or(|kind| member.actor.actor_type == kind)
                && matched
                    .insert(member.actor.id.clone(), member.clone())
                    .is_some()
            {
                return Err(ProviderError::InvalidResponse);
            }
        }
        if matched.len() != requested_order.len() {
            return Err(ProviderError::NotFound);
        }
        requested_order
            .into_iter()
            .map(|actor_id| {
                matched
                    .remove(&actor_id)
                    .ok_or(ProviderError::InvalidResponse)
            })
            .collect()
    }

    async fn exchange_child_proof(
        &self,
        _actor: &VerifiedActor,
        _request: &ChildProofRequest,
    ) -> Result<DelegatedOboProof, ProviderError> {
        Err(ProviderError::Forbidden)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use serde_json::{Value, json};
    use std::{
        future::Future,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };
    use time::{OffsetDateTime, format_description::well_known::Rfc3339};
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_json, header, method, path, query_param, query_param_is_missing},
    };

    type TestResult = Result<(), Box<dyn std::error::Error>>;
    const PRINCIPAL: &str = "018f268d-715a-7b72-8f0f-41f16f9af570";
    const ORGANIZATION: &str = "018f268d-715a-7b72-8f0f-41f16f9af572";

    fn client(server: &MockServer) -> Result<IamClient, ClientBuildError> {
        IamClient::new(
            &IamSettings {
                mode: crate::config::AuthenticationMode::Iam,
                base_url: server
                    .uri()
                    .parse()
                    .map_err(|_| ClientBuildError::InvalidEndpoint)?,
                app_id: Some("tos>commit".into()),
                app_secret: Some("application-secret".into()),
                audience: "tos>commit".into(),
                webhook_secret: None,
                webhook_key_version: 1,
            },
            Duration::from_secs(1),
            Duration::from_secs(2),
            16_384,
        )
    }
    fn request() -> Result<AuthenticationRequest, Box<dyn std::error::Error>> {
        Ok(AuthenticationRequest {
            credential: InboundCredential::Bearer("oat_test".into()),
            org_id: "test-org".parse()?,
            action: "commit.todos.list".into(),
            resource: None,
        })
    }
    fn snapshot() -> Value {
        json!({"principal_id": PRINCIPAL, "organization_id": ORGANIZATION,
            "membership_id":"test-carbon[test-org]", "actor_type":"carbon", "public_id":"test-carbon",
            "org_id":"test-org", "audience":"tos>commit", "membership_version":1,
            "authorization_epoch":1, "testing_environment_id":null,
            "scopes":["self.identity.read","self.membership.read"],"org_role":"owner","tags":null})
    }
    fn introspection() -> Value {
        json!({"active":true,"principal_id":PRINCIPAL,"membership_id":"test-carbon[test-org]",
            "actor_type":"carbon","org_id":"test-org","audience":"tos>commit",
            "expires_at":OffsetDateTime::now_utc().unix_timestamp()+300,"authorization":snapshot()})
    }
    async fn introspect_mock(server: &MockServer, body: Value) {
        Mock::given(method("POST"))
            .and(path("/api/v1/oauth/introspect"))
            .and(header("x-org-id", "test-org"))
            .and(header(
                "user-agent",
                "silicon-iam-client/2.0.0 silicon-commit/0.2.0",
            ))
            .and(header(
                "authorization",
                format!("Basic {}", STANDARD.encode("tos>commit:application-secret")),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .expect(1)
            .mount(server)
            .await;
    }
    async fn as_user<T>(future: impl Future<Output = T>) -> T {
        request_context::scope("iam-test".into(), async {
            request_context::set_iam_bearer_token(Some("oat_directory".into()));
            future.await
        })
        .await
    }
    fn organization() -> Value {
        json!({"id":ORGANIZATION,"org_id":"test-org"})
    }
    fn member(id: &str) -> Value {
        json!({"id":format!("{id}[test-org]"),"org_id":"test-org","status":"active",
            "principal":{"principal_id":PRINCIPAL,"type":"carbon","public_id":id}})
    }
    fn page(items: Vec<Value>, next: Option<&str>) -> Value {
        json!({"items":items,"page":{"next_cursor":next,"has_more":next.is_some()}})
    }
    async fn organization_mock(server: &MockServer, body: Value) {
        Mock::given(method("GET"))
            .and(path("/api/v1/organizations/test-org"))
            .and(header("authorization", "Bearer oat_directory"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .expect(1)
            .mount(server)
            .await;
    }
    async fn members_mock(server: &MockServer, body: Value) {
        Mock::given(method("GET"))
            .and(path("/api/v1/organizations/test-org/members"))
            .and(header("authorization", "Bearer oat_directory"))
            .and(query_param("limit", "100"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .expect(1)
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn official_introspection_accepts_public_membership_without_directory_reads() -> TestResult
    {
        let server = MockServer::start().await;
        introspect_mock(&server, introspection()).await;
        let actor = client(&server)?.authenticate(&request()?).await?;
        assert_eq!(actor.membership_id, "test-carbon[test-org]");
        assert_eq!(actor.actor.id.as_str(), "test-carbon");
        assert_eq!(actor.organization_role, OrganizationRole::Owner);
        assert_eq!(
            server.received_requests().await.ok_or("no requests")?.len(),
            1
        );
        Ok(())
    }

    #[tokio::test]
    async fn introspection_rejects_inactive_expired_and_mismatched_bindings() -> TestResult {
        for (pointer, replacement, expected) in [
            ("/active", json!(false), ProviderError::Unauthenticated),
            ("/expires_at", json!(1), ProviderError::Unauthenticated),
            (
                "/org_id",
                json!("other-org"),
                ProviderError::Unauthenticated,
            ),
            (
                "/audience",
                json!("tos>other"),
                ProviderError::Unauthenticated,
            ),
            (
                "/principal_id",
                json!(ORGANIZATION),
                ProviderError::Unauthenticated,
            ),
            (
                "/membership_id",
                json!("another[test-org]"),
                ProviderError::Unauthenticated,
            ),
            (
                "/actor_type",
                json!("silicon"),
                ProviderError::Unauthenticated,
            ),
            (
                "/authorization/org_id",
                json!("other-org"),
                ProviderError::Unauthenticated,
            ),
            (
                "/authorization/audience",
                json!("tos>other"),
                ProviderError::Unauthenticated,
            ),
            (
                "/authorization/public_id",
                json!("another"),
                ProviderError::InvalidResponse,
            ),
            (
                "/authorization/organization_id",
                json!(Uuid::nil()),
                ProviderError::InvalidResponse,
            ),
            (
                "/authorization/testing_environment_id",
                json!(ORGANIZATION),
                ProviderError::Unauthenticated,
            ),
            (
                "/authorization/org_role",
                json!("superuser"),
                ProviderError::InvalidResponse,
            ),
            (
                "/authorization",
                Value::Null,
                ProviderError::InvalidResponse,
            ),
        ] {
            let server = MockServer::start().await;
            let mut body = introspection();
            *body.pointer_mut(pointer).ok_or("missing field")? = replacement;
            introspect_mock(&server, body).await;
            let actual = client(&server)?.authenticate(&request()?).await;
            assert!(
                matches!(actual,Err(error) if error==expected),
                "{pointer}: {actual:?}"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn undisclosed_identity_and_role_are_forbidden_not_gateway_errors() -> TestResult {
        for field in ["actor_type", "public_id", "org_role"] {
            let server = MockServer::start().await;
            let mut body = introspection();
            body["authorization"]
                .as_object_mut()
                .ok_or("snapshot not object")?
                .remove(field);
            introspect_mock(&server, body).await;
            assert!(
                matches!(
                    client(&server)?.authenticate(&request()?).await,
                    Err(ProviderError::Forbidden)
                ),
                "{field}"
            );
        }
        let server = MockServer::start().await;
        let mut body = introspection();
        body.as_object_mut()
            .ok_or("not object")?
            .remove("actor_type");
        introspect_mock(&server, body).await;
        assert!(matches!(
            client(&server)?.authenticate(&request()?).await,
            Err(ProviderError::Forbidden)
        ));
        Ok(())
    }

    #[tokio::test]
    async fn legacy_uuid_membership_is_rejected_as_noncanonical() -> TestResult {
        let server = MockServer::start().await;
        let mut body = introspection();
        body["membership_id"] = json!(PRINCIPAL);
        body["authorization"]["membership_id"] = json!(PRINCIPAL);
        introspect_mock(&server, body).await;
        assert!(matches!(
            client(&server)?.authenticate(&request()?).await,
            Err(ProviderError::InvalidResponse)
        ));
        Ok(())
    }

    #[tokio::test]
    async fn canonical_silicon_identity_preserves_full_org_qualified_id() -> TestResult {
        let server = MockServer::start().await;
        let mut body = introspection();
        body["membership_id"] = json!("helper:test-org[test-org]");
        body["actor_type"] = json!("silicon");
        body["authorization"]["membership_id"] = body["membership_id"].clone();
        body["authorization"]["actor_type"] = json!("silicon");
        body["authorization"]["public_id"] = json!("helper:test-org");
        body["authorization"]["org_role"] = json!("member");
        introspect_mock(&server, body).await;
        assert_eq!(
            client(&server)?
                .authenticate(&request()?)
                .await?
                .membership_id,
            "helper:test-org[test-org]"
        );
        Ok(())
    }

    #[tokio::test]
    async fn sdk_failures_preserve_authentication_and_provider_distinction() -> TestResult {
        for (status, expected) in [
            (401, ProviderError::Unauthenticated),
            (403, ProviderError::Unavailable),
            (404, ProviderError::Unauthenticated),
            (409, ProviderError::Unauthenticated),
            (503, ProviderError::Unavailable),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/api/v1/oauth/introspect"))
                .respond_with(ResponseTemplate::new(status).set_body_json(json!({"error":{
                    "code":"test_failure","message":"Expected test failure"}})))
                .expect(1)
                .mount(&server)
                .await;
            assert!(
                matches!(client(&server)?.authenticate(&request()?).await,Err(error) if error==expected),
                "{status}"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn malformed_proxy_json_and_oversized_projections_fail_closed() -> TestResult {
        for response in [
            ResponseTemplate::new(200).set_body_string("not json"),
            ResponseTemplate::new(502).set_body_string("upstream proxy failure"),
            ResponseTemplate::new(200).set_body_json({
                let mut body = introspection();
                body["scope"] = json!("x".repeat(17000));
                body
            }),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/api/v1/oauth/introspect"))
                .respond_with(response)
                .expect(1)
                .mount(&server)
                .await;
            assert!(matches!(
                client(&server)?.authenticate(&request()?).await,
                Err(ProviderError::InvalidResponse)
            ));
        }
        Ok(())
    }

    #[tokio::test]
    async fn directory_resolves_public_assignees_without_role_profile_or_tag_fields() -> TestResult
    {
        let server = MockServer::start().await;
        organization_mock(&server, organization()).await;
        members_mock(&server, page(vec![member("second"), member("first")], None)).await;
        let ids = vec!["first".parse()?, "second".parse()?, "first".parse()?];
        let members =
            as_user(client(&server)?.resolve_active_members(&"test-org".parse()?, &ids, None))
                .await?;
        assert_eq!(
            members
                .iter()
                .map(|m| m.actor.id.as_str())
                .collect::<Vec<_>>(),
            ["first", "second"]
        );
        assert_eq!(members[0].membership_id, "first[test-org]");
        Ok(())
    }

    #[tokio::test]
    async fn directory_requires_active_matching_disclosed_identity() -> TestResult {
        for (pointer, replacement, expected) in [
            (
                "/org_id",
                json!("other-org"),
                ProviderError::InvalidResponse,
            ),
            ("/org_id", Value::Null, ProviderError::Forbidden),
            ("/principal", Value::Null, ProviderError::Forbidden),
            (
                "/principal/principal_id",
                json!(Uuid::nil()),
                ProviderError::InvalidResponse,
            ),
            (
                "/principal/public_id",
                json!("impostor"),
                ProviderError::InvalidResponse,
            ),
            (
                "/principal/type",
                json!("unknown"),
                ProviderError::InvalidResponse,
            ),
            ("/status", json!("unknown"), ProviderError::InvalidResponse),
            ("/status", Value::Null, ProviderError::InvalidResponse),
            ("/status", json!("removed"), ProviderError::NotFound),
        ] {
            let server = MockServer::start().await;
            let mut m = member("test-carbon");
            *m.pointer_mut(pointer).ok_or("missing field")? = replacement;
            organization_mock(&server, organization()).await;
            members_mock(&server, page(vec![m], None)).await;
            let actual = as_user(client(&server)?.resolve_active_members(
                &"test-org".parse()?,
                &["test-carbon".parse()?],
                None,
            ))
            .await;
            assert!(
                matches!(actual,Err(error) if error==expected),
                "{pointer}: {actual:?}"
            );
        }
        let server = MockServer::start().await;
        let mut m = member("test-carbon");
        m.as_object_mut().ok_or("not object")?.remove("status");
        organization_mock(&server, organization()).await;
        members_mock(&server, page(vec![m], None)).await;
        assert!(matches!(
            as_user(client(&server)?.resolve_active_members(
                &"test-org".parse()?,
                &["test-carbon".parse()?],
                None
            ))
            .await,
            Err(ProviderError::Forbidden)
        ));
        Ok(())
    }

    #[tokio::test]
    async fn directory_validates_organization_metadata_without_requiring_redacted_status()
    -> TestResult {
        for (field, value, expected) in [
            ("id", json!(Uuid::nil()), ProviderError::InvalidResponse),
            ("org_id", json!("other-org"), ProviderError::InvalidResponse),
            ("status", json!("disabled"), ProviderError::NotFound),
            ("status", Value::Null, ProviderError::InvalidResponse),
        ] {
            let server = MockServer::start().await;
            let mut org = organization();
            org[field] = value;
            organization_mock(&server, org).await;
            assert!(
                matches!(as_user(client(&server)?.resolve_active_members(&"test-org".parse()?,&["test-carbon".parse()?],None)).await,Err(error) if error==expected)
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn directory_walks_all_pages_and_rejects_duplicate_or_looping_results() -> TestResult {
        for duplicate in [false, true] {
            let server = MockServer::start().await;
            organization_mock(&server, organization()).await;
            Mock::given(method("GET"))
                .and(path("/api/v1/organizations/test-org/members"))
                .and(query_param_is_missing("cursor"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_json(page(vec![member("test-carbon")], Some("next"))),
                )
                .expect(1)
                .mount(&server)
                .await;
            let second = if duplicate {
                vec![member("test-carbon")]
            } else {
                vec![member("unrelated")]
            };
            Mock::given(method("GET"))
                .and(path("/api/v1/organizations/test-org/members"))
                .and(query_param("cursor", "next"))
                .respond_with(ResponseTemplate::new(200).set_body_json(page(second, None)))
                .expect(1)
                .mount(&server)
                .await;
            let result = as_user(client(&server)?.resolve_active_members(
                &"test-org".parse()?,
                &["test-carbon".parse()?],
                None,
            ))
            .await;
            if duplicate {
                assert!(matches!(result, Err(ProviderError::InvalidResponse)));
            } else {
                assert_eq!(result?.len(), 1);
            }
        }
        let server = MockServer::start().await;
        organization_mock(&server, organization()).await;
        Mock::given(method("GET"))
            .and(path("/api/v1/organizations/test-org/members"))
            .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![], Some("same"))))
            .expect(2)
            .mount(&server)
            .await;
        assert!(matches!(
            as_user(client(&server)?.resolve_active_members(
                &"test-org".parse()?,
                &["missing".parse()?],
                None
            ))
            .await,
            Err(ProviderError::InvalidResponse)
        ));
        Ok(())
    }

    #[tokio::test]
    async fn empty_directory_lookup_needs_no_credentials_or_requests() -> TestResult {
        let server = MockServer::start().await;
        assert!(
            client(&server)?
                .resolve_active_members(&"test-org".parse()?, &[], None)
                .await?
                .is_empty()
        );
        assert!(
            server
                .received_requests()
                .await
                .ok_or("missing requests")?
                .is_empty()
        );
        Ok(())
    }

    #[tokio::test]
    async fn missing_directory_identity_does_not_return_partial_matches() -> TestResult {
        let server = MockServer::start().await;
        organization_mock(&server, organization()).await;
        members_mock(&server, page(vec![member("first")], None)).await;
        assert!(matches!(
            as_user(client(&server)?.resolve_active_members(
                &"test-org".parse()?,
                &["first".parse()?, "missing".parse()?],
                None
            ))
            .await,
            Err(ProviderError::NotFound)
        ));
        Ok(())
    }

    fn testing_credentials(root: bool) -> IamTestingCredentials {
        IamTestingCredentials {
            app_id: "tos>commit".into(),
            app_secret: format!("ask_{}", "a".repeat(43)).into(),
            environment_key: if root {
                "a".repeat(32).into()
            } else {
                "".into()
            },
        }
    }

    #[tokio::test]
    async fn testing_selector_and_environment_binding_survive_sdk_auth_and_bearer_switch()
    -> TestResult {
        for root in [false, true] {
            let server = MockServer::start().await;
            let credentials = testing_credentials(root);
            let environment = Uuid::parse_str(ORGANIZATION)?;
            let mut body = introspection();
            body["authorization"]["testing_environment_id"] = json!(environment);
            let auth = format!(
                "Basic {}",
                STANDARD.encode(format!(
                    "tos>commit:{}",
                    credentials.app_secret.expose_secret()
                ))
            );
            let selector = if root {
                credentials.environment_key.expose_secret().to_owned()
            } else {
                auth.clone()
            };
            let selector_name = if root {
                "x-testing-environment-key"
            } else {
                "x-testing-application"
            };
            Mock::given(method("POST"))
                .and(path("/api/v1/oauth/introspect"))
                .and(header("authorization", auth))
                .and(header(selector_name, selector.clone()))
                .respond_with(ResponseTemplate::new(200).set_body_json(body))
                .expect(1)
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/api/v1/organizations/test-org"))
                .and(header("authorization", "Bearer oat_test"))
                .and(header(selector_name, selector.clone()))
                .respond_with(ResponseTemplate::new(200).set_body_json(organization()))
                .expect(1)
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/api/v1/organizations/test-org/members"))
                .and(header("authorization", "Bearer oat_test"))
                .and(header(selector_name, selector))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_json(page(vec![member("test-carbon")], None)),
                )
                .expect(1)
                .mount(&server)
                .await;
            request_context::scope("testing".into(), async {
                request_context::set_iam_testing_credentials(Some(credentials));
                request_context::set_testing_scope(Some(request_context::TestingScope {
                    id: environment,
                    version: 1,
                }));
                let adapter = client(&server)?;
                let req = request()?;
                assert_eq!(
                    adapter.authenticate(&req).await?.membership_id,
                    "test-carbon[test-org]"
                );
                assert_eq!(
                    adapter
                        .resolve_active_members(&req.org_id, &["test-carbon".parse()?], None)
                        .await?
                        .len(),
                    1
                );
                Ok::<_, Box<dyn std::error::Error>>(())
            })
            .await?;
        }
        Ok(())
    }

    #[tokio::test]
    async fn unpaired_testing_context_and_wrong_app_never_fall_back_to_production() -> TestResult {
        for wrong_app in [false, true] {
            let server = MockServer::start().await;
            let adapter = client(&server)?;
            let req = request()?;
            let result = request_context::scope("testing".into(), async {
                request_context::set_testing_scope(Some(request_context::TestingScope {
                    id: Uuid::nil(),
                    version: 1,
                }));
                if wrong_app {
                    let mut credentials = testing_credentials(false);
                    credentials.app_id = "tos>other".into();
                    request_context::set_iam_testing_credentials(Some(credentials));
                }
                adapter.authenticate(&req).await
            })
            .await;
            assert!(matches!(result, Err(ProviderError::Unauthenticated)));
            assert!(
                server
                    .received_requests()
                    .await
                    .ok_or("missing requests")?
                    .is_empty()
            );
        }
        Ok(())
    }

    fn binding() -> models::OboVerifyRequestBinding {
        models::OboVerifyRequestBinding {
            method: "GET".into(),
            path: "/api/v1/todos".into(),
            body_sha256: silicon_iam_client::api::obo::body_sha256(b""),
        }
    }
    fn obo_request() -> Result<AuthenticationRequest, Box<dyn std::error::Error>> {
        let mut req = request()?;
        req.credential = InboundCredential::Obo {
            app_id: "tos>interface".into(),
            proof: "obo_test".into(),
        };
        Ok(req)
    }
    fn verification() -> Result<Value, Box<dyn std::error::Error>> {
        Ok(
            json!({"valid":true,"proof_id":ORGANIZATION,"issuer_app_id":"tos>interface","audience":"tos>commit",
            "org_id":"test-org","actor":{"principal_id":PRINCIPAL,"type":"carbon","public_id":"test-carbon"},
            "authorization":snapshot(),"endpoint":{"endpoint_id":"todos-list","path":"/api/v1/todos"},
            "metadata":{},"expires_at":(OffsetDateTime::now_utc()+time::Duration::seconds(45)).format(&Rfc3339)?,
            "consumed_at":OffsetDateTime::now_utc().format(&Rfc3339)?}),
        )
    }
    async fn as_obo<T>(future: impl Future<Output = T>) -> T {
        request_context::scope("obo".into(), async {
            request_context::set_obo_request_binding(binding());
            future.await
        })
        .await
    }

    #[tokio::test]
    async fn obo_uses_exact_observed_request_and_snapshot_without_directory_reads() -> TestResult {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/obo-access/verify"))
            .and(body_json(
                json!({"access_proof":"obo_test","request":binding()}),
            ))
            .and(|req: &wiremock::Request| !req.headers.contains_key("idempotency-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(verification()?))
            .expect(1)
            .mount(&server)
            .await;
        assert_eq!(
            as_obo(client(&server)?.authenticate(&obo_request()?))
                .await?
                .membership_id,
            "test-carbon[test-org]"
        );
        assert_eq!(
            server
                .received_requests()
                .await
                .ok_or("missing requests")?
                .len(),
            1
        );
        Ok(())
    }

    #[tokio::test]
    async fn obo_refuses_missing_actual_binding_and_replayed_proof() -> TestResult {
        let server = MockServer::start().await;
        let adapter = client(&server)?;
        let req = obo_request()?;
        assert!(matches!(
            adapter.authenticate(&req).await,
            Err(ProviderError::Unauthenticated)
        ));
        let successes = Arc::new(AtomicUsize::new(0));
        let response = verification()?;
        Mock::given(method("POST")).and(path("/api/v1/obo-access/verify"))
            .respond_with(move|_:&wiremock::Request| if successes.fetch_add(1,Ordering::SeqCst)==0 {
                ResponseTemplate::new(200).set_body_json(response.clone())
            } else {ResponseTemplate::new(409).set_body_json(json!({"error":{"code":"obo_proof_consumed","message":"Proof already consumed"}}))})
            .expect(2).mount(&server).await;
        assert!(as_obo(adapter.authenticate(&req)).await.is_ok());
        assert!(matches!(
            as_obo(adapter.authenticate(&req)).await,
            Err(ProviderError::Unauthenticated)
        ));
        Ok(())
    }

    #[tokio::test]
    async fn obo_rejects_actor_scope_and_endpoint_mismatches() -> TestResult {
        for (pointer, value, expected) in [
            (
                "/issuer_app_id",
                json!("tos>other"),
                ProviderError::Unauthenticated,
            ),
            (
                "/audience",
                json!("tos>other"),
                ProviderError::Unauthenticated,
            ),
            (
                "/org_id",
                json!("other-org"),
                ProviderError::Unauthenticated,
            ),
            (
                "/actor/public_id",
                json!("impostor"),
                ProviderError::Unauthenticated,
            ),
            (
                "/actor/principal_id",
                json!(ORGANIZATION),
                ProviderError::Unauthenticated,
            ),
            (
                "/endpoint/path",
                json!("/api/v1/projects"),
                ProviderError::Unauthenticated,
            ),
            (
                "/authorization/org_role",
                Value::Null,
                ProviderError::Forbidden,
            ),
            (
                "/authorization/public_id",
                Value::Null,
                ProviderError::Forbidden,
            ),
            (
                "/proof_id",
                json!(Uuid::nil()),
                ProviderError::InvalidResponse,
            ),
        ] {
            let server = MockServer::start().await;
            let mut body = verification()?;
            *body.pointer_mut(pointer).ok_or("missing field")? = value;
            Mock::given(method("POST"))
                .and(path("/api/v1/obo-access/verify"))
                .respond_with(ResponseTemplate::new(200).set_body_json(body))
                .expect(1)
                .mount(&server)
                .await;
            let result = as_obo(client(&server)?.authenticate(&obo_request()?)).await;
            assert!(
                matches!(result,Err(error) if error==expected),
                "{pointer}: {result:?}"
            );
        }
        Ok(())
    }
    #[tokio::test]
    async fn retired_child_delegation_never_issues_unbound_proofs() -> TestResult {
        let server = MockServer::start().await;
        let adapter = client(&server)?;
        let actor = adapter.verified_snapshot(serde_json::from_value(snapshot())?, &request()?)?;
        let result = adapter
            .exchange_child_proof(
                &actor,
                &ChildProofRequest::briefcase_temporary_url(Uuid::nil()),
            )
            .await;
        assert!(matches!(result, Err(ProviderError::Forbidden)));
        assert!(
            server
                .received_requests()
                .await
                .ok_or("missing requests")?
                .is_empty()
        );
        Ok(())
    }

    #[tokio::test]
    async fn directory_rejects_invalid_pagination_and_oversized_pages() -> TestResult {
        for body in [
            json!({"items":[],"page":{"has_more":true,"next_cursor":null}}),
            json!({"items":[],"page":{"has_more":false,"next_cursor":"unexpected"}}),
            json!({"items":[],"page":{"has_more":true,"next_cursor":""}}),
            page(
                (0..101).map(|_| json!({"id":"other[test-org]"})).collect(),
                None,
            ),
        ] {
            let server = MockServer::start().await;
            organization_mock(&server, organization()).await;
            members_mock(&server, body).await;
            assert!(matches!(
                as_user(client(&server)?.resolve_active_members(
                    &"test-org".parse()?,
                    &["test-carbon".parse()?],
                    None
                ))
                .await,
                Err(ProviderError::InvalidResponse)
            ));
        }
        Ok(())
    }

    #[tokio::test]
    async fn directory_bounds_unique_cursor_walk_and_filters_actor_type() -> TestResult {
        let server = MockServer::start().await;
        organization_mock(&server, organization()).await;
        let count = Arc::new(AtomicUsize::new(0));
        Mock::given(method("GET"))
            .and(path("/api/v1/organizations/test-org/members"))
            .respond_with(move |_: &wiremock::Request| {
                let cursor = count.fetch_add(1, Ordering::SeqCst).to_string();
                ResponseTemplate::new(200).set_body_json(page(vec![], Some(&cursor)))
            })
            .expect(100)
            .mount(&server)
            .await;
        assert!(matches!(
            as_user(client(&server)?.resolve_active_members(
                &"test-org".parse()?,
                &["test-carbon".parse()?],
                None
            ))
            .await,
            Err(ProviderError::InvalidResponse)
        ));
        let server = MockServer::start().await;
        organization_mock(&server, organization()).await;
        members_mock(&server, page(vec![member("test-carbon")], None)).await;
        assert!(matches!(
            as_user(client(&server)?.resolve_active_members(
                &"test-org".parse()?,
                &["test-carbon".parse()?],
                Some(ActorType::Silicon)
            ))
            .await,
            Err(ProviderError::NotFound)
        ));
        Ok(())
    }
    #[tokio::test]
    async fn invalid_application_credentials_are_provider_failure_for_bearer_and_obo() -> TestResult
    {
        for obo in [false, true] {
            let server = MockServer::start().await;
            let endpoint = if obo {
                "/api/v1/obo-access/verify"
            } else {
                "/api/v1/oauth/introspect"
            };
            Mock::given(method("POST"))
                .and(path(endpoint))
                .respond_with(ResponseTemplate::new(401).set_body_json(json!({"error": {
                    "code":"invalid_client", "message":"Application authentication failed"}})))
                .expect(1)
                .mount(&server)
                .await;
            let adapter = client(&server)?;
            let req = if obo { obo_request()? } else { request()? };
            let result = if obo {
                as_obo(adapter.authenticate(&req)).await
            } else {
                adapter.authenticate(&req).await
            };
            assert!(matches!(result, Err(ProviderError::Unavailable)));
        }
        Ok(())
    }

    #[tokio::test]
    async fn obo_permission_denial_remains_forbidden() -> TestResult {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/obo-access/verify"))
            .respond_with(ResponseTemplate::new(403).set_body_json(json!({"error": {
                "code":"forbidden", "message":"The proof cannot authorize this endpoint"}})))
            .expect(1)
            .mount(&server)
            .await;
        assert!(matches!(
            as_obo(client(&server)?.authenticate(&obo_request()?)).await,
            Err(ProviderError::Forbidden)
        ));
        Ok(())
    }
    #[tokio::test]
    async fn obo_self_assignee_uses_verified_membership_and_other_targets_require_bearer()
    -> TestResult {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/obo-access/verify"))
            .respond_with(ResponseTemplate::new(200).set_body_json(verification()?))
            .expect(1)
            .mount(&server)
            .await;
        as_obo(async {
            let adapter = client(&server)?;
            let req = obo_request()?;
            adapter.authenticate(&req).await?;
            let self_id: ActorId = "test-carbon".parse()?;
            let self_members = adapter
                .resolve_active_members(
                    &req.org_id,
                    &[self_id.clone(), self_id.clone()],
                    Some(ActorType::Carbon),
                )
                .await?;
            assert_eq!(self_members.len(), 1);
            assert_eq!(self_members[0].membership_id, "test-carbon[test-org]");
            for (org, ids, kind) in [
                (req.org_id.clone(), vec!["another".parse()?], None),
                (
                    req.org_id.clone(),
                    vec![self_id.clone(), "another".parse()?],
                    None,
                ),
                ("other-org".parse()?, vec![self_id.clone()], None),
                (req.org_id.clone(), vec![self_id], Some(ActorType::Silicon)),
            ] {
                assert!(matches!(
                    adapter.resolve_active_members(&org, &ids, kind).await,
                    Err(ProviderError::Forbidden)
                ));
            }
            Ok::<_, Box<dyn std::error::Error>>(())
        })
        .await?;
        assert_eq!(
            server
                .received_requests()
                .await
                .ok_or("missing requests")?
                .len(),
            1
        );
        assert!(request_context::current_verified_iam_member().is_none());
        Ok(())
    }

    #[tokio::test]
    async fn rejected_obo_actor_never_populates_same_request_membership() -> TestResult {
        let server = MockServer::start().await;
        let mut response = verification()?;
        response["actor"]["public_id"] = json!("impostor");
        Mock::given(method("POST"))
            .and(path("/api/v1/obo-access/verify"))
            .respond_with(ResponseTemplate::new(200).set_body_json(response))
            .expect(1)
            .mount(&server)
            .await;
        as_obo(async {
            assert!(matches!(
                client(&server)?.authenticate(&obo_request()?).await,
                Err(ProviderError::Unauthenticated)
            ));
            assert!(request_context::current_verified_iam_member().is_none());
            Ok::<_, Box<dyn std::error::Error>>(())
        })
        .await?;
        Ok(())
    }
}
