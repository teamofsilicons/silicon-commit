//! PostgreSQL-backed integration coverage for Commit's transactional workflows.

use std::{
    collections::{HashMap, HashSet},
    env,
    num::{NonZeroU32, NonZeroUsize},
    sync::Arc,
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
        idempotency::{IdempotencyKey, MutationResponse},
        ports::{
            ActiveMember, AuthenticationRequest, CapabilitySet, ChildProofRequest,
            DelegatedOboProof, IdentityProvider, InboundCredential, OrganizationRole,
            ProviderError, TrustedIdentity, VerifiedActor,
        },
        projects::ProjectService,
        todos::TodoService,
    },
    config::DatabaseSettings,
    domain::{
        Actor, ActorId, ActorType, AttachmentUrlPolicy, BlockerCreate, BlockerStatus,
        CollectionQuery, DiaryUpdate, DomainLimits, ExpectedDiaryVersion, NullablePatch,
        OrganizationId, PageLimit, PrincipalId, ProjectCompletionCreate, ProjectCreate, ProjectId,
        ProjectLocator, ProjectPatch, ProjectQuery, ProjectStatus, ProjectTaskCreate,
        ProjectTaskId, ProjectTaskPatch, ProjectUpdateCreate, PublicOrganizationId, TodoCreate,
        TodoId, TodoNoteCreate, TodoPatch, TodoQuery, TodoStatus,
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

#[tokio::test]
async fn migrations_establish_the_complete_schema() -> anyhow::Result<()> {
    let Some(pool) = test_pool().await? else {
        return Ok(());
    };

    let successful_migrations =
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM public._sqlx_migrations WHERE success")
            .fetch_one(&pool)
            .await?;
    assert!(successful_migrations >= 12);

    for relation in [
        "commit.todos",
        "commit.projects",
        "commit.idempotency_records",
        "commit.outbox_events",
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
        attachment_policy()?,
        IDEMPOTENCY_TTL,
        AUDIT_RETENTION,
        Duration::from_hours(1_080),
    );
    let attachment = Url::parse(&format!(
        "https://briefcase.example/api/v1/entries/{}",
        Uuid::new_v4().hyphenated()
    ))?;
    let request = TodoCreate {
        title: "Prepare launch review".to_owned(),
        description: Some("Preserve this formatting.\n\n- first\n- second".to_owned()),
        assigned_to: assignee.actor.id.clone(),
        status: TodoStatus::YetToDo,
        attachments: vec![attachment],
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
            organization_id, todo_id, position, permanent_url, created_at
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

fn active_member(actor: &VerifiedActor) -> ActiveMember {
    ActiveMember {
        organization_id: actor.organization_id,
        org_id: actor.org_id.clone(),
        membership_id: actor.membership_id,
        actor: actor.actor.clone(),
    }
}

fn attachment_policy() -> anyhow::Result<AttachmentUrlPolicy> {
    let base = Url::parse("https://briefcase.example/api/v1")?;
    AttachmentUrlPolicy::new([base]).map_err(Into::into)
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
