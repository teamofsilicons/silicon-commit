//! Silicon-managed projects, diaries, tasks, and milestone entries.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::OffsetDateTime;
use uuid::Uuid;

use super::{
    actor::{Actor, ActorRef, ActorType},
    ids::{
        ActorId, OrganizationId, ProjectEntryId, ProjectId, ProjectTaskId, PublicOrganizationId,
    },
    pagination::{Page, PageCursor, PageLimit},
    todo::TodoStatus,
    validation::{
        DomainLimits, LimitedText, RequiredText, ValidationError, ValidationErrorKind,
        deserialize_optional_non_null, ensure_item_count, ensure_unique, validate_diary_markdown,
    },
};

/// Maximum normalized project slug length.
pub const MAX_PROJECT_SLUG_BYTES: usize = 200;
/// Maximum persisted project UID length.
pub const MAX_PROJECT_UID_BYTES: usize = 2_048;

/// Project lifecycle state.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "project_status", rename_all = "snake_case")]
pub enum ProjectStatus {
    /// Completed through the dedicated atomic completion operation.
    Completed,
    /// Progress cannot continue without intervention.
    Blocked,
    /// Project was intentionally abandoned.
    Canceled,
    /// Project has started.
    InProgress,
    /// Project has not started.
    #[default]
    YetToStart,
}

impl ProjectStatus {
    /// Returns the stable wire/database spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Blocked => "blocked",
            Self::Canceled => "canceled",
            Self::InProgress => "in_progress",
            Self::YetToStart => "yet_to_start",
        }
    }
}

impl fmt::Display for ProjectStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Invalid project status.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("invalid project status")]
pub struct ParseProjectStatusError;

impl FromStr for ProjectStatus {
    type Err = ParseProjectStatusError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "completed" => Ok(Self::Completed),
            "blocked" => Ok(Self::Blocked),
            "canceled" => Ok(Self::Canceled),
            "in_progress" => Ok(Self::InProgress),
            "yet_to_start" => Ok(Self::YetToStart),
            _ => Err(ParseProjectStatusError),
        }
    }
}

/// Immutable normalized locator derived from a project's creation name.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, sqlx::Type)]
#[serde(transparent)]
#[sqlx(transparent)]
pub struct ProjectSlug(String);

impl ProjectSlug {
    /// Slugifies a validated project creation name.
    pub fn from_name(name: &RequiredText) -> Result<Self, ProjectSlugError> {
        // Replace symbols before transliteration. The `slug` crate helpfully
        // names some emoji (for example 🚀 as "rocket"), but visual decoration
        // must not silently become part of a stable project identifier.
        let lexical_name = name
            .as_str()
            .chars()
            .map(|character| {
                if character.is_alphanumeric() {
                    character
                } else {
                    ' '
                }
            })
            .collect::<String>();
        let mut slug = slug::slugify(lexical_name);
        if slug.len() > MAX_PROJECT_SLUG_BYTES {
            slug.truncate(MAX_PROJECT_SLUG_BYTES);
            while slug.ends_with('-') {
                slug.pop();
            }
        }
        if slug.is_empty() {
            slug.push_str("project");
        }
        slug.parse()
    }

    /// Borrows the canonical slug.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consumes the slug into its storage representation.
    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl<'de> Deserialize<'de> for ProjectSlug {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

impl FromStr for ProjectSlug {
    type Err = ProjectSlugError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.is_empty() {
            return Err(ProjectSlugError::Empty);
        }
        if value.len() > MAX_PROJECT_SLUG_BYTES {
            return Err(ProjectSlugError::TooLong);
        }
        if value.starts_with('-')
            || value.ends_with('-')
            || value.contains("--")
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            return Err(ProjectSlugError::NotCanonical);
        }

        Ok(Self(value.to_owned()))
    }
}

impl fmt::Display for ProjectSlug {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Invalid canonical project slug.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ProjectSlugError {
    /// The name cannot produce an addressable slug.
    #[error("project name must contain at least one slug-compatible character")]
    Empty,
    /// The normalized slug exceeds its persistence limit.
    #[error("project slug must be at most {MAX_PROJECT_SLUG_BYTES} bytes")]
    TooLong,
    /// A stored/wire slug is not in canonical lowercase-hyphen form.
    #[error("project slug is not canonical")]
    NotCanonical,
}

/// Stable documented `{slug}:{creator_id}:{unix_ms}` identifier.
///
/// Actor IDs may themselves contain colons. The full UID is consequently
/// treated as an exact opaque locator by callers and persistence.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, sqlx::Type)]
#[serde(transparent)]
#[sqlx(transparent)]
pub struct ProjectUid(String);

impl ProjectUid {
    /// Generates the stable UID at project creation time.
    #[must_use]
    pub fn new(slug: &ProjectSlug, creator_id: &ActorId, created_at: OffsetDateTime) -> Self {
        let unix_millis = created_at.unix_timestamp_nanos().div_euclid(1_000_000);
        Self(format!("{slug}:{creator_id}:{unix_millis}"))
    }

    /// Borrows the opaque UID.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consumes the UID into its storage representation.
    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl<'de> Deserialize<'de> for ProjectUid {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

impl FromStr for ProjectUid {
    type Err = ProjectUidError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() > MAX_PROJECT_UID_BYTES {
            return Err(ProjectUidError::TooLong);
        }
        let Some((identity, millis)) = value.rsplit_once(':') else {
            return Err(ProjectUidError::Malformed);
        };
        let Some((slug, actor_id)) = identity.split_once(':') else {
            return Err(ProjectUidError::Malformed);
        };
        if actor_id.is_empty() || actor_id.chars().any(char::is_control) {
            return Err(ProjectUidError::Malformed);
        }
        slug.parse::<ProjectSlug>()
            .map_err(|_| ProjectUidError::Malformed)?;
        let millis = millis
            .parse::<i128>()
            .map_err(|_| ProjectUidError::Malformed)?;
        if millis.to_string() != value.rsplit_once(':').map_or("", |(_, value)| value) {
            return Err(ProjectUidError::Malformed);
        }

        Ok(Self(value.to_owned()))
    }
}

impl fmt::Display for ProjectUid {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Invalid stable project UID.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ProjectUidError {
    /// UID exceeds the defensive storage limit.
    #[error("project UID must be at most {MAX_PROJECT_UID_BYTES} bytes")]
    TooLong,
    /// UID does not have the canonical generated shape.
    #[error("project UID is malformed")]
    Malformed,
}

/// Accepted project path locator: UUID or exact stable UID, never a bare slug.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum ProjectLocator {
    /// Server-generated UUID.
    Id(ProjectId),
    /// Exact documented UID.
    Uid(ProjectUid),
}

impl FromStr for ProjectLocator {
    type Err = ProjectUidError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if let Ok(id) = Uuid::parse_str(value) {
            return Ok(Self::Id(ProjectId::from_uuid(id)));
        }

        value.parse().map(Self::Uid)
    }
}

/// Project visibility and content supplied at creation or returned on reads.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectDetails {
    /// Optional context, preserving Markdown formatting.
    #[serde(default)]
    pub description: String,
    /// HTTPS links; Commit does not upload these files.
    #[serde(default)]
    pub attachments: Vec<super::AttachmentUrl>,
    /// False means visible and editable within the organization.
    #[serde(default)]
    pub private: bool,
    /// Invited Carbon IDs, in addition to Silicon participants.
    #[serde(default)]
    pub carbon_ids: Vec<ActorId>,
    /// Live IAM membership tags granting access to a private project.
    #[serde(default)]
    pub tags: Vec<String>,
}
impl ProjectDetails {
    /// Bounds collections and content before persistence.
    pub fn validate(&self, limits: &DomainLimits) -> Result<(), ValidationError> {
        LimitedText::new(
            "description",
            self.description.clone(),
            limits.description_chars,
        )?;
        ensure_item_count(
            "attachments",
            self.attachments.len(),
            0,
            limits.attachments_per_todo,
        )?;
        ensure_unique("attachments", &self.attachments)?;
        ensure_item_count(
            "carbon_ids",
            self.carbon_ids.len(),
            0,
            limits.participants_per_project,
        )?;
        ensure_unique("carbon_ids", &self.carbon_ids)?;
        ensure_item_count("tags", self.tags.len(), 0, limits.participants_per_project)?;
        ensure_unique("tags", &self.tags)?;
        for tag in &self.tags {
            let value = RequiredText::new("tags", tag.clone(), 255)?;
            if value.as_str() != tag {
                return Err(ValidationError::invalid("tags", "tags must be trimmed"));
            }
        }
        Ok(())
    }
}

/// Nested creation input; generated task IDs establish parent relationships.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectSeedTask {
    /// Task title.
    pub title: String,
    /// Optional task context.
    #[serde(default)]
    pub description: String,
    /// Initial lifecycle state.
    #[serde(default)]
    pub status: TodoStatus,
    /// Optional active Carbon or Silicon ID.
    #[serde(default)]
    pub assigned_to: Option<ActorId>,
    /// Children belonging to this task, up to sixteen levels deep.
    #[serde(default)]
    pub subtasks: Vec<ProjectSeedTask>,
}

/// Untrusted `POST /projects` document.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectCreate {
    /// Project display name.
    pub name: String,
    /// Requested participating public Silicon IDs.
    #[serde(default)]
    pub silicon_ids: Vec<ActorId>,
    /// Visibility, invites and content.
    #[serde(flatten)]
    pub details: ProjectDetails,
    /// Optional tasks and subtasks created atomically with the project.
    #[serde(default)]
    pub tasks: Vec<ProjectSeedTask>,
}

impl ProjectCreate {
    /// Validates creation and ensures the creating Silicon is a participant.
    pub fn validate(
        self,
        limits: &DomainLimits,
        creator: &Actor,
    ) -> Result<ValidatedProjectCreate, ValidationError> {
        self.details.validate(limits)?;
        let mut details = self.details;
        if creator.actor_type == ActorType::Carbon && !details.carbon_ids.contains(&creator.id) {
            details.carbon_ids.push(creator.id.clone());
        }
        details.validate(limits)?;
        let name = RequiredText::new("name", self.name, limits.project_name_chars)?;
        let slug = ProjectSlug::from_name(&name)
            .map_err(|error| ValidationError::invalid("name", error.to_string()))?;
        ensure_item_count(
            "silicon_ids",
            self.silicon_ids.len(),
            0,
            limits.participants_per_project,
        )?;
        ensure_unique("silicon_ids", &self.silicon_ids)?;

        let mut silicon_ids = self.silicon_ids;
        if creator.actor_type == ActorType::Silicon && !silicon_ids.contains(&creator.id) {
            ensure_item_count(
                "silicon_ids",
                silicon_ids.len() + 1,
                1,
                limits.participants_per_project,
            )?;
            silicon_ids.push(creator.id.clone());
        }

        Ok(ValidatedProjectCreate {
            name,
            slug,
            silicon_ids,
            details,
            tasks: self.tasks,
        })
    }
}

/// Validated project creation command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatedProjectCreate {
    /// Trimmed project name.
    pub name: RequiredText,
    /// Immutable normalized creation-name slug.
    pub slug: ProjectSlug,
    /// Unique participant IDs, including the creator.
    pub silicon_ids: Vec<ActorId>,
    /// Validated project content and access settings.
    pub details: ProjectDetails,
    /// Initial nested work, validated by the service before its transaction.
    pub tasks: Vec<ProjectSeedTask>,
}

/// Untrusted `PATCH /projects/{project_id}` document.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectPatch {
    /// Replacement display name. It never changes the immutable slug.
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub name: Option<String>,
    /// Replacement lifecycle state. `completed` is forbidden here.
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub status: Option<ProjectStatus>,
    /// Complete replacement participant set.
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub silicon_ids: Option<Vec<ActorId>>,
    /// Replacement project description.
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub description: Option<String>,
    /// Replacement URL list.
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub attachments: Option<Vec<super::AttachmentUrl>>,
    /// Switch organization/private visibility.
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub private: Option<bool>,
    /// Replacement invited Carbon set.
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub carbon_ids: Option<Vec<ActorId>>,
    /// Replacement IAM tag set.
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub tags: Option<Vec<String>>,
}

impl ProjectPatch {
    /// Reports whether no fields were provided.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.status.is_none()
            && self.silicon_ids.is_none()
            && self.description.is_none()
            && self.attachments.is_none()
            && self.private.is_none()
            && self.carbon_ids.is_none()
            && self.tags.is_none()
    }

    /// Validates metadata changes without mutating the immutable slug.
    pub fn validate(self, limits: &DomainLimits) -> Result<ValidatedProjectPatch, ValidationError> {
        if self.is_empty() {
            return Err(ValidationError::new(
                "body",
                ValidationErrorKind::EmptyPatch,
            ));
        }
        if self.status == Some(ProjectStatus::Completed) {
            return Err(ValidationError::invalid(
                "status",
                "completed is only allowed through the completion endpoint",
            ));
        }

        let name = self
            .name
            .map(|value| RequiredText::new("name", value, limits.project_name_chars))
            .transpose()?;
        let silicon_ids = self
            .silicon_ids
            .map(|values| {
                ensure_item_count(
                    "silicon_ids",
                    values.len(),
                    0,
                    limits.participants_per_project,
                )?;
                ensure_unique("silicon_ids", &values)?;
                Ok(values)
            })
            .transpose()?;

        ProjectDetails {
            description: self.description.clone().unwrap_or_default(),
            attachments: self.attachments.clone().unwrap_or_default(),
            private: self.private.unwrap_or_default(),
            carbon_ids: self.carbon_ids.clone().unwrap_or_default(),
            tags: self.tags.clone().unwrap_or_default(),
        }
        .validate(limits)?;
        Ok(ValidatedProjectPatch {
            name,
            status: self.status,
            silicon_ids,
            description: self.description,
            attachments: self.attachments,
            private: self.private,
            carbon_ids: self.carbon_ids,
            tags: self.tags,
        })
    }
}

/// Fully validated project metadata patch.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ValidatedProjectPatch {
    /// Validated display-name replacement.
    pub name: Option<RequiredText>,
    /// Non-completion status replacement.
    pub status: Option<ProjectStatus>,
    /// Validated non-empty unique participant replacement.
    pub silicon_ids: Option<Vec<ActorId>>,
    /// Replacement project description.
    pub description: Option<String>,
    /// Replacement URL list.
    pub attachments: Option<Vec<super::AttachmentUrl>>,
    /// Switch organization/private visibility.
    pub private: Option<bool>,
    /// Replacement invited Carbon set.
    pub carbon_ids: Option<Vec<ActorId>>,
    /// Replacement IAM tag set.
    pub tags: Option<Vec<String>>,
}

impl ValidatedProjectPatch {
    /// Ensures a replacement participant set retains the immutable creator.
    ///
    /// A metadata-only patch has no participant effect and therefore always
    /// satisfies this invariant.
    pub fn ensure_creator_participates(&self, creator_id: &ActorId) -> Result<(), ValidationError> {
        if self
            .silicon_ids
            .as_ref()
            .is_some_and(|ids| !ids.contains(creator_id))
            && self
                .carbon_ids
                .as_ref()
                .is_none_or(|ids| !ids.contains(creator_id))
        {
            return Err(ValidationError::invalid(
                "participants",
                "must retain the project creator",
            ));
        }
        Ok(())
    }
}

/// Public project aggregate with internal tenant and principal identities.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Project {
    /// `UUIDv7` resource ID.
    pub id: ProjectId,
    /// Authoritative internal tenant key.
    pub organization_id: OrganizationId,
    /// Public organization ID snapshot.
    pub org_id: PublicOrganizationId,
    /// Mutable display name.
    pub name: RequiredText,
    /// Immutable normalized creation-name slug.
    pub slug: ProjectSlug,
    /// Immutable exact UID.
    pub uid: ProjectUid,
    /// Current state.
    pub status: ProjectStatus,
    /// Resolved unique Silicon participants.
    pub silicons: Vec<Actor>,
    /// Resolved creating Silicon.
    pub created_by: Actor,
    /// Creation timestamp.
    pub created_at: OffsetDateTime,
    /// Last meaningful update timestamp.
    pub updated_at: OffsetDateTime,
    /// Visibility, invites, description and attachments.
    pub details: ProjectDetails,
    /// Actors who have contributed to the project.
    pub collaborators: Vec<ActorRef>,
    /// Latest aggregate version.
    pub version: i64,
}

impl Project {
    /// Reports whether an internal IAM principal currently participates.
    #[must_use]
    pub fn has_participant(&self, actor: &Actor) -> bool {
        self.silicons
            .iter()
            .any(|participant| participant.principal_id == actor.principal_id)
    }
}

impl Serialize for Project {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        #[derive(Serialize)]
        struct WireProject<'a> {
            id: ProjectId,
            org_id: &'a PublicOrganizationId,
            name: &'a RequiredText,
            slug: &'a ProjectSlug,
            uid: &'a ProjectUid,
            status: ProjectStatus,
            silicon_ids: Vec<&'a ActorId>,
            #[serde(flatten)]
            details: &'a ProjectDetails,
            collaborators: &'a [ActorRef],
            version: i64,
            created_by: ActorRef,
            #[serde(with = "time::serde::rfc3339")]
            created_at: OffsetDateTime,
            #[serde(with = "time::serde::rfc3339")]
            updated_at: OffsetDateTime,
        }

        WireProject {
            id: self.id,
            org_id: &self.org_id,
            name: &self.name,
            slug: &self.slug,
            uid: &self.uid,
            status: self.status,
            silicon_ids: self
                .silicons
                .iter()
                .filter(|a| a.is_silicon())
                .map(|a| &a.id)
                .collect(),
            details: &self.details,
            collaborators: &self.collaborators,
            version: self.version,
            created_by: self.created_by.public_ref(),
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
        .serialize(serializer)
    }
}

/// Validated project list filters.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectQuery {
    /// Optional exact lifecycle state.
    pub status: Option<ProjectStatus>,
    /// Optional participating public Silicon ID.
    pub silicon_id: Option<ActorId>,
    /// Opaque keyset cursor.
    pub cursor: Option<PageCursor>,
    /// Validated page size.
    #[serde(default)]
    pub limit: PageLimit,
}

/// Paginated project response.
pub type ProjectPage = Page<Project>;

/// Positive optimistic-concurrency diary version.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, sqlx::Type)]
#[serde(transparent)]
#[sqlx(transparent)]
pub struct DiaryVersion(i64);

impl DiaryVersion {
    /// Creates a persisted version. Every diary starts at version 1.
    pub const fn new(value: i64) -> Result<Self, DiaryVersionError> {
        if value < 1 {
            Err(DiaryVersionError::OutOfRange)
        } else {
            Ok(Self(value))
        }
    }

    /// Returns the integer representation.
    #[must_use]
    pub const fn get(self) -> i64 {
        self.0
    }

    /// Computes the version after one successful replacement.
    pub fn next(self) -> Result<Self, DiaryVersionError> {
        self.0
            .checked_add(1)
            .ok_or(DiaryVersionError::Overflow)
            .and_then(Self::new)
    }
}

impl<'de> Deserialize<'de> for DiaryVersion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = i64::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// Invalid diary version.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum DiaryVersionError {
    /// Persisted versions must be positive.
    #[error("diary version must be at least 1")]
    OutOfRange,
    /// The version counter cannot be incremented safely.
    #[error("diary version is exhausted")]
    Overflow,
}

/// Parsed positive `If-Match` diary version.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ExpectedDiaryVersion(u64);

impl ExpectedDiaryVersion {
    /// Creates an expected version from a strictly positive header value.
    pub const fn new(value: u64) -> Result<Self, ExpectedDiaryVersionError> {
        if value == 0 {
            Err(ExpectedDiaryVersionError)
        } else {
            Ok(Self(value))
        }
    }

    /// Returns the header value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Reports whether this precondition matches a persisted version.
    #[must_use]
    pub fn matches(self, current: DiaryVersion) -> bool {
        i64::try_from(self.0).is_ok_and(|expected| expected == current.get())
    }
}

/// Invalid optimistic-concurrency precondition value.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("expected diary version must be at least 1")]
pub struct ExpectedDiaryVersionError;

/// Whole Markdown diary document.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Diary {
    /// Owning project UUID.
    pub project_id: ProjectId,
    /// Formatting-preserving Markdown.
    pub markdown: String,
    /// Current optimistic-concurrency version.
    pub version: DiaryVersion,
    /// Actor that wrote the current version.
    pub updated_by: Actor,
    /// Current version timestamp.
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

/// Untrusted whole-document diary replacement.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DiaryUpdate {
    /// Replacement Markdown; whitespace is significant.
    pub markdown: String,
}

impl DiaryUpdate {
    /// Enforces the Unicode word limit without altering the document.
    pub fn validate(self) -> Result<ValidatedDiaryUpdate, ValidationError> {
        let word_count = validate_diary_markdown(&self.markdown)?;
        Ok(ValidatedDiaryUpdate {
            markdown: self.markdown,
            word_count,
        })
    }
}

/// Validated diary replacement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatedDiaryUpdate {
    /// Exact replacement Markdown.
    pub markdown: String,
    /// Precomputed Unicode word count for audit/metrics.
    pub word_count: usize,
}

/// Untrusted task or subtask creation document.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ProjectTaskCreate {
    /// Optional parent task in the same project.
    #[serde(default)]
    pub parent_task_id: Option<ProjectTaskId>,
    /// Task title.
    pub title: String,
    /// Description, represented as empty when omitted.
    #[serde(default)]
    pub description: String,
    /// Initial lifecycle status.
    #[serde(default)]
    pub status: TodoStatus,
    /// Optional assignee public ID; omission leaves the task available to claim.
    #[serde(default)]
    pub assigned_to: Option<ActorId>,
}

impl ProjectTaskCreate {
    /// Validates title and description limits.
    pub fn validate(
        self,
        limits: &DomainLimits,
    ) -> Result<ValidatedProjectTaskCreate, ValidationError> {
        Ok(ValidatedProjectTaskCreate {
            assigned_to: self.assigned_to,
            parent_task_id: self.parent_task_id,
            title: RequiredText::new("title", self.title, limits.title_chars)?,
            description: LimitedText::new(
                "description",
                self.description,
                limits.description_chars,
            )?,
            status: self.status,
        })
    }
}

/// Validated task or subtask creation command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatedProjectTaskCreate {
    /// Optional same-project parent ID.
    pub parent_task_id: Option<ProjectTaskId>,
    /// Validated title.
    pub title: RequiredText,
    /// Always-represented description.
    pub description: LimitedText,
    /// Initial status.
    pub status: TodoStatus,
    /// Optional assignee public ID; omission leaves the task available to claim.
    pub assigned_to: Option<ActorId>,
}

/// Untrusted project-task patch document.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectTaskPatch {
    /// Replacement title.
    #[serde(default, deserialize_with = "deserialize_optional_non_null")]
    pub title: Option<String>,
    /// Replacement description.
    #[serde(default, deserialize_with = "deserialize_optional_non_null")]
    pub description: Option<String>,
    /// Replacement status.
    #[serde(default, deserialize_with = "deserialize_optional_non_null")]
    pub status: Option<TodoStatus>,
    /// Reassign, unassign with null, or preserve by omission.
    #[serde(default)]
    pub assigned_to: super::NullablePatch<ActorId>,
}

impl ProjectTaskPatch {
    /// Reports whether no change was requested.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.description.is_none()
            && self.status.is_none()
            && self.assigned_to.is_absent()
    }

    /// Validates supplied task changes.
    pub fn validate(
        self,
        limits: &DomainLimits,
    ) -> Result<ValidatedProjectTaskPatch, ValidationError> {
        if self.is_empty() {
            return Err(ValidationError::new(
                "body",
                ValidationErrorKind::EmptyPatch,
            ));
        }

        Ok(ValidatedProjectTaskPatch {
            assigned_to: self.assigned_to,
            title: self
                .title
                .map(|value| RequiredText::new("title", value, limits.title_chars))
                .transpose()?,
            description: self
                .description
                .map(|value| LimitedText::new("description", value, limits.description_chars))
                .transpose()?,
            status: self.status,
        })
    }
}

/// Validated project-task patch.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ValidatedProjectTaskPatch {
    /// Validated title replacement.
    pub title: Option<RequiredText>,
    /// Validated description replacement.
    pub description: Option<LimitedText>,
    /// Requested status replacement.
    pub status: Option<TodoStatus>,
    /// Reassign, unassign with null, or preserve by omission.
    pub assigned_to: super::NullablePatch<ActorId>,
}

/// Project-local task or subtask.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ProjectTask {
    /// `UUIDv7` task ID.
    pub id: ProjectTaskId,
    /// Owning project UUID.
    pub project_id: ProjectId,
    /// Optional same-project parent ID.
    pub parent_task_id: Option<ProjectTaskId>,
    /// Validated title.
    pub title: RequiredText,
    /// Always-represented description.
    pub description: LimitedText,
    /// Current task state.
    pub status: TodoStatus,
    /// Resolved creating actor.
    pub created_by: Actor,
    /// Creation timestamp.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Current assignee, when claimed or delegated.
    pub assigned_to: Option<ActorRef>,
    /// Todo sharing this task's work and lifecycle.
    pub todo_id: Option<super::TodoId>,
}

/// Project entry discriminator.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "project_entry_type", rename_all = "snake_case")]
pub enum ProjectEntryType {
    /// A blocking issue or question.
    Blocker,
    /// A milestone update.
    Update,
    /// The one immutable project completion statement.
    Completion,
}

/// Blocker lifecycle state.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "blocker_status", rename_all = "snake_case")]
pub enum BlockerStatus {
    /// Blocker still needs intervention.
    #[default]
    Open,
    /// Blocker has been addressed.
    Resolved,
}

/// Untrusted blocker creation document.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BlockerCreate {
    /// Blocker title.
    pub title: String,
    /// Blocker detail.
    pub description: String,
    /// Initial blocker state.
    #[serde(default)]
    pub status: BlockerStatus,
}

impl BlockerCreate {
    /// Validates a blocker append command.
    pub fn validate(
        self,
        limits: &DomainLimits,
    ) -> Result<ValidatedProjectEntryCreate, ValidationError> {
        validate_entry(
            ProjectEntryType::Blocker,
            self.title,
            self.description,
            Some(self.status),
            limits,
        )
    }
}

/// Untrusted project update creation document.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ProjectUpdateCreate {
    /// Update title.
    pub title: String,
    /// Update detail.
    pub description: String,
}

impl ProjectUpdateCreate {
    /// Validates a milestone update append command.
    pub fn validate(
        self,
        limits: &DomainLimits,
    ) -> Result<ValidatedProjectEntryCreate, ValidationError> {
        validate_entry(
            ProjectEntryType::Update,
            self.title,
            self.description,
            None,
            limits,
        )
    }
}

/// Untrusted atomic completion document.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ProjectCompletionCreate {
    /// Completion title.
    pub title: String,
    /// Completion statement.
    pub description: String,
}

impl ProjectCompletionCreate {
    /// Validates the append half of atomic project completion.
    pub fn validate(
        self,
        limits: &DomainLimits,
    ) -> Result<ValidatedProjectEntryCreate, ValidationError> {
        validate_entry(
            ProjectEntryType::Completion,
            self.title,
            self.description,
            None,
            limits,
        )
    }
}

/// Validated blocker, update, or completion append command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatedProjectEntryCreate {
    /// Entry discriminator fixed by the endpoint.
    pub entry_type: ProjectEntryType,
    /// Validated non-empty title.
    pub title: RequiredText,
    /// Bounded description.
    pub description: LimitedText,
    /// Present only for blocker entries.
    pub status: Option<BlockerStatus>,
}

/// Append-only project blocker, update, or completion statement.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ProjectEntry {
    /// `UUIDv7` entry ID.
    pub id: ProjectEntryId,
    /// Owning project UUID.
    pub project_id: ProjectId,
    /// Entry discriminator.
    #[serde(rename = "type")]
    pub entry_type: ProjectEntryType,
    /// Validated title.
    pub title: RequiredText,
    /// Bounded description.
    pub description: LimitedText,
    /// Blocker status; absent for updates and completion.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<BlockerStatus>,
    /// Resolved author.
    pub created_by: Actor,
    /// Creation timestamp.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

fn validate_entry(
    entry_type: ProjectEntryType,
    title: String,
    description: String,
    status: Option<BlockerStatus>,
    limits: &DomainLimits,
) -> Result<ValidatedProjectEntryCreate, ValidationError> {
    Ok(ValidatedProjectEntryCreate {
        entry_type,
        title: RequiredText::new("title", title, limits.title_chars)?,
        description: LimitedText::new("description", description, limits.description_chars)?,
        status,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use time::macros::datetime;
    use uuid::Uuid;

    use super::{
        DiaryUpdate, MAX_PROJECT_SLUG_BYTES, MAX_PROJECT_UID_BYTES, ProjectCreate, ProjectPatch,
        ProjectSlug, ProjectStatus, ProjectTaskCreate, ProjectTaskPatch, ProjectUid,
    };
    use crate::domain::{
        actor::{Actor, ActorType},
        ids::{ActorId, PrincipalId},
        validation::{DomainLimits, MAX_DIARY_WORDS, RequiredText, ValidationErrorKind},
    };

    fn creator() -> Option<Actor> {
        Some(Actor::new(
            PrincipalId::from_uuid(Uuid::nil()),
            ActorType::Silicon,
            ActorId::new("head_of_growth:tos").ok()?,
        ))
    }

    #[test]
    fn slug_is_normalized_once_from_the_creation_name() {
        let name = RequiredText::new("name", "  Launch Café 🚀  ", 200);
        let slug = name
            .as_ref()
            .ok()
            .and_then(|name| ProjectSlug::from_name(name).ok());
        assert_eq!(
            slug.map(ProjectSlug::into_inner),
            Some("launch-cafe".to_owned())
        );
    }

    #[test]
    fn symbol_only_project_names_receive_a_stable_generic_slug() {
        let name = RequiredText::new("name", "🚀", 200);
        let slug = name
            .as_ref()
            .ok()
            .and_then(|name| ProjectSlug::from_name(name).ok());
        assert_eq!(
            slug.map(ProjectSlug::into_inner),
            Some("project".to_owned())
        );
    }

    #[test]
    fn uid_preserves_colons_in_public_silicon_ids() {
        let slug = "launch".parse::<ProjectSlug>();
        let actor_id = ActorId::new("head_of_growth:tos");
        let uid = slug.ok().zip(actor_id.ok()).map(|(slug, actor_id)| {
            ProjectUid::new(&slug, &actor_id, datetime!(2026-08-31 12:00 UTC))
        });
        assert_eq!(
            uid.map(ProjectUid::into_inner),
            Some("launch:head_of_growth:tos:1788177600000".to_owned())
        );
    }

    #[test]
    fn uid_accepts_maximum_unicode_actor_ids_within_storage_ceiling() {
        let slug = "a".repeat(MAX_PROJECT_SLUG_BYTES).parse::<ProjectSlug>();
        let actor_id = ActorId::new("🦀".repeat(crate::domain::ids::MAX_PUBLIC_ID_CHARS));
        let uid = slug.ok().zip(actor_id.ok()).map(|(slug, actor_id)| {
            ProjectUid::new(&slug, &actor_id, datetime!(2026-08-31 12:00 UTC))
        });

        assert!(uid.as_ref().is_some_and(|uid| uid.as_str().len() > 1_024));
        assert!(uid.as_ref().is_some_and(|uid| {
            uid.as_str().len() <= MAX_PROJECT_UID_BYTES
                && uid.as_str().parse::<ProjectUid>().as_ref() == Ok(uid)
        }));
    }

    #[test]
    fn project_creation_adds_creator_without_duplicating_participants() {
        let Some(creator) = creator() else {
            return;
        };
        let request = serde_json::from_value::<ProjectCreate>(json!({
            "name": "Launch",
            "silicon_ids": ["engineer:tos"]
        }));
        let validated = request
            .ok()
            .and_then(|request| request.validate(&DomainLimits::default(), &creator).ok());
        assert_eq!(validated.map(|value| value.silicon_ids.len()), Some(2));
    }

    #[test]
    fn generic_project_patch_cannot_complete_a_project() {
        let patch = ProjectPatch {
            status: Some(ProjectStatus::Completed),
            ..ProjectPatch::default()
        };
        let result = patch.validate(&DomainLimits::default());
        assert!(matches!(
            result.map_err(|error| error.kind),
            Err(ValidationErrorKind::Invalid(_))
        ));
    }

    #[test]
    fn project_patch_rejects_explicit_null_fields() {
        for document in [
            json!({ "name": null }),
            json!({ "status": null }),
            json!({ "silicon_ids": null }),
        ] {
            assert!(serde_json::from_value::<ProjectPatch>(document).is_err());
        }
    }

    #[test]
    fn project_task_patch_rejects_explicit_null_fields() {
        for document in [
            json!({ "title": null }),
            json!({ "description": null }),
            json!({ "status": null }),
        ] {
            assert!(serde_json::from_value::<ProjectTaskPatch>(document).is_err());
        }
    }

    #[test]
    fn participant_replacement_must_retain_the_creator() {
        let Some(creator) = creator() else {
            return;
        };
        let other = ActorId::new("engineer:tos");
        let result = other.ok().and_then(|other| {
            ProjectPatch {
                silicon_ids: Some(vec![other]),
                ..ProjectPatch::default()
            }
            .validate(&DomainLimits::default())
            .ok()
        });

        assert!(
            result.is_some_and(|patch| patch.ensure_creator_participates(&creator.id).is_err())
        );
    }

    #[test]
    fn diary_validation_preserves_exact_markdown() {
        let markdown = "  # Heading\n\nbody  \n".to_owned();
        let result = DiaryUpdate {
            markdown: markdown.clone(),
        }
        .validate();
        assert_eq!(result.map(|value| value.markdown), Ok(markdown));
    }

    #[test]
    fn diary_rejects_one_word_over_the_product_limit() {
        let markdown = std::iter::repeat_n("word", MAX_DIARY_WORDS + 1)
            .collect::<Vec<_>>()
            .join(" ");
        assert!(DiaryUpdate { markdown }.validate().is_err());
    }

    #[test]
    fn project_task_defaults_are_contract_values() {
        let request = serde_json::from_value::<ProjectTaskCreate>(json!({ "title": "Design" }));
        assert!(matches!(
            request,
            Ok(ProjectTaskCreate {
                description,
                status: crate::domain::todo::TodoStatus::YetToDo,
                ..
            }) if description.is_empty()
        ));
    }
}
