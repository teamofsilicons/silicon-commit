//! Isolated storage identities for Commit testing environments.

use crate::{
    application::ports::{
        ActiveMember, AuthenticationRequest, ChildProofRequest, DelegatedOboProof,
        IdentityProvider, ProviderError, VerifiedActor,
    },
    domain::{ActorId, ActorType, OrganizationId, PublicOrganizationId},
    request_context,
};
use async_trait::async_trait;
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

/// Keeps provider identities unchanged on the wire and namespaces storage UUIDs.
pub struct ScopedIdentity {
    inner: Arc<dyn IdentityProvider>,
    pool: PgPool,
}
impl ScopedIdentity {
    /// Wraps a provider used by both authentication and directory services.
    pub fn new(inner: Arc<dyn IdentityProvider>, pool: PgPool) -> Self {
        Self { inner, pool }
    }
    async fn storage_id(&self, original: OrganizationId) -> Result<OrganizationId, ProviderError> {
        let Some(scope) = request_context::testing_scope() else {
            return Ok(original);
        };
        let id = sqlx::query_scalar::<_,Uuid>("INSERT INTO commit.testing_organizations (environment_id,iam_organization_id,storage_organization_id) SELECT environment_id,$2,$3 FROM commit.testing_environments WHERE environment_id=$1 AND status='active' AND version=$4 ON CONFLICT (environment_id,iam_organization_id) DO UPDATE SET iam_organization_id=EXCLUDED.iam_organization_id RETURNING storage_organization_id")
            .bind(scope.id).bind(original.into_uuid()).bind(Uuid::new_v4()).bind(scope.version).fetch_optional(&self.pool).await.map_err(|_| ProviderError::Unavailable)?.ok_or(ProviderError::Unauthenticated)?;
        Ok(id.into())
    }
}
#[async_trait]
impl IdentityProvider for ScopedIdentity {
    async fn authenticate(
        &self,
        request: &AuthenticationRequest,
    ) -> Result<VerifiedActor, ProviderError> {
        let mut actor = self.inner.authenticate(request).await?;
        actor.organization_id = self.storage_id(actor.organization_id).await?;
        Ok(actor)
    }
    async fn resolve_active_members(
        &self,
        org: &PublicOrganizationId,
        ids: &[ActorId],
        kind: Option<ActorType>,
    ) -> Result<Vec<ActiveMember>, ProviderError> {
        let mut members = self.inner.resolve_active_members(org, ids, kind).await?;
        for member in &mut members {
            member.organization_id = self.storage_id(member.organization_id).await?;
        }
        Ok(members)
    }
    async fn exchange_child_proof(
        &self,
        actor: &VerifiedActor,
        request: &ChildProofRequest,
    ) -> Result<DelegatedOboProof, ProviderError> {
        self.inner.exchange_child_proof(actor, request).await
    }
}
