//! Canonical permanent Briefcase attachment URLs.

use std::fmt;

use serde::Serialize;
use thiserror::Error;
use url::Url;
use uuid::Uuid;

/// Maximum stored length of a permanent attachment URL.
pub const MAX_ATTACHMENT_URL_BYTES: usize = 2_048;

/// Invalid Briefcase base URL configuration.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum AttachmentPolicyError {
    /// At least one trusted Briefcase base URL is required.
    #[error("at least one Briefcase base URL must be configured")]
    Empty,
    /// Trusted bases must be HTTPS URLs without authority tricks or suffixes.
    #[error("invalid Briefcase base URL: {0}")]
    InvalidBase(String),
}

/// A rejected untrusted attachment URL.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum AttachmentUrlError {
    /// The stored representation is not a syntactically valid absolute URL.
    #[error("URL is not syntactically valid")]
    Malformed,
    /// Persisted URLs must use the canonical serialization produced at ingress.
    #[error("URL is not canonically serialized")]
    NonCanonical,
    /// The serialized URL is too large to persist safely.
    #[error("URL must be at most {MAX_ATTACHMENT_URL_BYTES} bytes")]
    TooLong,
    /// Only HTTPS permanent URLs are accepted.
    #[error("URL must use HTTPS")]
    NotHttps,
    /// URL user information is forbidden.
    #[error("URL must not contain credentials")]
    Credentials,
    /// Fragments do not identify a Briefcase resource.
    #[error("URL must not contain a fragment")]
    Fragment,
    /// Queries identify temporary or otherwise non-canonical resources.
    #[error("URL must not contain a query")]
    Query,
    /// Only the default HTTPS port is allowed.
    #[error("URL must not use a non-default port")]
    Port,
    /// The origin is not in the configured Briefcase allowlist.
    #[error("URL origin is not an allowed Briefcase origin")]
    Origin,
    /// The path is not the canonical `/entries/{uuid}` resource path under a base.
    #[error("URL path is not a canonical Briefcase entry path")]
    Path,
}

/// Allowlist and canonical-path policy for permanent Briefcase URLs.
#[derive(Clone, Debug)]
pub struct AttachmentUrlPolicy {
    bases: Vec<TrustedBase>,
}

#[derive(Clone, Debug)]
struct TrustedBase {
    url: Url,
    path: String,
}

impl AttachmentUrlPolicy {
    /// Builds a policy from trusted Briefcase API base URLs.
    ///
    /// A typical base is `https://briefcase.teamofsilicons.com/api/v1`.
    pub fn new<I>(bases: I) -> Result<Self, AttachmentPolicyError>
    where
        I: IntoIterator<Item = Url>,
    {
        let bases = bases
            .into_iter()
            .map(validate_base)
            .collect::<Result<Vec<_>, _>>()?;
        if bases.is_empty() {
            return Err(AttachmentPolicyError::Empty);
        }

        Ok(Self { bases })
    }

    /// Validates an untrusted URL and extracts its Briefcase entry UUID.
    pub fn validate(&self, candidate: Url) -> Result<PermanentAttachmentUrl, AttachmentUrlError> {
        validate_common_url_rules(&candidate)?;

        let serialized = candidate.as_str();
        if serialized.len() > MAX_ATTACHMENT_URL_BYTES {
            return Err(AttachmentUrlError::TooLong);
        }

        let mut matched_origin = false;
        for base in &self.bases {
            if candidate.origin() != base.url.origin() {
                continue;
            }
            matched_origin = true;

            if let Some(entry_id) = canonical_entry_id(&candidate, &base.path) {
                return Ok(PermanentAttachmentUrl {
                    value: serialized.to_owned(),
                    entry_id,
                });
            }
        }

        if matched_origin {
            Err(AttachmentUrlError::Path)
        } else {
            Err(AttachmentUrlError::Origin)
        }
    }
}

/// A validated canonical, permanent Briefcase entry URL.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PermanentAttachmentUrl {
    value: String,
    entry_id: Uuid,
}

impl PermanentAttachmentUrl {
    /// Reconstructs a URL that was already validated and persisted by Commit.
    ///
    /// This deliberately verifies only immutable storage invariants. The
    /// deployment allowlist is enforced for new writes and temporary-URL
    /// issuance, but may rotate without making historical todos unreadable.
    pub(crate) fn from_persisted(value: String) -> Result<Self, AttachmentUrlError> {
        if value.len() > MAX_ATTACHMENT_URL_BYTES {
            return Err(AttachmentUrlError::TooLong);
        }

        let url = Url::parse(&value).map_err(|_| AttachmentUrlError::Malformed)?;
        validate_common_url_rules(&url)?;
        if url.as_str() != value {
            return Err(AttachmentUrlError::NonCanonical);
        }

        let entry_id = persisted_entry_id(&url).ok_or(AttachmentUrlError::Path)?;
        Ok(Self { value, entry_id })
    }

    /// Borrows the canonical URL.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.value
    }

    /// Returns the Briefcase entry UUID parsed from the canonical path.
    #[must_use]
    pub const fn entry_id(&self) -> Uuid {
        self.entry_id
    }

    /// Consumes the value object into its storage representation.
    #[must_use]
    pub fn into_inner(self) -> String {
        self.value
    }
}

impl fmt::Display for PermanentAttachmentUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for PermanentAttachmentUrl {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

fn validate_base(base: Url) -> Result<TrustedBase, AttachmentPolicyError> {
    validate_common_url_rules(&base)
        .map_err(|error| AttachmentPolicyError::InvalidBase(error.to_string()))?;

    let path = base.path().trim_end_matches('/').to_owned();
    Ok(TrustedBase { url: base, path })
}

fn validate_common_url_rules(url: &Url) -> Result<(), AttachmentUrlError> {
    if url.scheme() != "https" || url.host_str().is_none() {
        return Err(AttachmentUrlError::NotHttps);
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(AttachmentUrlError::Credentials);
    }
    if url.fragment().is_some() {
        return Err(AttachmentUrlError::Fragment);
    }
    if url.query().is_some() {
        return Err(AttachmentUrlError::Query);
    }
    if url.port().is_some() {
        return Err(AttachmentUrlError::Port);
    }

    Ok(())
}

fn canonical_entry_id(candidate: &Url, base_path: &str) -> Option<Uuid> {
    let expected_prefix = format!("{base_path}/entries/");
    let id_segment = candidate.path().strip_prefix(&expected_prefix)?;
    if id_segment.is_empty() || id_segment.contains('/') {
        return None;
    }

    let entry_id = Uuid::parse_str(id_segment).ok()?;
    (entry_id.hyphenated().to_string() == id_segment).then_some(entry_id)
}

fn persisted_entry_id(candidate: &Url) -> Option<Uuid> {
    let mut segments = candidate.path_segments()?.rev();
    let id_segment = segments.next()?;
    if segments.next()? != "entries" {
        return None;
    }

    let entry_id = Uuid::parse_str(id_segment).ok()?;
    (entry_id.hyphenated().to_string() == id_segment).then_some(entry_id)
}

#[cfg(test)]
mod tests {
    use super::{AttachmentUrlError, AttachmentUrlPolicy, PermanentAttachmentUrl};

    fn policy() -> Option<AttachmentUrlPolicy> {
        let base = url::Url::parse("https://briefcase.example/api/v1").ok()?;
        AttachmentUrlPolicy::new([base]).ok()
    }

    #[test]
    fn accepts_only_a_canonical_allowlisted_entry_url() {
        let Some(policy) = policy() else {
            return;
        };
        let candidate = url::Url::parse(
            "https://briefcase.example/api/v1/entries/018f268d-715a-7b72-8f0f-41f16f9af553",
        );
        let Ok(candidate) = candidate else {
            return;
        };

        let attachment = policy.validate(candidate);
        assert_eq!(
            attachment.map(|value| value.entry_id().to_string()),
            Ok("018f268d-715a-7b72-8f0f-41f16f9af553".to_owned())
        );
    }

    #[test]
    fn rejects_temporary_query_urls_and_lookalike_origins() {
        let Some(policy) = policy() else {
            return;
        };
        let with_query = url::Url::parse(
            "https://briefcase.example/api/v1/entries/018f268d-715a-7b72-8f0f-41f16f9af553?sig=x",
        );
        let lookalike = url::Url::parse(
            "https://briefcase.example.attacker.test/api/v1/entries/018f268d-715a-7b72-8f0f-41f16f9af553",
        );

        assert_eq!(
            with_query.ok().and_then(|url| policy.validate(url).err()),
            Some(AttachmentUrlError::Query)
        );
        assert_eq!(
            lookalike.ok().and_then(|url| policy.validate(url).err()),
            Some(AttachmentUrlError::Origin)
        );
    }

    #[test]
    fn persisted_urls_do_not_depend_on_the_current_origin_allowlist() {
        let value = String::from(
            "https://retired-briefcase.example/legacy/entries/018f268d-715a-7b72-8f0f-41f16f9af553",
        );

        let attachment = PermanentAttachmentUrl::from_persisted(value.clone());

        assert_eq!(
            attachment.map(|attachment| {
                let entry_id = attachment.entry_id();
                (attachment.into_inner(), entry_id)
            }),
            Ok((value, uuid::uuid!("018f268d-715a-7b72-8f0f-41f16f9af553")))
        );
    }

    #[test]
    fn persisted_urls_still_enforce_canonical_storage_invariants() {
        let lookalike = PermanentAttachmentUrl::from_persisted(
            "https://briefcase.example/api/v1/not-entries/018f268d-715a-7b72-8f0f-41f16f9af553"
                .to_owned(),
        );
        let uppercase_id = PermanentAttachmentUrl::from_persisted(
            "https://briefcase.example/api/v1/entries/018F268D-715A-7B72-8F0F-41F16F9AF553"
                .to_owned(),
        );

        assert_eq!(lookalike, Err(AttachmentUrlError::Path));
        assert_eq!(uppercase_id, Err(AttachmentUrlError::Path));
    }
}
