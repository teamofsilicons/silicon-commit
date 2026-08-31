//! Provider-neutral attachment URLs and strict Briefcase classification.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;
use url::Url;
use uuid::Uuid;

/// Maximum stored length of an attachment URL, measured in bytes.
pub const MAX_ATTACHMENT_URL_BYTES: usize = 2_048;

/// Invalid Briefcase base URL configuration.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum BriefcasePolicyError {
    /// At least one trusted Briefcase base URL is required.
    #[error("at least one Briefcase base URL must be configured")]
    Empty,
    /// Trusted bases must be HTTPS URLs without authority tricks or suffixes.
    #[error("invalid Briefcase base URL: {0}")]
    InvalidBase(String),
}

/// A rejected provider-neutral attachment URL.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum AttachmentUrlError {
    /// The representation is not a syntactically valid absolute URL.
    #[error("URL is not syntactically valid")]
    Malformed,
    /// Persisted URLs must use the canonical serialization produced at ingress.
    #[error("URL is not canonically serialized")]
    NonCanonical,
    /// The serialized URL is too large to persist safely.
    #[error("URL must be at most {MAX_ATTACHMENT_URL_BYTES} bytes")]
    TooLong,
    /// Control characters are never valid in a stored URL.
    #[error("URL must not contain control characters")]
    ControlCharacters,
    /// Surrounding whitespace must not be silently normalized.
    #[error("URL must not contain surrounding whitespace")]
    Whitespace,
    /// Only absolute HTTPS URLs with a host are accepted.
    #[error("URL must use HTTPS and include a host")]
    NotHttps,
    /// URL user information is forbidden.
    #[error("URL must not contain credentials")]
    Credentials,
    /// Fragments are client-side references and are not stable attachment identifiers.
    #[error("URL must not contain a fragment")]
    Fragment,
    /// Only the default HTTPS port is allowed.
    #[error("URL must not use a non-default port")]
    Port,
}

/// A generic attachment that cannot be classified as a configured Briefcase entry.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum BriefcaseUrlError {
    /// A previously validated URL could not be reparsed for classification.
    #[error("attachment URL could not be classified")]
    Malformed,
    /// Briefcase entry URLs never contain a query.
    #[error("Briefcase URL must not contain a query")]
    Query,
    /// The origin is not in the configured Briefcase allowlist.
    #[error("URL is not a configured Briefcase URL")]
    Origin,
    /// The path is not the canonical `/entries/{uuid}` resource path under a base.
    #[error("URL path is not a canonical Briefcase entry path")]
    Path,
}

/// Classifies provider-neutral attachments against configured Briefcase bases.
#[derive(Clone, Debug)]
pub struct BriefcaseUrlPolicy {
    bases: Vec<TrustedBase>,
}

#[derive(Clone, Debug)]
struct TrustedBase {
    url: Url,
    path: String,
}

impl BriefcaseUrlPolicy {
    /// Builds a classifier from trusted Briefcase API base URLs.
    ///
    /// A typical base is `https://briefcase.teamofsilicons.com/api/v1`.
    /// These bases classify attachments for temporary-URL issuance and do not
    /// restrict which HTTPS image provider may be stored on a todo.
    pub fn new<I>(bases: I) -> Result<Self, BriefcasePolicyError>
    where
        I: IntoIterator<Item = Url>,
    {
        let bases = bases
            .into_iter()
            .map(validate_base)
            .collect::<Result<Vec<_>, _>>()?;
        if bases.is_empty() {
            return Err(BriefcasePolicyError::Empty);
        }

        Ok(Self { bases })
    }

    /// Classifies an attachment as a configured canonical Briefcase entry.
    ///
    /// Only values produced here may cross the Briefcase provider boundary for
    /// temporary-URL generation.
    pub fn classify(
        &self,
        attachment: &AttachmentUrl,
    ) -> Result<BriefcaseAttachmentUrl, BriefcaseUrlError> {
        let candidate = attachment.parsed()?;
        if candidate.query().is_some() {
            return Err(BriefcaseUrlError::Query);
        }

        let mut matched_origin = false;
        for base in &self.bases {
            if candidate.origin() != base.url.origin() {
                continue;
            }
            matched_origin = true;

            if let Some(entry_id) = canonical_entry_id(&candidate, &base.path) {
                return Ok(BriefcaseAttachmentUrl {
                    attachment: attachment.clone(),
                    entry_id,
                });
            }
        }

        if matched_origin {
            Err(BriefcaseUrlError::Path)
        } else {
            Err(BriefcaseUrlError::Origin)
        }
    }
}

/// A validated, canonical HTTPS attachment URL from any image provider.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AttachmentUrl {
    value: String,
}

impl AttachmentUrl {
    /// Validates an untrusted URL and stores its canonical serialization.
    pub fn new(value: &str) -> Result<Self, AttachmentUrlError> {
        validate_raw_input(value)?;
        let url = Url::parse(value).map_err(|_| AttachmentUrlError::Malformed)?;
        validate_url_components(&url)?;
        let value = String::from(url);
        if value.len() > MAX_ATTACHMENT_URL_BYTES {
            return Err(AttachmentUrlError::TooLong);
        }

        Ok(Self { value })
    }

    /// Reconstructs a URL already validated and persisted by Commit.
    pub(crate) fn from_persisted(value: String) -> Result<Self, AttachmentUrlError> {
        let attachment = Self::new(&value)?;
        if attachment.value != value {
            return Err(AttachmentUrlError::NonCanonical);
        }
        Ok(attachment)
    }

    /// Borrows the canonical URL.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.value
    }

    /// Consumes the value object into its storage representation.
    #[must_use]
    pub fn into_inner(self) -> String {
        self.value
    }

    fn parsed(&self) -> Result<Url, BriefcaseUrlError> {
        Url::parse(&self.value).map_err(|_| BriefcaseUrlError::Malformed)
    }
}

impl fmt::Display for AttachmentUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for AttachmentUrl {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for AttachmentUrl {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(&value).map_err(serde::de::Error::custom)
    }
}

/// An attachment proven to be a configured canonical Briefcase entry.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct BriefcaseAttachmentUrl {
    attachment: AttachmentUrl,
    entry_id: Uuid,
}

impl BriefcaseAttachmentUrl {
    /// Borrows the canonical URL.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.attachment.as_str()
    }

    /// Borrows the provider-neutral attachment value.
    #[must_use]
    pub const fn attachment(&self) -> &AttachmentUrl {
        &self.attachment
    }

    /// Returns the Briefcase entry UUID parsed from the canonical path.
    #[must_use]
    pub const fn entry_id(&self) -> Uuid {
        self.entry_id
    }
}

fn validate_base(base: Url) -> Result<TrustedBase, BriefcasePolicyError> {
    validate_url_components(&base)
        .map_err(|error| BriefcasePolicyError::InvalidBase(error.to_string()))?;
    if base.query().is_some() {
        return Err(BriefcasePolicyError::InvalidBase(
            BriefcaseUrlError::Query.to_string(),
        ));
    }

    let path = base.path().trim_end_matches('/').to_owned();
    Ok(TrustedBase { url: base, path })
}

fn validate_raw_input(value: &str) -> Result<(), AttachmentUrlError> {
    if value.len() > MAX_ATTACHMENT_URL_BYTES {
        return Err(AttachmentUrlError::TooLong);
    }
    if value.chars().any(char::is_control) {
        return Err(AttachmentUrlError::ControlCharacters);
    }
    if value.trim() != value {
        return Err(AttachmentUrlError::Whitespace);
    }
    if raw_authority(value).is_some_and(|authority| authority.contains('@')) {
        return Err(AttachmentUrlError::Credentials);
    }
    Ok(())
}

fn raw_authority(value: &str) -> Option<&str> {
    value
        .split_once("://")
        .map(|(_, suffix)| suffix)
        .map(|suffix| suffix.split(['/', '?', '#']).next().unwrap_or_default())
}

fn validate_url_components(url: &Url) -> Result<(), AttachmentUrlError> {
    if url.scheme() != "https" || url.host_str().is_none() {
        return Err(AttachmentUrlError::NotHttps);
    }
    if !url.username().is_empty() || url.password().is_some() || has_userinfo(url) {
        return Err(AttachmentUrlError::Credentials);
    }
    if url.fragment().is_some() {
        return Err(AttachmentUrlError::Fragment);
    }
    if url.port().is_some() {
        return Err(AttachmentUrlError::Port);
    }

    Ok(())
}

fn has_userinfo(url: &Url) -> bool {
    url.as_str()
        .strip_prefix("https://")
        .and_then(|suffix| suffix.split(['/', '?', '#']).next())
        .is_some_and(|authority| authority.contains('@'))
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

#[cfg(test)]
mod tests {
    use super::{AttachmentUrl, AttachmentUrlError, BriefcaseUrlError, BriefcaseUrlPolicy};

    const ENTRY_ID: &str = "018f268d-715a-7b72-8f0f-41f16f9af553";

    fn policy() -> Option<BriefcaseUrlPolicy> {
        let base = url::Url::parse("https://briefcase.example/api/v1").ok()?;
        BriefcaseUrlPolicy::new([base]).ok()
    }

    #[test]
    fn accepts_and_canonicalizes_provider_neutral_urls() {
        let candidate = "https://IMAGES.example:443/assets/launch.png?width=1200&format=webp";

        let attachment = AttachmentUrl::new(candidate);

        assert_eq!(
            attachment.map(AttachmentUrl::into_inner),
            Ok("https://images.example/assets/launch.png?width=1200&format=webp".to_owned())
        );
    }

    #[test]
    fn rejects_unsafe_generic_urls_before_url_parser_normalization() {
        for (candidate, expected) in [
            (
                "https://images.example:8443/image.png",
                AttachmentUrlError::Port,
            ),
            (
                "https://user@images.example/image.png",
                AttachmentUrlError::Credentials,
            ),
            (
                "https://@images.example/image.png",
                AttachmentUrlError::Credentials,
            ),
            (
                "https://images.example/image.png#preview",
                AttachmentUrlError::Fragment,
            ),
            (
                "https://images.example/image\n.png",
                AttachmentUrlError::ControlCharacters,
            ),
            (
                " https://images.example/image.png ",
                AttachmentUrlError::Whitespace,
            ),
        ] {
            assert_eq!(AttachmentUrl::new(candidate), Err(expected));
        }
    }

    #[test]
    fn classifies_only_an_exact_canonical_briefcase_entry() {
        let Some(policy) = policy() else {
            return;
        };
        let attachment = AttachmentUrl::new(&format!(
            "https://briefcase.example/api/v1/entries/{ENTRY_ID}"
        ));
        let Ok(attachment) = attachment else {
            return;
        };

        let classified = policy.classify(&attachment);

        assert_eq!(
            classified.map(|value| (value.as_str().to_owned(), value.entry_id())),
            Ok((
                format!("https://briefcase.example/api/v1/entries/{ENTRY_ID}"),
                uuid::uuid!("018f268d-715a-7b72-8f0f-41f16f9af553")
            ))
        );
    }

    #[test]
    fn generic_urls_are_not_implicitly_briefcase_entries() {
        let Some(policy) = policy() else {
            return;
        };
        let cases = [
            (
                format!("https://briefcase.example/api/v1/not-entries/{ENTRY_ID}"),
                BriefcaseUrlError::Path,
            ),
            (
                format!("https://briefcase.example/api/v1/entries/{ENTRY_ID}?signature=opaque"),
                BriefcaseUrlError::Query,
            ),
            (
                format!("https://images.example/entries/{ENTRY_ID}"),
                BriefcaseUrlError::Origin,
            ),
            (
                "https://briefcase.example/api/v1/entries/018F268D-715A-7B72-8F0F-41F16F9AF553"
                    .to_owned(),
                BriefcaseUrlError::Path,
            ),
        ];

        for (candidate, expected) in cases {
            let attachment = AttachmentUrl::new(&candidate);
            assert!(
                attachment.is_ok(),
                "generic URL should be accepted: {candidate}"
            );
            assert_eq!(
                attachment.map(|value| policy.classify(&value)),
                Ok(Err(expected))
            );
        }
    }

    #[test]
    fn persisted_provider_urls_require_canonical_storage() {
        let canonical = String::from("https://retired-provider.example/image.png?variant=large");
        let noncanonical = String::from("https://RETIRED-provider.example:443/image.png");

        assert_eq!(
            AttachmentUrl::from_persisted(canonical.clone()).map(AttachmentUrl::into_inner),
            Ok(canonical)
        );
        assert_eq!(
            AttachmentUrl::from_persisted(noncanonical),
            Err(AttachmentUrlError::NonCanonical)
        );
    }

    #[test]
    fn serde_deserialization_canonicalizes_before_fingerprinting() {
        let parsed = serde_json::from_str::<AttachmentUrl>(
            r#""https://IMAGES.example:443/image.png?variant=large""#,
        );

        assert_eq!(
            parsed.ok().map(AttachmentUrl::into_inner),
            Some("https://images.example/image.png?variant=large".to_owned())
        );
    }
}
