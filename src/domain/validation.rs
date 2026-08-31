//! Reusable domain validation primitives.
//!
//! Wire models intentionally deserialize into ordinary strings first. Calling
//! their `validate` method turns those strings into the value objects in this
//! module, making the validation boundary explicit and keeping configuration-
//! dependent limits out of Serde visitors.

use std::{collections::HashSet, fmt, hash::Hash};

use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;
use unicode_segmentation::UnicodeSegmentation as _;

/// Hard product limit for project diary content.
pub const MAX_DIARY_WORDS: usize = 100_000;

/// Configurable denial-of-service limits for user-authored values.
///
/// These defaults are deliberately conservative. Deployments may lower them,
/// but raising the diary word limit would violate the public product contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DomainLimits {
    /// Maximum Unicode scalar values in a todo, task, blocker, or update title.
    pub title_chars: usize,
    /// Maximum Unicode scalar values in a project name.
    pub project_name_chars: usize,
    /// Maximum Unicode scalar values in a description.
    pub description_chars: usize,
    /// Maximum Unicode scalar values in a todo note.
    pub note_chars: usize,
    /// Maximum number of attachments on one todo.
    pub attachments_per_todo: usize,
    /// Maximum number of participating Silicons on one project.
    pub participants_per_project: usize,
}

impl Default for DomainLimits {
    fn default() -> Self {
        Self {
            title_chars: 500,
            project_name_chars: 200,
            description_chars: 20_000,
            note_chars: 20_000,
            attachments_per_todo: 20,
            participants_per_project: 100,
        }
    }
}

/// Deserializes a field that may be omitted but must not be explicit JSON
/// `null` when present.
///
/// Applying this helper to `Option<T>` preserves Serde's normal absent-field
/// default while delegating present values directly to `T`. This keeps PATCH
/// documents aligned with non-nullable `OpenAPI` properties.
pub(crate) fn deserialize_optional_non_null<'de, D, T>(
    deserializer: D,
) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

/// A precise category of semantic validation failure.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum ValidationErrorKind {
    /// A required value is empty after surrounding whitespace is removed.
    #[error("must not be empty")]
    Required,
    /// A string contains more Unicode scalar values than permitted.
    #[error("must contain at most {max} characters (received {actual})")]
    TooLong {
        /// Configured maximum.
        max: usize,
        /// Observed number of Unicode scalar values.
        actual: usize,
    },
    /// A collection contains too few items.
    #[error("must contain at least {min} item(s) (received {actual})")]
    TooFewItems {
        /// Required minimum.
        min: usize,
        /// Observed item count.
        actual: usize,
    },
    /// A collection contains too many items.
    #[error("must contain at most {max} item(s) (received {actual})")]
    TooManyItems {
        /// Configured maximum.
        max: usize,
        /// Observed item count.
        actual: usize,
    },
    /// A set-like collection contains a duplicate.
    #[error("must not contain duplicate items")]
    DuplicateItem,
    /// A patch document contains no changes.
    #[error("must contain at least one field")]
    EmptyPatch,
    /// A lower range boundary is later than its upper boundary.
    #[error("range start must not be later than range end")]
    InvertedRange,
    /// Diary content exceeds the product word limit.
    #[error("must contain at most {max} Unicode words (received {actual})")]
    TooManyWords {
        /// Product maximum.
        max: usize,
        /// Observed Unicode word count.
        actual: usize,
    },
    /// PostgreSQL text values cannot contain the zero code point.
    #[error("must not contain the NUL character (U+0000)")]
    NulCharacter,
    /// A value has the wrong shape or violates a domain-specific rule.
    #[error("{0}")]
    Invalid(String),
}

/// Semantic validation failure associated with one public field.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("{field} {kind}")]
pub struct ValidationError {
    /// Stable public field name.
    pub field: &'static str,
    /// Machine-distinguishable failure category.
    pub kind: ValidationErrorKind,
}

impl ValidationError {
    /// Creates a field-scoped validation error.
    #[must_use]
    pub const fn new(field: &'static str, kind: ValidationErrorKind) -> Self {
        Self { field, kind }
    }

    /// Creates a domain-specific invalid-value error.
    #[must_use]
    pub fn invalid(field: &'static str, message: impl Into<String>) -> Self {
        Self::new(field, ValidationErrorKind::Invalid(message.into()))
    }
}

/// Trimmed, non-empty, length-bounded text.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, sqlx::Type)]
#[serde(transparent)]
#[sqlx(transparent)]
pub struct RequiredText(String);

impl RequiredText {
    /// Validates and normalizes required text.
    pub fn new(
        field: &'static str,
        value: impl Into<String>,
        max_chars: usize,
    ) -> Result<Self, ValidationError> {
        let value = value.into();
        ensure_persistable_text(field, &value)?;
        ensure_char_limit(field, &value, max_chars)?;
        let normalized = value.trim().to_owned();
        if normalized.is_empty() {
            return Err(ValidationError::new(field, ValidationErrorKind::Required));
        }

        Ok(Self(normalized))
    }

    /// Borrows the normalized text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consumes the value object.
    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl fmt::Display for RequiredText {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Length-bounded text which is allowed to be empty and preserves formatting.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, sqlx::Type)]
#[serde(transparent)]
#[sqlx(transparent)]
pub struct LimitedText(String);

impl LimitedText {
    /// Validates a formatting-preserving text field.
    pub fn new(
        field: &'static str,
        value: impl Into<String>,
        max_chars: usize,
    ) -> Result<Self, ValidationError> {
        let value = value.into();
        ensure_persistable_text(field, &value)?;
        ensure_char_limit(field, &value, max_chars)?;
        Ok(Self(value))
    }

    /// Borrows the original text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consumes the value object.
    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl fmt::Display for LimitedText {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Enforces a Unicode scalar-value length limit.
pub fn ensure_char_limit(
    field: &'static str,
    value: &str,
    max_chars: usize,
) -> Result<(), ValidationError> {
    let actual = value.chars().count();
    if actual > max_chars {
        return Err(ValidationError::new(
            field,
            ValidationErrorKind::TooLong {
                max: max_chars,
                actual,
            },
        ));
    }

    Ok(())
}

fn ensure_persistable_text(field: &'static str, value: &str) -> Result<(), ValidationError> {
    if value.contains('\0') {
        return Err(ValidationError::new(
            field,
            ValidationErrorKind::NulCharacter,
        ));
    }

    Ok(())
}

/// Enforces inclusive item-count bounds.
pub fn ensure_item_count(
    field: &'static str,
    actual: usize,
    min: usize,
    max: usize,
) -> Result<(), ValidationError> {
    if actual < min {
        return Err(ValidationError::new(
            field,
            ValidationErrorKind::TooFewItems { min, actual },
        ));
    }
    if actual > max {
        return Err(ValidationError::new(
            field,
            ValidationErrorKind::TooManyItems { max, actual },
        ));
    }

    Ok(())
}

/// Rejects the first duplicate while preserving the caller's item order.
pub fn ensure_unique<T>(field: &'static str, values: &[T]) -> Result<(), ValidationError>
where
    T: Eq + Hash,
{
    let mut observed = HashSet::with_capacity(values.len());
    for value in values {
        if !observed.insert(value) {
            return Err(ValidationError::new(
                field,
                ValidationErrorKind::DuplicateItem,
            ));
        }
    }

    Ok(())
}

/// Counts words using Unicode Standard Annex #29 boundaries.
///
/// Markdown is deliberately not stripped. Words in link labels, code spans,
/// headings, and other Markdown content therefore count toward the limit.
#[must_use]
pub fn unicode_word_count(value: &str) -> usize {
    value.unicode_words().count()
}

/// Enforces the immutable 100,000-word project diary limit.
pub fn validate_diary_markdown(markdown: &str) -> Result<usize, ValidationError> {
    ensure_persistable_text("markdown", markdown)?;
    let actual = unicode_word_count(markdown);
    if actual > MAX_DIARY_WORDS {
        return Err(ValidationError::new(
            "markdown",
            ValidationErrorKind::TooManyWords {
                max: MAX_DIARY_WORDS,
                actual,
            },
        ));
    }

    Ok(actual)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::{
        LimitedText, MAX_DIARY_WORDS, RequiredText, ValidationErrorKind, ensure_unique,
        unicode_word_count, validate_diary_markdown,
    };

    #[test]
    fn required_text_is_trimmed_and_unicode_bounded() {
        let value = RequiredText::new("title", "  ship 🚀  ", 10);
        assert_eq!(
            value.map(RequiredText::into_inner),
            Ok("ship 🚀".to_owned())
        );

        let error = RequiredText::new("title", "🚀🚀", 1).err();
        assert!(matches!(
            error.map(|error| error.kind),
            Some(ValidationErrorKind::TooLong { max: 1, actual: 2 })
        ));

        let padded = RequiredText::new("title", " ship ", 4).err();
        assert!(matches!(
            padded.map(|error| error.kind),
            Some(ValidationErrorKind::TooLong { max: 4, actual: 6 })
        ));
    }

    #[test]
    fn limited_text_preserves_whitespace() {
        let value = LimitedText::new("description", "  formatted\n", 20);
        assert_eq!(
            value.map(LimitedText::into_inner),
            Ok("  formatted\n".to_owned())
        );
    }

    #[test]
    fn persisted_text_rejects_the_postgresql_nul_character() {
        for error in [
            RequiredText::new("title", "ship\0now", 20).err(),
            LimitedText::new("description", "context\0details", 20).err(),
            validate_diary_markdown("entry\0details").err(),
        ] {
            assert!(matches!(
                error.map(|error| error.kind),
                Some(ValidationErrorKind::NulCharacter)
            ));
        }
    }

    #[test]
    fn uniqueness_check_is_order_independent() {
        assert!(ensure_unique("silicon_ids", &["a", "b", "a"]).is_err());
        assert!(ensure_unique("silicon_ids", &["a", "b", "c"]).is_ok());
    }

    #[test]
    fn unicode_words_include_markdown_content_without_counting_punctuation() {
        assert_eq!(unicode_word_count("# Hello, 世界! `ship_now`"), 4);
    }

    #[test]
    fn diary_accepts_the_exact_product_limit() {
        let markdown = std::iter::repeat_n("word", MAX_DIARY_WORDS)
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(validate_diary_markdown(&markdown), Ok(MAX_DIARY_WORDS));
    }

    proptest! {
        #[test]
        fn whitespace_only_required_text_is_always_rejected(
            whitespace in "[ \\t\\n\\r]{0,64}"
        ) {
            prop_assert!(RequiredText::new("title", whitespace, 100).is_err());
        }

        #[test]
        fn unicode_word_count_matches_joined_ascii_tokens(tokens in 0_usize..1_000) {
            let markdown = std::iter::repeat_n("token", tokens)
                .collect::<Vec<_>>()
                .join(" ");
            prop_assert_eq!(unicode_word_count(&markdown), tokens);
        }
    }
}
