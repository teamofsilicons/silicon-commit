//! Stable keyset pagination values.

use std::str::FromStr;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::OffsetDateTime;
use uuid::Uuid;

/// Default number of resources returned by a list operation.
pub const DEFAULT_PAGE_LIMIT: u16 = 50;
/// Contract maximum for a single list operation.
pub const MAX_PAGE_LIMIT: u16 = 100;
const CURSOR_VERSION: u8 = 1;
const MAX_ENCODED_CURSOR_BYTES: usize = 1_024;

/// Invalid page size.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("limit must be between 1 and {MAX_PAGE_LIMIT}")]
pub struct PageLimitError;

/// Validated page size in the inclusive range 1–100.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PageLimit(u16);

impl PageLimit {
    /// Validates a caller-supplied limit.
    pub const fn new(value: u16) -> Result<Self, PageLimitError> {
        if value == 0 || value > MAX_PAGE_LIMIT {
            Err(PageLimitError)
        } else {
            Ok(Self(value))
        }
    }

    /// Returns the validated integer.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

impl Default for PageLimit {
    fn default() -> Self {
        Self(DEFAULT_PAGE_LIMIT)
    }
}

impl TryFrom<u16> for PageLimit {
    type Error = PageLimitError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl Serialize for PageLimit {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_u16(self.0)
    }
}

impl<'de> Deserialize<'de> for PageLimit {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = u16::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// Opaque cursor parsing or serialization failure.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum CursorError {
    /// Input is not canonical base64url-encoded cursor JSON.
    #[error("cursor is malformed")]
    Malformed,
    /// Cursor was produced by an unsupported future or retired format.
    #[error("cursor version is not supported")]
    UnsupportedVersion,
}

/// Keyset cursor for `(created_at DESC, id DESC)` ordering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PageCursor {
    created_at: OffsetDateTime,
    id: Uuid,
}

/// Common cursor and limit query for child-resource collections.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CollectionQuery {
    /// Opaque keyset cursor returned by the preceding page.
    pub cursor: Option<PageCursor>,
    /// Validated page size.
    #[serde(default)]
    pub limit: PageLimit,
}

impl PageCursor {
    /// Creates a cursor from the last item in a page.
    #[must_use]
    pub const fn new(created_at: OffsetDateTime, id: Uuid) -> Self {
        Self { created_at, id }
    }

    /// Returns the creation timestamp key.
    #[must_use]
    pub const fn created_at(self) -> OffsetDateTime {
        self.created_at
    }

    /// Returns the UUID tie-break key.
    #[must_use]
    pub const fn id(self) -> Uuid {
        self.id
    }

    /// Encodes a versioned canonical base64url JSON cursor.
    pub fn encode(self) -> Result<String, CursorError> {
        let payload = CursorPayload {
            version: CURSOR_VERSION,
            created_at: self.created_at,
            id: self.id,
        };
        let json = serde_json::to_vec(&payload).map_err(|_| CursorError::Malformed)?;
        Ok(URL_SAFE_NO_PAD.encode(json))
    }

    /// Decodes and validates an opaque cursor.
    pub fn decode(value: &str) -> Result<Self, CursorError> {
        if value.is_empty() || value.len() > MAX_ENCODED_CURSOR_BYTES {
            return Err(CursorError::Malformed);
        }

        let json = URL_SAFE_NO_PAD
            .decode(value)
            .map_err(|_| CursorError::Malformed)?;
        let payload: CursorPayload =
            serde_json::from_slice(&json).map_err(|_| CursorError::Malformed)?;
        if payload.version != CURSOR_VERSION {
            return Err(CursorError::UnsupportedVersion);
        }

        let canonical = URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&payload).map_err(|_| CursorError::Malformed)?);
        if canonical != value {
            return Err(CursorError::Malformed);
        }

        Ok(Self::new(payload.created_at, payload.id))
    }
}

impl FromStr for PageCursor {
    type Err = CursorError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::decode(value)
    }
}

impl Serialize for PageCursor {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let encoded = self.encode().map_err(serde::ser::Error::custom)?;
        serializer.serialize_str(&encoded)
    }
}

impl<'de> Deserialize<'de> for PageCursor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        Self::decode(&encoded).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CursorPayload {
    #[serde(rename = "v")]
    version: u8,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    id: Uuid,
}

/// Inclusive UTC creation-time filter.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CreatedAtRange {
    /// Optional inclusive lower boundary.
    pub from: Option<OffsetDateTime>,
    /// Optional inclusive upper boundary.
    pub to: Option<OffsetDateTime>,
}

impl CreatedAtRange {
    /// Validates an inclusive creation-time range.
    pub fn new(
        from: Option<OffsetDateTime>,
        to: Option<OffsetDateTime>,
    ) -> Result<Self, InvalidDateRange> {
        if from.zip(to).is_some_and(|(from, to)| from > to) {
            return Err(InvalidDateRange);
        }
        Ok(Self { from, to })
    }
}

/// Inverted creation-time range.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("created_from must not be later than created_to")]
pub struct InvalidDateRange;

/// One page of public resources.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Page<T> {
    /// Resources in stable descending creation order.
    pub items: Vec<T>,
    /// Cursor for the next page, or `null` at the end.
    pub next_cursor: Option<PageCursor>,
}

impl<T> Page<T> {
    /// Creates a page response.
    #[must_use]
    pub const fn new(items: Vec<T>, next_cursor: Option<PageCursor>) -> Self {
        Self { items, next_cursor }
    }

    /// Converts a `limit + 1` repository window into a public page.
    #[must_use]
    pub fn from_window(
        mut items: Vec<T>,
        limit: PageLimit,
        cursor: impl FnOnce(&T) -> PageCursor,
    ) -> Self {
        let has_more = items.len() > usize::from(limit.get());
        if has_more {
            items.truncate(usize::from(limit.get()));
        }
        let next_cursor = if has_more {
            items.last().map(cursor)
        } else {
            None
        };
        Self::new(items, next_cursor)
    }
}

#[cfg(test)]
mod tests {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use proptest::prelude::*;
    use time::OffsetDateTime;
    use uuid::Uuid;

    use super::{CursorError, Page, PageCursor, PageLimit};

    #[test]
    fn page_limit_enforces_the_contract_bounds() {
        assert!(PageLimit::new(0).is_err());
        assert_eq!(PageLimit::new(1).map(PageLimit::get), Ok(1));
        assert_eq!(PageLimit::new(100).map(PageLimit::get), Ok(100));
        assert!(PageLimit::new(101).is_err());
    }

    #[test]
    fn cursor_rejects_unknown_versions_and_fields() {
        let future = URL_SAFE_NO_PAD.encode(
            br#"{"v":2,"created_at":"2026-08-31T12:00:00Z","id":"00000000-0000-0000-0000-000000000000"}"#,
        );
        let extra = URL_SAFE_NO_PAD.encode(
            br#"{"v":1,"created_at":"2026-08-31T12:00:00Z","id":"00000000-0000-0000-0000-000000000000","extra":true}"#,
        );

        assert_eq!(
            PageCursor::decode(&future),
            Err(CursorError::UnsupportedVersion)
        );
        assert_eq!(PageCursor::decode(&extra), Err(CursorError::Malformed));
    }

    #[test]
    fn page_window_emits_a_cursor_only_when_more_items_exist() {
        let timestamp = OffsetDateTime::UNIX_EPOCH;
        let first = Uuid::from_u128(2);
        let second = Uuid::from_u128(1);
        let Ok(limit) = PageLimit::new(1) else {
            return;
        };

        let page = Page::from_window(vec![first, second], limit, |id| {
            PageCursor::new(timestamp, *id)
        });
        assert_eq!(page.items, vec![first]);
        assert_eq!(page.next_cursor, Some(PageCursor::new(timestamp, first)));

        let final_page =
            Page::from_window(vec![second], limit, |id| PageCursor::new(timestamp, *id));
        assert_eq!(final_page.items, vec![second]);
        assert_eq!(final_page.next_cursor, None);
    }

    proptest! {
        #[test]
        fn cursor_round_trips(epoch_seconds in 0_i64..4_102_444_800, raw_id in any::<u128>()) {
            let timestamp = OffsetDateTime::from_unix_timestamp(epoch_seconds);
            prop_assert!(timestamp.is_ok());
            let Some(timestamp) = timestamp.ok() else {
                return Ok(());
            };
            let cursor = PageCursor::new(timestamp, Uuid::from_u128(raw_id));
            let encoded = cursor.encode();
            prop_assert!(encoded.is_ok());
            let Some(encoded) = encoded.ok() else {
                return Ok(());
            };
            prop_assert_eq!(PageCursor::decode(&encoded), Ok(cursor));
        }
    }
}
