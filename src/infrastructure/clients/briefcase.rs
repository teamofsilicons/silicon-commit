//! Delegated Silicon Briefcase temporary-URL adapter.

use std::time::Duration;

use async_trait::async_trait;
use http::{HeaderValue, StatusCode};
use serde::Deserialize;
use time::OffsetDateTime;
use url::Url;

use crate::{
    application::ports::{BriefcaseProvider, DelegatedOboProof, ProviderError, TemporaryUrl},
    config::BriefcaseSettings,
    domain::{
        attachment::{BriefcaseAttachmentUrl, BriefcaseUrlPolicy},
        ids::PublicOrganizationId,
    },
};

use super::{
    BoundedBodyError, ClientBuildError, endpoint, http_client, is_json, provider_status,
    read_bounded,
};

const MAX_TEMPORARY_URL_BYTES: usize = 8_192;
const MAX_TEMPORARY_URL_LIFETIME: time::Duration = time::Duration::hours(12);

/// Redirect-free HTTP client which accepts only newly delegated OBO proofs.
#[derive(Clone, Debug)]
pub struct BriefcaseClient {
    client: reqwest::Client,
    base_url: Url,
    briefcase_url_policy: BriefcaseUrlPolicy,
    max_response_bytes: usize,
}

impl BriefcaseClient {
    /// Builds a Briefcase client and strict entry-URL classifier.
    ///
    /// `base_url` supplies the required `/api/v1` path used by permanent entry
    /// URLs. `allowed_origins` is checked independently so a base URL cannot
    /// silently expand the set classified as Briefcase-owned.
    ///
    /// # Errors
    ///
    /// Returns a redacted construction error for an unsafe endpoint or empty
    /// response bound.
    pub fn new(
        settings: &BriefcaseSettings,
        connect_timeout: Duration,
        request_timeout: Duration,
        max_response_bytes: usize,
    ) -> Result<Self, ClientBuildError> {
        if max_response_bytes == 0 {
            return Err(ClientBuildError::InvalidEndpoint);
        }
        let briefcase_url_policy = briefcase_url_policy(settings)?;
        Ok(Self {
            client: http_client(connect_timeout, request_timeout)?,
            base_url: settings.base_url.clone(),
            briefcase_url_policy,
            max_response_bytes,
        })
    }

    async fn response_json<T>(
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
}

/// Builds the Briefcase classifier shared by the application service and the
/// outbound adapter.
///
/// Each allowlisted origin inherits the canonical API path from `base_url`.
/// This supports separately hosted entry origins without weakening path
/// validation or changing the single outbound Briefcase API endpoint.
pub(crate) fn briefcase_url_policy(
    settings: &BriefcaseSettings,
) -> Result<BriefcaseUrlPolicy, ClientBuildError> {
    if !strict_https_url(&settings.base_url, true)
        || !settings.allowed_origins.iter().any(|origin| {
            strict_https_url(origin, false) && origin.origin() == settings.base_url.origin()
        })
    {
        return Err(ClientBuildError::InvalidEndpoint);
    }

    let api_path = settings.base_url.path().to_owned();
    let bases = settings
        .allowed_origins
        .iter()
        .map(|origin| {
            if !strict_https_url(origin, false) {
                return Err(ClientBuildError::InvalidEndpoint);
            }
            let mut base = origin.clone();
            base.set_path(&api_path);
            Ok(base)
        })
        .collect::<Result<Vec<_>, _>>()?;
    BriefcaseUrlPolicy::new(bases).map_err(|_| ClientBuildError::InvalidEndpoint)
}

#[async_trait]
impl BriefcaseProvider for BriefcaseClient {
    async fn temporary_url(
        &self,
        org_id: &PublicOrganizationId,
        attachment: &BriefcaseAttachmentUrl,
        proof: &DelegatedOboProof,
    ) -> Result<TemporaryUrl, ProviderError> {
        if proof.expires_at() <= OffsetDateTime::now_utc() {
            return Err(ProviderError::Unauthenticated);
        }

        let reparsed = self
            .briefcase_url_policy
            .classify(attachment.attachment())
            .map_err(|_| ProviderError::Forbidden)?;
        if reparsed.entry_id() != attachment.entry_id() || reparsed.as_str() != attachment.as_str()
        {
            return Err(ProviderError::Forbidden);
        }

        let url = endpoint(
            &self.base_url,
            &format!("entries/{}/download-url", attachment.entry_id()),
        )
        .map_err(|_| ProviderError::InvalidResponse)?;
        let mut proof_header = HeaderValue::from_str(proof.expose_proof())
            .map_err(|_| ProviderError::InvalidResponse)?;
        proof_header.set_sensitive(true);
        let response = self
            .client
            .post(url)
            .header("x-org-id", header_value(org_id.as_str())?)
            .header("x-app-id", header_value(proof.app_id())?)
            .header("x-iam-obo-access-proof", proof_header)
            .send()
            .await
            .map_err(|_| ProviderError::Unavailable)?;
        let response: TemporaryUrlResponse =
            self.response_json(response, StatusCode::CREATED).await?;

        let now = OffsetDateTime::now_utc();
        if !valid_temporary_url(&response.url)
            || response.expires_at <= now
            || response.expires_at > now + MAX_TEMPORARY_URL_LIFETIME
        {
            return Err(ProviderError::InvalidResponse);
        }
        Ok(TemporaryUrl {
            url: response.url,
            expires_at: response.expires_at,
        })
    }
}

#[derive(Debug, Deserialize)]
struct TemporaryUrlResponse {
    url: Url,
    #[serde(with = "time::serde::rfc3339")]
    expires_at: OffsetDateTime,
}

fn strict_https_url(url: &Url, allow_api_path: bool) -> bool {
    url.scheme() == "https"
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && (allow_api_path || url.path() == "/")
}

fn valid_temporary_url(url: &Url) -> bool {
    url.as_str().len() <= MAX_TEMPORARY_URL_BYTES
        && url.scheme() == "https"
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.fragment().is_none()
}

fn header_value(value: &str) -> Result<HeaderValue, ProviderError> {
    HeaderValue::from_str(value).map_err(|_| ProviderError::InvalidResponse)
}

fn body_error(error: BoundedBodyError) -> ProviderError {
    match error {
        BoundedBodyError::Transport => ProviderError::Unavailable,
        BoundedBodyError::TooLarge => ProviderError::InvalidResponse,
    }
}

#[cfg(test)]
mod tests {
    use crate::config::BriefcaseSettings;
    use url::Url;

    use super::{briefcase_url_policy, valid_temporary_url};

    #[test]
    fn classifier_honors_every_allowlisted_origin_under_the_api_path() {
        let base_url = Url::parse("https://briefcase.example/api/v1/");
        let primary_origin = Url::parse("https://briefcase.example/");
        let secondary_origin = Url::parse("https://files.example/");
        assert!(base_url.is_ok());
        assert!(primary_origin.is_ok());
        assert!(secondary_origin.is_ok());
        let settings = BriefcaseSettings {
            base_url: base_url.unwrap_or_else(|error| panic!("test URL failed: {error}")),
            allowed_origins: vec![
                primary_origin.unwrap_or_else(|error| panic!("test URL failed: {error}")),
                secondary_origin.unwrap_or_else(|error| panic!("test URL failed: {error}")),
            ],
        };
        let policy = briefcase_url_policy(&settings);
        assert!(policy.is_ok());
        let secondary =
            Url::parse("https://files.example/api/v1/entries/018f268d-715a-7b72-8f0f-41f16f9af553");
        assert!(secondary.is_ok());
        assert!(policy.is_ok_and(|policy| {
            secondary.is_ok_and(|candidate| {
                crate::domain::AttachmentUrl::new(candidate.as_str())
                    .is_ok_and(|attachment| policy.classify(&attachment).is_ok())
            })
        }));
    }

    #[test]
    fn temporary_url_must_not_contain_credentials_or_fragments() {
        let safe = Url::parse("https://cdn.example/files/one?signature=opaque");
        let credentialed = Url::parse("https://user@cdn.example/files/one");
        let fragmented = Url::parse("https://cdn.example/files/one#fragment");
        assert!(safe.is_ok_and(|url| valid_temporary_url(&url)));
        assert!(credentialed.is_ok_and(|url| !valid_temporary_url(&url)));
        assert!(fragmented.is_ok_and(|url| !valid_temporary_url(&url)));
    }
}
