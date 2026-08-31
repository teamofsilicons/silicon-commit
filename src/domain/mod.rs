//! Pure Commit domain values and validation policy.

pub mod actor;
pub mod attachment;
pub mod ids;
pub mod pagination;
pub mod project;
pub mod todo;
pub mod validation;

pub use actor::{Actor, ActorRef, ActorType, ParseActorTypeError};
pub use attachment::{
    AttachmentPolicyError, AttachmentUrlError, AttachmentUrlPolicy, MAX_ATTACHMENT_URL_BYTES,
    PermanentAttachmentUrl,
};
pub use ids::{
    ActorId, MAX_PUBLIC_ID_CHARS, OrganizationId, PrincipalId, ProjectEntryId, ProjectId,
    ProjectTaskId, PublicIdError, PublicOrganizationId, TodoId, TodoNoteId,
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
