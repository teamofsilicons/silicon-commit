//! Silicon-owned webhook and todo-notification subscription values.

use std::{
    fmt,
    net::{Ipv4Addr, Ipv6Addr},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::OffsetDateTime;
use url::{Host, Url};

use super::{
    ActorId, TodoId, TodoStatus,
    validation::{ValidationError, ValidationErrorKind, ensure_unique},
};

/// Maximum UTF-8 size accepted for a webhook URL.
pub const MAX_WEBHOOK_URL_BYTES: usize = 2_048;

/// Notification event classes understood by Commit subscriptions.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "notification_scope", rename_all = "snake_case")]
pub enum NotificationScope {
    /// Every supported update to a delegated todo.
    AnyUpdate,
    /// Only todo status transitions.
    StatusUpdates,
    /// Only transitions whose resulting status is explicitly selected.
    SpecificStatuses,
}

impl NotificationScope {
    /// Returns the stable protocol and database spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AnyUpdate => "any_update",
            Self::StatusUpdates => "status_updates",
            Self::SpecificStatuses => "specific_statuses",
        }
    }
}

impl fmt::Display for NotificationScope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Subscription resource which supplied an event's effective notification rule.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(
    type_name = "notification_subscription_level",
    rename_all = "snake_case"
)]
pub enum NotificationSubscriptionLevel {
    /// The Silicon's list-wide subscription supplied the rule.
    List,
    /// A todo-specific override supplied the rule.
    Todo,
}

impl NotificationSubscriptionLevel {
    /// Returns the stable protocol and database spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::List => "list",
            Self::Todo => "todo",
        }
    }
}

impl fmt::Display for NotificationSubscriptionLevel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Untrusted notification matching rule.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NotificationRuleInput {
    /// Event class selected by the Silicon.
    pub scope: NotificationScope,
    /// Resulting statuses selected when `scope` is `specific_statuses`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub statuses: Vec<TodoStatus>,
}

impl NotificationRuleInput {
    fn validate(self, field: &'static str) -> Result<NotificationRule, ValidationError> {
        NotificationRule::new(field, self.scope, self.statuses)
    }
}

/// Normalized notification matching rule.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct NotificationRule {
    /// Event class selected by the Silicon.
    pub scope: NotificationScope,
    /// Deterministically ordered resulting statuses for a status-specific rule.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    statuses: Vec<TodoStatus>,
}

impl NotificationRule {
    /// Validates a rule reconstructed from persistence.
    pub fn from_persisted(
        scope: NotificationScope,
        statuses: Vec<TodoStatus>,
    ) -> Result<Self, ValidationError> {
        Self::new("subscription", scope, statuses)
    }

    fn new(
        field: &'static str,
        scope: NotificationScope,
        mut statuses: Vec<TodoStatus>,
    ) -> Result<Self, ValidationError> {
        match scope {
            NotificationScope::SpecificStatuses if statuses.is_empty() => {
                return Err(ValidationError::new(
                    field,
                    ValidationErrorKind::TooFewItems { min: 1, actual: 0 },
                ));
            }
            NotificationScope::AnyUpdate | NotificationScope::StatusUpdates
                if !statuses.is_empty() =>
            {
                return Err(ValidationError::invalid(
                    field,
                    "statuses are only valid for the specific_statuses scope",
                ));
            }
            NotificationScope::SpecificStatuses
            | NotificationScope::AnyUpdate
            | NotificationScope::StatusUpdates => {}
        }

        ensure_unique(field, &statuses)?;
        statuses.sort_unstable_by_key(|status| status_sort_key(*status));
        Ok(Self { scope, statuses })
    }

    /// Borrows the normalized status filter.
    #[must_use]
    pub fn statuses(&self) -> &[TodoStatus] {
        &self.statuses
    }

    /// Reports whether this rule selects a meaningful todo mutation.
    ///
    /// `resulting_status` is present only when the mutation changed the todo's
    /// lifecycle status. Creation is intentionally not passed through this
    /// matcher because a delegating Silicon already knows about its own write.
    #[must_use]
    pub fn matches_update(&self, resulting_status: Option<TodoStatus>) -> bool {
        match self.scope {
            NotificationScope::AnyUpdate => true,
            NotificationScope::StatusUpdates => resulting_status.is_some(),
            NotificationScope::SpecificStatuses => {
                resulting_status.is_some_and(|status| self.statuses.contains(&status))
            }
        }
    }
}

const fn status_sort_key(status: TodoStatus) -> u8 {
    match status {
        TodoStatus::Completed => 0,
        TodoStatus::Canceled => 1,
        TodoStatus::InProgress => 2,
        TodoStatus::Blocked => 3,
        TodoStatus::YetToDo => 4,
    }
}

/// Canonical, externally dispatchable HTTPS webhook URL.
#[derive(Clone, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct WebhookUrl(String);

impl fmt::Debug for WebhookUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("WebhookUrl([REDACTED])")
    }
}

impl WebhookUrl {
    /// Validates and canonicalizes an actor-bound Silicon Hook endpoint URL.
    pub fn new(
        value: impl Into<String>,
        represented_actor_id: &ActorId,
    ) -> Result<Self, ValidationError> {
        let value = value.into();
        validate_webhook_url(&value, represented_actor_id)
            .map_err(|message| ValidationError::invalid("webhook_url", message))
    }

    /// Revalidates an actor-bound value read from persistence.
    pub fn from_persisted(
        value: String,
        represented_actor_id: &ActorId,
    ) -> Result<Self, ValidationError> {
        let validated = Self::new(value.clone(), represented_actor_id)?;
        if validated.as_str() != value {
            return Err(ValidationError::invalid(
                "webhook_url",
                "persisted webhook URL is not canonical",
            ));
        }
        Ok(validated)
    }

    /// Borrows the canonical URL.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for WebhookUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

fn validate_webhook_url(
    value: &str,
    represented_actor_id: &ActorId,
) -> Result<WebhookUrl, &'static str> {
    if value.is_empty() || value.len() > MAX_WEBHOOK_URL_BYTES {
        return Err("must contain between 1 and 2048 bytes");
    }
    if value.trim() != value || value.chars().any(char::is_control) {
        return Err("must not contain surrounding whitespace or control characters");
    }

    let url = Url::parse(value).map_err(|_| "must be an absolute HTTPS URL")?;
    if url.scheme() != "https" || url.cannot_be_a_base() || url.host().is_none() {
        return Err("must be an absolute HTTPS URL with a host");
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("must not contain user information");
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err("must not contain a query or fragment");
    }
    if url.port().is_some_and(|port| port != 443) {
        return Err("must not use a non-default port");
    }

    let host = url
        .host()
        .ok_or("must be an absolute HTTPS URL with a host")?;
    match host {
        Host::Domain(domain) => {
            let domain = domain.trim_end_matches('.');
            if domain.eq_ignore_ascii_case("localhost")
                || domain.to_ascii_lowercase().ends_with(".localhost")
            {
                return Err("must not target localhost");
            }
        }
        Host::Ipv4(address) if !is_public_ipv4(address) => {
            return Err("literal IP addresses must be publicly routable");
        }
        Host::Ipv6(address) if !is_public_ipv6(address) => {
            return Err("literal IP addresses must be publicly routable");
        }
        Host::Ipv4(_) | Host::Ipv6(_) => {}
    }

    let segments = url
        .path_segments()
        .ok_or("must use the canonical Silicon Hook ingress path")?
        .collect::<Vec<_>>();
    if segments.len() != 3 || segments[0] != "silicon" {
        return Err("must use /silicon/{silicon_id}/{endpoint_key}");
    }
    let endpoint_key = segments[2];
    if endpoint_key.len() != 6
        || !endpoint_key
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'A'..=b'F').contains(&byte))
    {
        return Err("endpoint key must contain exactly six uppercase hexadecimal characters");
    }

    // Rebuild the route exactly as Hook does. Equality both binds the endpoint
    // to the represented Silicon and rejects aliases, trailing slashes, and
    // non-canonical path-segment escaping without interpreting opaque actor IDs.
    let mut canonical = url.clone();
    canonical
        .path_segments_mut()
        .map_err(|()| "must use a hierarchical HTTPS URL")?
        .clear()
        .push("silicon")
        .push(represented_actor_id.as_str())
        .push(endpoint_key);
    if canonical.path() != url.path() {
        return Err("Silicon Hook endpoint must belong to the authenticated Silicon");
    }

    let canonical = url.to_string();
    if canonical.len() > MAX_WEBHOOK_URL_BYTES {
        return Err("canonical URL must be at most 2048 bytes");
    }
    Ok(WebhookUrl(canonical))
}

fn is_public_ipv4(address: Ipv4Addr) -> bool {
    let [first, second, third, _] = address.octets();
    !(first == 0
        || first == 10
        || first == 127
        || (first == 100 && (64..=127).contains(&second))
        || (first == 169 && second == 254)
        || (first == 172 && (16..=31).contains(&second))
        || (first == 192 && second == 0 && third == 0)
        || (first == 192 && second == 0 && third == 2)
        || (first == 192 && second == 88 && third == 99)
        || (first == 192 && second == 168)
        || (first == 198 && (second == 18 || second == 19))
        || (first == 198 && second == 51 && third == 100)
        || (first == 203 && second == 0 && third == 113)
        || first >= 224)
}

fn is_public_ipv6(address: Ipv6Addr) -> bool {
    let segments = address.segments();
    let in_global_unicast_block = (segments[0] & 0xe000) == 0x2000;
    let documentation = segments[0] == 0x2001 && segments[1] == 0x0db8;
    let benchmarking = segments[0] == 0x2001 && segments[1] == 0x0002;
    let orchid = segments[0] == 0x2001 && matches!(segments[1], 0x0010 | 0x0020);
    in_global_unicast_block && !documentation && !benchmarking && !orchid
}

/// Non-negative notification resource version, including virtual version zero.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, sqlx::Type)]
#[serde(transparent)]
#[sqlx(transparent)]
pub struct NotificationVersion(i64);

impl NotificationVersion {
    /// Creates a notification version from a database representation.
    pub const fn new(value: i64) -> Result<Self, NotificationVersionError> {
        if value < 0 {
            Err(NotificationVersionError::OutOfRange)
        } else {
            Ok(Self(value))
        }
    }

    /// Returns the integer representation.
    #[must_use]
    pub const fn get(self) -> i64 {
        self.0
    }
}

/// Invalid notification version.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum NotificationVersionError {
    /// Versions cannot be negative.
    #[error("notification version must be non-negative")]
    OutOfRange,
}

/// Parsed notification resource version from an `If-Match` header.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ExpectedNotificationVersion(u64);

impl ExpectedNotificationVersion {
    /// Creates a representable expected version. Version zero targets an absent resource.
    pub const fn new(value: u64) -> Result<Self, ExpectedNotificationVersionError> {
        if value > i64::MAX as u64 {
            Err(ExpectedNotificationVersionError)
        } else {
            Ok(Self(value))
        }
    }

    /// Returns the header value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Reports whether this precondition matches the current resource version.
    #[must_use]
    pub fn matches(self, current: NotificationVersion) -> bool {
        i64::try_from(self.0).is_ok_and(|expected| expected == current.get())
    }
}

/// Invalid optimistic-concurrency notification version.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("expected notification version is outside the supported range")]
pub struct ExpectedNotificationVersionError;

/// Public Silicon-level notification settings.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct NotificationSettings {
    /// Optional destination copied into future durable Hook events.
    pub webhook_url: Option<WebhookUrl>,
    /// Optional list-wide subscription used when a todo has no active override.
    pub todo_list_subscription: Option<NotificationRule>,
    /// Resource version used by `ETag` and `If-Match`.
    pub version: NotificationVersion,
    /// Last persisted replacement time, or `null` for virtual version zero.
    #[serde(with = "time::serde::rfc3339::option")]
    pub updated_at: Option<OffsetDateTime>,
}

impl NotificationSettings {
    /// Returns the virtual representation for a Silicon with no persisted settings.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            webhook_url: None,
            todo_list_subscription: None,
            version: NotificationVersion(0),
            updated_at: None,
        }
    }

    /// Compares only replaceable configuration, excluding resource version.
    #[must_use]
    pub fn configuration_eq(&self, desired: &ValidatedNotificationSettingsUpdate) -> bool {
        self.webhook_url == desired.webhook_url
            && self.todo_list_subscription == desired.todo_list_subscription
    }
}

/// Untrusted complete replacement for Silicon-level notification settings.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NotificationSettingsUpdate {
    /// Replacement webhook URL, or `null` to disable delivery.
    pub webhook_url: Option<String>,
    /// Replacement list-wide rule, or `null` to unsubscribe list-wide.
    pub todo_list_subscription: Option<NotificationRuleInput>,
}

impl<'de> Deserialize<'de> for NotificationSettingsUpdate {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct RequiredFields {
            #[serde(deserialize_with = "deserialize_nullable")]
            webhook_url: Option<String>,
            #[serde(deserialize_with = "deserialize_nullable")]
            todo_list_subscription: Option<NotificationRuleInput>,
        }

        let fields = RequiredFields::deserialize(deserializer)?;
        Ok(Self {
            webhook_url: fields.webhook_url,
            todo_list_subscription: fields.todo_list_subscription,
        })
    }
}

impl NotificationSettingsUpdate {
    /// Validates and normalizes the complete replacement.
    pub fn validate(
        self,
        represented_actor_id: &ActorId,
    ) -> Result<ValidatedNotificationSettingsUpdate, ValidationError> {
        let webhook_url = self
            .webhook_url
            .map(|value| WebhookUrl::new(value, represented_actor_id))
            .transpose()?;
        let todo_list_subscription = self
            .todo_list_subscription
            .map(|rule| rule.validate("todo_list_subscription"))
            .transpose()?;
        Ok(ValidatedNotificationSettingsUpdate {
            webhook_url,
            todo_list_subscription,
        })
    }
}

/// Validated complete replacement for Silicon-level settings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatedNotificationSettingsUpdate {
    /// Canonical replacement webhook URL.
    pub webhook_url: Option<WebhookUrl>,
    /// Normalized replacement list-wide rule.
    pub todo_list_subscription: Option<NotificationRule>,
}

/// Public todo-specific notification subscription resource.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TodoNotificationSubscription {
    /// Delegated todo governed by this override resource.
    pub todo_id: TodoId,
    /// Active override, or `null` to fall back to the list-wide rule.
    pub subscription: Option<NotificationRule>,
    /// Resource version used by `ETag` and `If-Match`.
    pub version: NotificationVersion,
    /// Last persisted replacement time, or `null` for virtual version zero.
    #[serde(with = "time::serde::rfc3339::option")]
    pub updated_at: Option<OffsetDateTime>,
}

impl TodoNotificationSubscription {
    /// Returns the virtual representation for a todo with no persisted override resource.
    #[must_use]
    pub fn empty(todo_id: TodoId) -> Self {
        Self {
            todo_id,
            subscription: None,
            version: NotificationVersion(0),
            updated_at: None,
        }
    }

    /// Compares only replaceable configuration, excluding resource version.
    #[must_use]
    pub fn configuration_eq(&self, desired: &ValidatedTodoNotificationSubscriptionUpdate) -> bool {
        self.subscription == desired.subscription
    }
}

/// Untrusted complete replacement for one todo-specific override.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TodoNotificationSubscriptionUpdate {
    /// Replacement override, or `null` to unsubscribe and fall back list-wide.
    pub subscription: Option<NotificationRuleInput>,
}

impl<'de> Deserialize<'de> for TodoNotificationSubscriptionUpdate {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct RequiredFields {
            #[serde(deserialize_with = "deserialize_nullable")]
            subscription: Option<NotificationRuleInput>,
        }

        let fields = RequiredFields::deserialize(deserializer)?;
        Ok(Self {
            subscription: fields.subscription,
        })
    }
}

impl TodoNotificationSubscriptionUpdate {
    /// Validates and normalizes the complete replacement.
    pub fn validate(self) -> Result<ValidatedTodoNotificationSubscriptionUpdate, ValidationError> {
        let subscription = self
            .subscription
            .map(|rule| rule.validate("subscription"))
            .transpose()?;
        Ok(ValidatedTodoNotificationSubscriptionUpdate { subscription })
    }
}

/// Validated complete replacement for one todo-specific override.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatedTodoNotificationSubscriptionUpdate {
    /// Normalized replacement override.
    pub subscription: Option<NotificationRule>,
}

fn deserialize_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        ExpectedNotificationVersion, NotificationRuleInput, NotificationScope,
        NotificationSettingsUpdate, NotificationVersion, TodoNotificationSubscriptionUpdate,
        WebhookUrl,
    };
    use crate::domain::{ActorId, TodoStatus};

    fn parse_actor_id(value: &str) -> Option<ActorId> {
        ActorId::new(value).ok()
    }

    #[test]
    fn status_specific_rules_are_required_unique_and_deterministic() {
        let rule = NotificationRuleInput {
            scope: NotificationScope::SpecificStatuses,
            statuses: vec![TodoStatus::Blocked, TodoStatus::Completed],
        }
        .validate("subscription");
        assert!(rule.is_ok());
        let Some(rule) = rule.ok() else {
            return;
        };
        assert_eq!(
            serde_json::to_value(rule).ok(),
            Some(json!({
                "scope": "specific_statuses",
                "statuses": ["completed", "blocked"]
            }))
        );

        let empty = NotificationRuleInput {
            scope: NotificationScope::SpecificStatuses,
            statuses: Vec::new(),
        }
        .validate("subscription");
        assert!(empty.is_err());

        let duplicate = NotificationRuleInput {
            scope: NotificationScope::SpecificStatuses,
            statuses: vec![TodoStatus::Blocked, TodoStatus::Blocked],
        }
        .validate("subscription");
        assert!(duplicate.is_err());
    }

    #[test]
    fn broad_rules_reject_status_filters_and_omit_empty_statuses() {
        let rule = NotificationRuleInput {
            scope: NotificationScope::AnyUpdate,
            statuses: Vec::new(),
        }
        .validate("subscription");
        assert!(rule.is_ok());
        assert_eq!(
            rule.ok().and_then(|value| serde_json::to_value(value).ok()),
            Some(json!({ "scope": "any_update" }))
        );

        let invalid = NotificationRuleInput {
            scope: NotificationScope::StatusUpdates,
            statuses: vec![TodoStatus::Completed],
        }
        .validate("subscription");
        assert!(invalid.is_err());
    }

    #[test]
    fn rules_match_only_their_selected_update_class() {
        let any_update = NotificationRuleInput {
            scope: NotificationScope::AnyUpdate,
            statuses: Vec::new(),
        }
        .validate("subscription");
        let status_updates = NotificationRuleInput {
            scope: NotificationScope::StatusUpdates,
            statuses: Vec::new(),
        }
        .validate("subscription");
        let completed = NotificationRuleInput {
            scope: NotificationScope::SpecificStatuses,
            statuses: vec![TodoStatus::Completed],
        }
        .validate("subscription");
        let (Ok(any_update), Ok(status_updates), Ok(completed)) =
            (any_update, status_updates, completed)
        else {
            return;
        };

        assert!(any_update.matches_update(None));
        assert!(any_update.matches_update(Some(TodoStatus::Blocked)));
        assert!(!status_updates.matches_update(None));
        assert!(status_updates.matches_update(Some(TodoStatus::Blocked)));
        assert!(!completed.matches_update(None));
        assert!(!completed.matches_update(Some(TodoStatus::Blocked)));
        assert!(completed.matches_update(Some(TodoStatus::Completed)));
    }

    #[test]
    fn webhook_urls_are_actor_bound_and_canonicalized() {
        let Some(actor_id) = parse_actor_id("support:acme") else {
            return;
        };
        let url = WebhookUrl::new(
            "https://EXAMPLE.com:443/silicon/support:acme/A1B2C3",
            &actor_id,
        );
        assert!(url.is_ok());
        assert_eq!(
            url.as_ref().ok().map(ToString::to_string),
            Some("https://example.com/silicon/support:acme/A1B2C3".to_owned())
        );
        assert!(
            url.as_ref()
                .is_ok_and(|value| !format!("{value:?}").contains("A1B2C3"))
        );

        let Some(escaped_actor_id) = parse_actor_id("support/team") else {
            return;
        };
        assert!(
            WebhookUrl::new(
                "https://hook.example.com/silicon/support%2Fteam/A1B2C3",
                &escaped_actor_id,
            )
            .is_ok()
        );
    }

    #[test]
    fn webhook_urls_reject_unsafe_authorities_and_components() {
        let Some(actor_id) = parse_actor_id("support:acme") else {
            return;
        };
        for value in [
            "http://example.com/silicon/support:acme/A1B2C3",
            "https://user@example.com/silicon/support:acme/A1B2C3",
            "https://example.com:8443/silicon/support:acme/A1B2C3",
            "https://localhost/silicon/support:acme/A1B2C3",
            "https://service.localhost/silicon/support:acme/A1B2C3",
            "https://127.0.0.1/silicon/support:acme/A1B2C3",
            "https://[::1]/silicon/support:acme/A1B2C3",
            "https://example.com/silicon/support:acme/A1B2C3?secret=value",
            "https://example.com/silicon/support:acme/A1B2C3#secret",
            "https://example.com/api/v1/silicon/support:acme/A1B2C3",
            "https://example.com/silicon/support:acme/A1B2C3/",
            "https://example.com/silicon/another-silicon/A1B2C3",
            "https://example.com/silicon/support:acme/a1b2c3",
        ] {
            assert!(
                WebhookUrl::new(value, &actor_id).is_err(),
                "accepted {value}"
            );
        }
    }

    #[test]
    fn webhook_url_limit_applies_after_canonical_percent_encoding() {
        let actor_value = "🦀".repeat(255);
        let Some(actor_id) = parse_actor_id(&actor_value) else {
            return;
        };
        let url = format!("https://hook.example.com/silicon/{actor_value}/A1B2C3");

        assert!(WebhookUrl::new(url, &actor_id).is_err());
    }

    #[test]
    fn replacement_documents_accept_explicitly_disabled_configuration() {
        let settings: Result<NotificationSettingsUpdate, _> = serde_json::from_value(json!({
            "webhook_url": null,
            "todo_list_subscription": null
        }));
        assert!(settings.is_ok());
        let Some(actor_id) = parse_actor_id("support:acme") else {
            return;
        };
        assert!(
            settings
                .ok()
                .and_then(|value| value.validate(&actor_id).ok())
                .is_some()
        );

        let todo: Result<TodoNotificationSubscriptionUpdate, _> =
            serde_json::from_value(json!({ "subscription": null }));
        assert!(todo.is_ok());
        assert!(todo.ok().and_then(|value| value.validate().ok()).is_some());
    }

    #[test]
    fn replacement_documents_require_every_nullable_property() {
        assert!(
            serde_json::from_value::<NotificationSettingsUpdate>(json!({
                "todo_list_subscription": null
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<NotificationSettingsUpdate>(json!({
                "webhook_url": null
            }))
            .is_err()
        );
        assert!(serde_json::from_value::<TodoNotificationSubscriptionUpdate>(json!({})).is_err());
    }

    #[test]
    fn expected_version_zero_matches_only_the_virtual_resource() {
        let expected = ExpectedNotificationVersion::new(0);
        let virtual_version = NotificationVersion::new(0);
        let persisted_version = NotificationVersion::new(1);
        assert!(expected.is_ok() && virtual_version.is_ok() && persisted_version.is_ok());
        assert!(
            expected
                .ok()
                .zip(virtual_version.ok())
                .is_some_and(|(expected, current)| expected.matches(current))
        );
        assert!(
            !ExpectedNotificationVersion::new(0)
                .ok()
                .zip(NotificationVersion::new(1).ok())
                .is_some_and(|(expected, current)| expected.matches(current))
        );
    }
}
