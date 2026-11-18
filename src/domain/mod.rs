//! Pure Commit domain values and validation policy.

pub mod actor;
pub mod attachment;
pub mod ids;
pub mod notification;
pub mod pagination;
pub mod project;
pub mod todo;
pub mod validation;

pub use actor::{Actor, ActorRef, ActorType, ParseActorTypeError};
pub use attachment::{AttachmentUrl, AttachmentUrlError, MAX_ATTACHMENT_URL_BYTES};
pub use ids::{
    AccountUuid, AccountUuidError, ActorId, MAX_ACCOUNT_UUID_BYTES, MAX_PUBLIC_ID_CHARS,
    ProjectEntryId, ProjectId, ProjectTaskId, PublicIdError, TodoId, TodoNoteId,
};
pub use notification::{
    ExpectedNotificationVersion, ExpectedNotificationVersionError, MAX_WEBHOOK_URL_BYTES,
    NotificationRule, NotificationRuleInput, NotificationScope, NotificationSettings,
    NotificationSettingsUpdate, NotificationSubscriptionLevel, NotificationVersion,
    NotificationVersionError, TodoNotificationSubscription, TodoNotificationSubscriptionUpdate,
    ValidatedNotificationSettingsUpdate, ValidatedTodoNotificationSubscriptionUpdate, WebhookUrl,
};
pub use pagination::{
    CollectionQuery, CreatedAtRange, CursorError, DEFAULT_PAGE_LIMIT, InvalidDateRange,
    MAX_PAGE_LIMIT, Page, PageCursor, PageLimit, PageLimitError,
};
pub use project::{
    BlockerCreate, BlockerStatus, Diary, DiaryUpdate, DiaryVersion, DiaryVersionError,
    ExpectedDiaryVersion, MAX_PROJECT_SLUG_BYTES, MAX_PROJECT_UID_BYTES, Project,
    ProjectCompletionCreate, ProjectCreate, ProjectEntry, ProjectEntryType, ProjectLocator,
    ProjectPage, ProjectPatch, ProjectQuery, ProjectSlug, ProjectSlugError, ProjectStatus,
    ProjectTask, ProjectTaskCreate, ProjectTaskPatch, ProjectUid, ProjectUidError,
    ProjectUpdateCreate, ValidatedDiaryUpdate, ValidatedProjectCreate, ValidatedProjectEntryCreate,
    ValidatedProjectPatch, ValidatedProjectTaskCreate, ValidatedProjectTaskPatch,
};
pub use todo::{
    NullablePatch, ParseTodoStatusError, Todo, TodoCreate, TodoNote, TodoNoteCreate, TodoPage,
    TodoPatch, TodoQuery, TodoStatus, TodoView, ValidatedTodoCreate, ValidatedTodoNoteCreate,
    ValidatedTodoPatch,
};
pub use validation::{
    DomainLimits, LimitedText, MAX_DIARY_WORDS, RequiredText, ValidationError, ValidationErrorKind,
    ensure_char_limit, ensure_item_count, ensure_unique, unicode_word_count,
    validate_diary_markdown,
};

/// Validates and normalizes the category names on a project or todo.
pub(crate) fn categories(values: Vec<String>) -> Result<Vec<String>, ValidationError> {
    validation::ensure_item_count("categories", values.len(), 0, 32)?;
    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::new();
    for value in values {
        let name = value.trim();
        if name.is_empty() || name.chars().count() > 64 || name.chars().any(char::is_control) {
            return Err(ValidationError::invalid(
                "categories",
                "use names of 1 to 64 printable characters",
            ));
        }
        if seen.insert(name.to_lowercase()) {
            result.push(name.to_owned());
        }
    }
    Ok(result)
}

/// Estimated minutes and optional manual completion for a task or todo.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize, Eq, PartialEq)]
pub struct WorkPlan {
    /// Ideal time budget in minutes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ideal_minutes: Option<i32>,
    /// Manual completion percentage. Completed work always counts as 100 percent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<i16>,
}
impl WorkPlan {
    /// Enforce bounds before writing work estimates.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self
            .ideal_minutes
            .is_some_and(|n| !(1..=525_600).contains(&n))
            || self.progress.is_some_and(|n| !(0..=100).contains(&n))
        {
            return Err(ValidationError::invalid(
                "progress",
                "use 0–100 percent and 1–525600 ideal minutes",
            ));
        }
        Ok(())
    }
}
/// Partial update, with null to clear an estimate or progress override.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize, Eq, PartialEq)]
pub struct PlanPatch {
    /// Optional time change.
    #[serde(default, skip_serializing_if = "NullablePatch::is_absent")]
    pub ideal_minutes: NullablePatch<i32>,
    /// Optional progress change.
    #[serde(default, skip_serializing_if = "NullablePatch::is_absent")]
    pub progress: NullablePatch<i16>,
}
impl PlanPatch {
    /// Whether both settings are omitted.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.ideal_minutes.is_absent() && self.progress.is_absent()
    }
    /// Apply only supplied fields.
    #[must_use]
    pub fn apply(&self, current: &WorkPlan) -> WorkPlan {
        WorkPlan {
            ideal_minutes: self.ideal_minutes.apply(current.ideal_minutes),
            progress: self.progress.apply(current.progress),
        }
    }
}
impl<T: Copy> NullablePatch<T> {
    /// Apply a patch to an optional scalar.
    pub fn apply(&self, current: Option<T>) -> Option<T> {
        match self {
            Self::Absent => current,
            Self::Null => None,
            Self::Value(v) => Some(*v),
        }
    }
}
