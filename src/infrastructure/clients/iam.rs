//! Online, fail-closed Silicon IAM adapter.

use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use http::{HeaderValue, StatusCode, header};
use secrecy::{ExposeSecret as _, SecretString};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use time::OffsetDateTime;
use url::Url;
use uuid::Uuid;

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

use super::{
    BoundedBodyError, ClientBuildError, endpoint, http_client, is_json, provider_status,
    read_bounded,
};

const MAX_CHILD_PROOF_LIFETIME_SECONDS: u64 = 60;
const DIRECTORY_PAGE_SIZE: &str = "100";
const MAX_DIRECTORY_PAGES: usize = 100;

/// Authenticated HTTP client for IAM introspection, directory, and OBO APIs.
#[derive(Clone, Debug)]
pub struct IamClient {
    client: reqwest::Client,
    introspection_url: Url,
    obo_verify_url: Url,
    obo_exchange_url: Url,
    members_url: Url,
    app_id: String,
    audience: String,
    application_authorization: HeaderValue,
    max_response_bytes: usize,
}

impl IamClient {
    /// Builds a redirect-free client from validated integration settings.
    ///
    /// # Errors
    ///
    /// Returns a redacted construction error when IAM application credentials,
    /// URLs, or the HTTP client are invalid.
    pub fn new(
        settings: &IamSettings,
        connect_timeout: Duration,
        request_timeout: Duration,
        max_response_bytes: usize,
    ) -> Result<Self, ClientBuildError> {
        let app_id = settings
            .app_id
            .as_deref()
            .ok_or(ClientBuildError::InvalidCredential)?;
        let app_secret = settings
            .app_secret
            .as_ref()
            .ok_or(ClientBuildError::InvalidCredential)?;
        if !valid_app_id(app_id) || settings.audience.trim().is_empty() || max_response_bytes == 0 {
            return Err(ClientBuildError::InvalidCredential);
        }

        let application_authorization = basic_authorization(app_id, app_secret)?;
        Ok(Self {
            client: http_client(connect_timeout, request_timeout)?,
            introspection_url: endpoint(&settings.base_url, "oauth/introspect")?,
            obo_verify_url: endpoint(&settings.base_url, "obo-access/verify")?,
            obo_exchange_url: endpoint(&settings.base_url, "obo-access/exchanges")?,
            members_url: endpoint(&settings.base_url, "organizations/")?,
            app_id: app_id.to_owned(),
            audience: settings.audience.clone(),
            application_authorization,
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

    fn request_application_authorization(&self) -> Result<HeaderValue, ProviderError> {
        self.request_credentials()?.map_or_else(
            || Ok(self.application_authorization.clone()),
            |credentials| {
                basic_authorization(&credentials.app_id, &credentials.app_secret)
                    .map_err(|_| ProviderError::Unauthenticated)
            },
        )
    }

    fn apply_testing(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::RequestBuilder, ProviderError> {
        let Some(credentials) = self.request_credentials()? else {
            return Ok(request);
        };
        if credentials.environment_key.expose_secret().is_empty() {
            Ok(request.header(
                "x-testing-application",
                self.request_application_authorization()?,
            ))
        } else {
            Ok(request.header(
                "x-testing-environment-key",
                credentials.environment_key.expose_secret(),
            ))
        }
    }

    async fn authenticate_bearer(
        &self,
        token: &SecretString,
        request: &AuthenticationRequest,
    ) -> Result<VerifiedActor, ProviderError> {
        // Reuse the authenticated user's bearer for subsequent directory
        // reads in this request. No long-lived directory credential is kept
        // by Commit.
        crate::request_context::set_iam_bearer_token(Some(token.clone()));
        let mut outbound = self
            .client
            .post(self.introspection_url.clone())
            .header(
                header::AUTHORIZATION,
                self.request_application_authorization()?,
            )
            .header("x-org-id", org_header(&request.org_id)?)
            .form(&[("token", token.expose_secret())]);
        outbound = self.apply_testing(outbound)?;
        let response = outbound
            .send()
            .await
            .map_err(|_| ProviderError::Unavailable)?;

        let introspection: IntrospectionResponse = self
            .success_json(response, StatusCode::OK)
            .await
            .map_err(map_authentication_provider_error)?;
        let now = OffsetDateTime::now_utc().unix_timestamp();
        if !introspection.active {
            return Err(ProviderError::Unauthenticated);
        }
        let principal_id = required_non_nil(introspection.principal_id)?;
        let membership_id = required_non_nil(introspection.membership_id)?;
        let actor_type = parse_actor_type(introspection.actor_type.as_deref())?;
        let org_id = introspection.org_id.ok_or(ProviderError::InvalidResponse)?;
        if org_id != request.org_id.as_str()
            || introspection
                .expires_at
                .is_none_or(|expires_at| expires_at <= now)
            || introspection.audience.as_deref() != Some(self.audience.as_str())
        {
            return Err(ProviderError::Unauthenticated);
        }

        if let Some(snapshot) = introspection.authorization {
            let credentials = request_context::current_iam_testing_credentials();
            if credentials.is_none() && snapshot.testing_environment_id.is_some() {
                return Err(ProviderError::Unauthenticated);
            }
            if credentials
                .as_ref()
                .is_some_and(|c| c.environment_key.expose_secret().is_empty())
                && snapshot.testing_environment_id != request_context::testing_scope().map(|s| s.id)
            {
                return Err(ProviderError::Unauthenticated);
            }

            if snapshot.principal_id != principal_id
                || snapshot.membership_id != membership_id
                || snapshot.org_id != org_id
                || snapshot.audience != self.audience
                || parse_actor_type(Some(&snapshot.actor_type))? != actor_type
                || snapshot.organization_id.is_nil()
            {
                return Err(ProviderError::Unauthenticated);
            }
            return Ok(VerifiedActor::new(
                OrganizationId::from_uuid(snapshot.organization_id),
                request.org_id.clone(),
                membership_id,
                Actor {
                    id: ActorId::new(snapshot.public_id)
                        .map_err(|_| ProviderError::InvalidResponse)?,
                    principal_id: PrincipalId::from_uuid(principal_id),
                    actor_type,
                },
                parse_organization_role(&snapshot.org_role.ok_or(ProviderError::Forbidden)?)?,
                // OAuth scopes describe IAM disclosure, not Commit management grants.
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
            ));
        }

        self.verified_from_membership(
            &request.org_id,
            membership_id,
            principal_id,
            actor_type,
            request.credential.clone(),
        )
        .await
        .map_err(map_authentication_directory_error)
    }

    async fn authenticate_obo(
        &self,
        issuer_app_id: &str,
        proof: &SecretString,
        request: &AuthenticationRequest,
    ) -> Result<VerifiedActor, ProviderError> {
        let body = OboVerifyRequest {
            access_proof: proof.expose_secret(),
            audience: &self.audience,
            action: &request.action,
            resource: request.resource.as_deref(),
        };
        // IAM verification consumes a single-use proof. A fresh key is
        // essential: reusing a deterministic key would make IAM replay its
        // first success and authorize a second, fresh Commit operation.
        let idempotency_key = fresh_verification_key();
        let mut outbound = self
            .client
            .post(self.obo_verify_url.clone())
            .header(
                header::AUTHORIZATION,
                self.request_application_authorization()?,
            )
            .header("x-org-id", org_header(&request.org_id)?)
            .header("idempotency-key", idempotency_key)
            .json(&body);
        outbound = self.apply_testing(outbound)?;
        let response = outbound
            .send()
            .await
            .map_err(|_| ProviderError::Unavailable)?;
        let verification: OboVerification = self
            .success_json(response, StatusCode::OK)
            .await
            .map_err(map_authentication_provider_error)?;

        let now = OffsetDateTime::now_utc();
        let expires_at = verification
            .expires_at
            .ok_or(ProviderError::InvalidResponse)?;
        if verification
            .proof_id
            .is_none_or(|proof_id| proof_id.is_nil())
            || verification.consumed_at.is_none()
            || expires_at > now + time::Duration::seconds(60)
        {
            return Err(ProviderError::InvalidResponse);
        }
        if !verification.valid
            || verification.issuer_app_id.as_deref() != Some(issuer_app_id)
            || verification.audience.as_deref() != Some(self.audience.as_str())
            || verification.action.as_deref() != Some(request.action.as_str())
            || verification.org_id.as_deref() != Some(request.org_id.as_str())
            || expires_at <= now
            || verification.resource.as_deref() != request.resource.as_deref()
        {
            return Err(ProviderError::Unauthenticated);
        }

        let actor = verification.actor.ok_or(ProviderError::InvalidResponse)?;
        let principal_id = required_non_nil(Some(actor.principal_id))?;
        let actor_type = parse_actor_type(Some(&actor.actor_type))?;
        let actor_id = ActorId::new(actor.public_id).map_err(|_| ProviderError::InvalidResponse)?;
        let active = self
            .active_member_by_actor(
                &request.org_id,
                &actor_id,
                Some(actor_type),
                Some(principal_id),
            )
            .await
            .map_err(map_authentication_directory_error)?;
        self.verified_from_active(active, request.credential.clone())
            .await
            .map_err(map_authentication_directory_error)
    }

    async fn verified_from_membership(
        &self,
        org_id: &PublicOrganizationId,
        membership_id: Uuid,
        principal_id: Uuid,
        actor_type: ActorType,
        grant: InboundCredential,
    ) -> Result<VerifiedActor, ProviderError> {
        let (organization, member) = tokio::try_join!(
            self.organization_by_id(org_id),
            self.member_by_id(org_id, membership_id),
        )?;
        let organization_id = organization.internal_id(org_id)?;
        let member = member.into_active(org_id, organization_id)?;
        if member.active.membership_id != membership_id
            || member.active.actor.principal_id.as_uuid() != &principal_id
            || member.active.actor.actor_type != actor_type
        {
            return Err(ProviderError::Unauthenticated);
        }
        self.verified_from_active(member, grant).await
    }

    async fn verified_from_active(
        &self,
        member: ResolvedMember,
        grant: InboundCredential,
    ) -> Result<VerifiedActor, ProviderError> {
        let authorization = self
            .member_authorization(&member.active.org_id, member.active.membership_id)
            .await?;
        if authorization.membership_id != member.active.membership_id {
            return Err(ProviderError::InvalidResponse);
        }
        let organization_role = authorization.role()?;
        if organization_role != member.organization_role {
            return Err(ProviderError::InvalidResponse);
        }
        let capabilities = CapabilitySet::try_from_names(authorization.capabilities)
            .map_err(|_| ProviderError::InvalidResponse)?;

        Ok(VerifiedActor::new(
            member.active.organization_id,
            member.active.org_id,
            member.active.membership_id,
            member.active.actor,
            organization_role,
            capabilities,
            grant,
        ))
    }

    async fn organization_by_id(
        &self,
        org_id: &PublicOrganizationId,
    ) -> Result<OrganizationResponse, ProviderError> {
        let authorization = Self::directory_header()?;
        let url = self.organization_endpoint(org_id, &[])?;
        let mut request = self
            .client
            .get(url)
            .header(header::AUTHORIZATION, authorization);
        request = self.apply_testing(request)?;
        let response = request
            .send()
            .await
            .map_err(|_| ProviderError::Unavailable)?;
        self.success_json(response, StatusCode::OK).await
    }

    async fn active_member_by_actor(
        &self,
        org_id: &PublicOrganizationId,
        actor_id: &ActorId,
        required_type: Option<ActorType>,
        required_principal: Option<Uuid>,
    ) -> Result<ResolvedMember, ProviderError> {
        let mut members = self
            .active_members_by_actor(
                org_id,
                std::slice::from_ref(actor_id),
                required_type,
                required_principal,
            )
            .await?;
        if members.len() != 1 {
            return Err(ProviderError::InvalidResponse);
        }
        members.pop().ok_or(ProviderError::InvalidResponse)
    }

    async fn active_members_by_actor(
        &self,
        org_id: &PublicOrganizationId,
        actor_ids: &[ActorId],
        required_type: Option<ActorType>,
        required_principal: Option<Uuid>,
    ) -> Result<Vec<ResolvedMember>, ProviderError> {
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

        let organization = self.organization_by_id(org_id).await?;
        let organization_id = organization.internal_id(org_id)?;
        let authorization = Self::directory_header()?;
        let url = self.organization_endpoint(org_id, &["members"])?;
        let mut cursor = None::<String>;
        let mut seen_cursors = HashSet::new();
        let mut matched = HashMap::with_capacity(requested_order.len());

        for _ in 0..MAX_DIRECTORY_PAGES {
            let mut request = self
                .client
                .get(url.clone())
                .header(header::AUTHORIZATION, authorization.clone())
                .query(&[("status", "active"), ("limit", DIRECTORY_PAGE_SIZE)]);
            if let Some(actor_type) = required_type {
                request = request.query(&[("principal_type", actor_type.as_str())]);
            }
            if let Some(cursor) = cursor.as_deref() {
                request = request.query(&[("cursor", cursor)]);
            }
            request = self.apply_testing(request)?;
            let response = request
                .send()
                .await
                .map_err(|_| ProviderError::Unavailable)?;
            let page: MembershipPage = self.success_json(response, StatusCode::OK).await?;
            if page.items.len() > 100 {
                return Err(ProviderError::InvalidResponse);
            }
            let next_cursor = page.next_cursor()?;
            for member in page.items {
                let member = member.into_active(org_id, organization_id)?;
                if requested_ids.contains(&member.active.actor.id)
                    && required_type.is_none_or(|kind| member.active.actor.actor_type == kind)
                    && required_principal.is_none_or(|principal_id| {
                        member.active.actor.principal_id.as_uuid() == &principal_id
                    })
                    && matched
                        .insert(member.active.actor.id.clone(), member)
                        .is_some()
                {
                    return Err(ProviderError::InvalidResponse);
                }
            }

            // Exhaust the bounded walk even after every ID has matched: IAM
            // does not yet guarantee globally unique public IDs, and a later
            // page may reveal an ambiguous member.
            let Some(next_cursor) = next_cursor else {
                if matched.len() != requested_order.len() {
                    return Err(ProviderError::NotFound);
                }
                return requested_order
                    .into_iter()
                    .map(|actor_id| {
                        matched
                            .remove(&actor_id)
                            .ok_or(ProviderError::InvalidResponse)
                    })
                    .collect();
            };
            if !seen_cursors.insert(next_cursor.clone()) {
                return Err(ProviderError::InvalidResponse);
            }
            cursor = Some(next_cursor);
        }
        Err(ProviderError::InvalidResponse)
    }

    async fn member_by_id(
        &self,
        org_id: &PublicOrganizationId,
        membership_id: Uuid,
    ) -> Result<MembershipResponse, ProviderError> {
        let authorization = Self::directory_header()?;
        let membership = membership_id.hyphenated().to_string();
        let url = self.organization_endpoint(org_id, &["members", &membership])?;
        let mut request = self
            .client
            .get(url)
            .header(header::AUTHORIZATION, authorization);
        request = self.apply_testing(request)?;
        let response = request
            .send()
            .await
            .map_err(|_| ProviderError::Unavailable)?;
        self.success_json(response, StatusCode::OK).await
    }

    async fn member_authorization(
        &self,
        org_id: &PublicOrganizationId,
        membership_id: Uuid,
    ) -> Result<MembershipAuthorization, ProviderError> {
        let authorization = Self::directory_header()?;
        let membership = membership_id.hyphenated().to_string();
        let url = self.organization_endpoint(org_id, &["members", &membership, "authorization"])?;
        let mut request = self
            .client
            .get(url)
            .header(header::AUTHORIZATION, authorization);
        request = self.apply_testing(request)?;
        let response = request
            .send()
            .await
            .map_err(|_| ProviderError::Unavailable)?;
        self.success_json(response, StatusCode::OK).await
    }

    async fn success_json<T>(
        &self,
        response: reqwest::Response,
        expected_status: StatusCode,
    ) -> Result<T, ProviderError>
    where
        T: for<'de> Deserialize<'de>,
    {
        let status = response.status();
        let headers = response.headers().clone();
        let response_is_json = is_json(&headers);
        let body = read_bounded(response, self.max_response_bytes)
            .await
            .map_err(body_error)?;
        if status != expected_status {
            return Err(provider_status(status, &headers));
        }
        if !response_is_json {
            return Err(ProviderError::InvalidResponse);
        }
        serde_json::from_slice(&body).map_err(|_| ProviderError::InvalidResponse)
    }

    fn directory_header() -> Result<HeaderValue, ProviderError> {
        crate::request_context::current_iam_bearer_token()
            .map(|token| bearer_authorization(&token))
            .transpose()
            .map_err(|_| ProviderError::Unauthenticated)?
            .ok_or(ProviderError::Unauthenticated)
    }

    fn organization_endpoint(
        &self,
        org_id: &PublicOrganizationId,
        tail: &[&str],
    ) -> Result<Url, ProviderError> {
        let mut url = self.members_url.clone();
        let mut segments = url
            .path_segments_mut()
            .map_err(|()| ProviderError::InvalidResponse)?;
        segments.pop_if_empty().push(org_id.as_str());
        for segment in tail {
            segments.push(segment);
        }
        drop(segments);
        Ok(url)
    }
}

fn map_authentication_directory_error(error: ProviderError) -> ProviderError {
    match error {
        ProviderError::NotFound => ProviderError::Unauthenticated,
        ProviderError::Unauthenticated | ProviderError::Forbidden => ProviderError::Unavailable,
        error => error,
    }
}

fn map_authentication_provider_error(error: ProviderError) -> ProviderError {
    match error {
        ProviderError::Unauthenticated | ProviderError::NotFound | ProviderError::Conflict => {
            ProviderError::Unauthenticated
        }
        ProviderError::Forbidden => ProviderError::Unavailable,
        error => error,
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
        self.active_members_by_actor(org_id, actor_ids, required_type, None)
            .await
            .map(|members| members.into_iter().map(|member| member.active).collect())
    }

    async fn exchange_child_proof(
        &self,
        actor: &VerifiedActor,
        request: &ChildProofRequest,
    ) -> Result<DelegatedOboProof, ProviderError> {
        let subject_token = match actor.grant() {
            InboundCredential::Bearer(token) => token.expose_secret(),
            InboundCredential::Obo { .. } | InboundCredential::Trusted(_) => {
                return Err(ProviderError::Forbidden);
            }
        };
        let body = OboExchangeRequest {
            subject_token,
            org_id: actor.org_id.as_str(),
            audience: &request.audience,
            action: &request.action,
            resource: Some(&request.resource),
        };
        let idempotency_key = proof_key(
            "exchange",
            subject_token,
            &request.audience,
            &request.action,
            Some(&request.resource),
        );
        let mut outbound = self
            .client
            .post(self.obo_exchange_url.clone())
            .header(
                header::AUTHORIZATION,
                self.request_application_authorization()?,
            )
            .header("x-org-id", org_header(&actor.org_id)?)
            .header("idempotency-key", idempotency_key)
            .json(&body);
        outbound = self.apply_testing(outbound)?;
        let response = outbound
            .send()
            .await
            .map_err(|_| ProviderError::Unavailable)?;
        let response: OboProofResponse = self.success_json(response, StatusCode::CREATED).await?;
        let now = OffsetDateTime::now_utc();
        let expires_at = response.expiry()?;
        let maximum_expiry = now + time::Duration::seconds(60);
        if expires_at <= now
            || expires_at > maximum_expiry
            || response.proof_id.is_nil()
            || !valid_obo_access_proof(response.access_proof.expose_secret())
        {
            return Err(ProviderError::InvalidResponse);
        }
        DelegatedOboProof::new(self.app_id.clone(), response.access_proof, expires_at)
            .map_err(|_| ProviderError::InvalidResponse)
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
            *membership_id,
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

#[derive(Debug, Deserialize)]
struct IntrospectionResponse {
    #[serde(default)]
    active: bool,
    principal_id: Option<Uuid>,
    actor_type: Option<String>,
    org_id: Option<String>,
    membership_id: Option<Uuid>,
    audience: Option<String>,
    #[serde(alias = "exp")]
    expires_at: Option<i64>,
    authorization: Option<AuthorizationSnapshot>,
}

#[derive(Debug, Deserialize)]
struct AuthorizationSnapshot {
    principal_id: Uuid,
    membership_id: Uuid,
    organization_id: Uuid,
    actor_type: String,
    public_id: String,
    org_id: String,
    audience: String,
    org_role: Option<String>,
    #[serde(default)]
    tags: Option<Vec<silicon_iam_client::models::AuthorizationTag>>,
    #[serde(default)]
    testing_environment_id: Option<Uuid>,
}

#[derive(Debug, Serialize)]
struct OboVerifyRequest<'a> {
    access_proof: &'a str,
    audience: &'a str,
    action: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    resource: Option<&'a str>,
}

#[derive(Debug, Deserialize)]
struct OboVerification {
    #[serde(default)]
    valid: bool,
    proof_id: Option<Uuid>,
    issuer_app_id: Option<String>,
    audience: Option<String>,
    action: Option<String>,
    resource: Option<String>,
    org_id: Option<String>,
    actor: Option<DirectoryActor>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    expires_at: Option<OffsetDateTime>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    consumed_at: Option<OffsetDateTime>,
}

#[derive(Clone, Debug, Deserialize)]
struct OrganizationResponse {
    id: Uuid,
    org_id: String,
    status: String,
}

impl OrganizationResponse {
    fn internal_id(
        &self,
        requested_org_id: &PublicOrganizationId,
    ) -> Result<OrganizationId, ProviderError> {
        if self.id.is_nil() || self.org_id != requested_org_id.as_str() {
            return Err(ProviderError::InvalidResponse);
        }
        require_active_directory_status(&self.status, "disabled")?;
        Ok(OrganizationId::from_uuid(self.id))
    }
}

#[derive(Clone, Debug, Deserialize)]
struct MembershipResponse {
    id: Uuid,
    org_id: String,
    #[serde(rename = "principal")]
    actor: DirectoryActor,
    status: String,
    org_role: String,
}

impl MembershipResponse {
    fn into_active(
        self,
        requested_org_id: &PublicOrganizationId,
        organization_id: OrganizationId,
    ) -> Result<ResolvedMember, ProviderError> {
        if self.id.is_nil()
            || self.org_id != requested_org_id.as_str()
            || self.actor.principal_id.is_nil()
        {
            return Err(ProviderError::InvalidResponse);
        }
        require_active_directory_status(&self.status, "removed")?;
        let actor_type = parse_actor_type(Some(&self.actor.actor_type))?;
        let actor_id =
            ActorId::new(self.actor.public_id).map_err(|_| ProviderError::InvalidResponse)?;
        Ok(ResolvedMember {
            active: ActiveMember {
                organization_id,
                org_id: requested_org_id.clone(),
                membership_id: self.id,
                actor: Actor::new(
                    PrincipalId::from_uuid(self.actor.principal_id),
                    actor_type,
                    actor_id,
                ),
            },
            organization_role: parse_organization_role(&self.org_role)?,
        })
    }
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

#[derive(Clone, Debug)]
struct ResolvedMember {
    active: ActiveMember,
    organization_role: OrganizationRole,
}

#[derive(Clone, Debug, Deserialize)]
struct DirectoryActor {
    principal_id: Uuid,
    #[serde(rename = "type")]
    actor_type: String,
    public_id: String,
}

#[derive(Debug, Deserialize)]
struct MembershipPage {
    items: Vec<MembershipResponse>,
    page: PageInfo,
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

#[derive(Debug, Deserialize)]
struct PageInfo {
    next_cursor: Option<String>,
    has_more: bool,
}

#[derive(Debug, Deserialize)]
struct MembershipAuthorization {
    membership_id: Uuid,
    #[serde(rename = "org_role")]
    role: String,
    #[serde(default)]
    capabilities: Vec<String>,
}

impl MembershipAuthorization {
    fn role(&self) -> Result<OrganizationRole, ProviderError> {
        parse_organization_role(&self.role)
    }
}

#[derive(Debug, Serialize)]
struct OboExchangeRequest<'a> {
    subject_token: &'a str,
    audience: &'a str,
    action: &'a str,
    resource: Option<&'a str>,
    org_id: &'a str,
}

#[derive(Debug, Deserialize)]
struct OboProofResponse {
    access_proof: SecretString,
    proof_id: Uuid,
    expires_in: u64,
    #[serde(with = "time::serde::rfc3339")]
    expires_at: OffsetDateTime,
}

impl OboProofResponse {
    fn expiry(&self) -> Result<OffsetDateTime, ProviderError> {
        if !(1..=MAX_CHILD_PROOF_LIFETIME_SECONDS).contains(&self.expires_in) {
            return Err(ProviderError::InvalidResponse);
        }
        Ok(self.expires_at)
    }
}

fn basic_authorization(
    app_id: &str,
    app_secret: &SecretString,
) -> Result<HeaderValue, ClientBuildError> {
    if app_secret.expose_secret().is_empty()
        || app_secret
            .expose_secret()
            .bytes()
            .any(|byte| byte.is_ascii_control())
    {
        return Err(ClientBuildError::InvalidCredential);
    }
    let credentials = format!("{app_id}:{}", app_secret.expose_secret());
    let encoded = STANDARD.encode(credentials.as_bytes());
    sensitive_header(&format!("Basic {encoded}"))
}

fn bearer_authorization(token: &SecretString) -> Result<HeaderValue, ClientBuildError> {
    let token = token.expose_secret();
    if token.is_empty() || token.bytes().any(|byte| byte.is_ascii_whitespace()) {
        return Err(ClientBuildError::InvalidCredential);
    }
    sensitive_header(&format!("Bearer {token}"))
}

fn sensitive_header(value: &str) -> Result<HeaderValue, ClientBuildError> {
    let mut header =
        HeaderValue::from_str(value).map_err(|_| ClientBuildError::InvalidCredential)?;
    header.set_sensitive(true);
    Ok(header)
}

fn required_non_nil(value: Option<Uuid>) -> Result<Uuid, ProviderError> {
    value
        .filter(|value| !value.is_nil())
        .ok_or(ProviderError::InvalidResponse)
}

fn org_header(org_id: &PublicOrganizationId) -> Result<HeaderValue, ProviderError> {
    HeaderValue::from_str(org_id.as_str()).map_err(|_| ProviderError::InvalidResponse)
}

fn parse_actor_type(value: Option<&str>) -> Result<ActorType, ProviderError> {
    match value {
        Some("carbon") => Ok(ActorType::Carbon),
        Some("silicon") => Ok(ActorType::Silicon),
        Some(_) | None => Err(ProviderError::InvalidResponse),
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

fn body_error(error: BoundedBodyError) -> ProviderError {
    match error {
        BoundedBodyError::Transport => ProviderError::Unavailable,
        BoundedBodyError::TooLarge => ProviderError::InvalidResponse,
    }
}

fn proof_key(
    purpose: &str,
    grant: &str,
    audience: &str,
    action: &str,
    resource: Option<&str>,
) -> String {
    let mut digest = Sha256::new();
    for part in [purpose, grant, audience, action, resource.unwrap_or("")] {
        digest.update(part.len().to_be_bytes());
        digest.update(part.as_bytes());
    }
    format!("commit-{purpose}-{}", hex::encode(digest.finalize()))
}

fn fresh_verification_key() -> String {
    format!("commit-verify-{}", Uuid::now_v7().simple())
}

fn valid_app_id(value: &str) -> bool {
    (3..=80).contains(&value.len())
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'>')
        })
}

fn valid_obo_access_proof(value: &str) -> bool {
    value.len() == 47
        && value.starts_with("obo_")
        && value[4..]
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use secrecy::SecretString;
    use time::{Duration as TimeDuration, OffsetDateTime, format_description::well_known::Rfc3339};
    use url::Url;
    use uuid::Uuid;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{
            body_json, body_string_contains, header, method, path, query_param,
            query_param_is_missing,
        },
    };

    use crate::{
        application::ports::{
            AuthenticationRequest, CapabilitySet, ChildProofRequest, IdentityProvider,
            InboundCredential, OrganizationRole, ProviderError, VerifiedActor,
        },
        config::{AuthenticationMode, IamSettings},
        domain::{Actor, ActorId, ActorType, OrganizationId, PrincipalId, PublicOrganizationId},
        request_context::{self, IamTestingCredentials, TestingScope},
    };

    use super::{
        IamClient, OboProofResponse, fresh_verification_key, map_authentication_directory_error,
        map_authentication_provider_error, parse_actor_type, proof_key,
        require_active_directory_status,
    };

    fn settings(server: &MockServer) -> Result<IamSettings, Box<dyn std::error::Error>> {
        Ok(IamSettings {
            webhook_secret: None,
            webhook_key_version: 1,
            mode: AuthenticationMode::Iam,
            base_url: Url::parse(&format!("{}/api/v1/", server.uri()))?,
            app_id: Some("silicon-commit".to_owned()),
            app_secret: Some(SecretString::from("application-secret")),
            audience: "silicon-commit".to_owned(),
        })
    }

    fn client(server: &MockServer) -> Result<IamClient, Box<dyn std::error::Error>> {
        Ok(IamClient::new(
            &settings(server)?,
            Duration::from_secs(1),
            Duration::from_secs(2),
            16_384,
        )?)
    }

    async fn as_user<T>(
        future: impl std::future::Future<Output = Result<T, ProviderError>>,
    ) -> Result<T, ProviderError> {
        crate::request_context::scope("test".to_owned(), async {
            crate::request_context::set_iam_bearer_token(Some(SecretString::from("user-token")));
            future.await
        })
        .await
    }

    fn testing_credentials(environment_key: &str, app_secret: &str) -> IamTestingCredentials {
        IamTestingCredentials {
            environment_key: SecretString::from(environment_key.to_owned()),
            app_id: "silicon-commit".to_owned(),
            app_secret: SecretString::from(app_secret.to_owned()),
        }
    }

    fn verified_bearer_actor() -> Result<VerifiedActor, Box<dyn std::error::Error>> {
        Ok(VerifiedActor::new(
            OrganizationId::from_uuid(Uuid::parse_str("018f268d-715a-7b72-8f0f-41f16f9af550")?),
            PublicOrganizationId::new("test-org")?,
            Uuid::parse_str("018f268d-715a-7b72-8f0f-41f16f9af551")?,
            Actor::new(
                PrincipalId::from_uuid(Uuid::parse_str("018f268d-715a-7b72-8f0f-41f16f9af552")?),
                ActorType::Carbon,
                ActorId::new("carbon-one")?,
            ),
            OrganizationRole::Owner,
            CapabilitySet::default(),
            InboundCredential::Bearer(SecretString::from(
                "iat_abcdefghijklmnopqrstuvwxyz0123456789ABCDEFG",
            )),
        ))
    }

    #[test]
    fn exchange_idempotency_key_is_stable_and_does_not_embed_the_grant() {
        let first = proof_key(
            "exchange",
            "secret-proof",
            "commit",
            "todos.read",
            Some("todo-1"),
        );
        let second = proof_key(
            "exchange",
            "secret-proof",
            "commit",
            "todos.read",
            Some("todo-1"),
        );
        assert_eq!(first, second);
        assert!(!first.contains("secret-proof"));
    }

    #[test]
    fn each_single_use_proof_verification_gets_a_fresh_retry_key() {
        let first = fresh_verification_key();
        let second = fresh_verification_key();
        assert_ne!(first, second);
        assert!(first.starts_with("commit-verify-"));
        assert!(second.starts_with("commit-verify-"));
    }

    #[test]
    fn authentication_maps_directory_absence_without_hiding_service_failures() {
        assert!(matches!(
            map_authentication_directory_error(ProviderError::NotFound),
            ProviderError::Unauthenticated
        ));
        assert!(matches!(
            map_authentication_directory_error(ProviderError::Forbidden),
            ProviderError::Unavailable
        ));
    }

    #[test]
    fn known_inactive_directory_states_are_absence_not_malformed_responses() {
        assert_eq!(
            require_active_directory_status("removed", "removed"),
            Err(ProviderError::NotFound)
        );
        assert_eq!(
            require_active_directory_status("disabled", "disabled"),
            Err(ProviderError::NotFound)
        );
        assert_eq!(
            require_active_directory_status("unexpected", "removed"),
            Err(ProviderError::InvalidResponse)
        );
        assert_eq!(
            map_authentication_directory_error(ProviderError::NotFound),
            ProviderError::Unauthenticated
        );
    }

    #[test]
    fn unknown_actor_types_are_invalid_provider_responses() {
        assert_eq!(
            parse_actor_type(Some("future-principal")),
            Err(ProviderError::InvalidResponse)
        );
        assert_eq!(parse_actor_type(None), Err(ProviderError::InvalidResponse));
    }

    #[test]
    fn consumed_or_expired_authentication_proofs_are_unauthenticated() {
        for error in [
            ProviderError::NotFound,
            ProviderError::Conflict,
            ProviderError::Unauthenticated,
        ] {
            assert_eq!(
                map_authentication_provider_error(error),
                ProviderError::Unauthenticated
            );
        }
        assert_eq!(
            map_authentication_provider_error(ProviderError::Forbidden),
            ProviderError::Unavailable
        );
    }

    #[test]
    fn child_proof_response_deserializes_without_exposing_the_secret() {
        let parsed = serde_json::from_value::<OboProofResponse>(serde_json::json!({
            "access_proof": "obo_opaque",
            "proof_id": "018f268d-715a-7b72-8f0f-41f16f9af553",
            "expires_in": 60,
            "expires_at": "2030-01-01T00:00:00Z"
        }));
        assert!(parsed.is_ok());
        let Some(parsed) = parsed.ok() else {
            return;
        };
        assert!(!format!("{parsed:?}").contains("obo_opaque"));
    }

    #[tokio::test]
    async fn testing_child_exchange_sends_the_paired_iam_credentials_and_published_body()
    -> Result<(), Box<dyn std::error::Error>> {
        let server = MockServer::start().await;
        let expires_at =
            (OffsetDateTime::now_utc() + TimeDuration::seconds(30)).format(&Rfc3339)?;
        Mock::given(method("POST"))
            .and(path("/api/v1/obo-access/exchanges"))
            .and(header("x-org-id", "test-org"))
            .and(header("x-testing-environment-key", "iam-testing-root"))
            .and(header(
                "authorization",
                format!("Basic {}", STANDARD.encode("silicon-commit:testing-secret")),
            ))
            .and(body_json(serde_json::json!({
                "subject_token": "iat_abcdefghijklmnopqrstuvwxyz0123456789ABCDEFG",
                "audience": "silicon-briefcase",
                "action": "briefcase.file.temporary_url",
                "resource": "018f268d-715a-7b72-8f0f-41f16f9af553",
                "org_id": "test-org"
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "access_proof": "obo_abcdefghijklmnopqrstuvwxyz0123456789ABCDEFG",
                "proof_id": "018f268d-715a-7b72-8f0f-41f16f9af554",
                "expires_in": 30,
                "expires_at": expires_at
            })))
            .expect(1)
            .mount(&server)
            .await;

        let request = ChildProofRequest::briefcase_temporary_url(Uuid::parse_str(
            "018f268d-715a-7b72-8f0f-41f16f9af553",
        )?);
        let iam = client(&server)?;
        let actor = verified_bearer_actor()?;
        let proof = request_context::scope("testing-exchange".to_owned(), async {
            request_context::set_environment_key(Some("incoming-commit-key".to_owned()));
            request_context::set_iam_testing_credentials(Some(testing_credentials(
                "iam-testing-root",
                "testing-secret",
            )));
            iam.exchange_child_proof(&actor, &request).await
        })
        .await?;
        assert_eq!(proof.app_id(), "silicon-commit");
        Ok(())
    }

    #[tokio::test]
    async fn bearer_introspection_sends_the_required_org_context()
    -> Result<(), Box<dyn std::error::Error>> {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/oauth/introspect"))
            .and(header("x-org-id", "test-org"))
            .and(body_string_contains(
                "token=iat_abcdefghijklmnopqrstuvwxyz0123456789ABCDEFG",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "active": false
            })))
            .expect(1)
            .mount(&server)
            .await;

        let result = client(&server)?
            .authenticate(&AuthenticationRequest {
                credential: InboundCredential::Bearer(SecretString::from(
                    "iat_abcdefghijklmnopqrstuvwxyz0123456789ABCDEFG",
                )),
                org_id: PublicOrganizationId::new("test-org")?,
                action: "commit.todos.list".to_owned(),
                resource: None,
            })
            .await;
        assert!(matches!(result, Err(ProviderError::Unauthenticated)));
        Ok(())
    }

    #[tokio::test]
    async fn testing_credentials_stay_paired_across_concurrent_requests_and_leave_production_intact()
    -> Result<(), Box<dyn std::error::Error>> {
        let server = MockServer::start().await;
        for (environment_key, secret) in [
            (Some("iam-root-a"), "testing-secret-a"),
            (Some("iam-root-b"), "testing-secret-b"),
            (None, "application-secret"),
        ] {
            let mock = Mock::given(method("POST"))
                .and(path("/api/v1/oauth/introspect"))
                .and(header(
                    "authorization",
                    format!(
                        "Basic {}",
                        STANDARD.encode(format!("silicon-commit:{secret}"))
                    ),
                ));
            let mock = if let Some(key) = environment_key {
                mock.and(header("x-testing-environment-key", key))
            } else {
                mock.and(|request: &wiremock::Request| {
                    !request.headers.contains_key("x-testing-environment-key")
                })
            };
            mock.respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "active": false
            })))
            .expect(1)
            .mount(&server)
            .await;
        }
        let iam = client(&server)?;
        let request = AuthenticationRequest {
            credential: InboundCredential::Bearer(SecretString::from("iat_testing")),
            org_id: PublicOrganizationId::new("test-org")?,
            action: "commit.todos.list".to_owned(),
            resource: None,
        };
        let barrier = tokio::sync::Barrier::new(2);
        let authenticate = |key: &'static str, secret: &'static str| {
            request_context::scope(key.to_owned(), async {
                request_context::set_environment_key(Some("incoming-commit-key".to_owned()));
                request_context::set_iam_testing_credentials(Some(testing_credentials(
                    key, secret,
                )));
                barrier.wait().await;
                iam.authenticate(&request).await
            })
        };
        let (first, second) = tokio::join!(
            authenticate("iam-root-a", "testing-secret-a"),
            authenticate("iam-root-b", "testing-secret-b"),
        );
        assert!(matches!(first, Err(ProviderError::Unauthenticated)));
        assert!(matches!(second, Err(ProviderError::Unauthenticated)));
        assert!(matches!(
            iam.authenticate(&request).await,
            Err(ProviderError::Unauthenticated)
        ));
        assert!(
            server
                .received_requests()
                .await
                .is_some_and(|requests| requests.len() == 3)
        );
        Ok(())
    }

    #[tokio::test]
    async fn testing_requests_without_a_matching_iam_pair_fail_before_network_io()
    -> Result<(), Box<dyn std::error::Error>> {
        let server = MockServer::start().await;
        let iam = client(&server)?;
        let actor = verified_bearer_actor()?;
        let child_request = ChildProofRequest::briefcase_temporary_url(Uuid::new_v4());
        for context in ["commit-key-only", "scope-only", "wrong-app-id"] {
            request_context::scope(context.to_owned(), async {
                match context {
                    "commit-key-only" => {
                        request_context::set_environment_key(Some(
                            "incoming-commit-key".to_owned(),
                        ));
                    }
                    "scope-only" => {
                        request_context::set_testing_scope(Some(TestingScope {
                            id: Uuid::new_v4(),
                            version: 1,
                        }));
                    }
                    _ => {
                        let mut credentials =
                            testing_credentials("iam-testing-root", "testing-secret");
                        credentials.app_id = "another-app".to_owned();
                        request_context::set_iam_testing_credentials(Some(credentials));
                    }
                }
                for credential in [
                    actor.grant().clone(),
                    InboundCredential::Obo {
                        app_id: "source-app".to_owned(),
                        proof: SecretString::from(
                            "obo_abcdefghijklmnopqrstuvwxyz0123456789ABCDEFG",
                        ),
                    },
                ] {
                    let result = iam
                        .authenticate(&AuthenticationRequest {
                            credential,
                            org_id: actor.org_id.clone(),
                            action: "commit.todos.list".to_owned(),
                            resource: None,
                        })
                        .await;
                    assert!(
                        matches!(result, Err(ProviderError::Unauthenticated)),
                        "{context}"
                    );
                }
                assert!(
                    matches!(
                        iam.exchange_child_proof(&actor, &child_request).await,
                        Err(ProviderError::Unauthenticated)
                    ),
                    "{context}"
                );
                request_context::set_iam_bearer_token(Some(SecretString::from("user-token")));
                assert_eq!(
                    iam.resolve_active_members(
                        &actor.org_id,
                        std::slice::from_ref(&actor.actor.id),
                        None
                    )
                    .await,
                    Err(ProviderError::Unauthenticated),
                    "{context}",
                );
            })
            .await;
        }
        assert!(
            server
                .received_requests()
                .await
                .is_some_and(|requests| requests.is_empty())
        );
        Ok(())
    }

    #[tokio::test]
    async fn live_snapshot_authenticates_without_administrative_directory_access()
    -> Result<(), Box<dyn std::error::Error>> {
        for snapshot_org in ["test-org", "another-org"] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/api/v1/oauth/introspect"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "active": true,
                    "principal_id": "018f268d-715a-7b72-8f0f-41f16f9af570",
                    "membership_id": "018f268d-715a-7b72-8f0f-41f16f9af571",
                    "actor_type": "carbon", "org_id": "test-org", "audience": "silicon-commit",
                    "expires_at": OffsetDateTime::now_utc().unix_timestamp() + 300,
                    "authorization": {
                        "principal_id": "018f268d-715a-7b72-8f0f-41f16f9af570",
                        "membership_id": "018f268d-715a-7b72-8f0f-41f16f9af571",
                        "organization_id": "018f268d-715a-7b72-8f0f-41f16f9af572",
                        "actor_type": "carbon", "public_id": "test-carbon",
                        "org_id": snapshot_org, "audience": "silicon-commit", "org_role": "owner"
                    }
                })))
                .expect(1)
                .mount(&server)
                .await;
            let result = client(&server)?
                .authenticate(&AuthenticationRequest {
                    credential: InboundCredential::Bearer(SecretString::from("iat_snapshot")),
                    org_id: PublicOrganizationId::new("test-org")?,
                    action: "commit.todos.list".to_owned(),
                    resource: None,
                })
                .await;
            if snapshot_org == "test-org" {
                assert_eq!(result?.actor.id.as_str(), "test-carbon");
            } else {
                assert!(matches!(result, Err(ProviderError::Unauthenticated)));
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn batch_directory_resolution_uses_one_scan_and_preserves_request_order()
    -> Result<(), Box<dyn std::error::Error>> {
        let server = MockServer::start().await;
        let organization_id = "018f268d-715a-7b72-8f0f-41f16f9af570";

        Mock::given(method("GET"))
            .and(path("/api/v1/organizations/test-org"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": organization_id,
                "org_id": "test-org",
                "status": "active"
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/organizations/test-org/members"))
            .and(query_param("status", "active"))
            .and(query_param("principal_type", "silicon"))
            .and(query_param("limit", "100"))
            .and(query_param_is_missing("cursor"))
            .and(query_param_is_missing("public_id"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "items": [{
                    "id": "018f268d-715a-7b72-8f0f-41f16f9af571",
                    "org_id": "test-org",
                    "principal": {
                        "principal_id": "018f268d-715a-7b72-8f0f-41f16f9af572",
                        "type": "silicon",
                        "public_id": "silicon-two"
                    },
                    "status": "active",
                    "org_role": "member"
                }],
                "page": { "next_cursor": "page-two", "has_more": true }
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/organizations/test-org/members"))
            .and(query_param("status", "active"))
            .and(query_param("principal_type", "silicon"))
            .and(query_param("limit", "100"))
            .and(query_param("cursor", "page-two"))
            .and(query_param_is_missing("public_id"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "items": [{
                    "id": "018f268d-715a-7b72-8f0f-41f16f9af573",
                    "org_id": "test-org",
                    "principal": {
                        "principal_id": "018f268d-715a-7b72-8f0f-41f16f9af574",
                        "type": "silicon",
                        "public_id": "silicon-one"
                    },
                    "status": "active",
                    "org_role": "member"
                }],
                "page": { "next_cursor": null, "has_more": false }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let requested = [ActorId::new("silicon-one")?, ActorId::new("silicon-two")?];
        let members = as_user(client(&server)?.resolve_active_members(
            &PublicOrganizationId::new("test-org")?,
            &requested,
            Some(ActorType::Silicon),
        ))
        .await?;

        assert_eq!(members.len(), 2);
        assert_eq!(members[0].actor.id, requested[0]);
        assert_eq!(members[1].actor.id, requested[1]);
        Ok(())
    }

    #[tokio::test]
    async fn untyped_directory_resolution_rejects_cross_page_public_id_ambiguity()
    -> Result<(), Box<dyn std::error::Error>> {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/api/v1/organizations/test-org"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "018f268d-715a-7b72-8f0f-41f16f9af580",
                "org_id": "test-org",
                "status": "active"
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/organizations/test-org/members"))
            .and(query_param("status", "active"))
            .and(query_param("limit", "100"))
            .and(query_param_is_missing("principal_type"))
            .and(query_param_is_missing("cursor"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "items": [{
                    "id": "018f268d-715a-7b72-8f0f-41f16f9af581",
                    "org_id": "test-org",
                    "principal": {
                        "principal_id": "018f268d-715a-7b72-8f0f-41f16f9af582",
                        "type": "carbon",
                        "public_id": "shared-public-id"
                    },
                    "status": "active",
                    "org_role": "member"
                }],
                "page": { "next_cursor": "page-two", "has_more": true }
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/organizations/test-org/members"))
            .and(query_param("status", "active"))
            .and(query_param("limit", "100"))
            .and(query_param_is_missing("principal_type"))
            .and(query_param("cursor", "page-two"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "items": [{
                    "id": "018f268d-715a-7b72-8f0f-41f16f9af583",
                    "org_id": "test-org",
                    "principal": {
                        "principal_id": "018f268d-715a-7b72-8f0f-41f16f9af584",
                        "type": "silicon",
                        "public_id": "shared-public-id"
                    },
                    "status": "active",
                    "org_role": "member"
                }],
                "page": { "next_cursor": null, "has_more": false }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let result = as_user(client(&server)?.resolve_active_members(
            &PublicOrganizationId::new("test-org")?,
            &[ActorId::new("shared-public-id")?],
            None,
        ))
        .await;
        assert_eq!(result, Err(ProviderError::InvalidResponse));
        Ok(())
    }

    #[tokio::test]
    async fn testing_obo_verification_pairs_app_credentials_and_keeps_directory_bearer_auth()
    -> Result<(), Box<dyn std::error::Error>> {
        let server = MockServer::start().await;
        let organization_id = "018f268d-715a-7b72-8f0f-41f16f9af560";
        let membership_id = "018f268d-715a-7b72-8f0f-41f16f9af561";
        let principal_id = "018f268d-715a-7b72-8f0f-41f16f9af562";
        let expires_at =
            (OffsetDateTime::now_utc() + TimeDuration::seconds(30)).format(&Rfc3339)?;
        let consumed_at = OffsetDateTime::now_utc().format(&Rfc3339)?;

        Mock::given(method("POST"))
            .and(path("/api/v1/obo-access/verify"))
            .and(header("x-org-id", "test-org"))
            .and(header("x-testing-environment-key", "iam-testing-root"))
            .and(header(
                "authorization",
                format!("Basic {}", STANDARD.encode("silicon-commit:testing-secret")),
            ))
            .and(body_json(serde_json::json!({
                "access_proof": "obo_abcdefghijklmnopqrstuvwxyz0123456789ABCDEFG",
                "audience": "silicon-commit",
                "action": "commit.projects.read",
                "resource": "project-one"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "valid": true,
                "proof_id": "018f268d-715a-7b72-8f0f-41f16f9af563",
                "issuer_app_id": "source-app",
                "audience": "silicon-commit",
                "actor": {
                    "principal_id": principal_id,
                    "type": "silicon",
                    "public_id": "silicon-one"
                },
                "org_id": "test-org",
                "action": "commit.projects.read",
                "resource": "project-one",
                "expires_at": expires_at,
                "consumed_at": consumed_at
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/organizations/test-org"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": organization_id,
                "org_id": "test-org",
                "status": "active"
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/organizations/test-org/members"))
            .and(query_param("status", "active"))
            .and(query_param("principal_type", "silicon"))
            .and(query_param("limit", "100"))
            .and(query_param_is_missing("cursor"))
            .and(query_param_is_missing("public_id"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "items": [],
                "page": { "next_cursor": "page-two", "has_more": true }
            })))
            .expect(1)
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/organizations/test-org/members"))
            .and(query_param("status", "active"))
            .and(query_param("principal_type", "silicon"))
            .and(query_param("limit", "100"))
            .and(query_param("cursor", "page-two"))
            .and(query_param_is_missing("public_id"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "items": [{
                    "id": membership_id,
                    "org_id": "test-org",
                    "principal": {
                        "principal_id": principal_id,
                        "type": "silicon",
                        "public_id": "silicon-one"
                    },
                    "status": "active",
                    "org_role": "owner"
                }],
                "page": { "next_cursor": null, "has_more": false }
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/api/v1/organizations/test-org/members/{membership_id}/authorization"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "membership_id": membership_id,
                "org_role": "owner",
                "capabilities": []
            })))
            .expect(1)
            .mount(&server)
            .await;

        let iam = client(&server)?;
        let request = AuthenticationRequest {
            credential: InboundCredential::Obo {
                app_id: "source-app".to_owned(),
                proof: SecretString::from("obo_abcdefghijklmnopqrstuvwxyz0123456789ABCDEFG"),
            },
            org_id: PublicOrganizationId::new("test-org")?,
            action: "commit.projects.read".to_owned(),
            resource: Some("project-one".to_owned()),
        };
        let actor = as_user(async {
            request_context::set_environment_key(Some("incoming-commit-key".to_owned()));
            request_context::set_iam_testing_credentials(Some(testing_credentials(
                "iam-testing-root",
                "testing-secret",
            )));
            iam.authenticate(&request).await
        })
        .await?;
        assert_eq!(
            actor.organization_id.into_uuid(),
            Uuid::parse_str(organization_id)?
        );
        assert_eq!(actor.membership_id, Uuid::parse_str(membership_id)?);

        let Some(received) = server.received_requests().await else {
            return Err("wiremock request recording was unavailable".into());
        };
        for request in received.iter().filter(|request| request.method == "GET") {
            assert_eq!(
                request
                    .headers
                    .get("authorization")
                    .and_then(|value| value.to_str().ok()),
                Some("Bearer user-token"),
            );
            assert_eq!(
                request
                    .headers
                    .get("x-testing-environment-key")
                    .and_then(|value| value.to_str().ok()),
                Some("iam-testing-root"),
            );
        }
        let directory_request = received
            .iter()
            .find(|request| request.url.path() == "/api/v1/organizations/test-org/members");
        let Some(directory_request) = directory_request else {
            return Err("member directory request was not recorded".into());
        };
        assert!(
            !directory_request
                .url
                .query_pairs()
                .any(|(name, _)| name == "public_id")
        );
        Ok(())
    }

    #[tokio::test]
    async fn child_exchange_rejects_an_inbound_obo_grant_without_network_io()
    -> Result<(), Box<dyn std::error::Error>> {
        let server = MockServer::start().await;
        let bearer_actor = verified_bearer_actor()?;
        let obo_actor = VerifiedActor::new(
            bearer_actor.organization_id,
            bearer_actor.org_id.clone(),
            bearer_actor.membership_id,
            bearer_actor.actor.clone(),
            bearer_actor.organization_role,
            bearer_actor.capabilities.clone(),
            InboundCredential::Obo {
                app_id: "source-app".to_owned(),
                proof: SecretString::from("obo_abcdefghijklmnopqrstuvwxyz0123456789ABCDEFG"),
            },
        );
        let request = ChildProofRequest::briefcase_temporary_url(Uuid::parse_str(
            "018f268d-715a-7b72-8f0f-41f16f9af553",
        )?);
        assert!(matches!(
            client(&server)?
                .exchange_child_proof(&obo_actor, &request)
                .await,
            Err(ProviderError::Forbidden)
        ));
        assert!(
            server
                .received_requests()
                .await
                .is_some_and(|requests| requests.is_empty())
        );
        Ok(())
    }
}
