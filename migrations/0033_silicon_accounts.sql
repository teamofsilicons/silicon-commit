-- Silicon Accounts identity (no organizations, no Teams).
--
-- Additive and data-preserving. Nothing is deleted and no history is rewritten:
--
-- * commit.accounts holds every Silicon Accounts account Commit knows, keyed by the account's permanent
--   uuid (text, case-sensitive; never cast to uuid, never lowercased).
-- * Every IAM-era principal that owns or touched data gets a placeholder account
--   'iam:<organization_id>:<principal_id>' with status 'unlinked'. Placeholders can never sign in (no
--   Silicon Accounts token carries such a subject), so their rows stay invisible until the operator links
--   them with `commit-migrate link-identities` (see commit_private.identity_links).
-- * Every identity column gains an account twin (*_account) filled from the placeholders. organization_id
--   and *_principal_id stay as nullable IAM-era provenance; rows created after this migration leave them
--   NULL and are keyed by account and by their globally unique row ids only.
-- * Keys, uniques and foreign keys that relied on organization_id get account/id-based twins; every
--   function that joined on organization_id is redefined without it.
--
-- Stop the API and worker and drain the outbox before running it in production (docs/migration/cutover.md).

SET CONSTRAINTS ALL IMMEDIATE;

DO $$ BEGIN
 IF EXISTS (
   SELECT 1 FROM pg_trigger t
   JOIN pg_class c ON c.oid = t.tgrelid
   JOIN pg_namespace n ON n.oid = c.relnamespace
   WHERE n.nspname = 'commit' AND NOT t.tgisinternal AND t.tgenabled <> 'O'
 ) THEN
   RAISE EXCEPTION 'the Silicon Accounts migration needs every commit.* trigger in its ordinary enabled mode; inspect pg_trigger.tgenabled and re-enable the triggers before migrating';
 END IF;
END $$;

-- ---------------------------------------------------------------------------------------------------
-- Accounts
-- ---------------------------------------------------------------------------------------------------

CREATE TABLE commit.accounts (
    uuid text PRIMARY KEY,
    kind commit.actor_type NOT NULL,
    public_id text NOT NULL,
    display_name text NOT NULL DEFAULT '',
    pfp_url text NOT NULL DEFAULT '',
    email text,
    status text NOT NULL DEFAULT 'active',
    custodian_uuid text,
    revoked_before timestamptz,
    accounts_version bigint NOT NULL DEFAULT 0,
    first_seen_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    refreshed_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT accounts_uuid_kind_key UNIQUE (uuid, kind),
    CONSTRAINT accounts_uuid_format CHECK (
        uuid = btrim(uuid)
        AND octet_length(uuid) BETWEEN 1 AND 160
        AND uuid !~ '[[:space:][:cntrl:]]'
    ),
    CONSTRAINT accounts_public_id_format CHECK (
        public_id = btrim(public_id) AND octet_length(public_id) <= 255
    ),
    CONSTRAINT accounts_status_known CHECK (
        status IN ('active', 'unclaimed', 'pending_custodian', 'deleted', 'unlinked')
    ),
    CONSTRAINT accounts_placeholders_are_unlinked CHECK ((uuid LIKE 'iam:%') = (status = 'unlinked')),
    CONSTRAINT accounts_custodian_only_for_silicons CHECK (kind = 'silicon' OR custodian_uuid IS NULL),
    CONSTRAINT accounts_email_shape CHECK (
        email IS NULL OR (octet_length(email) BETWEEN 3 AND 254 AND position('@' IN email) > 1)
    ),
    CONSTRAINT accounts_display_bounds CHECK (
        octet_length(display_name) <= 1024 AND octet_length(pfp_url) <= 2048
    )
);

COMMENT ON TABLE commit.accounts IS
    'Every Silicon Accounts account Commit has seen (token claims, lookups, webhooks), plus one unlinked placeholder per IAM-era principal. Rows are keyed by the permanent Accounts uuid; public_id is display data that changes.';
COMMENT ON COLUMN commit.accounts.uuid IS
    'Permanent Silicon Accounts uuid (short, case-sensitive) or iam:<organization_id>:<principal_id> for an IAM-era principal not linked yet.';
COMMENT ON COLUMN commit.accounts.public_id IS
    'Current c:/si: id as last seen (claims, lookups, account.id_changed); empty for deleted accounts.';
COMMENT ON COLUMN commit.accounts.email IS
    'The email the Carbon shared with Commit (optional sign-in field); NULL when none was shared.';
COMMENT ON COLUMN commit.accounts.custodian_uuid IS
    'A Silicon''s custodian (cached from Accounts; refreshed on silicon.custodian_changed, sign-in and lookups).';
COMMENT ON COLUMN commit.accounts.revoked_before IS
    'Access tokens issued before this instant are refused (sign-out, removed access, deleted account).';
COMMENT ON COLUMN commit.accounts.accounts_version IS
    'Accounts'' own account version; webhook updates with an older version are ignored.';

CREATE INDEX accounts_custodian_idx ON commit.accounts (custodian_uuid) WHERE custodian_uuid IS NOT NULL;
CREATE INDEX accounts_public_id_idx ON commit.accounts (public_id);

CREATE FUNCTION commit_private.touch_account() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    NEW.updated_at := greatest(OLD.updated_at, clock_timestamp());
    RETURN NEW;
END $$;

CREATE TRIGGER accounts_touch_updated_at BEFORE UPDATE ON commit.accounts
FOR EACH ROW EXECUTE FUNCTION commit_private.touch_account();

CREATE FUNCTION commit_private.preserve_account_identity() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'accounts are never deleted; account.deleted anonymises the row instead'
            USING ERRCODE = '23514';
    END IF;
    IF NEW.uuid IS DISTINCT FROM OLD.uuid
       OR NEW.kind IS DISTINCT FROM OLD.kind
       OR NEW.first_seen_at IS DISTINCT FROM OLD.first_seen_at THEN
        RAISE EXCEPTION 'an account''s uuid, kind and first sighting are immutable'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END $$;

CREATE TRIGGER accounts_preserve_identity BEFORE UPDATE OR DELETE ON commit.accounts
FOR EACH ROW EXECUTE FUNCTION commit_private.preserve_account_identity();

-- ---------------------------------------------------------------------------------------------------
-- IAM-era principals: placeholders and the operator's link table
-- ---------------------------------------------------------------------------------------------------

CREATE FUNCTION commit_private.placeholder_account(p_organization uuid, p_principal uuid) RETURNS text
LANGUAGE sql IMMUTABLE STRICT
SET search_path = pg_catalog, pg_temp
AS $$ SELECT 'iam:' || p_organization::text || ':' || p_principal::text $$;

INSERT INTO commit.accounts (uuid, kind, public_id, display_name, status, first_seen_at, refreshed_at, updated_at)
SELECT commit_private.placeholder_account(actor.organization_id, actor.principal_id),
       actor.actor_type,
       actor.actor_id,
       actor.actor_id,
       'unlinked',
       actor.first_seen_at,
       actor.first_seen_at,
       actor.first_seen_at
  FROM commit.actor_projection AS actor;

CREATE TABLE commit_private.identity_links (
    organization_id uuid NOT NULL,
    iam_principal_id uuid NOT NULL,
    actor_type commit.actor_type NOT NULL,
    iam_public_id text NOT NULL,
    org_id text NOT NULL,
    environment_id uuid,
    placeholder_uuid text NOT NULL UNIQUE REFERENCES commit.accounts (uuid),
    accounts_uuid text REFERENCES commit.accounts (uuid),
    linked_at timestamptz,
    source text NOT NULL,
    PRIMARY KEY (organization_id, iam_principal_id),
    CONSTRAINT identity_links_link_complete CHECK ((accounts_uuid IS NULL) = (linked_at IS NULL)),
    CONSTRAINT identity_links_link_is_real CHECK (accounts_uuid IS NULL OR accounts_uuid NOT LIKE 'iam:%')
);
REVOKE ALL ON commit_private.identity_links FROM PUBLIC;

COMMENT ON TABLE commit_private.identity_links IS
    'IAM principal -> Silicon Accounts uuid mapping. accounts_uuid NULL means the principal''s rows stay on its placeholder. Written only by `commit-migrate link-identities`; re-running with another mapping re-points the rows again (rows created after the cutover are never touched).';

INSERT INTO commit_private.identity_links (
    organization_id, iam_principal_id, actor_type, iam_public_id, org_id, environment_id, placeholder_uuid, source
)
SELECT actor.organization_id,
       actor.principal_id,
       actor.actor_type,
       actor.actor_id,
       organization.org_id,
       organization.environment_id,
       commit_private.placeholder_account(actor.organization_id, actor.principal_id),
       'migration 0033'
  FROM commit.actor_projection AS actor
  JOIN commit.organization_projection AS organization USING (organization_id);

-- The account that owns an IAM-era principal's rows right now: the linked account, else its placeholder.
CREATE FUNCTION commit_private.linked_account(p_organization uuid, p_principal uuid) RETURNS text
LANGUAGE sql STABLE
SET search_path = pg_catalog, pg_temp
AS $$
    SELECT CASE WHEN p_organization IS NULL OR p_principal IS NULL THEN NULL ELSE coalesce(
        (SELECT coalesce(link.accounts_uuid, link.placeholder_uuid)
           FROM commit_private.identity_links AS link
          WHERE link.organization_id = p_organization AND link.iam_principal_id = p_principal),
        commit_private.placeholder_account(p_organization, p_principal)
    ) END
$$;

-- ---------------------------------------------------------------------------------------------------
-- Todos and their children
-- ---------------------------------------------------------------------------------------------------

ALTER TABLE commit.todos
    ADD COLUMN assigned_by_account text,
    ADD COLUMN assigned_to_account text,
    ADD COLUMN deleted_by_account text;
ALTER TABLE commit.todos DISABLE TRIGGER USER;
UPDATE commit.todos SET
    assigned_by_account = commit_private.linked_account(organization_id, assigned_by_principal_id),
    assigned_to_account = commit_private.linked_account(organization_id, assigned_to_principal_id),
    deleted_by_account = commit_private.linked_account(organization_id, deleted_by_principal_id);
ALTER TABLE commit.todos ENABLE TRIGGER USER;
ALTER TABLE commit.todos
    ALTER COLUMN assigned_by_account SET NOT NULL,
    ALTER COLUMN assigned_to_account SET NOT NULL,
    ALTER COLUMN organization_id DROP NOT NULL,
    ALTER COLUMN assigned_by_principal_id DROP NOT NULL,
    ALTER COLUMN assigned_to_principal_id DROP NOT NULL,
    DROP CONSTRAINT todos_deletion_actor_consistency,
    ADD CONSTRAINT todos_deletion_actor_consistency CHECK (
        (deleted_at IS NULL AND content_retain_until IS NULL AND deleted_by_account IS NULL)
        OR (
            deleted_at IS NOT NULL
            AND content_retain_until IS NOT NULL
            AND content_retain_until > deleted_at
            AND deleted_by_account IS NOT NULL
        )
    ),
    ADD CONSTRAINT todos_legacy_identity_complete CHECK (
        (organization_id IS NULL) = (assigned_by_principal_id IS NULL)
        AND (organization_id IS NULL) = (assigned_to_principal_id IS NULL)
    ),
    ADD CONSTRAINT todos_assigned_by_account_fk FOREIGN KEY (assigned_by_account)
        REFERENCES commit.accounts (uuid) DEFERRABLE INITIALLY IMMEDIATE,
    ADD CONSTRAINT todos_assigned_to_account_fk FOREIGN KEY (assigned_to_account)
        REFERENCES commit.accounts (uuid) DEFERRABLE INITIALLY IMMEDIATE,
    ADD CONSTRAINT todos_deleted_by_account_fk FOREIGN KEY (deleted_by_account)
        REFERENCES commit.accounts (uuid) DEFERRABLE INITIALLY IMMEDIATE,
    ADD CONSTRAINT todos_assigner_account_key UNIQUE (id, assigned_by_account);
COMMENT ON COLUMN commit.todos.organization_id IS 'IAM-era provenance; NULL for todos created under Silicon Accounts.';
COMMENT ON COLUMN commit.todos.assigned_by_account IS 'Owner: the account that created the todo (or its link/placeholder).';
COMMENT ON COLUMN commit.todos.assigned_to_account IS 'Assignee account; assigning shares the todo with it.';

CREATE INDEX todos_assigned_to_account_created_idx
    ON commit.todos (assigned_to_account, created_at DESC, id DESC) WHERE deleted_at IS NULL;
CREATE INDEX todos_assigned_by_account_created_idx
    ON commit.todos (assigned_by_account, created_at DESC, id DESC) WHERE deleted_at IS NULL;
CREATE INDEX todos_project_id_idx ON commit.todos (project_id) WHERE project_id IS NOT NULL;
CREATE INDEX todos_retention_idx ON commit.todos (content_retain_until, id) WHERE deleted_at IS NOT NULL;

ALTER TABLE commit.todo_attachments DROP CONSTRAINT todo_attachments_pkey;
ALTER TABLE commit.todo_attachments
    ADD CONSTRAINT todo_attachments_pkey PRIMARY KEY (todo_id, position),
    ADD CONSTRAINT todo_attachments_todo_url_key UNIQUE (todo_id, url),
    ADD CONSTRAINT todo_attachments_todo_id_fk FOREIGN KEY (todo_id) REFERENCES commit.todos (id);
ALTER TABLE commit.todo_attachments ALTER COLUMN organization_id DROP NOT NULL;

ALTER TABLE commit.todo_notes ADD COLUMN author_account text;
ALTER TABLE commit.todo_notes DISABLE TRIGGER USER;
UPDATE commit.todo_notes SET author_account = commit_private.linked_account(organization_id, author_principal_id);
ALTER TABLE commit.todo_notes ENABLE TRIGGER USER;
ALTER TABLE commit.todo_notes
    ALTER COLUMN author_account SET NOT NULL,
    ALTER COLUMN organization_id DROP NOT NULL,
    ALTER COLUMN author_principal_id DROP NOT NULL,
    ADD CONSTRAINT todo_notes_author_account_fk FOREIGN KEY (author_account)
        REFERENCES commit.accounts (uuid) DEFERRABLE INITIALLY IMMEDIATE,
    ADD CONSTRAINT todo_notes_todo_id_fk FOREIGN KEY (todo_id) REFERENCES commit.todos (id);
CREATE INDEX todo_notes_todo_id_created_idx ON commit.todo_notes (todo_id, created_at, id);

ALTER TABLE commit.todo_activity ADD COLUMN actor_account text;
ALTER TABLE commit.todo_activity DISABLE TRIGGER USER;
UPDATE commit.todo_activity SET actor_account = commit_private.linked_account(organization_id, actor_principal_id);
ALTER TABLE commit.todo_activity ENABLE TRIGGER USER;
ALTER TABLE commit.todo_activity
    ALTER COLUMN actor_account SET NOT NULL,
    ALTER COLUMN organization_id DROP NOT NULL,
    ALTER COLUMN actor_principal_id DROP NOT NULL,
    ADD CONSTRAINT todo_activity_actor_account_fk FOREIGN KEY (actor_account)
        REFERENCES commit.accounts (uuid) DEFERRABLE INITIALLY IMMEDIATE,
    ADD CONSTRAINT todo_activity_todo_id_fk FOREIGN KEY (todo_id) REFERENCES commit.todos (id);
CREATE INDEX todo_activity_todo_id_created_idx ON commit.todo_activity (todo_id, created_at DESC, id DESC);
CREATE INDEX todo_activity_retain_idx ON commit.todo_activity (retain_until, id);

ALTER TABLE commit.idempotency_records ADD COLUMN actor_account text;
ALTER TABLE commit.idempotency_records DISABLE TRIGGER USER;
UPDATE commit.idempotency_records SET actor_account = commit_private.linked_account(organization_id, actor_principal_id);
ALTER TABLE commit.idempotency_records ENABLE TRIGGER USER;
ALTER TABLE commit.idempotency_records
    ALTER COLUMN actor_account SET NOT NULL,
    ALTER COLUMN organization_id DROP NOT NULL,
    ALTER COLUMN actor_principal_id DROP NOT NULL,
    ADD CONSTRAINT idempotency_records_actor_account_fk FOREIGN KEY (actor_account)
        REFERENCES commit.accounts (uuid) DEFERRABLE INITIALLY IMMEDIATE,
    ADD CONSTRAINT idempotency_records_todo_id_fk FOREIGN KEY (todo_id) REFERENCES commit.todos (id),
    ADD CONSTRAINT idempotency_records_account_scope_key
        UNIQUE (actor_account, operation, resource_path, idempotency_key);
CREATE INDEX idempotency_records_todo_id_expiry_idx
    ON commit.idempotency_records (todo_id, expires_at) WHERE todo_id IS NOT NULL;

ALTER TABLE commit.audit_events ADD COLUMN actor_account text;
ALTER TABLE commit.audit_events DISABLE TRIGGER USER;
UPDATE commit.audit_events SET actor_account = commit_private.linked_account(organization_id, actor_principal_id);
ALTER TABLE commit.audit_events ENABLE TRIGGER USER;
ALTER TABLE commit.audit_events
    ALTER COLUMN actor_account SET NOT NULL,
    ALTER COLUMN organization_id DROP NOT NULL,
    ALTER COLUMN actor_principal_id DROP NOT NULL,
    ADD CONSTRAINT audit_events_actor_account_fk FOREIGN KEY (actor_account)
        REFERENCES commit.accounts (uuid) DEFERRABLE INITIALLY IMMEDIATE;
CREATE INDEX audit_events_resource_id_idx
    ON commit.audit_events (resource_type, resource_id, occurred_at DESC, id DESC);
CREATE INDEX audit_events_actor_account_idx ON commit.audit_events (actor_account, occurred_at DESC, id DESC);

ALTER TABLE commit.outbox_events ADD COLUMN recipient_silicon_account text;
ALTER TABLE commit.outbox_events DISABLE TRIGGER USER;
UPDATE commit.outbox_events
   SET recipient_silicon_account = commit_private.linked_account(organization_id, recipient_silicon_principal_id);
ALTER TABLE commit.outbox_events ENABLE TRIGGER USER;
ALTER TABLE commit.outbox_events
    ALTER COLUMN recipient_silicon_account SET NOT NULL,
    ALTER COLUMN organization_id DROP NOT NULL,
    ALTER COLUMN recipient_silicon_principal_id DROP NOT NULL,
    ADD CONSTRAINT outbox_events_recipient_account_fk
        FOREIGN KEY (recipient_silicon_account, recipient_actor_type)
        REFERENCES commit.accounts (uuid, kind) DEFERRABLE INITIALLY IMMEDIATE,
    ADD CONSTRAINT outbox_events_todo_id_fk FOREIGN KEY (todo_id) REFERENCES commit.todos (id);
CREATE INDEX outbox_events_todo_id_idx ON commit.outbox_events (todo_id, created_at DESC, id DESC);
CREATE INDEX outbox_events_recipient_open_idx
    ON commit.outbox_events (recipient_silicon_account)
    WHERE status IN ('pending'::commit.outbox_status, 'in_flight'::commit.outbox_status);

-- ---------------------------------------------------------------------------------------------------
-- Projects and their children
-- ---------------------------------------------------------------------------------------------------

DO $$ BEGIN
 IF EXISTS (SELECT 1 FROM commit.projects GROUP BY uid HAVING count(*) > 1)
    OR EXISTS (SELECT 1 FROM commit.projects WHERE legacy_uid IS NOT NULL GROUP BY legacy_uid HAVING count(*) > 1)
    OR EXISTS (SELECT 1 FROM commit.projects p JOIN commit.projects q ON p.id <> q.id AND p.uid = q.legacy_uid) THEN
   RAISE EXCEPTION 'project UIDs must be unique across the former organizations before the Silicon Accounts cutover; list them with: SELECT uid, array_agg(id) FROM commit.projects GROUP BY uid HAVING count(*) > 1, and rename the dormant copy''s uid (it is only a locator) before migrating';
 END IF;
END $$;

ALTER TABLE commit.projects
    ADD COLUMN created_by_account text,
    ADD COLUMN owner_account text,
    ADD COLUMN deleted_at timestamptz;
ALTER TABLE commit.projects DISABLE TRIGGER USER;
UPDATE commit.projects SET
    created_by_account = commit_private.linked_account(organization_id, created_by_principal_id),
    owner_account = commit_private.linked_account(organization_id, created_by_principal_id);
ALTER TABLE commit.projects ENABLE TRIGGER USER;
ALTER TABLE commit.projects
    ALTER COLUMN created_by_account SET NOT NULL,
    ALTER COLUMN owner_account SET NOT NULL,
    ALTER COLUMN organization_id DROP NOT NULL,
    ALTER COLUMN created_by_principal_id DROP NOT NULL,
    ADD CONSTRAINT projects_legacy_identity_complete CHECK (
        (organization_id IS NULL) = (created_by_principal_id IS NULL)
    ),
    ADD CONSTRAINT projects_created_by_account_fk FOREIGN KEY (created_by_account)
        REFERENCES commit.accounts (uuid) DEFERRABLE INITIALLY IMMEDIATE,
    ADD CONSTRAINT projects_owner_account_fk FOREIGN KEY (owner_account)
        REFERENCES commit.accounts (uuid) DEFERRABLE INITIALLY IMMEDIATE,
    ADD CONSTRAINT projects_deleted_after_creation CHECK (deleted_at IS NULL OR deleted_at >= created_at);
COMMENT ON COLUMN commit.projects.created_by_account IS 'Immutable creator (history; the uid embeds its id at creation).';
COMMENT ON COLUMN commit.projects.owner_account IS 'Current owner: the creator until its account is deleted, then the longest-standing member.';
COMMENT ON COLUMN commit.projects.deleted_at IS 'Set when the owner''s account was deleted and no other member remained; deleted projects are invisible.';
COMMENT ON COLUMN commit.projects.private IS 'false: visible to the owner''s custodian circle and to members; true: members (and custodians of member Silicons) only.';
COMMENT ON COLUMN commit.projects.tags IS 'IAM-era tags, kept as history only; Silicon Accounts has no tags and nothing reads them.';

CREATE UNIQUE INDEX projects_uid_key ON commit.projects (uid);
CREATE UNIQUE INDEX projects_legacy_uid_key ON commit.projects (legacy_uid) WHERE legacy_uid IS NOT NULL;
CREATE INDEX projects_created_idx ON commit.projects (created_at DESC, id DESC) WHERE deleted_at IS NULL;
CREATE INDEX projects_owner_account_idx ON commit.projects (owner_account) WHERE deleted_at IS NULL;
CREATE INDEX projects_creator_slug_idx ON commit.projects (created_by_account, slug, created_at DESC);

ALTER TABLE commit.project_participants
    ADD COLUMN participant_account text,
    ADD COLUMN added_by_account text,
    ADD COLUMN removed_by_account text;
ALTER TABLE commit.project_participants DISABLE TRIGGER USER;
UPDATE commit.project_participants SET
    participant_account = commit_private.linked_account(organization_id, silicon_principal_id),
    added_by_account = commit_private.linked_account(organization_id, added_by_principal_id),
    removed_by_account = commit_private.linked_account(organization_id, removed_by_principal_id);
ALTER TABLE commit.project_participants ENABLE TRIGGER USER;
ALTER TABLE commit.project_participants
    ALTER COLUMN participant_account SET NOT NULL,
    ALTER COLUMN added_by_account SET NOT NULL,
    ALTER COLUMN organization_id DROP NOT NULL,
    ALTER COLUMN silicon_principal_id DROP NOT NULL,
    ALTER COLUMN added_by_principal_id DROP NOT NULL,
    DROP CONSTRAINT project_participants_removal_consistency,
    ADD CONSTRAINT project_participants_removal_consistency CHECK (
        (removed_at IS NULL AND removed_by_account IS NULL)
        OR (removed_at IS NOT NULL AND removed_by_account IS NOT NULL)
    ),
    ADD CONSTRAINT project_participants_participant_account_fk FOREIGN KEY (participant_account)
        REFERENCES commit.accounts (uuid) DEFERRABLE INITIALLY IMMEDIATE,
    ADD CONSTRAINT project_participants_added_by_account_fk FOREIGN KEY (added_by_account)
        REFERENCES commit.accounts (uuid) DEFERRABLE INITIALLY IMMEDIATE,
    ADD CONSTRAINT project_participants_removed_by_account_fk FOREIGN KEY (removed_by_account)
        REFERENCES commit.accounts (uuid) DEFERRABLE INITIALLY IMMEDIATE,
    ADD CONSTRAINT project_participants_project_id_fk FOREIGN KEY (project_id) REFERENCES commit.projects (id);
COMMENT ON COLUMN commit.project_participants.participant_account IS
    'Member (explicit share) of the project: a Carbon or a Silicon. The legacy column name silicon_principal_id held both kinds too.';
CREATE UNIQUE INDEX project_participants_one_active_account_idx
    ON commit.project_participants (project_id, participant_account) WHERE removed_at IS NULL;
CREATE INDEX project_participants_project_added_idx ON commit.project_participants (project_id, added_at, id);
CREATE INDEX project_participants_account_projects_idx
    ON commit.project_participants (participant_account, project_id) WHERE removed_at IS NULL;

ALTER TABLE commit.project_diaries ADD COLUMN updated_by_account text;
ALTER TABLE commit.project_diaries DISABLE TRIGGER USER;
UPDATE commit.project_diaries SET updated_by_account = commit_private.linked_account(organization_id, updated_by_principal_id);
ALTER TABLE commit.project_diaries ENABLE TRIGGER USER;
ALTER TABLE commit.project_diaries DROP CONSTRAINT project_diaries_pkey;
ALTER TABLE commit.project_diaries
    ADD CONSTRAINT project_diaries_pkey PRIMARY KEY (project_id),
    ALTER COLUMN updated_by_account SET NOT NULL,
    ADD CONSTRAINT project_diaries_updated_by_account_fk FOREIGN KEY (updated_by_account)
        REFERENCES commit.accounts (uuid) DEFERRABLE INITIALLY IMMEDIATE,
    ADD CONSTRAINT project_diaries_project_id_fk FOREIGN KEY (project_id) REFERENCES commit.projects (id);
ALTER TABLE commit.project_diaries
    ALTER COLUMN organization_id DROP NOT NULL,
    ALTER COLUMN updated_by_principal_id DROP NOT NULL;

ALTER TABLE commit.project_tasks
    ADD COLUMN created_by_account text,
    ADD COLUMN assigned_to_account text;
ALTER TABLE commit.project_tasks DISABLE TRIGGER USER;
UPDATE commit.project_tasks SET
    created_by_account = commit_private.linked_account(organization_id, created_by_principal_id),
    assigned_to_account = commit_private.linked_account(organization_id, assigned_to_principal_id);
ALTER TABLE commit.project_tasks ENABLE TRIGGER USER;
ALTER TABLE commit.project_tasks
    ALTER COLUMN created_by_account SET NOT NULL,
    ALTER COLUMN organization_id DROP NOT NULL,
    ALTER COLUMN created_by_principal_id DROP NOT NULL,
    ADD CONSTRAINT project_tasks_created_by_account_fk FOREIGN KEY (created_by_account)
        REFERENCES commit.accounts (uuid) DEFERRABLE INITIALLY IMMEDIATE,
    ADD CONSTRAINT project_tasks_assigned_to_account_fk FOREIGN KEY (assigned_to_account)
        REFERENCES commit.accounts (uuid) DEFERRABLE INITIALLY IMMEDIATE,
    ADD CONSTRAINT project_tasks_project_id_fk FOREIGN KEY (project_id) REFERENCES commit.projects (id),
    ADD CONSTRAINT project_tasks_project_task_key UNIQUE (project_id, id),
    ADD CONSTRAINT project_tasks_todo_id_key UNIQUE (todo_id),
    ADD CONSTRAINT project_tasks_todo_id_fk FOREIGN KEY (todo_id) REFERENCES commit.todos (id);
ALTER TABLE commit.project_tasks
    ADD CONSTRAINT project_tasks_parent_id_fk FOREIGN KEY (project_id, parent_task_id)
        REFERENCES commit.project_tasks (project_id, id);
CREATE INDEX project_tasks_project_id_created_idx ON commit.project_tasks (project_id, created_at, id);
CREATE INDEX project_tasks_parent_id_created_idx
    ON commit.project_tasks (project_id, parent_task_id, created_at, id);
CREATE INDEX project_tasks_assigned_to_account_idx
    ON commit.project_tasks (assigned_to_account) WHERE assigned_to_account IS NOT NULL AND deleted_at IS NULL;

ALTER TABLE commit.project_entries ADD COLUMN created_by_account text;
ALTER TABLE commit.project_entries DISABLE TRIGGER USER;
UPDATE commit.project_entries SET created_by_account = commit_private.linked_account(organization_id, created_by_principal_id);
ALTER TABLE commit.project_entries ENABLE TRIGGER USER;
ALTER TABLE commit.project_entries
    ALTER COLUMN created_by_account SET NOT NULL,
    ALTER COLUMN organization_id DROP NOT NULL,
    ALTER COLUMN created_by_principal_id DROP NOT NULL,
    ADD CONSTRAINT project_entries_created_by_account_fk FOREIGN KEY (created_by_account)
        REFERENCES commit.accounts (uuid) DEFERRABLE INITIALLY IMMEDIATE,
    ADD CONSTRAINT project_entries_project_id_fk FOREIGN KEY (project_id) REFERENCES commit.projects (id);
CREATE UNIQUE INDEX project_entries_one_completion_per_project_idx
    ON commit.project_entries (project_id) WHERE entry_type = 'completion'::commit.project_entry_type;
CREATE INDEX project_entries_project_id_created_idx
    ON commit.project_entries (project_id, created_at DESC, id DESC);

ALTER TABLE commit.project_collaborators ADD COLUMN account text;
UPDATE commit.project_collaborators SET account = commit_private.linked_account(organization_id, principal_id);
ALTER TABLE commit.project_collaborators DROP CONSTRAINT project_collaborators_pkey;
ALTER TABLE commit.project_collaborators
    ALTER COLUMN account SET NOT NULL,
    ADD CONSTRAINT project_collaborators_pkey PRIMARY KEY (project_id, account),
    ADD CONSTRAINT project_collaborators_account_fk FOREIGN KEY (account)
        REFERENCES commit.accounts (uuid) DEFERRABLE INITIALLY IMMEDIATE,
    ADD CONSTRAINT project_collaborators_project_id_fk FOREIGN KEY (project_id)
        REFERENCES commit.projects (id) ON DELETE CASCADE;
ALTER TABLE commit.project_collaborators
    ALTER COLUMN organization_id DROP NOT NULL,
    ALTER COLUMN principal_id DROP NOT NULL;

ALTER TABLE commit.project_versions DROP CONSTRAINT project_versions_pkey;
ALTER TABLE commit.project_versions
    ADD CONSTRAINT project_versions_pkey PRIMARY KEY (project_id, version),
    ADD CONSTRAINT project_versions_project_id_fk FOREIGN KEY (project_id)
        REFERENCES commit.projects (id) ON DELETE CASCADE;
ALTER TABLE commit.project_versions ALTER COLUMN organization_id DROP NOT NULL;

-- ---------------------------------------------------------------------------------------------------
-- Notification settings, subscriptions and email
-- ---------------------------------------------------------------------------------------------------

ALTER TABLE commit.silicon_notification_settings ADD COLUMN silicon_account text;
ALTER TABLE commit.silicon_notification_settings DISABLE TRIGGER USER;
UPDATE commit.silicon_notification_settings
   SET silicon_account = commit_private.linked_account(organization_id, silicon_principal_id);
ALTER TABLE commit.silicon_notification_settings ENABLE TRIGGER USER;
ALTER TABLE commit.silicon_notification_settings DROP CONSTRAINT silicon_notification_settings_pkey;
ALTER TABLE commit.silicon_notification_settings
    ALTER COLUMN silicon_account SET NOT NULL,
    ADD CONSTRAINT silicon_notification_settings_pkey PRIMARY KEY (silicon_account),
    ADD CONSTRAINT silicon_notification_settings_account_fk
        FOREIGN KEY (silicon_account, silicon_actor_type)
        REFERENCES commit.accounts (uuid, kind) DEFERRABLE INITIALLY IMMEDIATE;
ALTER TABLE commit.silicon_notification_settings
    ALTER COLUMN organization_id DROP NOT NULL,
    ALTER COLUMN silicon_principal_id DROP NOT NULL;

ALTER TABLE commit.todo_notification_subscriptions ADD COLUMN silicon_account text;
ALTER TABLE commit.todo_notification_subscriptions DISABLE TRIGGER USER;
UPDATE commit.todo_notification_subscriptions
   SET silicon_account = commit_private.linked_account(organization_id, silicon_principal_id);
ALTER TABLE commit.todo_notification_subscriptions ENABLE TRIGGER USER;
ALTER TABLE commit.todo_notification_subscriptions DROP CONSTRAINT todo_notification_subscriptions_pkey;
ALTER TABLE commit.todo_notification_subscriptions
    ALTER COLUMN silicon_account SET NOT NULL,
    ADD CONSTRAINT todo_notification_subscriptions_pkey PRIMARY KEY (silicon_account, todo_id),
    ADD CONSTRAINT todo_notification_subscriptions_account_fk
        FOREIGN KEY (silicon_account, silicon_actor_type)
        REFERENCES commit.accounts (uuid, kind) DEFERRABLE INITIALLY IMMEDIATE,
    ADD CONSTRAINT todo_notification_subscriptions_assigner_account_fk
        FOREIGN KEY (todo_id, silicon_account)
        REFERENCES commit.todos (id, assigned_by_account) DEFERRABLE INITIALLY IMMEDIATE;
ALTER TABLE commit.todo_notification_subscriptions
    ALTER COLUMN organization_id DROP NOT NULL,
    ALTER COLUMN silicon_principal_id DROP NOT NULL;

ALTER TABLE commit.email_preferences ADD COLUMN account text;
UPDATE commit.email_preferences SET account = commit_private.linked_account(organization_id, principal_id);
ALTER TABLE commit.email_preferences DROP CONSTRAINT email_preferences_pkey;
ALTER TABLE commit.email_preferences
    ALTER COLUMN account SET NOT NULL,
    ADD CONSTRAINT email_preferences_pkey PRIMARY KEY (account),
    ADD CONSTRAINT email_preferences_account_fk FOREIGN KEY (account)
        REFERENCES commit.accounts (uuid) DEFERRABLE INITIALLY IMMEDIATE,
    ADD CONSTRAINT email_preferences_legacy_identity_key UNIQUE (organization_id, principal_id);
ALTER TABLE commit.email_preferences
    ALTER COLUMN organization_id DROP NOT NULL,
    ALTER COLUMN principal_id DROP NOT NULL;
COMMENT ON TABLE commit.email_preferences IS
    'One email preference per account. Rows from former organizations stay on their placeholder until linked; link-identities keeps the newest row per account and leaves older ones on their placeholders.';

ALTER TABLE commit.email_jobs ADD COLUMN account text;
UPDATE commit.email_jobs SET account = commit_private.linked_account(organization_id, principal_id);
ALTER TABLE commit.email_jobs
    ALTER COLUMN organization_id DROP NOT NULL,
    ADD CONSTRAINT email_jobs_account_fk FOREIGN KEY (account)
        REFERENCES commit.accounts (uuid) DEFERRABLE INITIALLY IMMEDIATE,
    ADD CONSTRAINT email_jobs_account_report_key UNIQUE (account, report_key);
CREATE INDEX email_jobs_account_reports_idx ON commit.email_jobs (account, created_at DESC) WHERE kind = 'bug_report';

-- ---------------------------------------------------------------------------------------------------
-- New: Accounts webhook inbox and the Silicon allow-list
-- ---------------------------------------------------------------------------------------------------

CREATE TABLE commit.accounts_webhook_events (
    event_id text PRIMARY KEY,
    event_type text NOT NULL,
    account_uuid text,
    occurred_at timestamptz,
    received_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    payload_sha256 text NOT NULL,
    outcome text NOT NULL,
    CONSTRAINT accounts_webhook_events_id_format CHECK (octet_length(event_id) BETWEEN 1 AND 128),
    CONSTRAINT accounts_webhook_events_type_format CHECK (octet_length(event_type) BETWEEN 1 AND 128),
    CONSTRAINT accounts_webhook_events_outcome_format CHECK (octet_length(outcome) BETWEEN 1 AND 2000)
);
COMMENT ON TABLE commit.accounts_webhook_events IS
    'Silicon Accounts app-webhook deliveries already applied (dedupe on event_id; retries and replays reuse it). Only a body hash is kept, never the payload.';
CREATE INDEX accounts_webhook_events_account_idx ON commit.accounts_webhook_events (account_uuid, occurred_at DESC);

CREATE TABLE commit.silicon_allowed_accounts (
    silicon_account text NOT NULL,
    silicon_kind commit.actor_type GENERATED ALWAYS AS ('silicon'::commit.actor_type) STORED,
    allowed_account text NOT NULL,
    added_by_account text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (silicon_account, allowed_account),
    CONSTRAINT silicon_allowed_accounts_silicon_fk FOREIGN KEY (silicon_account, silicon_kind)
        REFERENCES commit.accounts (uuid, kind) DEFERRABLE INITIALLY IMMEDIATE,
    CONSTRAINT silicon_allowed_accounts_allowed_fk FOREIGN KEY (allowed_account)
        REFERENCES commit.accounts (uuid) DEFERRABLE INITIALLY IMMEDIATE,
    CONSTRAINT silicon_allowed_accounts_added_by_fk FOREIGN KEY (added_by_account)
        REFERENCES commit.accounts (uuid) DEFERRABLE INITIALLY IMMEDIATE,
    CONSTRAINT silicon_allowed_accounts_not_self CHECK (silicon_account <> allowed_account)
);
COMMENT ON TABLE commit.silicon_allowed_accounts IS
    'Accounts outside a Silicon''s custodian circle that the Silicon (or its custodian) allowed to assign it work or add it to projects. Silicons are not open to the world.';
CREATE INDEX silicon_allowed_accounts_allowed_idx ON commit.silicon_allowed_accounts (allowed_account);

-- ---------------------------------------------------------------------------------------------------
-- Access policy: the custodian circle, projects and todos
-- ---------------------------------------------------------------------------------------------------

-- circle(Carbon C) = C and every Silicon whose custodian is C.
-- circle(Silicon S) = S, its custodian, and every other Silicon with the same custodian.
-- The relation is symmetric, so "b is in a's circle" and "a is in b's circle" are the same question.
CREATE FUNCTION commit.in_circle(p_a text, p_b text) RETURNS boolean
LANGUAGE sql STABLE
SET search_path = pg_catalog, pg_temp
AS $$
    SELECT p_a = p_b OR EXISTS (
        SELECT 1
          FROM commit.accounts AS a
          JOIN commit.accounts AS b ON b.uuid = p_b
         WHERE a.uuid = p_a
           AND a.status <> 'unlinked' AND b.status <> 'unlinked'
           AND (
               b.custodian_uuid = a.uuid
               OR a.custodian_uuid = b.uuid
               OR (a.custodian_uuid IS NOT NULL AND a.custodian_uuid = b.custodian_uuid)
           )
    )
$$;
COMMENT ON FUNCTION commit.in_circle(text, text) IS
    'True when both accounts are the same, one is the other''s custodian, or both are Silicons with the same custodian.';

-- Every account in p_account's circle (itself first).
CREATE FUNCTION commit.circle_of(p_account text) RETURNS text[]
LANGUAGE sql STABLE
SET search_path = pg_catalog, pg_temp
AS $$
    SELECT array_prepend(p_account, coalesce(array_agg(DISTINCT member.uuid) FILTER (WHERE member.uuid <> p_account), '{}'))
      FROM commit.accounts AS me
      LEFT JOIN commit.accounts AS member
        ON member.status <> 'unlinked'
       AND (
           member.custodian_uuid = me.uuid
           OR me.custodian_uuid = member.uuid
           OR (me.custodian_uuid IS NOT NULL AND member.custodian_uuid = me.custodian_uuid)
       )
     WHERE me.uuid = p_account
$$;

-- Changing a project: its members and the custodians of its member Silicons.
CREATE FUNCTION commit.project_writable(p_project uuid, p_account text) RETURNS boolean
LANGUAGE sql STABLE
SET search_path = pg_catalog, pg_temp
AS $$
    SELECT EXISTS (
        SELECT 1
          FROM commit.project_participants AS member
          JOIN commit.projects AS project ON project.id = member.project_id AND project.deleted_at IS NULL
          LEFT JOIN commit.accounts AS member_account ON member_account.uuid = member.participant_account
         WHERE member.project_id = p_project
           AND member.removed_at IS NULL
           AND (
               member.participant_account = p_account
               OR (member_account.kind = 'silicon' AND member_account.custodian_uuid = p_account)
           )
    )
$$;

-- Reading a project: its members, the custodians of its member Silicons, and (unless private) the
-- owner's circle. Deleted projects are visible to nobody.
CREATE FUNCTION commit.project_access(p_project uuid, p_account text) RETURNS boolean
LANGUAGE sql STABLE
SET search_path = pg_catalog, pg_temp
AS $$
    SELECT EXISTS (
        SELECT 1
          FROM commit.projects AS project
         WHERE project.id = p_project
           AND project.deleted_at IS NULL
           AND (
               commit.project_writable(project.id, p_account)
               OR (NOT project.private AND commit.in_circle(project.owner_account, p_account))
           )
    )
$$;

-- Reading a todo: everyone in the circle of its owner or of its assignee, plus whoever can read its project.
CREATE FUNCTION commit.todo_access(p_todo uuid, p_account text) RETURNS boolean
LANGUAGE sql STABLE
SET search_path = pg_catalog, pg_temp
AS $$
    SELECT EXISTS (
        SELECT 1
          FROM commit.todos AS todo
         WHERE todo.id = p_todo
           AND todo.deleted_at IS NULL
           AND (
               commit.in_circle(todo.assigned_by_account, p_account)
               OR commit.in_circle(todo.assigned_to_account, p_account)
               OR (todo.project_id IS NOT NULL AND commit.project_access(todo.project_id, p_account))
           )
    )
$$;

-- Silicons are not open to the world: work and invitations from outside a Silicon's circle need its allow-list.
CREATE FUNCTION commit.may_reach(p_from text, p_to text) RETURNS boolean
LANGUAGE sql STABLE
SET search_path = pg_catalog, pg_temp
AS $$
    SELECT NOT EXISTS (SELECT 1 FROM commit.accounts WHERE uuid = p_to AND kind = 'silicon')
        OR commit.in_circle(p_from, p_to)
        OR EXISTS (
            SELECT 1 FROM commit.silicon_allowed_accounts AS allowed
             WHERE allowed.silicon_account = p_to AND allowed.allowed_account = p_from
        )
$$;

DROP FUNCTION commit.project_access(uuid, uuid, uuid, text[]);

-- ---------------------------------------------------------------------------------------------------
-- Invariant triggers, redefined on row ids and accounts (organization_id is provenance only)
-- ---------------------------------------------------------------------------------------------------

CREATE OR REPLACE FUNCTION commit_private.assert_project_completion() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    target_project_id uuid;
    current_status commit.project_status;
BEGIN
    IF TG_TABLE_NAME = 'projects' THEN
        target_project_id := NEW.id;
    ELSE
        target_project_id := NEW.project_id;
    END IF;

    SELECT project.status INTO current_status
      FROM commit.projects AS project
     WHERE project.id = target_project_id;
    IF NOT FOUND THEN
        RETURN NULL;
    END IF;

    IF current_status = 'completed' AND NOT EXISTS (
        SELECT 1 FROM commit.project_entries AS entry
         WHERE entry.project_id = target_project_id AND entry.entry_type = 'completion'
    ) THEN
        RAISE EXCEPTION 'a completed project requires a completion statement' USING ERRCODE = '23514';
    END IF;

    IF TG_TABLE_NAME = 'project_entries' THEN
        -- Nested: NEW has no entry_type when the trigger fires for commit.projects.
        IF NEW.entry_type = 'completion' AND current_status <> 'completed' THEN
            RAISE EXCEPTION 'a completion statement must atomically complete its project' USING ERRCODE = '23514';
        END IF;
    END IF;
    RETURN NULL;
END;
$$;

CREATE OR REPLACE FUNCTION commit_private.assert_project_participation() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    target_project_id uuid;
    project_owner text;
    active_participant_count integer;
BEGIN
    IF TG_TABLE_NAME = 'projects' THEN
        target_project_id := NEW.id;
    ELSE
        target_project_id := COALESCE(NEW.project_id, OLD.project_id);
    END IF;

    SELECT project.owner_account INTO project_owner
      FROM commit.projects AS project
     WHERE project.id = target_project_id;
    IF NOT FOUND THEN
        RETURN NULL;
    END IF;

    SELECT count(*) INTO active_participant_count
      FROM commit.project_participants AS participant
     WHERE participant.project_id = target_project_id AND participant.removed_at IS NULL;

    IF active_participant_count = 0 THEN
        RAISE EXCEPTION 'project % must retain at least one active member', target_project_id
            USING ERRCODE = '23514';
    END IF;
    IF active_participant_count > 100 THEN
        RAISE EXCEPTION 'project % exceeds the 100 member database limit', target_project_id
            USING ERRCODE = '23514';
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM commit.project_participants AS participant
         WHERE participant.project_id = target_project_id
           AND participant.participant_account = project_owner
           AND participant.removed_at IS NULL
    ) THEN
        RAISE EXCEPTION 'the project owner must remain an active member' USING ERRCODE = '23514';
    END IF;
    RETURN NULL;
END;
$$;

CREATE OR REPLACE FUNCTION commit_private.create_initial_project_diary() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO commit.project_diaries (
        organization_id, project_id, markdown, version, updated_by_principal_id, updated_by_account, updated_at
    )
    VALUES (
        NEW.organization_id, NEW.id, '', 1, NEW.created_by_principal_id, NEW.created_by_account, NEW.created_at
    );
    RETURN NEW;
END;
$$;

CREATE OR REPLACE FUNCTION commit_private.preserve_todo_tombstone() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'todos must be soft-deleted' USING ERRCODE = '23514';
    END IF;

    IF NEW.id IS DISTINCT FROM OLD.id
       OR NEW.organization_id IS DISTINCT FROM OLD.organization_id
       OR NEW.assigned_by_principal_id IS DISTINCT FROM OLD.assigned_by_principal_id
       OR NEW.assigned_by_account IS DISTINCT FROM OLD.assigned_by_account THEN
        RAISE EXCEPTION 'a todo''s id and owner are immutable' USING ERRCODE = '23514';
    END IF;

    IF OLD.deleted_at IS NOT NULL THEN
        -- D-025: retention may erase user-authored content; identity, assignment, status and deletion
        -- history of a tombstone stay as they are.
        IF NEW.assigned_to_principal_id IS DISTINCT FROM OLD.assigned_to_principal_id
            OR NEW.assigned_to_account IS DISTINCT FROM OLD.assigned_to_account
            OR NEW.status IS DISTINCT FROM OLD.status
            OR NEW.version IS DISTINCT FROM OLD.version
            OR NEW.created_at IS DISTINCT FROM OLD.created_at
            OR NEW.updated_at IS DISTINCT FROM OLD.updated_at
            OR NEW.deleted_at IS DISTINCT FROM OLD.deleted_at
            OR NEW.content_retain_until IS DISTINCT FROM OLD.content_retain_until
            OR NEW.deleted_by_principal_id IS DISTINCT FROM OLD.deleted_by_principal_id
            OR NEW.deleted_by_account IS DISTINCT FROM OLD.deleted_by_account
            OR NEW.title IS DISTINCT FROM '[deleted]'
            OR NEW.description IS NOT NULL
        THEN
            RAISE EXCEPTION 'deleted todo tombstones only permit retention redaction' USING ERRCODE = '23514';
        END IF;
    END IF;
    RETURN NEW;
END;
$$;

CREATE OR REPLACE FUNCTION commit_private.enforce_todo_activity_redaction() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.id IS DISTINCT FROM OLD.id
       OR NEW.organization_id IS DISTINCT FROM OLD.organization_id
       OR NEW.todo_id IS DISTINCT FROM OLD.todo_id
       OR NEW.activity_type IS DISTINCT FROM OLD.activity_type
       OR NEW.actor_principal_id IS DISTINCT FROM OLD.actor_principal_id
       OR NEW.actor_account IS DISTINCT FROM OLD.actor_account
       OR NEW.request_id IS DISTINCT FROM OLD.request_id
       OR NEW.created_at IS DISTINCT FROM OLD.created_at
       OR NEW.retain_until IS DISTINCT FROM OLD.retain_until
       OR NEW.changes IS DISTINCT FROM '{}'::jsonb THEN
        RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'todo activity permits only canonical changes redaction';
    END IF;

    IF OLD.changes <> '{}'::jsonb AND NOT EXISTS (
        SELECT 1 FROM commit.todos AS todo
         WHERE todo.id = OLD.todo_id
           AND todo.deleted_at IS NOT NULL
           AND todo.content_retain_until <= transaction_timestamp()
           AND NOT EXISTS (
               SELECT 1 FROM commit.idempotency_records AS replay
                WHERE replay.todo_id = todo.id AND replay.expires_at > transaction_timestamp()
           )
    ) THEN
        RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'todo activity content retention deadline has not elapsed';
    END IF;
    RETURN NEW;
END;
$$;

CREATE OR REPLACE FUNCTION commit_private.prevent_participant_history_rewrite() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'project participant history must not be deleted' USING ERRCODE = '23514';
    END IF;
    IF NEW.id <> OLD.id
       OR NEW.organization_id IS DISTINCT FROM OLD.organization_id
       OR NEW.project_id <> OLD.project_id
       OR NEW.silicon_principal_id IS DISTINCT FROM OLD.silicon_principal_id
       OR NEW.participant_account IS DISTINCT FROM OLD.participant_account
       OR NEW.added_by_principal_id IS DISTINCT FROM OLD.added_by_principal_id
       OR NEW.added_by_account IS DISTINCT FROM OLD.added_by_account
       OR NEW.added_at <> OLD.added_at
       OR OLD.removed_at IS NOT NULL THEN
        RAISE EXCEPTION 'project participant identity and closed history are immutable' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;

CREATE OR REPLACE FUNCTION commit_private.prevent_project_task_reparenting() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.id <> OLD.id
       OR NEW.organization_id IS DISTINCT FROM OLD.organization_id
       OR NEW.project_id <> OLD.project_id
       OR NEW.parent_task_id IS DISTINCT FROM OLD.parent_task_id
       OR NEW.created_by_principal_id IS DISTINCT FROM OLD.created_by_principal_id
       OR NEW.created_by_account IS DISTINCT FROM OLD.created_by_account
       OR NEW.created_at <> OLD.created_at THEN
        RAISE EXCEPTION 'project task identity, parent, creator, and creation time are immutable'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;

CREATE OR REPLACE FUNCTION commit_private.prevent_project_identity_change() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.id <> OLD.id
       OR NEW.organization_id IS DISTINCT FROM OLD.organization_id
       OR NEW.slug <> OLD.slug
       OR NEW.uid <> OLD.uid
       OR NEW.legacy_uid IS DISTINCT FROM OLD.legacy_uid
       OR NEW.created_by_principal_id IS DISTINCT FROM OLD.created_by_principal_id
       OR NEW.created_by_account IS DISTINCT FROM OLD.created_by_account
       OR NEW.created_at <> OLD.created_at
       OR (OLD.deleted_at IS NOT NULL AND NEW.deleted_at IS DISTINCT FROM OLD.deleted_at) THEN
        RAISE EXCEPTION 'project identifiers, creator, creation time and deletion are immutable'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;

CREATE OR REPLACE FUNCTION commit_private.preserve_project_diary_identity() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'a project diary must not be deleted' USING ERRCODE = '23514';
    END IF;
    IF NEW.organization_id IS DISTINCT FROM OLD.organization_id OR NEW.project_id <> OLD.project_id THEN
        RAISE EXCEPTION 'a project diary''s project is immutable' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;

CREATE OR REPLACE FUNCTION commit_private.preserve_silicon_notification_settings_identity() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.organization_id IS DISTINCT FROM OLD.organization_id
        OR NEW.silicon_principal_id IS DISTINCT FROM OLD.silicon_principal_id
        OR NEW.silicon_account IS DISTINCT FROM OLD.silicon_account
        OR NEW.created_at IS DISTINCT FROM OLD.created_at THEN
        RAISE EXCEPTION 'Silicon notification settings identity is immutable' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;

CREATE OR REPLACE FUNCTION commit_private.preserve_todo_notification_subscription_identity() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.organization_id IS DISTINCT FROM OLD.organization_id
        OR NEW.silicon_principal_id IS DISTINCT FROM OLD.silicon_principal_id
        OR NEW.silicon_account IS DISTINCT FROM OLD.silicon_account
        OR NEW.todo_id IS DISTINCT FROM OLD.todo_id
        OR NEW.created_at IS DISTINCT FROM OLD.created_at THEN
        RAISE EXCEPTION 'todo notification subscription identity is immutable' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;

CREATE OR REPLACE FUNCTION commit_private.preserve_project_entries() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'project entries are append-only' USING ERRCODE = '23514';
    RETURN OLD;
END;
$$;

-- Project tasks mirror their linked todos (and back), now keyed by row ids and accounts. IAM-era principal
-- columns are provenance and are never rewritten after this migration.
CREATE OR REPLACE FUNCTION commit_private.sync_project_work() RETURNS trigger
LANGUAGE plpgsql
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE descendants uuid[];
BEGIN
 IF pg_trigger_depth() > 1 THEN RETURN NEW; END IF;
 IF TG_TABLE_NAME = 'todos' THEN
   UPDATE commit.project_tasks
      SET title = NEW.title,
          description = coalesce(NEW.description, ''),
          status = NEW.status,
          assigned_to_account = NEW.assigned_to_account,
          deleted_at = NEW.deleted_at
    WHERE todo_id = NEW.id;
   IF OLD.deleted_at IS NULL AND NEW.deleted_at IS NOT NULL THEN
     WITH RECURSIVE subtree AS (
       SELECT id, project_id, deleted_at FROM commit.project_tasks WHERE todo_id = NEW.id
       UNION
       SELECT child.id, child.project_id, child.deleted_at
         FROM commit.project_tasks AS child
         JOIN subtree AS parent ON child.parent_task_id = parent.id AND child.project_id = parent.project_id
     ) SELECT array_agg(id) INTO descendants FROM subtree WHERE deleted_at IS NULL;

     -- Nested mirror triggers deliberately do not recurse. Update both tables explicitly so assigned
     -- and unassigned descendants share one lifecycle.
     UPDATE commit.todos SET
       deleted_at = greatest(updated_at, NEW.deleted_at, clock_timestamp()),
       content_retain_until = greatest(updated_at, NEW.deleted_at, clock_timestamp())
         + (NEW.content_retain_until - NEW.deleted_at),
       deleted_by_account = NEW.deleted_by_account
      WHERE deleted_at IS NULL
        AND id IN (SELECT todo_id FROM commit.project_tasks WHERE id = ANY(descendants));
     UPDATE commit.project_tasks SET deleted_at = greatest(updated_at, NEW.deleted_at, clock_timestamp())
      WHERE id = ANY(descendants) AND deleted_at IS NULL;
   END IF;
 ELSE
   IF NEW.todo_id IS NOT NULL THEN
     UPDATE commit.todos
        SET title = NEW.title,
            description = NEW.description,
            status = NEW.status,
            assigned_to_account = NEW.assigned_to_account
      WHERE id = NEW.todo_id AND deleted_at IS NULL;
   END IF;
 END IF;
 RETURN NEW;
END;
$$;

DROP TRIGGER tasks_sync_todo ON commit.project_tasks;
CREATE TRIGGER tasks_sync_todo
AFTER UPDATE OF title, description, status, assigned_to_account ON commit.project_tasks
FOR EACH ROW EXECUTE FUNCTION commit_private.sync_project_work();

DROP TRIGGER todos_sync_project ON commit.todos;
CREATE TRIGGER todos_sync_project
AFTER UPDATE OF title, description, status, assigned_to_account, deleted_at ON commit.todos
FOR EACH ROW EXECUTE FUNCTION commit_private.sync_project_work();

-- Project revisions record accounts (type, current id, uuid). Older snapshots keep their original shape.
CREATE OR REPLACE FUNCTION commit_private.record_project_revision() RETURNS trigger
LANGUAGE plpgsql
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE pid uuid; v bigint; who jsonb; doc jsonb;
BEGIN
 IF NEW.resource_type IN ('project', 'project_diary') THEN pid := NEW.resource_id;
 ELSIF NEW.resource_type IN ('project_task', 'project_entry') THEN pid := (NEW.change_summary->>'project_id')::uuid;
 ELSIF NEW.resource_type = 'todo' THEN SELECT project_id INTO pid FROM commit.todos WHERE id = NEW.resource_id;
 ELSE RETURN NEW; END IF;
 IF pid IS NULL THEN RETURN NEW; END IF;
 PERFORM 1 FROM commit.projects WHERE id = pid FOR UPDATE;
 IF NOT FOUND THEN RETURN NEW; END IF;
 SELECT jsonb_build_object('type', kind, 'id', public_id, 'uuid', uuid) INTO who
   FROM commit.accounts WHERE uuid = NEW.actor_account;
 INSERT INTO commit.project_collaborators (project_id, account) VALUES (pid, NEW.actor_account)
 ON CONFLICT DO NOTHING;
 SELECT coalesce(max(version), 0) + 1 INTO v FROM commit.project_versions WHERE project_id = pid;
 SELECT jsonb_build_object(
   'id', p.id, 'name', p.name, 'slug', p.slug, 'uid', p.uid, 'status', p.status,
   'description', p.description, 'attachments', p.attachments, 'private', p.private,
   'owner', (SELECT jsonb_build_object('type', a.kind, 'id', a.public_id, 'uuid', a.uuid)
               FROM commit.accounts a WHERE a.uuid = p.owner_account),
   'participants', (SELECT coalesce(jsonb_agg(jsonb_build_object('type', a.kind, 'id', a.public_id, 'uuid', a.uuid)
                                               ORDER BY a.public_id, a.uuid), '[]')
                      FROM commit.project_participants m JOIN commit.accounts a ON a.uuid = m.participant_account
                     WHERE m.project_id = p.id AND m.removed_at IS NULL),
   'diary', (SELECT markdown FROM commit.project_diaries WHERE project_id = p.id),
   'tasks', (SELECT coalesce(jsonb_agg(jsonb_build_object(
                 'id', t.id, 'parent_task_id', t.parent_task_id, 'title', t.title, 'description', t.description,
                 'status', t.status, 'assigned_to', a.public_id, 'assigned_to_uuid', a.uuid, 'todo_id', t.todo_id)
               ORDER BY t.created_at, t.id), '[]')
               FROM commit.project_tasks t LEFT JOIN commit.accounts a ON a.uuid = t.assigned_to_account
              WHERE t.project_id = p.id AND t.deleted_at IS NULL),
   'entries', (SELECT coalesce(jsonb_agg(to_jsonb(e) - 'organization_id' - 'created_by_principal_id'
                                         ORDER BY e.created_at, e.id), '[]')
                 FROM commit.project_entries e WHERE e.project_id = p.id))
   INTO doc FROM commit.projects p WHERE p.id = pid;
 INSERT INTO commit.project_versions (project_id, version, actor, action, snapshot) VALUES (pid, v, who, NEW.action, doc);
 DELETE FROM commit.project_versions WHERE project_id = pid AND version <= v - 1000;
 RETURN NEW;
END;
$$;

-- Email goes to the address each Carbon chose for Commit (one preference per account).
CREATE OR REPLACE FUNCTION commit_private.queue_project_email() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE pid uuid; v_kind text; v_title text; assignee text;
BEGIN
 IF NEW.resource_type = 'project' THEN pid := NEW.resource_id;
 ELSIF NEW.change_summary ? 'project_id' THEN pid := (NEW.change_summary->>'project_id')::uuid;
 ELSIF NEW.resource_type = 'todo' THEN
   SELECT t.project_id, t.assigned_to_account, t.title INTO pid, assignee, v_title FROM commit.todos t WHERE t.id = NEW.resource_id;
 END IF;
 IF NEW.action = 'project.completed' THEN v_kind := 'project_completed';
 ELSIF NEW.action IN ('project.task.created', 'project.task.claimed', 'todo.created', 'todo.reassigned') THEN v_kind := 'task_assigned';
 ELSIF NEW.action = 'todo.updated' AND (NEW.change_summary->'fields') ? 'assigned_to' THEN v_kind := 'task_assigned';
 ELSIF NEW.action = 'todo.updated' AND (NEW.change_summary->'fields') ? 'status'
       AND EXISTS (SELECT 1 FROM commit.todos WHERE id = NEW.resource_id AND status = 'completed') THEN v_kind := 'task_completed';
 ELSIF NEW.action = 'project.task.updated' AND coalesce((NEW.change_summary->>'assignment_changed')::boolean, false) THEN v_kind := 'task_assigned';
 ELSIF NEW.action = 'project.task.updated' AND coalesce((NEW.change_summary->>'status_changed')::boolean, false)
       AND EXISTS (SELECT 1 FROM commit.project_tasks WHERE id = NEW.resource_id AND status = 'completed') THEN v_kind := 'task_completed';
 ELSIF NEW.action = 'todo.status_changed'
       AND EXISTS (SELECT 1 FROM commit.todos WHERE id = NEW.resource_id AND status = 'completed') THEN v_kind := 'task_completed';
 ELSIF pid IS NOT NULL THEN v_kind := 'project_updates';
 ELSE RETURN NEW; END IF;
 IF pid IS NOT NULL THEN SELECT name INTO v_title FROM commit.projects WHERE id = pid; END IF;
 IF NEW.resource_type = 'project_task' THEN
   SELECT assigned_to_account INTO assignee FROM commit.project_tasks WHERE id = NEW.resource_id;
 END IF;
 INSERT INTO commit.email_jobs (account, project_id, kind, recipient, subject, body)
 SELECT e.account, pid, v_kind, e.email, 'Commit: ' || replace(v_kind, '_', ' '),
        coalesce(v_title, 'Work item') || E'\nEvent: ' || NEW.action
          || E'\nOpen Commit: https://commit.teamofsilicons.com\nManage delivery in Commit email settings.'
   FROM commit.email_preferences e
   JOIN commit.accounts a ON a.uuid = e.account AND a.status = 'active'
  WHERE e.enabled AND e.email <> ''
    AND CASE v_kind
          WHEN 'project_completed' THEN e.project_completed
          WHEN 'project_updates' THEN e.project_updates
          WHEN 'task_completed' THEN e.task_completed
          WHEN 'task_assigned' THEN e.task_assigned AND e.account = assignee
          ELSE false END
    AND (pid IS NULL OR commit.project_access(pid, e.account))
    AND (e.account = assignee OR (pid IS NOT NULL AND (
          EXISTS (SELECT 1 FROM commit.project_participants m
                   WHERE m.project_id = pid AND m.participant_account = e.account AND m.removed_at IS NULL)
          OR EXISTS (SELECT 1 FROM commit.project_collaborators c WHERE c.project_id = pid AND c.account = e.account))));
 IF NEW.action = 'project.created' THEN
  INSERT INTO commit.email_jobs (account, project_id, kind, recipient, subject, body)
  SELECT e.account, pid, 'task_assigned', e.email, 'Commit: task assigned',
         t.title || E'\nProject: ' || v_title
           || E'\nOpen Commit: https://commit.teamofsilicons.com\nManage delivery in Commit email settings.'
    FROM commit.project_tasks t
    JOIN commit.email_preferences e ON e.account = t.assigned_to_account
    JOIN commit.accounts a ON a.uuid = e.account AND a.status = 'active'
   WHERE t.project_id = pid AND t.deleted_at IS NULL AND e.enabled AND e.task_assigned AND e.email <> ''
     AND commit.project_access(pid, e.account);
 END IF;
 RETURN NEW;
END $$;

CREATE OR REPLACE FUNCTION commit.claim_email() RETURNS SETOF jsonb
LANGUAGE plpgsql SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE job commit.email_jobs; preference commit.email_preferences; allowed boolean := true;
BEGIN
 SELECT j.* INTO job FROM commit.email_jobs j
  WHERE j.status = 'pending' AND j.next_attempt_at <= clock_timestamp()
  ORDER BY j.next_attempt_at LIMIT 1 FOR UPDATE OF j SKIP LOCKED;
 IF NOT FOUND THEN RETURN; END IF;
 IF job.kind <> 'bug_report' THEN
  allowed := EXISTS (SELECT 1 FROM commit.accounts WHERE uuid = job.account AND status = 'active');
  IF job.project_id IS NOT NULL THEN
   PERFORM 1 FROM commit.projects WHERE id = job.project_id FOR SHARE;
   allowed := allowed AND commit.project_access(job.project_id, job.account);
  END IF;
  SELECT * INTO preference FROM commit.email_preferences WHERE account = job.account FOR SHARE;
  allowed := allowed AND FOUND AND preference.enabled AND preference.email = job.recipient
             AND coalesce((to_jsonb(preference)->>job.kind)::boolean, false);
 END IF;
 IF NOT allowed THEN
  UPDATE commit.email_jobs SET status = 'suppressed' WHERE id = job.id;
  RETURN NEXT jsonb_build_object('simulated', true);
  RETURN;
 END IF;
 RETURN NEXT to_jsonb(job) || jsonb_build_object('simulated', false);
END $$;

-- A delegated-work webhook goes out only while its recipient is a live account that can still read the work.
CREATE OR REPLACE FUNCTION commit.lock_notification_access(p_event uuid) RETURNS boolean
LANGUAGE plpgsql SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE pid uuid; recipient text;
BEGIN
 SELECT t.project_id, e.recipient_silicon_account INTO pid, recipient
   FROM commit.outbox_events e JOIN commit.todos t ON t.id = e.todo_id
  WHERE e.id = p_event;
 IF NOT FOUND THEN RETURN false; END IF;
 IF NOT EXISTS (SELECT 1 FROM commit.accounts WHERE uuid = recipient AND status = 'active') THEN
   RETURN false;
 END IF;
 IF pid IS NULL THEN RETURN true; END IF;
 PERFORM 1 FROM commit.projects WHERE id = pid FOR SHARE;
 RETURN commit.project_access(pid, recipient);
END $$;

-- Retention keyed by row ids, so todos created without an organization are purged too.
CREATE OR REPLACE FUNCTION commit.run_retention_pass(p_batch_size integer)
RETURNS TABLE (
    notes_purged bigint, attachments_purged bigint, activity_changes_redacted bigint, activity_purged bigint,
    todos_redacted bigint, idempotency_purged bigint, audit_purged bigint, delivered_outbox_purged bigint,
    dead_letter_outbox_purged bigint
)
LANGUAGE plpgsql SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF p_batch_size < 1 OR p_batch_size > 10000 THEN
        RAISE EXCEPTION 'retention batch size must be between 1 and 10000' USING ERRCODE = '22023';
    END IF;

    WITH candidates AS MATERIALIZED (
        SELECT note.id
          FROM commit.todo_notes AS note
          JOIN commit.todos AS todo ON todo.id = note.todo_id
         WHERE todo.content_retain_until <= transaction_timestamp()
           AND NOT EXISTS (SELECT 1 FROM commit.idempotency_records AS replay
                            WHERE replay.todo_id = todo.id AND replay.expires_at > transaction_timestamp())
         ORDER BY todo.content_retain_until, note.id
         LIMIT p_batch_size
         FOR UPDATE OF note SKIP LOCKED
    )
    DELETE FROM commit.todo_notes AS note USING candidates WHERE note.id = candidates.id;
    GET DIAGNOSTICS notes_purged = ROW_COUNT;

    WITH candidates AS MATERIALIZED (
        SELECT attachment.todo_id, attachment.position
          FROM commit.todo_attachments AS attachment
          JOIN commit.todos AS todo ON todo.id = attachment.todo_id
         WHERE todo.content_retain_until <= transaction_timestamp()
           AND NOT EXISTS (SELECT 1 FROM commit.idempotency_records AS replay
                            WHERE replay.todo_id = todo.id AND replay.expires_at > transaction_timestamp())
         ORDER BY todo.content_retain_until, attachment.todo_id, attachment.position
         LIMIT p_batch_size
         FOR UPDATE OF attachment SKIP LOCKED
    )
    DELETE FROM commit.todo_attachments AS attachment USING candidates
     WHERE attachment.todo_id = candidates.todo_id AND attachment.position = candidates.position;
    GET DIAGNOSTICS attachments_purged = ROW_COUNT;

    WITH candidates AS MATERIALIZED (
        SELECT activity.id
          FROM commit.todo_activity AS activity
         WHERE activity.retain_until <= transaction_timestamp()
         ORDER BY activity.retain_until, activity.id
         LIMIT p_batch_size
         FOR UPDATE OF activity SKIP LOCKED
    )
    DELETE FROM commit.todo_activity AS activity USING candidates WHERE activity.id = candidates.id;
    GET DIAGNOSTICS activity_purged = ROW_COUNT;

    WITH candidates AS MATERIALIZED (
        SELECT activity.id
          FROM commit.todo_activity AS activity
          JOIN commit.todos AS todo ON todo.id = activity.todo_id
         WHERE activity.changes <> '{}'::jsonb
           AND todo.content_retain_until <= transaction_timestamp()
           AND NOT EXISTS (SELECT 1 FROM commit.idempotency_records AS replay
                            WHERE replay.todo_id = todo.id AND replay.expires_at > transaction_timestamp())
         ORDER BY todo.content_retain_until, activity.created_at, activity.id
         LIMIT p_batch_size
         FOR UPDATE OF activity SKIP LOCKED
    )
    UPDATE commit.todo_activity AS activity SET changes = '{}'::jsonb
      FROM candidates WHERE activity.id = candidates.id;
    GET DIAGNOSTICS activity_changes_redacted = ROW_COUNT;

    WITH candidates AS MATERIALIZED (
        SELECT todo.id
          FROM commit.todos AS todo
         WHERE todo.content_retain_until <= transaction_timestamp()
           AND (todo.title <> '[deleted]' OR todo.description IS NOT NULL)
           AND NOT EXISTS (SELECT 1 FROM commit.idempotency_records AS replay
                            WHERE replay.todo_id = todo.id AND replay.expires_at > transaction_timestamp())
         ORDER BY todo.content_retain_until, todo.id
         LIMIT p_batch_size
         FOR UPDATE OF todo SKIP LOCKED
    )
    UPDATE commit.todos AS todo SET title = '[deleted]', description = NULL
      FROM candidates WHERE todo.id = candidates.id;
    GET DIAGNOSTICS todos_redacted = ROW_COUNT;

    WITH candidates AS MATERIALIZED (
        SELECT record.id FROM commit.idempotency_records AS record
         WHERE record.expires_at <= transaction_timestamp()
         ORDER BY record.expires_at, record.id
         LIMIT p_batch_size
         FOR UPDATE OF record SKIP LOCKED
    )
    DELETE FROM commit.idempotency_records AS record USING candidates WHERE record.id = candidates.id;
    GET DIAGNOSTICS idempotency_purged = ROW_COUNT;

    WITH candidates AS MATERIALIZED (
        SELECT event.id FROM commit.audit_events AS event
         WHERE event.retain_until <= transaction_timestamp()
         ORDER BY event.retain_until, event.id
         LIMIT p_batch_size
         FOR UPDATE OF event SKIP LOCKED
    )
    DELETE FROM commit.audit_events AS event USING candidates WHERE event.id = candidates.id;
    GET DIAGNOSTICS audit_purged = ROW_COUNT;

    WITH candidates AS MATERIALIZED (
        SELECT event.id FROM commit.outbox_events AS event
         WHERE event.status = 'delivered' AND event.purge_after <= transaction_timestamp()
         ORDER BY event.purge_after, event.id
         LIMIT p_batch_size
         FOR UPDATE OF event SKIP LOCKED
    )
    DELETE FROM commit.outbox_events AS event USING candidates WHERE event.id = candidates.id;
    GET DIAGNOSTICS delivered_outbox_purged = ROW_COUNT;

    WITH candidates AS MATERIALIZED (
        SELECT event.id FROM commit.outbox_events AS event
         WHERE event.status = 'dead_letter' AND event.purge_after <= transaction_timestamp()
         ORDER BY event.purge_after, event.id
         LIMIT p_batch_size
         FOR UPDATE OF event SKIP LOCKED
    )
    DELETE FROM commit.outbox_events AS event USING candidates WHERE event.id = candidates.id;
    GET DIAGNOSTICS dead_letter_outbox_purged = ROW_COUNT;

    RETURN NEXT;
END;
$$;

-- account.deleted: remove the account's personal data and keep everyone else's work intact.
--   * todos it made for itself are deleted (normal tombstones; retention purges their content later);
--   * projects it owns pass to their longest-standing other member, or are deleted when it was alone;
--   * its project memberships end; its notification settings, subscriptions, email preference and
--     allow-list entries are removed; undelivered webhooks and emails to it are cancelled;
--   * the account row is anonymised (status deleted) and every earlier token is refused.
-- Work it delegated to others or received from others stays, shown as a deleted account.
CREATE FUNCTION commit.forget_account(
    p_account text,
    p_at timestamptz,
    p_request_id text,
    p_tombstone_seconds bigint,
    p_audit_seconds bigint
) RETURNS jsonb
LANGUAGE plpgsql SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    todos_deleted bigint := 0;
    memberships_ended bigint := 0;
    projects_transferred bigint := 0;
    projects_deleted bigint := 0;
    settings_removed bigint := 0;
    subscriptions_removed bigint := 0;
    preferences_removed bigint := 0;
    allowlist_removed bigint := 0;
    deliveries_cancelled bigint := 0;
    emails_cancelled bigint := 0;
    owned record;
    successor text;
BEGIN
    IF p_tombstone_seconds < 1 OR p_audit_seconds < 1 THEN
        RAISE EXCEPTION 'retention windows must be positive' USING ERRCODE = '22023';
    END IF;
    PERFORM 1 FROM commit.accounts WHERE uuid = p_account FOR UPDATE;
    IF NOT FOUND THEN
        RETURN jsonb_build_object('known', false);
    END IF;

    UPDATE commit.todos
       SET deleted_at = greatest(updated_at, clock_timestamp()),
           content_retain_until = greatest(updated_at, clock_timestamp()) + make_interval(secs => p_tombstone_seconds),
           deleted_by_account = p_account
     WHERE assigned_by_account = p_account AND assigned_to_account = p_account AND deleted_at IS NULL;
    GET DIAGNOSTICS todos_deleted = ROW_COUNT;

    FOR owned IN
        SELECT project.id FROM commit.projects AS project
         WHERE project.owner_account = p_account AND project.deleted_at IS NULL
         ORDER BY project.created_at, project.id
         FOR UPDATE
    LOOP
        SELECT member.participant_account INTO successor
          FROM commit.project_participants AS member
          JOIN commit.accounts AS account ON account.uuid = member.participant_account AND account.status = 'active'
         WHERE member.project_id = owned.id AND member.removed_at IS NULL AND member.participant_account <> p_account
         ORDER BY member.added_at, member.id
         LIMIT 1;
        IF successor IS NULL THEN
            UPDATE commit.projects SET deleted_at = greatest(updated_at, clock_timestamp()) WHERE id = owned.id;
            projects_deleted := projects_deleted + 1;
            INSERT INTO commit.audit_events (id, actor_account, action, resource_type, resource_id, request_id, change_summary, retain_until)
            VALUES (gen_random_uuid(), p_account, 'project.deleted', 'project', owned.id, p_request_id,
                    jsonb_build_object('reason', 'owner_account_deleted'),
                    transaction_timestamp() + make_interval(secs => p_audit_seconds));
        ELSE
            UPDATE commit.projects SET owner_account = successor WHERE id = owned.id;
            projects_transferred := projects_transferred + 1;
            INSERT INTO commit.audit_events (id, actor_account, action, resource_type, resource_id, request_id, change_summary, retain_until)
            VALUES (gen_random_uuid(), p_account, 'project.owner_transferred', 'project', owned.id, p_request_id,
                    jsonb_build_object('reason', 'owner_account_deleted', 'new_owner', successor),
                    transaction_timestamp() + make_interval(secs => p_audit_seconds));
        END IF;
    END LOOP;

    UPDATE commit.project_participants AS member
       SET removed_at = greatest(member.added_at, clock_timestamp()), removed_by_account = p_account
     WHERE member.participant_account = p_account
       AND member.removed_at IS NULL
       AND EXISTS (SELECT 1 FROM commit.projects AS project
                    WHERE project.id = member.project_id AND project.deleted_at IS NULL);
    GET DIAGNOSTICS memberships_ended = ROW_COUNT;

    DELETE FROM commit.todo_notification_subscriptions WHERE silicon_account = p_account;
    GET DIAGNOSTICS subscriptions_removed = ROW_COUNT;
    DELETE FROM commit.silicon_notification_settings WHERE silicon_account = p_account;
    GET DIAGNOSTICS settings_removed = ROW_COUNT;
    DELETE FROM commit.email_preferences WHERE account = p_account;
    GET DIAGNOSTICS preferences_removed = ROW_COUNT;
    DELETE FROM commit.silicon_allowed_accounts WHERE silicon_account = p_account OR allowed_account = p_account;
    GET DIAGNOSTICS allowlist_removed = ROW_COUNT;

    UPDATE commit.outbox_events
       SET status = 'dead_letter', lease_owner = NULL, lease_expires_at = NULL,
           last_error_code = 'recipient_account_deleted',
           dead_lettered_at = greatest(updated_at, clock_timestamp()),
           purge_after = greatest(updated_at, clock_timestamp()) + interval '90 days'
     WHERE recipient_silicon_account = p_account AND status IN ('pending', 'in_flight');
    GET DIAGNOSTICS deliveries_cancelled = ROW_COUNT;
    UPDATE commit.email_jobs SET status = 'suppressed' WHERE account = p_account AND status = 'pending';
    GET DIAGNOSTICS emails_cancelled = ROW_COUNT;

    UPDATE commit.accounts
       SET status = 'deleted', public_id = '', display_name = '', pfp_url = '', email = NULL,
           revoked_before = greatest(coalesce(revoked_before, p_at), p_at),
           refreshed_at = greatest(refreshed_at, clock_timestamp())
     WHERE uuid = p_account;

    RETURN jsonb_build_object(
        'known', true,
        'todos_deleted', todos_deleted,
        'projects_transferred', projects_transferred,
        'projects_deleted', projects_deleted,
        'memberships_ended', memberships_ended,
        'notification_settings_removed', settings_removed,
        'todo_subscriptions_removed', subscriptions_removed,
        'email_preferences_removed', preferences_removed,
        'allowlist_entries_removed', allowlist_removed,
        'webhook_deliveries_cancelled', deliveries_cancelled,
        'emails_cancelled', emails_cancelled
    );
END $$;
REVOKE ALL ON FUNCTION commit.forget_account(text, timestamptz, text, bigint, bigint) FROM PUBLIC;

-- ---------------------------------------------------------------------------------------------------
-- Contract governance: contract 2 is the Silicon Accounts contract (no org_id/tags, actor refs carry uuid,
-- outbox payload version 3). Contract 1 authenticated with IAM and can no longer be served; it is marked
-- deprecated so it sunsets after seven idle days like any replaced contract.
-- ---------------------------------------------------------------------------------------------------

INSERT INTO commit.contract_versions (version, status) VALUES (2, 'active');
UPDATE commit.contract_versions SET status = 'deprecated', deprecated_at = clock_timestamp()
 WHERE version = 1 AND status = 'active';

COMMENT ON TABLE commit.actor_projection IS
    'IAM-era principal registry, kept as provenance for rows created before the Silicon Accounts cutover. Nothing new is written here.';
COMMENT ON TABLE commit.organization_projection IS
    'IAM-era organizations, kept as provenance. Silicon Accounts has no organizations; nothing new is written here.';
