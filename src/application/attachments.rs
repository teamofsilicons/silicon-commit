//! Briefcase temporary-URL use case.

use std::sync::Arc;

use serde::Serialize;
use sqlx::PgPool;
use time::OffsetDateTime;
use url::Url;

use crate::{
    application::ports::{
        BriefcaseProvider, ChildProofRequest, IdentityProvider, ProviderError, VerifiedActor,
    },
    domain::{AttachmentUrl, BriefcaseUrlPolicy},
    error::AppError,
};

/// Request document for temporary attachment delivery.
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemporaryUrlRequest {
    /// Canonical attachment URL stored on a visible todo.
    pub permanent_url: AttachmentUrl,
}

/// Public temporary attachment delivery response.
#[derive(Clone, Debug, Serialize)]
pub struct TemporaryUrlResponse {
    /// Short-lived Briefcase CDN URL.
    pub url: Url,
    /// Provider-declared expiration timestamp.
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
}

/// Coordinates local attachment ownership with IAM delegation and Briefcase.
#[derive(Clone)]
pub struct AttachmentService {
    pool: PgPool,
    identity: Arc<dyn IdentityProvider>,
    briefcase: Arc<dyn BriefcaseProvider>,
    briefcase_policy: BriefcaseUrlPolicy,
}

impl AttachmentService {
    /// Creates the attachment use case.
    #[must_use]
    pub fn new(
        pool: PgPool,
        identity: Arc<dyn IdentityProvider>,
        briefcase: Arc<dyn BriefcaseProvider>,
        briefcase_policy: BriefcaseUrlPolicy,
    ) -> Self {
        Self {
            pool,
            identity,
            briefcase,
            briefcase_policy,
        }
    }

    /// Generates a temporary URL for a Briefcase entry attached to a current todo.
    ///
    /// # Errors
    ///
    /// Rejects untrusted/non-attached URLs, IAM delegation failure, Briefcase
    /// authorization failure, and database outages without weakening authority.
    pub async fn temporary_url(
        &self,
        actor: &VerifiedActor,
        input: TemporaryUrlRequest,
    ) -> Result<TemporaryUrlResponse, AppError> {
        let briefcase_attachment = self
            .briefcase_policy
            .classify(&input.permanent_url)
            .map_err(|error| AppError::Validation {
                details: serde_json::json!({ "permanent_url": error.to_string() }),
            })?;

        let exists = sqlx::query_scalar::<_, bool>(
            r#"
            SELECT EXISTS (
                SELECT 1
                FROM commit.todo_attachments AS attachment
                JOIN commit.todos AS todo
                  ON todo.organization_id = attachment.organization_id
                 AND todo.id = attachment.todo_id
                WHERE attachment.organization_id = $1
                  AND attachment.url = $2
                  AND todo.deleted_at IS NULL
            )
            "#,
        )
        .bind(actor.organization_id.into_uuid())
        .bind(input.permanent_url.as_str())
        .fetch_one(&self.pool)
        .await?;
        if !exists {
            return Err(AppError::NotFound);
        }

        let proof_request =
            ChildProofRequest::briefcase_temporary_url(briefcase_attachment.entry_id());
        let proof = self
            .identity
            .exchange_child_proof(actor, &proof_request)
            .await
            .map_err(map_provider_error)?;
        let temporary = self
            .briefcase
            .temporary_url(&actor.org_id, &briefcase_attachment, &proof)
            .await
            .map_err(map_provider_error)?;

        Ok(TemporaryUrlResponse {
            url: temporary.url,
            expires_at: temporary.expires_at,
        })
    }
}

/// Maps a redacted platform dependency failure to Commit's stable API error.
pub(crate) fn map_provider_error(error: ProviderError) -> AppError {
    match error {
        ProviderError::Unauthenticated => AppError::Unauthenticated,
        ProviderError::Forbidden => AppError::Forbidden,
        ProviderError::NotFound => AppError::NotFound,
        ProviderError::Conflict => AppError::Conflict {
            code: "provider_conflict".into(),
        },
        ProviderError::RateLimited { retry_after } => AppError::RateLimited {
            retry_after_seconds: retry_after.map_or(1, |duration| duration.as_secs().max(1)),
        },
        ProviderError::InvalidResponse => AppError::BadGateway,
        ProviderError::Unavailable => AppError::ProviderUnavailable,
    }
}
