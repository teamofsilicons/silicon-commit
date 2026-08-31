//! Todo aggregates, commands, filters, and lifecycle values.

use std::{fmt, str::FromStr};

use super::{
    actor::{Actor, ActorRef, ActorType},
    attachment::AttachmentUrl,
    ids::{ActorId, OrganizationId, PublicOrganizationId, TodoId, TodoNoteId},
    pagination::{CreatedAtRange, Page, PageCursor, PageLimit},
    validation::{
        DomainLimits, LimitedText, RequiredText, ValidationError, ValidationErrorKind,
        deserialize_optional_non_null, ensure_item_count, ensure_unique,
    },
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::OffsetDateTime;

/// Lifecycle state shared by todos and project tasks.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "todo_status", rename_all = "snake_case")]
pub enum TodoStatus {
    /// Work is finished.
    Completed,
    /// Work was intentionally abandoned.
    Canceled,
    /// Work has started.
    InProgress,
    /// Work cannot progress without intervention.
    Blocked,
    /// Work has not started.
    #[default]
    YetToDo,
}

impl TodoStatus {
    /// Returns the stable protocol and database spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Canceled => "canceled",
            Self::InProgress => "in_progress",
            Self::Blocked => "blocked",
            Self::YetToDo => "yet_to_do",
        }
    }
}

impl fmt::Display for TodoStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Invalid todo status.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("invalid todo status")]
pub struct ParseTodoStatusError;

impl FromStr for TodoStatus {
    type Err = ParseTodoStatusError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "completed" => Ok(Self::Completed),
            "canceled" => Ok(Self::Canceled),
            "in_progress" => Ok(Self::InProgress),
            "blocked" => Ok(Self::Blocked),
            "yet_to_do" => Ok(Self::YetToDo),
            _ => Err(ParseTodoStatusError),
        }
    }
}

/// Todo list projection requested by the caller.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoView {
    /// Todos assigned to the current actor, including self-assigned work.
    #[default]
    AssignedToMe,
    /// Todos assigned by the current actor to somebody else.
    DelegatedByMe,
    /// Every organization-visible todo.
    All,
}

/// Untrusted `POST /todos` document.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TodoCreate {
    /// Human-readable title.
    pub title: String,
    /// Optional formatting-preserving description.
    #[serde(default)]
    pub description: Option<String>,
    /// Public IAM ID of the requested assignee.
    pub assigned_to: ActorId,
    /// Initial lifecycle state.
    #[serde(default)]
    pub status: TodoStatus,
    /// Canonical HTTPS attachment URLs from any image provider.
    #[serde(default)]
    pub attachments: Vec<AttachmentUrl>,
}

impl TodoCreate {
    /// Applies configurable field and collection limits.
    pub fn validate(self, limits: &DomainLimits) -> Result<ValidatedTodoCreate, ValidationError> {
        let title = RequiredText::new("title", self.title, limits.title_chars)?;
        let description = self
            .description
            .map(|value| LimitedText::new("description", value, limits.description_chars))
            .transpose()?;
        let attachments = validate_attachments(self.attachments, limits.attachments_per_todo)?;

        Ok(ValidatedTodoCreate {
            title,
            description,
            assigned_to: self.assigned_to,
            status: self.status,
            attachments,
        })
    }
}

/// Fully validated todo creation command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatedTodoCreate {
    /// Trimmed non-empty title.
    pub title: RequiredText,
    /// Bounded description, with `None` representing JSON absence.
    pub description: Option<LimitedText>,
    /// IAM-resolved public assignee ID.
    pub assigned_to: ActorId,
    /// Initial status, defaulting to `yet_to_do`.
    pub status: TodoStatus,
    /// Canonical HTTPS attachment URLs.
    pub attachments: Vec<AttachmentUrl>,
}

/// Three-state field used to distinguish absent PATCH fields from JSON `null`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum NullablePatch<T> {
    /// Field was omitted and must not be changed.
    #[default]
    Absent,
    /// Field was explicitly set to JSON `null`.
    Null,
    /// Field was supplied with a value.
    Value(T),
}

impl<T> NullablePatch<T> {
    /// Reports whether the field was omitted.
    #[must_use]
    pub const fn is_absent(&self) -> bool {
        matches!(self, Self::Absent)
    }

    /// Maps the contained value without changing absent/null semantics.
    pub fn try_map<U, E>(self, map: impl FnOnce(T) -> Result<U, E>) -> Result<NullablePatch<U>, E> {
        match self {
            Self::Absent => Ok(NullablePatch::Absent),
            Self::Null => Ok(NullablePatch::Null),
            Self::Value(value) => map(value).map(NullablePatch::Value),
        }
    }
}

impl<T> Serialize for NullablePatch<T>
where
    T: Serialize,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Absent | Self::Null => serializer.serialize_none(),
            Self::Value(value) => serializer.serialize_some(value),
        }
    }
}

impl<'de, T> Deserialize<'de> for NullablePatch<T>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Option::<T>::deserialize(deserializer).map(|value| match value {
            Some(value) => Self::Value(value),
            None => Self::Null,
        })
    }
}

/// Untrusted `PATCH /todos/{todo_id}` document.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TodoPatch {
    /// Replacement title.
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub title: Option<String>,
    /// Replacement, clearing operation, or absence.
    #[serde(default, skip_serializing_if = "NullablePatch::is_absent")]
    pub description: NullablePatch<String>,
    /// Replacement assignee public ID.
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub assigned_to: Option<ActorId>,
    /// Replacement lifecycle state.
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub status: Option<TodoStatus>,
    /// Complete replacement canonical HTTPS attachment set.
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub attachments: Option<Vec<AttachmentUrl>>,
}

impl TodoPatch {
    /// Reports whether the patch contains no fields.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.description.is_absent()
            && self.assigned_to.is_none()
            && self.status.is_none()
            && self.attachments.is_none()
    }

    /// Validates all supplied fields and rejects an empty patch.
    pub fn validate(self, limits: &DomainLimits) -> Result<ValidatedTodoPatch, ValidationError> {
        if self.is_empty() {
            return Err(ValidationError::new(
                "body",
                ValidationErrorKind::EmptyPatch,
            ));
        }

        let title = self
            .title
            .map(|value| RequiredText::new("title", value, limits.title_chars))
            .transpose()?;
        let description = self
            .description
            .try_map(|value| LimitedText::new("description", value, limits.description_chars))?;
        let attachments = self
            .attachments
            .map(|values| validate_attachments(values, limits.attachments_per_todo))
            .transpose()?;

        Ok(ValidatedTodoPatch {
            title,
            description,
            assigned_to: self.assigned_to,
            status: self.status,
            attachments,
        })
    }
}

/// Fully validated todo patch command.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ValidatedTodoPatch {
    /// Validated title replacement.
    pub title: Option<RequiredText>,
    /// Validated description operation.
    pub description: NullablePatch<LimitedText>,
    /// Requested assignee replacement.
    pub assigned_to: Option<ActorId>,
    /// Requested status replacement.
    pub status: Option<TodoStatus>,
    /// Validated replacement attachment set.
    pub attachments: Option<Vec<AttachmentUrl>>,
}

/// Public todo aggregate with internal tenant and principal keys retained.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Todo {
    /// `UUIDv7` resource ID.
    pub id: TodoId,
    /// Authoritative internal tenant key.
    pub organization_id: OrganizationId,
    /// Public organization ID snapshot.
    pub org_id: PublicOrganizationId,
    /// Validated title.
    pub title: RequiredText,
    /// Optional description.
    pub description: Option<LimitedText>,
    /// IAM-resolved assignee.
    pub assigned_to: Actor,
    /// IAM-resolved assigner.
    pub assigned_by: Actor,
    /// Current lifecycle state.
    pub status: TodoStatus,
    /// Canonical attachment URLs.
    pub attachments: Vec<AttachmentUrl>,
    /// Creation timestamp.
    pub created_at: OffsetDateTime,
    /// Last meaningful update timestamp.
    pub updated_at: OffsetDateTime,
}

impl Todo {
    /// Reports whether work was delegated to a different IAM principal.
    #[must_use]
    pub fn is_delegated(&self) -> bool {
        self.assigned_by.principal_id != self.assigned_to.principal_id
    }

    /// Applies the product rule for notifying a delegating Silicon.
    #[must_use]
    pub fn should_notify_assigner(&self) -> bool {
        self.assigned_by.actor_type == ActorType::Silicon && self.is_delegated()
    }
}

impl Serialize for Todo {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        #[derive(Serialize)]
        struct WireTodo<'a> {
            id: TodoId,
            org_id: &'a PublicOrganizationId,
            title: &'a RequiredText,
            description: &'a Option<LimitedText>,
            assigned_to: &'a ActorId,
            assigned_by: ActorRef,
            status: TodoStatus,
            attachments: &'a [AttachmentUrl],
            #[serde(with = "time::serde::rfc3339")]
            created_at: OffsetDateTime,
            #[serde(with = "time::serde::rfc3339")]
            updated_at: OffsetDateTime,
        }

        WireTodo {
            id: self.id,
            org_id: &self.org_id,
            title: &self.title,
            description: &self.description,
            assigned_to: &self.assigned_to.id,
            assigned_by: self.assigned_by.public_ref(),
            status: self.status,
            attachments: &self.attachments,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
        .serialize(serializer)
    }
}

/// Untrusted body for appending a todo note.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TodoNoteCreate {
    /// Note body.
    pub body: String,
}

impl TodoNoteCreate {
    /// Trims, bounds, and requires the note body.
    pub fn validate(
        self,
        limits: &DomainLimits,
    ) -> Result<ValidatedTodoNoteCreate, ValidationError> {
        RequiredText::new("body", self.body, limits.note_chars)
            .map(|body| ValidatedTodoNoteCreate { body })
    }
}

/// Validated append-only note command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatedTodoNoteCreate {
    /// Trimmed non-empty note body.
    pub body: RequiredText,
}

/// Append-only todo note.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TodoNote {
    /// `UUIDv7` note ID.
    pub id: TodoNoteId,
    /// Parent todo ID; omitted from the nested v1 response.
    #[serde(skip_serializing)]
    pub todo_id: TodoId,
    /// Author-authored body.
    pub body: RequiredText,
    /// IAM-resolved author, serialized as a public actor reference.
    pub author: Actor,
    /// Creation timestamp.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// Validated list/filter parameters for todos.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TodoQuery {
    /// Caller-relative list projection.
    #[serde(default)]
    pub view: TodoView,
    /// Optional exact status filter.
    pub status: Option<TodoStatus>,
    /// Optional public assignee ID filter.
    pub assigned_to: Option<ActorId>,
    /// Optional public assigner ID filter.
    pub assigned_by: Option<ActorId>,
    /// Inclusive creation lower bound.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub created_from: Option<OffsetDateTime>,
    /// Inclusive creation upper bound.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub created_to: Option<OffsetDateTime>,
    /// Opaque keyset cursor.
    pub cursor: Option<PageCursor>,
    /// Validated page size.
    #[serde(default)]
    pub limit: PageLimit,
}

impl TodoQuery {
    /// Returns a validated inclusive creation-time range.
    pub fn created_at_range(&self) -> Result<CreatedAtRange, ValidationError> {
        CreatedAtRange::new(self.created_from, self.created_to)
            .map_err(|_| ValidationError::new("created_from", ValidationErrorKind::InvertedRange))
    }
}

/// Paginated todo response.
pub type TodoPage = Page<Todo>;

fn validate_attachments(
    attachments: Vec<AttachmentUrl>,
    max: usize,
) -> Result<Vec<AttachmentUrl>, ValidationError> {
    ensure_item_count("attachments", attachments.len(), 0, max)?;
    ensure_unique("attachments", &attachments)?;
    Ok(attachments)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{NullablePatch, TodoCreate, TodoPatch, TodoStatus};
    use crate::domain::validation::{DomainLimits, ValidationErrorKind};

    #[test]
    fn create_defaults_status_and_rejects_unknown_fields() {
        let decoded = serde_json::from_value::<TodoCreate>(json!({
            "title": "Ship it",
            "assigned_to": "head_of_growth:tos"
        }));
        assert!(decoded.is_ok());
        assert_eq!(
            decoded.ok().map(|request| request.status),
            Some(TodoStatus::YetToDo)
        );

        let unknown = serde_json::from_value::<TodoCreate>(json!({
            "title": "Ship it",
            "assigned_to": "head_of_growth:tos",
            "surprise": true
        }));
        assert!(unknown.is_err());
    }

    #[test]
    fn patch_distinguishes_absent_null_and_value_description() {
        let absent = serde_json::from_value::<TodoPatch>(json!({ "status": "blocked" }));
        let null = serde_json::from_value::<TodoPatch>(json!({ "description": null }));
        let value = serde_json::from_value::<TodoPatch>(json!({ "description": "context" }));

        assert!(matches!(
            absent.map(|patch| patch.description),
            Ok(NullablePatch::Absent)
        ));
        assert!(matches!(
            null.map(|patch| patch.description),
            Ok(NullablePatch::Null)
        ));
        assert!(matches!(
            value.map(|patch| patch.description),
            Ok(NullablePatch::Value(body)) if body == "context"
        ));
    }

    #[test]
    fn patch_rejects_null_for_non_nullable_fields() {
        for request in [
            json!({ "title": null }),
            json!({ "assigned_to": null }),
            json!({ "status": null }),
            json!({ "attachments": null }),
        ] {
            assert!(serde_json::from_value::<TodoPatch>(request).is_err());
        }
    }

    #[test]
    fn empty_patch_is_rejected_semantically() {
        let result = TodoPatch::default().validate(&DomainLimits::default());
        assert!(matches!(
            result.map_err(|error| error.kind),
            Err(ValidationErrorKind::EmptyPatch)
        ));
    }

    #[test]
    fn duplicate_attachments_are_rejected_after_canonicalization() {
        let request = serde_json::from_value::<TodoCreate>(json!({
            "title": "Ship it",
            "assigned_to": "head_of_growth:tos",
            "attachments": [
                "https://IMAGES.example:443/image.png",
                "https://images.example/image.png"
            ]
        }));
        let result = request
            .ok()
            .and_then(|request| request.validate(&DomainLimits::default()).err());
        assert!(matches!(
            result.map(|error| error.kind),
            Some(ValidationErrorKind::DuplicateItem)
        ));
    }
}
