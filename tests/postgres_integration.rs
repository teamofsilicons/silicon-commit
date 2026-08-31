//! PostgreSQL-backed integration coverage for Commit's transactional workflows.

use std::{
    collections::{HashMap, HashSet},
    env,
    num::{NonZeroU32, NonZeroUsize},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use anyhow::{Context as _, bail};
use async_trait::async_trait;
use secrecy::SecretString;
use sqlx::PgPool;
use time::OffsetDateTime;
use url::Url;
use uuid::Uuid;

use silicon_commit::{
    application::{
        attachments::{AttachmentService, TemporaryUrlRequest},
        idempotency::{IdempotencyKey, MutationResponse},
        notifications::NotificationSettingsService,
        ports::{
            ActiveMember, AuthenticationRequest, BriefcaseProvider, CapabilitySet,
            ChildProofRequest, DelegatedOboProof, IdentityProvider, InboundCredential,
            OrganizationRole, ProviderError, TemporaryUrl, TrustedIdentity, VerifiedActor,
        },
        projects::ProjectService,
        todos::TodoService,
    },
    config::DatabaseSettings,
    domain::{
        Actor, ActorId, ActorType, AttachmentUrl, BlockerCreate, BlockerStatus,
        BriefcaseAttachmentUrl, BriefcaseUrlPolicy, CollectionQuery, DiaryUpdate, DomainLimits,
        ExpectedDiaryVersion, ExpectedNotificationVersion, NotificationRuleInput,
        NotificationScope, NotificationSettingsUpdate, NullablePatch, OrganizationId, PageLimit,
        PrincipalId, ProjectCompletionCreate, ProjectCreate, ProjectId, ProjectLocator,
        ProjectPatch, ProjectQuery, ProjectStatus, ProjectTaskCreate, ProjectTaskId,
        ProjectTaskPatch, ProjectUpdateCreate, PublicOrganizationId, TodoCreate, TodoId,
        TodoNoteCreate, TodoNotificationSubscriptionUpdate, TodoPatch, TodoQuery, TodoStatus,
    },
    error::AppError,
    infrastructure::postgres,
    worker::retention::{self, RetentionPolicy},
};

const IDEMPOTENCY_TTL: Duration = Duration::from_hours(24);
const AUDIT_RETENTION: Duration = Duration::from_hours(7 * 365 * 24);

#[derive(Clone)]
struct TestOrganization {
    id: OrganizationId,
    public_id: PublicOrganizationId,
}

impl TestOrganization {
    fn unique(label: &str) -> anyhow::Result<Self> {
        let suffix = Uuid::new_v4().simple().to_string();
        Ok(Self {
            id: OrganizationId::from_uuid(Uuid::new_v4()),
            public_id: PublicOrganizationId::new(format!("{label}-{suffix}"))?,
        })
    }

    fn actor(
        &self,
        label: &str,
        actor_type: ActorType,
        role: OrganizationRole,
    ) -> anyhow::Result<VerifiedActor> {
        let suffix = Uuid::new_v4().simple().to_string();
        let actor = Actor::new(
            PrincipalId::from_uuid(Uuid::new_v4()),
            actor_type,
            ActorId::new(format!("{label}-{suffix}"))?,
        );
        let capabilities = CapabilitySet::default();
        let membership_id = Uuid::new_v4();
        let identity = TrustedIdentity {
            organization_id: self.id,
            org_id: self.public_id.clone(),
            membership_id,
            actor: actor.clone(),
            organization_role: role,
            capabilities: capabilities.clone(),
        };
        Ok(VerifiedActor::new(
            self.id,
            self.public_id.clone(),
            membership_id,
            actor,
            role,
            capabilities,
            InboundCredential::trusted(identity),
        ))
    }
}

#[derive(Default)]
struct TestDirectory {
    members: HashMap<(PublicOrganizationId, ActorId), ActiveMember>,
}

impl TestDirectory {
    fn new(members: impl IntoIterator<Item = ActiveMember>) -> Self {
        Self {
            members: members
                .into_iter()
                .map(|member| ((member.org_id.clone(), member.actor.id.clone()), member))
                .collect(),
        }
    }
}

#[async_trait]
impl IdentityProvider for TestDirectory {
    async fn authenticate(
        &self,
        _request: &AuthenticationRequest,
    ) -> Result<VerifiedActor, ProviderError> {
        Err(ProviderError::Unavailable)
    }

    async fn resolve_active_members(
        &self,
        org_id: &PublicOrganizationId,
        actor_ids: &[ActorId],
        required_type: Option<ActorType>,
    ) -> Result<Vec<ActiveMember>, ProviderError> {
        let mut seen = HashSet::with_capacity(actor_ids.len());
        let mut members = Vec::with_capacity(actor_ids.len());
        for actor_id in actor_ids {
            if !seen.insert(actor_id.clone()) {
                continue;
            }
            let member = self
                .members
                .get(&(org_id.clone(), actor_id.clone()))
                .cloned()
                .ok_or(ProviderError::NotFound)?;
            if required_type.is_some_and(|actor_type| actor_type != member.actor.actor_type) {
                return Err(ProviderError::NotFound);
            }
            members.push(member);
        }
        Ok(members)
    }

    async fn exchange_child_proof(
        &self,
        _actor: &VerifiedActor,
        _request: &ChildProofRequest,
    ) -> Result<DelegatedOboProof, ProviderError> {
        Err(ProviderError::Unavailable)
    }
}

#[derive(Default)]
struct AttachmentDependencyProbe {
    iam_calls: AtomicUsize,
    briefcase_calls: AtomicUsize,
}

#[async_trait]
impl IdentityProvider for AttachmentDependencyProbe {
    async fn authenticate(
        &self,
        _request: &AuthenticationRequest,
    ) -> Result<VerifiedActor, ProviderError> {
        Err(ProviderError::Unavailable)
    }

    async fn resolve_active_members(
        &self,
        _org_id: &PublicOrganizationId,
        _actor_ids: &[ActorId],
        _required_type: Option<ActorType>,
    ) -> Result<Vec<ActiveMember>, ProviderError> {
        Err(ProviderError::Unavailable)
    }

    async fn exchange_child_proof(
        &self,
        _actor: &VerifiedActor,
        _request: &ChildProofRequest,
    ) -> Result<DelegatedOboProof, ProviderError> {
        self.iam_calls.fetch_add(1, Ordering::Relaxed);
        Err(ProviderError::Unavailable)
    }
}

#[async_trait]
impl BriefcaseProvider for AttachmentDependencyProbe {
    async fn temporary_url(
        &self,
        _org_id: &PublicOrganizationId,
        _attachment: &BriefcaseAttachmentUrl,
        _proof: &DelegatedOboProof,
    ) -> Result<TemporaryUrl, ProviderError> {
        self.briefcase_calls.fetch_add(1, Ordering::Relaxed);
        Err(ProviderError::Unavailable)
    }
}

#[tokio::test]
async fn migrations_establish_the_complete_schema() -> anyhow::Result<()> {
    let Some(pool) = test_pool().await? else {
        return Ok(());
    };

    let successful_migrations =
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM public._sqlx_migrations WHERE success")
            .fetch_one(&pool)
            .await?;
    assert!(successful_migrations >= 15);

    for relation in [
        "commit.todos",
        "commit.projects",
        "commit.idempotency_records",
        "commit.outbox_events",
        "commit.silicon_notification_settings",
        "commit.todo_notification_subscriptions",
    ] {
        let exists = sqlx::query_scalar::<_, bool>("SELECT to_regclass($1) IS NOT NULL")
            .bind(relation)
            .fetch_one(&pool)
            .await?;
        assert!(exists, "migration did not create {relation}");
    }
    let migration_ledgers = sqlx::query_as::<_, (bool, bool)>(
        r"
        SELECT
            to_regclass('public._sqlx_migrations') IS NOT NULL,
            to_regclass('commit._sqlx_migrations') IS NOT NULL
        ",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(migration_ledgers, (true, false));

    let fixed_runtime_role = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'silicon_commit_runtime')",
    )
    .fetch_one(&pool)
    .await?;
    assert!(
        !fixed_runtime_role,
        "application migrations must not provision a cluster-global runtime role"
    );

    let public_privileges = sqlx::query_as::<_, (bool, bool, bool, bool)>(
        r"
        SELECT
            NOT EXISTS (
                SELECT 1
                FROM pg_catalog.pg_namespace AS namespace
                CROSS JOIN LATERAL aclexplode(
                    COALESCE(
                        namespace.nspacl,
                        acldefault('n', namespace.nspowner)
                    )
                ) AS privilege
                WHERE namespace.nspname IN ('commit', 'commit_private')
                  AND privilege.grantee = 0
            ),
            NOT EXISTS (
                SELECT 1
                FROM pg_catalog.pg_class AS relation
                JOIN pg_catalog.pg_namespace AS namespace
                  ON namespace.oid = relation.relnamespace
                CROSS JOIN LATERAL aclexplode(
                    COALESCE(relation.relacl, '{}'::aclitem[])
                ) AS privilege
                WHERE namespace.nspname = 'commit'
                  AND relation.relkind IN ('r', 'p', 'v', 'm', 'S', 'f')
                  AND privilege.grantee = 0
            ),
            NOT EXISTS (
                SELECT 1
                FROM pg_catalog.pg_proc AS routine
                JOIN pg_catalog.pg_namespace AS namespace
                  ON namespace.oid = routine.pronamespace
                CROSS JOIN LATERAL aclexplode(
                    COALESCE(routine.proacl, acldefault('f', routine.proowner))
                ) AS privilege
                WHERE namespace.nspname IN ('commit', 'commit_private')
                  AND privilege.grantee = 0
            ),
            NOT EXISTS (
                SELECT 1
                FROM pg_catalog.pg_type AS data_type
                JOIN pg_catalog.pg_namespace AS namespace
                  ON namespace.oid = data_type.typnamespace
                CROSS JOIN LATERAL aclexplode(
                    COALESCE(data_type.typacl, acldefault('T', data_type.typowner))
                ) AS privilege
                WHERE namespace.nspname = 'commit'
                  AND data_type.typtype = 'e'
                  AND privilege.grantee = 0
            )
        ",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(public_privileges, (true, true, true, true));

    let retention_schema_shape = sqlx::query_as::<_, (bool, bool, bool, bool, bool, bool)>(
        r"
        SELECT
            EXISTS (
                SELECT 1
                FROM information_schema.columns
                WHERE table_schema = 'commit'
                  AND table_name = 'todo_activity'
                  AND column_name = 'retain_until'
                  AND is_nullable = 'NO'
            ),
            to_regclass('commit.todo_activity_retention_idx') IS NOT NULL,
            EXISTS (
                SELECT 1
                FROM information_schema.columns
                WHERE table_schema = 'commit'
                  AND table_name = 'todos'
                  AND column_name = 'content_retain_until'
                  AND is_nullable = 'YES'
            ),
            EXISTS (
                SELECT 1
                FROM information_schema.columns
                WHERE table_schema = 'commit'
                  AND table_name = 'outbox_events'
                  AND column_name = 'purge_after'
                  AND is_nullable = 'YES'
            ),
            EXISTS (
                SELECT 1
                FROM information_schema.columns
                WHERE table_schema = 'commit'
                  AND table_name = 'idempotency_records'
                  AND column_name = 'todo_id'
                  AND is_nullable = 'YES'
            ),
            to_regclass('commit.idempotency_records_todo_expiry_idx') IS NOT NULL
        ",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(retention_schema_shape, (true, true, true, true, true, true));

    let attachment_schema_shape = sqlx::query_as::<_, (bool, bool, bool, bool)>(
        r"
        SELECT
            EXISTS (
                SELECT 1
                FROM information_schema.columns
                WHERE table_schema = 'commit'
                  AND table_name = 'todo_attachments'
                  AND column_name = 'url'
                  AND is_nullable = 'NO'
            ),
            NOT EXISTS (
                SELECT 1
                FROM information_schema.columns
                WHERE table_schema = 'commit'
                  AND table_name = 'todo_attachments'
                  AND column_name = 'permanent_url'
            ),
            EXISTS (
                SELECT 1
                FROM pg_catalog.pg_constraint
                WHERE conrelid = 'commit.todo_attachments'::regclass
                  AND conname = 'todo_attachments_https_url'
                  AND contype = 'c'
            ),
            EXISTS (
                SELECT 1
                FROM pg_catalog.pg_constraint
                WHERE conrelid = 'commit.todo_attachments'::regclass
                  AND conname = 'todo_attachments_organization_id_todo_id_url_key'
                  AND contype = 'u'
            )
        ",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(attachment_schema_shape, (true, true, true, true));

    let mut transaction = pool.begin().await?;
    let organization_id = Uuid::new_v4();
    let unicode_public_id = "🦀".repeat(255);
    sqlx::query(
        r"
        INSERT INTO commit.organization_projection (organization_id, org_id)
        VALUES ($1, $2)
        ",
    )
    .bind(organization_id)
    .bind(&unicode_public_id)
    .execute(&mut *transaction)
    .await?;
    sqlx::query(
        r"
        INSERT INTO commit.actor_projection (
            organization_id,
            principal_id,
            membership_id,
            actor_type,
            actor_id
        )
        VALUES ($1, $2, $3, 'silicon', $4)
        ",
    )
    .bind(organization_id)
    .bind(Uuid::new_v4())
    .bind(Uuid::new_v4())
    .bind(&unicode_public_id)
    .execute(&mut *transaction)
    .await?;
    transaction.rollback().await?;

    Ok(())
}

#[tokio::test]
async fn versioned_timestamps_do_not_regress_in_an_older_transaction() -> anyhow::Result<()> {
    let Some(pool) = test_pool().await? else {
        return Ok(());
    };
    let organization = TestOrganization::unique("timestamp-org")?;
    let actor = organization.actor(
        "timestamp-carbon",
        ActorType::Carbon,
        OrganizationRole::Member,
    )?;
    seed_identity(&pool, &actor).await?;
    let todo_id = TodoId::new();
    sqlx::query(
        r"
        INSERT INTO commit.todos (
            id,
            organization_id,
            title,
            assigned_by_principal_id,
            assigned_to_principal_id
        ) VALUES ($1, $2, 'initial', $3, $3)
        ",
    )
    .bind(todo_id.into_uuid())
    .bind(organization.id.into_uuid())
    .bind(actor.actor.principal_id.into_uuid())
    .execute(&pool)
    .await?;

    let mut older_transaction = pool.begin().await?;
    let _: OffsetDateTime = sqlx::query_scalar("SELECT transaction_timestamp()")
        .fetch_one(&mut *older_transaction)
        .await?;

    let newer_mutation = sqlx::query_as::<_, (OffsetDateTime, i64)>(
        r"
        UPDATE commit.todos
        SET title = 'newer transaction'
        WHERE organization_id = $1 AND id = $2
        RETURNING updated_at, version
        ",
    )
    .bind(organization.id.into_uuid())
    .bind(todo_id.into_uuid())
    .fetch_one(&pool)
    .await?;

    let older_mutation = sqlx::query_as::<_, (OffsetDateTime, i64)>(
        r"
        UPDATE commit.todos
        SET title = 'older transaction, later mutation'
        WHERE organization_id = $1 AND id = $2
        RETURNING updated_at, version
        ",
    )
    .bind(organization.id.into_uuid())
    .bind(todo_id.into_uuid())
    .fetch_one(&mut *older_transaction)
    .await?;
    older_transaction.commit().await?;

    assert!(older_mutation.0 >= newer_mutation.0);
    assert_eq!(newer_mutation.1, 2);
    assert_eq!(older_mutation.1, 3);

    Ok(())
}

#[tokio::test]
async fn retained_identity_mappings_reject_iam_tenant_remapping() -> anyhow::Result<()> {
    let Some(pool) = test_pool().await? else {
        return Ok(());
    };
    let organization = TestOrganization::unique("mapping-org")?;
    let actor = organization.actor(
        "mapping-carbon",
        ActorType::Carbon,
        OrganizationRole::Member,
    )?;
    seed_identity(&pool, &actor).await?;
    postgres::assert_identity_consistency(&pool, &actor).await?;

    let changed_public = TestOrganization {
        id: organization.id,
        public_id: PublicOrganizationId::new(format!(
            "{}-remapped",
            organization.public_id.as_str()
        ))?,
    }
    .actor(
        "mapping-carbon",
        ActorType::Carbon,
        OrganizationRole::Member,
    )?;
    assert!(matches!(
        postgres::assert_identity_consistency(&pool, &changed_public).await,
        Err(AppError::BadGateway)
    ));

    let changed_internal = TestOrganization {
        id: OrganizationId::from_uuid(Uuid::new_v4()),
        public_id: organization.public_id,
    }
    .actor(
        "mapping-carbon",
        ActorType::Carbon,
        OrganizationRole::Member,
    )?;
    assert!(matches!(
        postgres::assert_identity_consistency(&pool, &changed_internal).await,
        Err(AppError::BadGateway)
    ));
    Ok(())
}

#[tokio::test]
async fn todo_lifecycle_enforces_replay_tenant_and_actor_boundaries() -> anyhow::Result<()> {
    let Some(pool) = test_pool().await? else {
        return Ok(());
    };
    let organization = TestOrganization::unique("todo-org")?;
    let assigner = organization.actor(
        "delegating-silicon",
        ActorType::Silicon,
        OrganizationRole::Member,
    )?;
    let assignee = organization.actor(
        "assigned-carbon",
        ActorType::Carbon,
        OrganizationRole::Member,
    )?;
    let outsider = organization.actor(
        "unrelated-carbon",
        ActorType::Carbon,
        OrganizationRole::Member,
    )?;
    let other_organization = TestOrganization::unique("other-todo-org")?;
    let other_tenant_actor =
        other_organization.actor("other-carbon", ActorType::Carbon, OrganizationRole::Owner)?;
    let directory = Arc::new(TestDirectory::new([active_member(&assignee)]));
    let service = TodoService::new(
        pool.clone(),
        directory,
        DomainLimits::default(),
        IDEMPOTENCY_TTL,
        AUDIT_RETENTION,
        Duration::from_hours(1_080),
    );
    let notification_service = NotificationSettingsService::new(pool.clone(), AUDIT_RETENTION);
    let notification_settings = notification_service
        .replace_settings(
            &assigner,
            NotificationSettingsUpdate {
                webhook_url: Some(format!(
                    "https://hook.example.com/silicon/{}/A1B2C3",
                    assigner.actor.id
                )),
                todo_list_subscription: Some(NotificationRuleInput {
                    scope: NotificationScope::AnyUpdate,
                    statuses: Vec::new(),
                }),
            },
            ExpectedNotificationVersion::new(0)?,
            "req-notification-settings-create",
        )
        .await?;
    assert_eq!(notification_settings.version.get(), 1);
    let attachment = format!(
        "https://briefcase.example/api/v1/entries/{}",
        Uuid::new_v4().hyphenated()
    );
    let external_attachment = format!(
        "https://images.example/assets/{}.png?variant=large",
        Uuid::new_v4().simple()
    );
    let request = TodoCreate {
        title: "Prepare launch review".to_owned(),
        description: Some("Preserve this formatting.\n\n- first\n- second".to_owned()),
        assigned_to: assignee.actor.id.clone(),
        status: TodoStatus::YetToDo,
        attachments: vec![
            AttachmentUrl::new(&attachment)?,
            AttachmentUrl::new(&external_attachment)?,
        ],
    };
    let create_key = unique_key("todo-create")?;

    let created = service
        .create(
            &assigner,
            request.clone(),
            create_key.clone(),
            "req-todo-create",
        )
        .await?;
    assert_eq!(created.status, 201);
    assert!(!created.replayed);
    let todo_id = TodoId::from_uuid(response_uuid(&created, "id")?);
    let round_tripped = service.get(&assigner, todo_id).await?;
    assert_eq!(
        round_tripped
            .attachments
            .iter()
            .map(AttachmentUrl::as_str)
            .collect::<Vec<_>>(),
        vec![attachment.as_str(), external_attachment.as_str()]
    );
    let stored_attachments = sqlx::query_scalar::<_, String>(
        r"
        SELECT url
        FROM commit.todo_attachments
        WHERE organization_id = $1
          AND todo_id = $2
        ORDER BY position
        ",
    )
    .bind(organization.id.into_uuid())
    .bind(todo_id.into_uuid())
    .fetch_all(&pool)
    .await?;
    assert_eq!(
        stored_attachments,
        vec![attachment, external_attachment.clone()]
    );

    let dependency_probe = Arc::new(AttachmentDependencyProbe::default());
    let identity: Arc<dyn IdentityProvider> = dependency_probe.clone();
    let briefcase: Arc<dyn BriefcaseProvider> = dependency_probe.clone();
    let attachment_service =
        AttachmentService::new(pool.clone(), identity, briefcase, briefcase_policy()?);
    let external_temporary_url = attachment_service
        .temporary_url(
            &assigner,
            TemporaryUrlRequest {
                permanent_url: AttachmentUrl::new(&external_attachment)?,
            },
        )
        .await;
    assert!(matches!(
        external_temporary_url,
        Err(AppError::Validation { ref details })
            if details.get("permanent_url").is_some()
    ));
    assert_eq!(dependency_probe.iam_calls.load(Ordering::Relaxed), 0);
    assert_eq!(dependency_probe.briefcase_calls.load(Ordering::Relaxed), 0);
    let initial_projection_versions = sqlx::query_as::<_, (i64, i64, i64)>(
        r"
        SELECT organization.xmin::text::bigint,
               assigner.xmin::text::bigint,
               assignee.xmin::text::bigint
        FROM commit.organization_projection AS organization
        JOIN commit.actor_projection AS assigner
          ON assigner.organization_id = organization.organization_id
         AND assigner.principal_id = $2
        JOIN commit.actor_projection AS assignee
          ON assignee.organization_id = organization.organization_id
         AND assignee.principal_id = $3
        WHERE organization.organization_id = $1
        ",
    )
    .bind(organization.id.into_uuid())
    .bind(assigner.actor.principal_id.into_uuid())
    .bind(assignee.actor.principal_id.into_uuid())
    .fetch_one(&pool)
    .await?;

    let replay = service
        .create(
            &assigner,
            request.clone(),
            create_key.clone(),
            "req-todo-replay",
        )
        .await?;
    assert!(replay.replayed);
    assert_eq!(replay.status, created.status);
    assert_eq!(replay.body, created.body);

    let remapped_actor = Actor::new(
        assignee.actor.principal_id,
        assignee.actor.actor_type,
        ActorId::new(format!("remapped-{}", Uuid::new_v4().simple()))?,
    );
    let remapped_identity = TrustedIdentity {
        organization_id: assignee.organization_id,
        org_id: assignee.org_id.clone(),
        membership_id: assignee.membership_id,
        actor: remapped_actor.clone(),
        organization_role: assignee.organization_role,
        capabilities: assignee.capabilities.clone(),
    };
    let remapped_assignee = VerifiedActor::new(
        assignee.organization_id,
        assignee.org_id.clone(),
        assignee.membership_id,
        remapped_actor,
        assignee.organization_role,
        assignee.capabilities.clone(),
        InboundCredential::trusted(remapped_identity),
    );
    let remap_attempt = service
        .update(
            &remapped_assignee,
            todo_id,
            TodoPatch {
                status: Some(TodoStatus::InProgress),
                ..TodoPatch::default()
            },
            unique_key("todo-remapped-identity")?,
            "req-todo-remapped-identity",
        )
        .await;
    assert!(matches!(remap_attempt, Err(AppError::BadGateway)));

    let mut conflicting_request = request;
    conflicting_request.title = "Different semantic request".to_owned();
    let conflict = service
        .create(
            &assigner,
            conflicting_request,
            create_key,
            "req-todo-conflict",
        )
        .await;
    assert_conflict(conflict, "idempotency_key_reused")?;

    let hidden = service.get(&other_tenant_actor, todo_id).await;
    assert!(matches!(hidden, Err(AppError::NotFound)));

    let forbidden = service
        .update(
            &outsider,
            todo_id,
            TodoPatch {
                title: Some("Unauthorized rewrite".to_owned()),
                ..TodoPatch::default()
            },
            unique_key("todo-forbidden")?,
            "req-todo-forbidden",
        )
        .await;
    assert!(matches!(forbidden, Err(AppError::Forbidden)));

    let updated = service
        .update(
            &assignee,
            todo_id,
            TodoPatch {
                title: None,
                description: NullablePatch::Absent,
                assigned_to: None,
                status: Some(TodoStatus::InProgress),
                attachments: None,
            },
            unique_key("todo-update")?,
            "req-todo-update",
        )
        .await?;
    assert_eq!(
        updated.body.get("status"),
        Some(&serde_json::json!("in_progress"))
    );

    let note = service
        .add_note(
            &assignee,
            todo_id,
            TodoNoteCreate {
                body: "Work has started".to_owned(),
            },
            unique_key("todo-note")?,
            "req-todo-note",
        )
        .await?;
    assert_eq!(note.status, 201);
    let notes = service
        .list_notes(&assigner, todo_id, CollectionQuery::default())
        .await?;
    assert_eq!(notes.items.len(), 1);
    assert_eq!(notes.items[0].body.as_str(), "Work has started");

    service
        .add_note(
            &assignee,
            todo_id,
            TodoNoteCreate {
                body: "Work is still progressing".to_owned(),
            },
            unique_key("todo-note-second")?,
            "req-todo-note-second",
        )
        .await?;
    let page_limit = PageLimit::new(1)?;
    let first_note_page = service
        .list_notes(
            &assigner,
            todo_id,
            CollectionQuery {
                cursor: None,
                limit: page_limit,
            },
        )
        .await?;
    assert_eq!(first_note_page.items.len(), 1);
    assert!(first_note_page.next_cursor.is_some());
    let second_note_page = service
        .list_notes(
            &assigner,
            todo_id,
            CollectionQuery {
                cursor: first_note_page.next_cursor,
                limit: page_limit,
            },
        )
        .await?;
    assert_eq!(second_note_page.items.len(), 1);
    assert!(second_note_page.next_cursor.is_none());
    assert_ne!(first_note_page.items[0].id, second_note_page.items[0].id);

    let page = service.list(&assignee, TodoQuery::default()).await?;
    assert!(page.items.iter().any(|todo| todo.id == todo_id));

    service
        .delete(&assigner, todo_id, "req-todo-delete")
        .await?;
    assert!(matches!(
        service
            .delete(&outsider, todo_id, "req-todo-delete-outsider")
            .await,
        Err(AppError::Forbidden)
    ));
    service
        .delete(&assigner, todo_id, "req-todo-delete-retry")
        .await?;
    assert!(matches!(
        service.get(&assigner, todo_id).await,
        Err(AppError::NotFound)
    ));
    assert!(matches!(
        service
            .list_notes(&assigner, todo_id, CollectionQuery::default())
            .await,
        Err(AppError::NotFound)
    ));

    let notification_count = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM commit.outbox_events WHERE organization_id = $1 AND todo_id = $2",
    )
    .bind(organization.id.into_uuid())
    .bind(todo_id.into_uuid())
    .fetch_one(&pool)
    .await?;
    assert_eq!(notification_count, 4);
    let routing_snapshots = sqlx::query_as::<_, (i16, String, i64, String, String, i64)>(
        r"
        SELECT payload_version,
               webhook_url,
               destination_version,
               subscription_level::text,
               subscription_scope::text,
               subscription_version
          FROM commit.outbox_events
         WHERE organization_id = $1
           AND todo_id = $2
         ORDER BY created_at, id
        ",
    )
    .bind(organization.id.into_uuid())
    .bind(todo_id.into_uuid())
    .fetch_all(&pool)
    .await?;
    assert_eq!(routing_snapshots.len(), 4);
    assert!(routing_snapshots.iter().all(|snapshot| {
        snapshot.0 == 2
            && snapshot.1
                == format!(
                    "https://hook.example.com/silicon/{}/A1B2C3",
                    assigner.actor.id
                )
            && snapshot.2 == 1
            && snapshot.3 == "list"
            && snapshot.4 == "any_update"
            && snapshot.5 == 1
    }));
    let expected_audit_seconds = i64::try_from(AUDIT_RETENTION.as_secs())?;
    let retention_windows_match = sqlx::query_as::<_, (bool, bool)>(
        r"
        SELECT
            (
                SELECT COALESCE(
                    bool_and(
                        round(extract(epoch FROM retain_until - occurred_at))::bigint = $2
                    ),
                    false
                )
                FROM commit.audit_events
                WHERE organization_id = $1
                  AND action LIKE 'todo.%'
            ),
            (
                SELECT COALESCE(
                    bool_and(
                        round(extract(epoch FROM retain_until - created_at))::bigint = $2
                    ),
                    false
                )
                FROM commit.todo_activity
                WHERE organization_id = $1
                  AND todo_id = $3
            )
        ",
    )
    .bind(organization.id.into_uuid())
    .bind(expected_audit_seconds)
    .bind(todo_id.into_uuid())
    .fetch_one(&pool)
    .await?;
    assert_eq!(retention_windows_match, (true, true));

    let linked_todo_replays = sqlx::query_as::<_, (i64, i64, i64)>(
        r"
        SELECT count(*),
               count(todo_id),
               count(DISTINCT operation)
        FROM commit.idempotency_records
        WHERE organization_id = $1
          AND (resource_path = '/todos' OR resource_path LIKE '/todos/%')
          AND todo_id = $2
        ",
    )
    .bind(organization.id.into_uuid())
    .bind(todo_id.into_uuid())
    .fetch_one(&pool)
    .await?;
    assert_eq!(linked_todo_replays, (4, 4, 3));

    let final_projection_versions = sqlx::query_as::<_, (i64, i64, i64)>(
        r"
        SELECT organization.xmin::text::bigint,
               assigner.xmin::text::bigint,
               assignee.xmin::text::bigint
        FROM commit.organization_projection AS organization
        JOIN commit.actor_projection AS assigner
          ON assigner.organization_id = organization.organization_id
         AND assigner.principal_id = $2
        JOIN commit.actor_projection AS assignee
          ON assignee.organization_id = organization.organization_id
         AND assignee.principal_id = $3
        WHERE organization.organization_id = $1
        ",
    )
    .bind(organization.id.into_uuid())
    .bind(assigner.actor.principal_id.into_uuid())
    .bind(assignee.actor.principal_id.into_uuid())
    .fetch_one(&pool)
    .await?;
    assert_eq!(final_projection_versions, initial_projection_versions);

    Ok(())
}

#[tokio::test]
async fn notification_settings_drive_effective_routing_and_immutable_snapshots()
-> anyhow::Result<()> {
    let Some(pool) = test_pool().await? else {
        return Ok(());
    };
    let organization = TestOrganization::unique("notification-org")?;
    let delegating_silicon = organization.actor(
        "notification-owner",
        ActorType::Silicon,
        OrganizationRole::Member,
    )?;
    let other_silicon = organization.actor(
        "notification-outsider",
        ActorType::Silicon,
        OrganizationRole::Member,
    )?;
    let assignee = organization.actor(
        "notification-assignee",
        ActorType::Carbon,
        OrganizationRole::Member,
    )?;
    let settings_service = NotificationSettingsService::new(pool.clone(), AUDIT_RETENTION);

    let empty_settings = settings_service.get_settings(&delegating_silicon).await?;
    assert_eq!(empty_settings.version.get(), 0);
    assert!(empty_settings.webhook_url.is_none());
    assert!(empty_settings.todo_list_subscription.is_none());
    assert!(empty_settings.updated_at.is_none());
    assert!(matches!(
        settings_service.get_settings(&assignee).await,
        Err(AppError::Forbidden)
    ));

    let webhook_v1 = format!(
        "https://hook.example.com/silicon/{}/A1B2C3",
        delegating_silicon.actor.id
    );
    let initial_settings = NotificationSettingsUpdate {
        webhook_url: Some(webhook_v1.clone()),
        todo_list_subscription: Some(NotificationRuleInput {
            scope: NotificationScope::StatusUpdates,
            statuses: Vec::new(),
        }),
    };
    let created_settings = settings_service
        .replace_settings(
            &delegating_silicon,
            initial_settings.clone(),
            ExpectedNotificationVersion::new(0)?,
            "req-notification-create",
        )
        .await?;
    assert_eq!(created_settings.version.get(), 1);
    assert!(created_settings.updated_at.is_some());
    assert_eq!(
        settings_service
            .get_settings(&delegating_silicon)
            .await?
            .version,
        created_settings.version
    );

    let stale_identical = settings_service
        .replace_settings(
            &delegating_silicon,
            initial_settings,
            ExpectedNotificationVersion::new(0)?,
            "req-notification-stale-identical",
        )
        .await?;
    assert_eq!(stale_identical, created_settings);

    let stale_different = settings_service
        .replace_settings(
            &delegating_silicon,
            NotificationSettingsUpdate {
                webhook_url: Some(webhook_v1.clone()),
                todo_list_subscription: Some(NotificationRuleInput {
                    scope: NotificationScope::AnyUpdate,
                    statuses: Vec::new(),
                }),
            },
            ExpectedNotificationVersion::new(0)?,
            "req-notification-stale-different",
        )
        .await;
    assert!(matches!(
        stale_different,
        Err(AppError::Conflict { ref code })
            if code.as_ref() == "notification_settings_version_conflict"
    ));
    assert!(matches!(
        settings_service
            .replace_settings(
                &assignee,
                NotificationSettingsUpdate::default(),
                ExpectedNotificationVersion::new(0)?,
                "req-notification-carbon-forbidden",
            )
            .await,
        Err(AppError::Forbidden)
    ));

    let todo_service = TodoService::new(
        pool.clone(),
        Arc::new(TestDirectory::new([active_member(&assignee)])),
        DomainLimits::default(),
        IDEMPOTENCY_TTL,
        AUDIT_RETENTION,
        Duration::from_hours(1_080),
    );
    let created_todo = todo_service
        .create(
            &delegating_silicon,
            TodoCreate {
                title: "Exercise notification routing".to_owned(),
                description: None,
                assigned_to: assignee.actor.id.clone(),
                status: TodoStatus::YetToDo,
                attachments: Vec::new(),
            },
            unique_key("notification-todo-create")?,
            "req-notification-todo-create",
        )
        .await?;
    let todo_id = TodoId::from_uuid(response_uuid(&created_todo, "id")?);
    assert_eq!(
        notification_outbox_count(&pool, organization.id, todo_id).await?,
        0
    );

    let empty_override = settings_service
        .get_todo_subscription(&delegating_silicon, todo_id)
        .await?;
    assert_eq!(empty_override.version.get(), 0);
    assert!(empty_override.subscription.is_none());
    assert!(empty_override.updated_at.is_none());
    assert!(matches!(
        settings_service
            .get_todo_subscription(&other_silicon, todo_id)
            .await,
        Err(AppError::Forbidden)
    ));
    assert!(matches!(
        settings_service
            .replace_todo_subscription(
                &other_silicon,
                todo_id,
                TodoNotificationSubscriptionUpdate {
                    subscription: Some(NotificationRuleInput {
                        scope: NotificationScope::AnyUpdate,
                        statuses: Vec::new(),
                    }),
                },
                ExpectedNotificationVersion::new(0)?,
                "req-notification-override-forbidden",
            )
            .await,
        Err(AppError::Forbidden)
    ));

    let ignored_note_key = unique_key("notification-note-ignored")?;
    let ignored_note_request = TodoNoteCreate {
        body: "A status-only list rule must ignore this note.".to_owned(),
    };
    todo_service
        .add_note(
            &assignee,
            todo_id,
            ignored_note_request.clone(),
            ignored_note_key.clone(),
            "req-notification-note-ignored",
        )
        .await?;
    let ignored_note_replay = todo_service
        .add_note(
            &assignee,
            todo_id,
            ignored_note_request,
            ignored_note_key,
            "req-notification-note-ignored-replay",
        )
        .await?;
    assert!(ignored_note_replay.replayed);
    assert_eq!(
        notification_outbox_count(&pool, organization.id, todo_id).await?,
        0
    );

    let first_status_patch = TodoPatch {
        status: Some(TodoStatus::InProgress),
        ..TodoPatch::default()
    };
    let first_status_key = unique_key("notification-status-first")?;
    todo_service
        .update(
            &assignee,
            todo_id,
            first_status_patch.clone(),
            first_status_key.clone(),
            "req-notification-status-first",
        )
        .await?;
    let first_status_replay = todo_service
        .update(
            &assignee,
            todo_id,
            first_status_patch,
            first_status_key,
            "req-notification-status-first-replay",
        )
        .await?;
    assert!(first_status_replay.replayed);
    assert_eq!(
        notification_outbox_count(&pool, organization.id, todo_id).await?,
        1
    );

    todo_service
        .update(
            &assignee,
            todo_id,
            TodoPatch {
                status: Some(TodoStatus::InProgress),
                ..TodoPatch::default()
            },
            unique_key("notification-status-noop")?,
            "req-notification-status-noop",
        )
        .await?;
    assert_eq!(
        notification_outbox_count(&pool, organization.id, todo_id).await?,
        1
    );

    let override_resource = settings_service
        .replace_todo_subscription(
            &delegating_silicon,
            todo_id,
            TodoNotificationSubscriptionUpdate {
                subscription: Some(NotificationRuleInput {
                    scope: NotificationScope::SpecificStatuses,
                    statuses: vec![TodoStatus::Blocked],
                }),
            },
            ExpectedNotificationVersion::new(0)?,
            "req-notification-override-create",
        )
        .await?;
    assert_eq!(override_resource.version.get(), 1);

    todo_service
        .update(
            &assignee,
            todo_id,
            TodoPatch {
                status: Some(TodoStatus::Completed),
                ..TodoPatch::default()
            },
            unique_key("notification-override-nonmatch")?,
            "req-notification-override-nonmatch",
        )
        .await?;
    assert_eq!(
        notification_outbox_count(&pool, organization.id, todo_id).await?,
        1,
        "an active nonmatching todo override must suppress list fallback"
    );

    todo_service
        .update(
            &assignee,
            todo_id,
            TodoPatch {
                status: Some(TodoStatus::Blocked),
                ..TodoPatch::default()
            },
            unique_key("notification-override-match")?,
            "req-notification-override-match",
        )
        .await?;
    assert_eq!(
        notification_outbox_count(&pool, organization.id, todo_id).await?,
        2
    );

    let fallback_resource = settings_service
        .replace_todo_subscription(
            &delegating_silicon,
            todo_id,
            TodoNotificationSubscriptionUpdate { subscription: None },
            ExpectedNotificationVersion::new(override_resource.version.get().try_into()?)?,
            "req-notification-override-fallback",
        )
        .await?;
    assert_eq!(fallback_resource.version.get(), 2);
    assert!(fallback_resource.subscription.is_none());

    todo_service
        .update(
            &assignee,
            todo_id,
            TodoPatch {
                status: Some(TodoStatus::InProgress),
                ..TodoPatch::default()
            },
            unique_key("notification-list-fallback")?,
            "req-notification-list-fallback",
        )
        .await?;
    let snapshots_before_replacement =
        notification_routing_snapshots(&pool, organization.id, todo_id).await?;
    assert_eq!(snapshots_before_replacement.len(), 3);
    assert_eq!(
        snapshots_before_replacement
            .iter()
            .map(|snapshot| (
                snapshot.payload_version,
                snapshot.destination_version,
                snapshot.subscription_level.as_str(),
                snapshot.subscription_scope.as_str(),
                snapshot.subscription_version,
            ))
            .collect::<Vec<_>>(),
        vec![
            (2, 1, "list", "status_updates", 1),
            (2, 1, "todo", "specific_statuses", 1),
            (2, 1, "list", "status_updates", 1),
        ]
    );
    assert!(
        snapshots_before_replacement
            .iter()
            .all(|snapshot| snapshot.webhook_url == webhook_v1)
    );

    let webhook_v2 = format!(
        "https://hook.example.com/silicon/{}/D4E5F6",
        delegating_silicon.actor.id
    );
    let replacement_settings = settings_service
        .replace_settings(
            &delegating_silicon,
            NotificationSettingsUpdate {
                webhook_url: Some(webhook_v2.clone()),
                todo_list_subscription: Some(NotificationRuleInput {
                    scope: NotificationScope::AnyUpdate,
                    statuses: Vec::new(),
                }),
            },
            ExpectedNotificationVersion::new(created_settings.version.get().try_into()?)?,
            "req-notification-settings-replace",
        )
        .await?;
    assert_eq!(replacement_settings.version.get(), 2);
    assert_eq!(
        notification_routing_snapshots(&pool, organization.id, todo_id).await?,
        snapshots_before_replacement,
        "later settings replacements must not rewrite durable routing decisions"
    );

    todo_service
        .add_note(
            &assignee,
            todo_id,
            TodoNoteCreate {
                body: "The replacement any-update rule should select this note.".to_owned(),
            },
            unique_key("notification-note-selected")?,
            "req-notification-note-selected",
        )
        .await?;
    let snapshots_after_replacement =
        notification_routing_snapshots(&pool, organization.id, todo_id).await?;
    assert_eq!(snapshots_after_replacement.len(), 4);
    assert_eq!(
        snapshots_after_replacement[..3],
        snapshots_before_replacement
    );
    let latest = snapshots_after_replacement
        .last()
        .context("replacement settings did not produce a new routed event")?;
    assert_eq!(latest.event_type, "todo.note_added");
    assert_eq!(latest.payload_version, 2);
    assert_eq!(latest.webhook_url, webhook_v2);
    assert_eq!(latest.destination_version, 2);
    assert_eq!(latest.subscription_level, "list");
    assert_eq!(latest.subscription_scope, "any_update");
    assert_eq!(latest.subscription_version, 2);

    let forged_routing_rewrite = sqlx::query(
        r"
        UPDATE commit.outbox_events
           SET webhook_url = 'https://hook.example.com/silicon/forged/ABCDEF'
         WHERE id = $1
        ",
    )
    .bind(latest.event_id)
    .execute(&pool)
    .await;
    assert_database_code(forged_routing_rewrite, "23514")?;

    Ok(())
}

#[tokio::test]
async fn project_lifecycle_enforces_authorization_diary_cas_and_atomic_completion()
-> anyhow::Result<()> {
    let Some(pool) = test_pool().await? else {
        return Ok(());
    };
    let organization = TestOrganization::unique("project-org")?;
    let creator = organization.actor(
        "project-silicon",
        ActorType::Silicon,
        OrganizationRole::Member,
    )?;
    let outsider = organization.actor(
        "outside-silicon",
        ActorType::Silicon,
        OrganizationRole::Member,
    )?;
    let other_organization = TestOrganization::unique("other-project-org")?;
    let other_tenant_actor =
        other_organization.actor("other-silicon", ActorType::Silicon, OrganizationRole::Owner)?;
    let service = ProjectService::new(
        pool.clone(),
        Arc::new(TestDirectory::default()),
        DomainLimits::default(),
        IDEMPOTENCY_TTL,
        AUDIT_RETENTION,
    );
    let request = ProjectCreate {
        name: "Ship the Commit backend".to_owned(),
        silicon_ids: vec![creator.actor.id.clone()],
    };
    let create_key = unique_key("project-create")?;

    let created = service
        .create_project(
            &creator,
            request.clone(),
            create_key.clone(),
            "req-project-create",
        )
        .await?;
    assert_eq!(created.status, 201);
    let project_id = ProjectId::from_uuid(response_uuid(&created, "id")?);
    let locator = ProjectLocator::Id(project_id);

    let replay = service
        .create_project(
            &creator,
            request.clone(),
            create_key.clone(),
            "req-project-replay",
        )
        .await?;
    assert!(replay.replayed);
    assert_eq!(replay.body, created.body);

    let conflict = service
        .create_project(
            &creator,
            ProjectCreate {
                name: "A different project".to_owned(),
                silicon_ids: request.silicon_ids,
            },
            create_key,
            "req-project-conflict",
        )
        .await;
    assert_conflict(conflict, "idempotency_key_reused")?;

    assert!(matches!(
        service.get_project(&other_tenant_actor, &locator).await,
        Err(AppError::NotFound)
    ));
    let forbidden = service
        .update_project(
            &outsider,
            &locator,
            ProjectPatch {
                name: Some("Unauthorized name".to_owned()),
                status: None,
                silicon_ids: None,
            },
            unique_key("project-forbidden")?,
            "req-project-forbidden",
        )
        .await;
    assert!(matches!(forbidden, Err(AppError::Forbidden)));

    let creator_removal = service
        .update_project(
            &creator,
            &locator,
            ProjectPatch {
                name: None,
                status: None,
                silicon_ids: Some(vec![outsider.actor.id.clone()]),
            },
            unique_key("project-remove-creator")?,
            "req-project-remove-creator",
        )
        .await;
    assert!(matches!(
        creator_removal,
        Err(AppError::Validation { ref details }) if details.get("silicon_ids").is_some()
    ));

    let mut creator_removal_transaction = pool.begin().await?;
    sqlx::query(
        r"
        UPDATE commit.project_participants
           SET removed_by_principal_id = $3,
               removed_at = transaction_timestamp()
         WHERE organization_id = $1
           AND project_id = $2
           AND silicon_principal_id = $3
           AND removed_at IS NULL
        ",
    )
    .bind(organization.id.into_uuid())
    .bind(project_id.into_uuid())
    .bind(creator.actor.principal_id.into_uuid())
    .execute(&mut *creator_removal_transaction)
    .await?;
    let creator_constraint = sqlx::query("SET CONSTRAINTS ALL IMMEDIATE")
        .execute(&mut *creator_removal_transaction)
        .await;
    assert_database_code(creator_constraint, "23514")?;
    creator_removal_transaction.rollback().await?;

    let updated = service
        .update_project(
            &creator,
            &locator,
            ProjectPatch {
                name: Some("Ship the production Commit backend".to_owned()),
                status: Some(ProjectStatus::InProgress),
                silicon_ids: None,
            },
            unique_key("project-update")?,
            "req-project-update",
        )
        .await?;
    assert_eq!(
        updated.body.get("status"),
        Some(&serde_json::json!("in_progress"))
    );

    let initial_diary = service.get_diary(&creator, &locator).await?;
    assert_eq!(initial_diary.version.get(), 1);
    let diary = service
        .replace_diary(
            &creator,
            &locator,
            DiaryUpdate {
                markdown: "# Delivery log\n\nThe first production milestone is live.".to_owned(),
            },
            ExpectedDiaryVersion::new(1)?,
            "req-diary-update",
        )
        .await?;
    assert_eq!(diary.version.get(), 2);
    assert!(diary.markdown.starts_with("# Delivery log"));
    let stale_diary = service
        .replace_diary(
            &creator,
            &locator,
            DiaryUpdate {
                markdown: "stale replacement".to_owned(),
            },
            ExpectedDiaryVersion::new(1)?,
            "req-diary-stale",
        )
        .await;
    assert!(matches!(
        stale_diary,
        Err(AppError::Conflict { ref code }) if code == "diary_version_mismatch"
    ));

    let task_request = ProjectTaskCreate {
        parent_task_id: None,
        title: "Deploy PostgreSQL migrations".to_owned(),
        description: "Apply all forward-only migrations.".to_owned(),
        status: TodoStatus::YetToDo,
    };
    let task_key = unique_key("project-task")?;
    let task = service
        .create_task(
            &creator,
            &locator,
            task_request.clone(),
            task_key.clone(),
            "req-task-create",
        )
        .await?;
    let task_id = ProjectTaskId::from_uuid(response_uuid(&task, "id")?);
    let task_replay = service
        .create_task(
            &creator,
            &locator,
            task_request,
            task_key,
            "req-task-replay",
        )
        .await?;
    assert!(task_replay.replayed);
    let completed_task = service
        .update_task(
            &creator,
            &locator,
            task_id,
            ProjectTaskPatch {
                title: None,
                description: None,
                status: Some(TodoStatus::Completed),
            },
            "req-task-update",
        )
        .await?;
    assert_eq!(completed_task.status, TodoStatus::Completed);
    let subtask = service
        .create_task(
            &creator,
            &locator,
            ProjectTaskCreate {
                parent_task_id: Some(task_id),
                title: "Verify the migrated schema".to_owned(),
                description: "Run the PostgreSQL integration gate.".to_owned(),
                status: TodoStatus::Completed,
            },
            unique_key("project-subtask")?,
            "req-subtask-create",
        )
        .await?;
    let subtask_id = ProjectTaskId::from_uuid(response_uuid(&subtask, "id")?);

    service
        .create_blocker(
            &creator,
            &locator,
            BlockerCreate {
                title: "Production credential".to_owned(),
                description: "Awaiting the deployment secret.".to_owned(),
                status: BlockerStatus::Open,
            },
            unique_key("project-blocker")?,
            "req-blocker-create",
        )
        .await?;
    service
        .create_update(
            &creator,
            &locator,
            ProjectUpdateCreate {
                title: "Credential supplied".to_owned(),
                description: "Deployment may continue.".to_owned(),
            },
            unique_key("project-entry-update")?,
            "req-entry-update",
        )
        .await?;
    let completion_request = ProjectCompletionCreate {
        title: "Backend shipped".to_owned(),
        description: "The production service is operational.".to_owned(),
    };
    let completion_key = unique_key("project-completion")?;
    let completion = service
        .complete_project(
            &creator,
            &locator,
            completion_request.clone(),
            completion_key.clone(),
            "req-project-complete",
        )
        .await?;
    assert_eq!(completion.status, 201);
    let completion_replay = service
        .complete_project(
            &creator,
            &locator,
            completion_request.clone(),
            completion_key,
            "req-project-complete-replay",
        )
        .await?;
    assert!(completion_replay.replayed);
    let duplicate_completion = service
        .complete_project(
            &creator,
            &locator,
            completion_request,
            unique_key("project-completion-duplicate")?,
            "req-project-complete-duplicate",
        )
        .await;
    assert!(matches!(
        duplicate_completion,
        Err(AppError::Conflict { ref code }) if code == "project_already_completed"
    ));

    let reopen = service
        .update_project(
            &creator,
            &locator,
            ProjectPatch {
                name: None,
                status: Some(ProjectStatus::InProgress),
                silicon_ids: None,
            },
            unique_key("project-reopen")?,
            "req-project-reopen",
        )
        .await;
    assert_conflict(reopen, "project_already_completed")?;

    let direct_reopen = sqlx::query(
        r"
        UPDATE commit.projects
           SET status = 'in_progress'
         WHERE organization_id = $1
           AND id = $2
        ",
    )
    .bind(organization.id.into_uuid())
    .bind(project_id.into_uuid())
    .execute(&pool)
    .await;
    assert_database_code(direct_reopen, "23514")?;

    let completed = service.get_project(&creator, &locator).await?;
    assert_eq!(completed.status, ProjectStatus::Completed);
    let projects = service
        .list_projects(&creator, ProjectQuery::default())
        .await?;
    assert!(
        projects
            .items
            .iter()
            .any(|project| project.id == project_id)
    );
    let tasks = service
        .list_tasks(&creator, &locator, CollectionQuery::default())
        .await?;
    assert_eq!(tasks.items.len(), 2);
    assert!(
        tasks
            .items
            .iter()
            .any(|task| { task.id == task_id && task.status == TodoStatus::Completed })
    );
    assert!(tasks.items.iter().any(|task| task.id == subtask_id));

    let page_limit = PageLimit::new(1)?;
    let first_task_page = service
        .list_tasks(
            &creator,
            &locator,
            CollectionQuery {
                cursor: None,
                limit: page_limit,
            },
        )
        .await?;
    assert_eq!(first_task_page.items.len(), 1);
    assert!(first_task_page.next_cursor.is_some());
    let second_task_page = service
        .list_tasks(
            &creator,
            &locator,
            CollectionQuery {
                cursor: first_task_page.next_cursor,
                limit: page_limit,
            },
        )
        .await?;
    assert_eq!(second_task_page.items.len(), 1);
    assert!(second_task_page.next_cursor.is_none());
    assert_ne!(first_task_page.items[0].id, second_task_page.items[0].id);

    let entry_count = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM commit.project_entries WHERE organization_id = $1 AND project_id = $2",
    )
    .bind(organization.id.into_uuid())
    .bind(project_id.into_uuid())
    .fetch_one(&pool)
    .await?;
    assert_eq!(entry_count, 3);
    let expected_audit_seconds = i64::try_from(AUDIT_RETENTION.as_secs())?;
    let audit_window_matches = sqlx::query_scalar::<_, bool>(
        r"
        SELECT COALESCE(
            bool_and(
                round(extract(epoch FROM retain_until - occurred_at))::bigint = $2
            ),
            false
        )
        FROM commit.audit_events
        WHERE organization_id = $1
          AND action LIKE 'project.%'
        ",
    )
    .bind(organization.id.into_uuid())
    .bind(expected_audit_seconds)
    .fetch_one(&pool)
    .await?;
    assert!(audit_window_matches);

    Ok(())
}

#[tokio::test]
async fn project_retries_replay_after_participation_is_revoked() -> anyhow::Result<()> {
    let Some(pool) = test_pool().await? else {
        return Ok(());
    };
    let organization = TestOrganization::unique("project-replay-org")?;
    let creator = organization.actor(
        "project-replay-creator",
        ActorType::Silicon,
        OrganizationRole::Member,
    )?;
    let participant = organization.actor(
        "project-replay-participant",
        ActorType::Silicon,
        OrganizationRole::Member,
    )?;
    let service = ProjectService::new(
        pool,
        Arc::new(TestDirectory::new([active_member(&participant)])),
        DomainLimits::default(),
        IDEMPOTENCY_TTL,
        AUDIT_RETENTION,
    );
    let created = service
        .create_project(
            &creator,
            ProjectCreate {
                name: "Replay authorization boundary".to_owned(),
                silicon_ids: vec![creator.actor.id.clone(), participant.actor.id.clone()],
            },
            unique_key("project-replay-create")?,
            "req-project-replay-create",
        )
        .await?;
    let project_id = ProjectId::from_uuid(response_uuid(&created, "id")?);
    let locator = ProjectLocator::Id(project_id);

    let patch_request = ProjectPatch {
        name: Some("Replay authorization boundary updated".to_owned()),
        status: Some(ProjectStatus::InProgress),
        silicon_ids: None,
    };
    let patch_key = unique_key("project-replay-patch")?;
    let patch_response = service
        .update_project(
            &participant,
            &locator,
            patch_request.clone(),
            patch_key.clone(),
            "req-project-replay-patch",
        )
        .await?;

    let task_request = ProjectTaskCreate {
        parent_task_id: None,
        title: "Persist retry response".to_owned(),
        description: "The response survives mutable authorization.".to_owned(),
        status: TodoStatus::YetToDo,
    };
    let task_key = unique_key("project-replay-task")?;
    let task_response = service
        .create_task(
            &participant,
            &locator,
            task_request.clone(),
            task_key.clone(),
            "req-project-replay-task",
        )
        .await?;

    let blocker_request = BlockerCreate {
        title: "Mutable participation".to_owned(),
        description: "Participation may be revoked after commit.".to_owned(),
        status: BlockerStatus::Open,
    };
    let blocker_key = unique_key("project-replay-blocker")?;
    let blocker_response = service
        .create_blocker(
            &participant,
            &locator,
            blocker_request.clone(),
            blocker_key.clone(),
            "req-project-replay-blocker",
        )
        .await?;

    let update_request = ProjectUpdateCreate {
        title: "Retry captured".to_owned(),
        description: "The original response is durable.".to_owned(),
    };
    let update_key = unique_key("project-replay-update")?;
    let update_response = service
        .create_update(
            &participant,
            &locator,
            update_request.clone(),
            update_key.clone(),
            "req-project-replay-update",
        )
        .await?;

    let completion_request = ProjectCompletionCreate {
        title: "Replay test completed".to_owned(),
        description: "Completion is immutable and replayable.".to_owned(),
    };
    let completion_key = unique_key("project-replay-completion")?;
    let completion_response = service
        .complete_project(
            &participant,
            &locator,
            completion_request.clone(),
            completion_key.clone(),
            "req-project-replay-completion",
        )
        .await?;

    service
        .update_project(
            &creator,
            &locator,
            ProjectPatch {
                name: None,
                status: None,
                silicon_ids: Some(vec![creator.actor.id.clone()]),
            },
            unique_key("project-revoke-participant")?,
            "req-project-revoke-participant",
        )
        .await?;

    let unauthorized_new_task = service
        .create_task(
            &participant,
            &locator,
            ProjectTaskCreate {
                title: "Must not commit".to_owned(),
                ..task_request.clone()
            },
            unique_key("project-new-task-after-revocation")?,
            "req-project-new-task-after-revocation",
        )
        .await;
    assert!(matches!(unauthorized_new_task, Err(AppError::Forbidden)));

    let patch_replay = service
        .update_project(
            &participant,
            &locator,
            patch_request,
            patch_key,
            "req-project-replay-patch-again",
        )
        .await?;
    let task_replay = service
        .create_task(
            &participant,
            &locator,
            task_request,
            task_key,
            "req-project-replay-task-again",
        )
        .await?;
    let blocker_replay = service
        .create_blocker(
            &participant,
            &locator,
            blocker_request,
            blocker_key,
            "req-project-replay-blocker-again",
        )
        .await?;
    let update_replay = service
        .create_update(
            &participant,
            &locator,
            update_request,
            update_key,
            "req-project-replay-update-again",
        )
        .await?;
    let completion_replay = service
        .complete_project(
            &participant,
            &locator,
            completion_request,
            completion_key,
            "req-project-replay-completion-again",
        )
        .await?;

    for (replay, original) in [
        (patch_replay, patch_response),
        (task_replay, task_response),
        (blocker_replay, blocker_response),
        (update_replay, update_response),
        (completion_replay, completion_response),
    ] {
        assert!(replay.replayed);
        assert_eq!(replay.status, original.status);
        assert_eq!(replay.body, original.body);
    }

    Ok(())
}

#[tokio::test]
async fn retention_redacts_only_expired_tombstone_content() -> anyhow::Result<()> {
    let Some(pool) = test_pool().await? else {
        return Ok(());
    };
    let organization = TestOrganization::unique("retention-org")?;
    let actor = organization.actor(
        "retention-carbon",
        ActorType::Carbon,
        OrganizationRole::Member,
    )?;
    let recipient = organization.actor(
        "retention-silicon",
        ActorType::Silicon,
        OrganizationRole::Member,
    )?;
    seed_identity(&pool, &actor).await?;
    seed_identity(&pool, &recipient).await?;

    let todo_id = TodoId::new();
    let note_id = Uuid::now_v7();
    let activity_id = Uuid::now_v7();
    let expired_activity_id = Uuid::now_v7();
    sqlx::query(
        r"
        INSERT INTO commit.todos (
            id,
            organization_id,
            title,
            description,
            assigned_by_principal_id,
            assigned_to_principal_id,
            status,
            created_at,
            updated_at,
            deleted_at,
            content_retain_until,
            deleted_by_principal_id
        ) VALUES (
            $1, $2, 'private retired title', 'private retired description', $3, $3,
            'completed'::commit.todo_status, '1900-01-01 UTC', '1901-01-01 UTC',
            '1901-01-01 UTC', '1902-01-01 UTC', $3
        )
        ",
    )
    .bind(todo_id.into_uuid())
    .bind(organization.id.into_uuid())
    .bind(actor.actor.principal_id.into_uuid())
    .execute(&pool)
    .await?;
    sqlx::query(
        r"
        INSERT INTO commit.todo_attachments (
            organization_id, todo_id, position, url, created_at
        ) VALUES (
            $1, $2, 0,
            'https://briefcase.example/api/v1/entries/018f268d-715a-7b72-8f0f-41f16f9af553',
            '1900-01-01 UTC'
        )
        ",
    )
    .bind(organization.id.into_uuid())
    .bind(todo_id.into_uuid())
    .execute(&pool)
    .await?;
    sqlx::query(
        r"
        INSERT INTO commit.todo_notes (
            id, organization_id, todo_id, author_principal_id, body, created_at
        ) VALUES ($1, $2, $3, $4, 'private retired note', '1900-01-01 UTC')
        ",
    )
    .bind(note_id)
    .bind(organization.id.into_uuid())
    .bind(todo_id.into_uuid())
    .bind(actor.actor.principal_id.into_uuid())
    .execute(&pool)
    .await?;
    sqlx::query(
        r#"
        INSERT INTO commit.todo_activity (
            id, organization_id, todo_id, activity_type, actor_principal_id,
            request_id, changes, created_at, retain_until
        ) VALUES (
            $1, $2, $3, 'deleted'::commit.todo_activity_type, $4,
            'req-retention-fixture', '{"private":"retired detail"}'::jsonb,
            '1901-01-01 UTC', '2300-01-01 UTC'
        ), (
            $5, $2, $3, 'deleted'::commit.todo_activity_type, $4,
            'req-expired-activity-fixture', '{"private":"expired detail"}'::jsonb,
            '1901-01-01 UTC', '1902-01-01 UTC'
        )
        "#,
    )
    .bind(activity_id)
    .bind(organization.id.into_uuid())
    .bind(todo_id.into_uuid())
    .bind(actor.actor.principal_id.into_uuid())
    .bind(expired_activity_id)
    .execute(&pool)
    .await?;

    let canonical_activity_redaction = sqlx::query(
        r"
        UPDATE commit.todo_activity
        SET changes = '{}'::jsonb
        WHERE organization_id = $1 AND id = $2
        ",
    )
    .bind(organization.id.into_uuid())
    .bind(expired_activity_id)
    .execute(&pool)
    .await?;
    assert_eq!(canonical_activity_redaction.rows_affected(), 1);
    let forged_activity_rewrite = sqlx::query(
        r#"
        UPDATE commit.todo_activity
        SET changes = '{"forged":true}'::jsonb
        WHERE organization_id = $1 AND id = $2
        "#,
    )
    .bind(organization.id.into_uuid())
    .bind(expired_activity_id)
    .execute(&pool)
    .await;
    assert_database_code(forged_activity_rewrite, "55000")?;

    let immutable_note_update = sqlx::query(
        r"
        UPDATE commit.todo_notes
        SET id = id
        WHERE organization_id = $1 AND id = $2
        ",
    )
    .bind(organization.id.into_uuid())
    .bind(note_id)
    .execute(&pool)
    .await;
    assert_database_code(immutable_note_update, "55000")?;

    let live_replay_id = Uuid::now_v7();
    sqlx::query(
        r#"
        INSERT INTO commit.idempotency_records (
            id,
            organization_id,
            todo_id,
            actor_principal_id,
            operation,
            resource_path,
            idempotency_key,
            request_fingerprint,
            response_status,
            response_body,
            created_at,
            expires_at
        ) VALUES (
            $1,
            $2,
            $3,
            $4,
            'updateTodo',
            '/todos/' || $3::text,
            'retention-replay-key',
            decode(repeat('00', 32), 'hex'),
            200,
            '{"title":"private retired title","description":"private retired description"}'::jsonb,
            '1900-01-01 UTC',
            '2300-01-01 UTC'
        )
        "#,
    )
    .bind(live_replay_id)
    .bind(organization.id.into_uuid())
    .bind(todo_id.into_uuid())
    .bind(actor.actor.principal_id.into_uuid())
    .execute(&pool)
    .await?;

    let delivered_event_ids = vec![Uuid::now_v7(), Uuid::now_v7()];
    sqlx::query(
        r#"
        INSERT INTO commit.outbox_events (
            id,
            organization_id,
            todo_id,
            recipient_silicon_principal_id,
            event_type,
            payload,
            status,
            attempt_count,
            available_at,
            created_at,
            updated_at,
            delivered_at,
            purge_after
        )
        SELECT event.id,
               $2,
               $3,
               $4,
               'todo.retention_test',
               '{}'::jsonb,
               'delivered'::commit.outbox_status,
               1,
               '1900-01-01 UTC'::timestamptz,
               '1900-01-01 UTC'::timestamptz,
               '1901-01-01 UTC'::timestamptz,
               '1901-01-01 UTC'::timestamptz,
               '2002-01-01 UTC'::timestamptz
        FROM unnest($1::uuid[]) AS event(id)
        "#,
    )
    .bind(delivered_event_ids)
    .bind(organization.id.into_uuid())
    .bind(todo_id.into_uuid())
    .bind(recipient.actor.principal_id.into_uuid())
    .execute(&pool)
    .await?;

    let dead_letter_event_ids = vec![Uuid::now_v7(), Uuid::now_v7()];
    sqlx::query(
        r#"
        INSERT INTO commit.outbox_events (
            id,
            organization_id,
            todo_id,
            recipient_silicon_principal_id,
            event_type,
            payload,
            status,
            attempt_count,
            available_at,
            last_error_code,
            created_at,
            updated_at,
            dead_lettered_at,
            purge_after
        )
        SELECT event.id,
               $2,
               $3,
               $4,
               'todo.retention_test',
               '{}'::jsonb,
               'dead_letter'::commit.outbox_status,
               1,
               '1900-01-01 UTC'::timestamptz,
               'provider_unavailable',
               '1900-01-01 UTC'::timestamptz,
               '1901-01-01 UTC'::timestamptz,
               '1901-01-01 UTC'::timestamptz,
               '2002-01-01 UTC'::timestamptz
        FROM unnest($1::uuid[]) AS event(id)
        "#,
    )
    .bind(dead_letter_event_ids)
    .bind(organization.id.into_uuid())
    .bind(todo_id.into_uuid())
    .bind(recipient.actor.principal_id.into_uuid())
    .execute(&pool)
    .await?;

    let batch_size = NonZeroUsize::new(1)
        .ok_or_else(|| anyhow::anyhow!("the fixed maintenance batch must be non-zero"))?;
    let policy = RetentionPolicy::new(batch_size);
    let report = retention::run_once(&pool, policy).await?;
    assert_eq!(report.todos_redacted, 0);
    assert_eq!(report.notes_purged, 0);
    assert_eq!(report.attachments_purged, 0);
    assert_eq!(report.activity_changes_redacted, 0);
    assert_eq!(report.activity_purged, 1);
    assert_eq!(report.delivered_outbox_purged, 1);
    assert_eq!(report.dead_letter_outbox_purged, 1);

    let tombstone = sqlx::query_as::<_, (String, Option<String>, i64, bool)>(
        r"
        SELECT title,
               description,
               version,
               updated_at = '1901-01-01 UTC'::timestamptz
        FROM commit.todos
        WHERE organization_id = $1 AND id = $2
        ",
    )
    .bind(organization.id.into_uuid())
    .bind(todo_id.into_uuid())
    .fetch_one(&pool)
    .await?;
    assert_eq!(
        tombstone,
        (
            "private retired title".to_owned(),
            Some("private retired description".to_owned()),
            1,
            true
        )
    );
    let notes = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM commit.todo_notes WHERE organization_id = $1 AND todo_id = $2",
    )
    .bind(organization.id.into_uuid())
    .bind(todo_id.into_uuid())
    .fetch_one(&pool)
    .await?;
    let attachments = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM commit.todo_attachments WHERE organization_id = $1 AND todo_id = $2",
    )
    .bind(organization.id.into_uuid())
    .bind(todo_id.into_uuid())
    .fetch_one(&pool)
    .await?;
    let activity_changes = sqlx::query_scalar::<_, serde_json::Value>(
        "SELECT changes FROM commit.todo_activity WHERE organization_id = $1 AND id = $2",
    )
    .bind(organization.id.into_uuid())
    .bind(activity_id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(notes, 1);
    assert_eq!(attachments, 1);
    assert_eq!(
        activity_changes,
        serde_json::json!({"private": "retired detail"})
    );

    let retained_outbox = sqlx::query_as::<_, (i64, i64)>(
        r"
        SELECT count(*) FILTER (WHERE status = 'delivered'),
               count(*) FILTER (WHERE status = 'dead_letter')
        FROM commit.outbox_events
        WHERE organization_id = $1
          AND todo_id = $2
        ",
    )
    .bind(organization.id.into_uuid())
    .bind(todo_id.into_uuid())
    .fetch_one(&pool)
    .await?;
    assert_eq!(retained_outbox, (1, 1));

    let removed_replay = sqlx::query(
        "DELETE FROM commit.idempotency_records WHERE organization_id = $1 AND id = $2",
    )
    .bind(organization.id.into_uuid())
    .bind(live_replay_id)
    .execute(&pool)
    .await?;
    assert_eq!(removed_replay.rows_affected(), 1);

    let second_cycle = retention::run_cycle(&pool, policy).await?;
    assert_eq!(second_cycle.passes, 2);
    assert!(!second_cycle.pass_budget_exhausted);
    assert_eq!(second_cycle.totals.activity_purged, 0);
    assert_eq!(second_cycle.totals.todos_redacted, 1);
    assert_eq!(second_cycle.totals.notes_purged, 1);
    assert_eq!(second_cycle.totals.attachments_purged, 1);
    assert_eq!(second_cycle.totals.activity_changes_redacted, 1);
    assert_eq!(second_cycle.totals.delivered_outbox_purged, 1);
    assert_eq!(second_cycle.totals.dead_letter_outbox_purged, 1);

    let redacted_content =
        sqlx::query_as::<_, (String, Option<String>, i64, i64, serde_json::Value)>(
            r"
        SELECT todo.title,
               todo.description,
               (SELECT count(*) FROM commit.todo_notes AS note
                 WHERE note.organization_id = todo.organization_id
                   AND note.todo_id = todo.id),
               (SELECT count(*) FROM commit.todo_attachments AS attachment
                 WHERE attachment.organization_id = todo.organization_id
                   AND attachment.todo_id = todo.id),
               (SELECT changes FROM commit.todo_activity AS activity
                 WHERE activity.organization_id = todo.organization_id
                   AND activity.id = $3)
        FROM commit.todos AS todo
        WHERE todo.organization_id = $1 AND todo.id = $2
        ",
        )
        .bind(organization.id.into_uuid())
        .bind(todo_id.into_uuid())
        .bind(activity_id)
        .fetch_one(&pool)
        .await?;
    assert_eq!(
        redacted_content,
        ("[deleted]".to_owned(), None, 0, 0, serde_json::json!({}))
    );
    let activity_count = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM commit.todo_activity WHERE organization_id = $1 AND todo_id = $2",
    )
    .bind(organization.id.into_uuid())
    .bind(todo_id.into_uuid())
    .fetch_one(&pool)
    .await?;
    assert_eq!(activity_count, 1);

    Ok(())
}

async fn test_pool() -> anyhow::Result<Option<PgPool>> {
    let Ok(database_url) = env::var("COMMIT_TEST_DATABASE_URL") else {
        eprintln!("skipping PostgreSQL integration test: COMMIT_TEST_DATABASE_URL is not set");
        return Ok(None);
    };
    let Some(max_connections) = NonZeroU32::new(8) else {
        bail!("the fixed integration-test pool size must be non-zero");
    };
    let settings = DatabaseSettings {
        url: SecretString::from(database_url),
        max_connections,
        min_connections: 0,
        acquire_timeout: Duration::from_secs(5),
        statement_timeout: Duration::from_secs(30),
    };
    let migration_pool = postgres::connect_migrator(&settings, "commit-integration-migrator")
        .await
        .context("connect migrator to COMMIT_TEST_DATABASE_URL")?;
    let migration_owner = sqlx::query_scalar::<_, String>("SELECT current_user::text")
        .fetch_one(&migration_pool)
        .await
        .context("read integration migration owner")?;
    postgres::migrate(&migration_pool, &migration_owner)
        .await
        .context("apply Commit migrations to the test database")?;
    postgres::migrate(&migration_pool, &migration_owner)
        .await
        .context("reapply Commit migrations using the stable public ledger")?;
    migration_pool.close().await;

    let pool = postgres::connect(&settings, "commit-integration-runtime")
        .await
        .context("connect to COMMIT_TEST_DATABASE_URL")?;
    Ok(Some(pool))
}

#[derive(Debug, Eq, PartialEq, sqlx::FromRow)]
struct PersistedRoutingSnapshot {
    event_id: Uuid,
    event_type: String,
    payload_version: i16,
    webhook_url: String,
    destination_version: i64,
    subscription_level: String,
    subscription_scope: String,
    subscription_version: i64,
}

async fn notification_outbox_count(
    pool: &PgPool,
    organization_id: OrganizationId,
    todo_id: TodoId,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        r"
        SELECT count(*)
          FROM commit.outbox_events
         WHERE organization_id = $1
           AND todo_id = $2
        ",
    )
    .bind(organization_id.into_uuid())
    .bind(todo_id.into_uuid())
    .fetch_one(pool)
    .await
}

async fn notification_routing_snapshots(
    pool: &PgPool,
    organization_id: OrganizationId,
    todo_id: TodoId,
) -> Result<Vec<PersistedRoutingSnapshot>, sqlx::Error> {
    sqlx::query_as(
        r"
        SELECT id AS event_id,
               event_type,
               payload_version,
               webhook_url,
               destination_version,
               subscription_level::text AS subscription_level,
               subscription_scope::text AS subscription_scope,
               subscription_version
          FROM commit.outbox_events
         WHERE organization_id = $1
           AND todo_id = $2
         ORDER BY created_at, id
        ",
    )
    .bind(organization_id.into_uuid())
    .bind(todo_id.into_uuid())
    .fetch_all(pool)
    .await
}

fn active_member(actor: &VerifiedActor) -> ActiveMember {
    ActiveMember {
        organization_id: actor.organization_id,
        org_id: actor.org_id.clone(),
        membership_id: actor.membership_id,
        actor: actor.actor.clone(),
    }
}

fn briefcase_policy() -> anyhow::Result<BriefcaseUrlPolicy> {
    let base = Url::parse("https://briefcase.example/api/v1")?;
    BriefcaseUrlPolicy::new([base]).map_err(Into::into)
}

fn unique_key(prefix: &str) -> anyhow::Result<IdempotencyKey> {
    IdempotencyKey::new(format!("{prefix}-{}", Uuid::new_v4().simple())).map_err(Into::into)
}

fn response_uuid(response: &MutationResponse, field: &str) -> anyhow::Result<Uuid> {
    response
        .body
        .get(field)
        .and_then(serde_json::Value::as_str)
        .with_context(|| format!("mutation response is missing string field {field}"))?
        .parse()
        .with_context(|| format!("mutation response field {field} is not a UUID"))
}

fn assert_conflict(
    result: Result<MutationResponse, AppError>,
    expected_code: &str,
) -> anyhow::Result<()> {
    match result {
        Err(AppError::Conflict { code }) if code == expected_code => Ok(()),
        other => bail!("expected conflict code {expected_code}, received {other:?}"),
    }
}

fn assert_database_code<T>(
    result: Result<T, sqlx::Error>,
    expected_code: &str,
) -> anyhow::Result<()> {
    match result {
        Err(error)
            if error
                .as_database_error()
                .and_then(sqlx::error::DatabaseError::code)
                .as_deref()
                == Some(expected_code) =>
        {
            Ok(())
        }
        Err(error) => bail!("expected database code {expected_code}, received {error:?}"),
        Ok(_) => bail!("expected database code {expected_code}, but the statement succeeded"),
    }
}

async fn seed_identity(pool: &PgPool, actor: &VerifiedActor) -> anyhow::Result<()> {
    let mut transaction = pool.begin().await?;
    sqlx::query(
        r"
        INSERT INTO commit.organization_projection (organization_id, org_id)
        VALUES ($1, $2)
        ON CONFLICT DO NOTHING
        ",
    )
    .bind(actor.organization_id.into_uuid())
    .bind(actor.org_id.as_str())
    .execute(&mut *transaction)
    .await?;
    let organization_matches = sqlx::query_scalar::<_, bool>(
        r"
        SELECT EXISTS (
            SELECT 1
            FROM commit.organization_projection
            WHERE organization_id = $1 AND org_id = $2
        )
        ",
    )
    .bind(actor.organization_id.into_uuid())
    .bind(actor.org_id.as_str())
    .fetch_one(&mut *transaction)
    .await?;
    if !organization_matches {
        bail!("test identity attempts to remap an existing organization");
    }
    sqlx::query(
        r"
        INSERT INTO commit.actor_projection (
            organization_id, principal_id, membership_id, actor_type, actor_id
        ) VALUES ($1, $2, $3, $4, $5)
        ON CONFLICT DO NOTHING
        ",
    )
    .bind(actor.organization_id.into_uuid())
    .bind(actor.actor.principal_id.into_uuid())
    .bind(actor.membership_id)
    .bind(actor.actor.actor_type)
    .bind(actor.actor.id.as_str())
    .execute(&mut *transaction)
    .await?;
    let actor_matches = sqlx::query_scalar::<_, bool>(
        r"
        SELECT EXISTS (
            SELECT 1
            FROM commit.actor_projection
            WHERE organization_id = $1
              AND principal_id = $2
              AND membership_id = $3
              AND actor_type = $4
              AND actor_id = $5
        )
        ",
    )
    .bind(actor.organization_id.into_uuid())
    .bind(actor.actor.principal_id.into_uuid())
    .bind(actor.membership_id)
    .bind(actor.actor.actor_type)
    .bind(actor.actor.id.as_str())
    .fetch_one(&mut *transaction)
    .await?;
    if !actor_matches {
        bail!("test identity attempts to remap an existing actor");
    }
    transaction.commit().await?;
    Ok(())
}
