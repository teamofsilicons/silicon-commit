//! Provider-neutral attachment URLs.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;
use url::Url;

/// Maximum stored length of an attachment URL, measured in bytes.
pub const MAX_ATTACHMENT_URL_BYTES: usize = 2_048;

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

#[cfg(test)]
mod tests {
    use super::{AttachmentUrl, AttachmentUrlError};

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
