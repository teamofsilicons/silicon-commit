-- Silicon-managed organization projects and their project-local work records.

CREATE TABLE commit.projects (
    id uuid PRIMARY KEY,
    organization_id uuid NOT NULL,
    name text NOT NULL,
    slug text NOT NULL,
    uid text NOT NULL,
    status commit.project_status NOT NULL DEFAULT 'yet_to_start',
    created_by_principal_id uuid NOT NULL,
    created_by_actor_type commit.actor_type
        GENERATED ALWAYS AS ('silicon'::commit.actor_type) STORED,
    version bigint NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    UNIQUE (organization_id, id),
    UNIQUE (organization_id, uid),
    CONSTRAINT projects_organization_fk
        FOREIGN KEY (organization_id)
        REFERENCES commit.organization_projection (organization_id)
        ON DELETE RESTRICT,
    CONSTRAINT projects_creator_silicon_fk
        FOREIGN KEY (
            organization_id,
            created_by_principal_id,
            created_by_actor_type
        )
        REFERENCES commit.actor_projection (
            organization_id,
            principal_id,
            actor_type
        )
        ON DELETE RESTRICT,
    CONSTRAINT projects_non_nil_id
        CHECK (id <> '00000000-0000-0000-0000-000000000000'::uuid),
    CONSTRAINT projects_name_content CHECK (
        name = btrim(name)
        AND btrim(name) <> ''
        AND char_length(name) <= 200
    ),
    CONSTRAINT projects_slug_format CHECK (
        char_length(slug) BETWEEN 1 AND 255
        AND slug ~ '^[a-z0-9]+(-[a-z0-9]+)*$'
    ),
    CONSTRAINT projects_uid_format CHECK (
        uid = btrim(uid)
        AND octet_length(uid) BETWEEN 3 AND 2048
    ),
    CONSTRAINT projects_positive_version CHECK (version > 0),
    CONSTRAINT projects_timestamp_order CHECK (updated_at >= created_at)
);

COMMENT ON TABLE commit.projects IS
    'Organization-visible projects created and managed by Silicons.';
COMMENT ON COLUMN commit.projects.slug IS
    'Immutable locator component derived from the creation name; it is not a unique path identifier.';
COMMENT ON COLUMN commit.projects.uid IS
    'Stable {slug}:{creator_silicon_id}:{utc_unix_milliseconds} public identifier.';

CREATE TRIGGER projects_touch_version
BEFORE UPDATE ON commit.projects
FOR EACH ROW EXECUTE FUNCTION commit_private.touch_versioned_row();

CREATE FUNCTION commit_private.prevent_project_identity_change()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.id <> OLD.id
       OR NEW.organization_id <> OLD.organization_id
       OR NEW.slug <> OLD.slug
       OR NEW.uid <> OLD.uid
       OR NEW.created_by_principal_id <> OLD.created_by_principal_id
       OR NEW.created_at <> OLD.created_at THEN
        RAISE EXCEPTION 'project tenant, identifiers, creator, and creation time are immutable'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER projects_preserve_identity
BEFORE UPDATE ON commit.projects
FOR EACH ROW EXECUTE FUNCTION commit_private.prevent_project_identity_change();

CREATE INDEX projects_org_created_idx
    ON commit.projects (organization_id, created_at DESC, id DESC);
CREATE INDEX projects_org_status_created_idx
    ON commit.projects (organization_id, status, created_at DESC, id DESC);

CREATE TABLE commit.project_participants (
    id uuid PRIMARY KEY,
    organization_id uuid NOT NULL,
    project_id uuid NOT NULL,
    silicon_principal_id uuid NOT NULL,
    silicon_actor_type commit.actor_type
        GENERATED ALWAYS AS ('silicon'::commit.actor_type) STORED,
    added_by_principal_id uuid NOT NULL,
    added_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    removed_by_principal_id uuid,
    removed_at timestamptz,
    UNIQUE (organization_id, id),
    CONSTRAINT project_participants_project_fk
        FOREIGN KEY (organization_id, project_id)
        REFERENCES commit.projects (organization_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT project_participants_silicon_fk
        FOREIGN KEY (
            organization_id,
            silicon_principal_id,
            silicon_actor_type
        )
        REFERENCES commit.actor_projection (
            organization_id,
            principal_id,
            actor_type
        )
        ON DELETE RESTRICT,
    CONSTRAINT project_participants_added_by_fk
        FOREIGN KEY (organization_id, added_by_principal_id)
        REFERENCES commit.actor_projection (organization_id, principal_id)
        ON DELETE RESTRICT,
    CONSTRAINT project_participants_removed_by_fk
        FOREIGN KEY (organization_id, removed_by_principal_id)
        REFERENCES commit.actor_projection (organization_id, principal_id)
        ON DELETE RESTRICT,
    CONSTRAINT project_participants_non_nil_id
        CHECK (id <> '00000000-0000-0000-0000-000000000000'::uuid),
    CONSTRAINT project_participants_removal_consistency CHECK (
        (removed_at IS NULL AND removed_by_principal_id IS NULL)
        OR (removed_at IS NOT NULL AND removed_by_principal_id IS NOT NULL)
    ),
    CONSTRAINT project_participants_timestamp_order
        CHECK (removed_at IS NULL OR removed_at >= added_at)
);

COMMENT ON TABLE commit.project_participants IS
    'Temporal project participation. Removal closes a row instead of erasing historical participation.';

CREATE UNIQUE INDEX project_participants_one_active_membership_idx
    ON commit.project_participants (
        organization_id,
        project_id,
        silicon_principal_id
    )
    WHERE removed_at IS NULL;

CREATE INDEX project_participants_silicon_projects_idx
    ON commit.project_participants (
        organization_id,
        silicon_principal_id,
        project_id
    )
    WHERE removed_at IS NULL;

CREATE INDEX project_participants_project_history_idx
    ON commit.project_participants (
        organization_id,
        project_id,
        added_at,
        id
    );

CREATE FUNCTION commit_private.prevent_participant_history_rewrite()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'project participant history must not be deleted'
            USING ERRCODE = '23514';
    END IF;

    IF NEW.id <> OLD.id
       OR NEW.organization_id <> OLD.organization_id
       OR NEW.project_id <> OLD.project_id
       OR NEW.silicon_principal_id <> OLD.silicon_principal_id
       OR NEW.added_by_principal_id <> OLD.added_by_principal_id
       OR NEW.added_at <> OLD.added_at
       OR OLD.removed_at IS NOT NULL THEN
        RAISE EXCEPTION 'project participant identity and closed history are immutable'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER project_participants_preserve_history
BEFORE UPDATE OR DELETE ON commit.project_participants
FOR EACH ROW EXECUTE FUNCTION commit_private.prevent_participant_history_rewrite();

CREATE TABLE commit.project_diaries (
    organization_id uuid NOT NULL,
    project_id uuid NOT NULL,
    markdown text NOT NULL DEFAULT '',
    version bigint NOT NULL DEFAULT 1,
    updated_by_principal_id uuid NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    PRIMARY KEY (organization_id, project_id),
    CONSTRAINT project_diaries_project_fk
        FOREIGN KEY (organization_id, project_id)
        REFERENCES commit.projects (organization_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT project_diaries_updated_by_fk
        FOREIGN KEY (organization_id, updated_by_principal_id)
        REFERENCES commit.actor_projection (organization_id, principal_id)
        ON DELETE RESTRICT,
    CONSTRAINT project_diaries_positive_version CHECK (version > 0)
);

COMMENT ON TABLE commit.project_diaries IS
    'Current complete Markdown diary document. The application enforces the 100,000 Unicode-word limit.';
COMMENT ON COLUMN commit.project_diaries.version IS
    'Optimistic-concurrency token compared with If-Match and advanced by the database trigger.';

CREATE TRIGGER project_diaries_touch_version
BEFORE UPDATE ON commit.project_diaries
FOR EACH ROW EXECUTE FUNCTION commit_private.touch_versioned_row();

CREATE FUNCTION commit_private.preserve_project_diary_identity()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'a project diary must not be deleted'
            USING ERRCODE = '23514';
    END IF;

    IF NEW.organization_id <> OLD.organization_id
       OR NEW.project_id <> OLD.project_id THEN
        RAISE EXCEPTION 'project diary tenant and project are immutable'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER project_diaries_preserve_identity
BEFORE UPDATE OR DELETE ON commit.project_diaries
FOR EACH ROW EXECUTE FUNCTION commit_private.preserve_project_diary_identity();

CREATE FUNCTION commit_private.create_initial_project_diary()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    INSERT INTO commit.project_diaries (
        organization_id,
        project_id,
        markdown,
        version,
        updated_by_principal_id,
        updated_at
    )
    VALUES (
        NEW.organization_id,
        NEW.id,
        '',
        1,
        NEW.created_by_principal_id,
        NEW.created_at
    );
    RETURN NEW;
END;
$$;

COMMENT ON FUNCTION commit_private.create_initial_project_diary() IS
    'Guarantees that every project starts with exactly one empty diary at version 1.';

CREATE TRIGGER projects_create_initial_diary
AFTER INSERT ON commit.projects
FOR EACH ROW EXECUTE FUNCTION commit_private.create_initial_project_diary();

CREATE TABLE commit.project_tasks (
    id uuid PRIMARY KEY,
    organization_id uuid NOT NULL,
    project_id uuid NOT NULL,
    parent_task_id uuid,
    title text NOT NULL,
    description text NOT NULL DEFAULT '',
    status commit.todo_status NOT NULL DEFAULT 'yet_to_do',
    created_by_principal_id uuid NOT NULL,
    version bigint NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    UNIQUE (organization_id, id),
    UNIQUE (organization_id, project_id, id),
    CONSTRAINT project_tasks_project_fk
        FOREIGN KEY (organization_id, project_id)
        REFERENCES commit.projects (organization_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT project_tasks_parent_fk
        FOREIGN KEY (organization_id, project_id, parent_task_id)
        REFERENCES commit.project_tasks (organization_id, project_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT project_tasks_created_by_fk
        FOREIGN KEY (organization_id, created_by_principal_id)
        REFERENCES commit.actor_projection (organization_id, principal_id)
        ON DELETE RESTRICT,
    CONSTRAINT project_tasks_non_nil_id
        CHECK (id <> '00000000-0000-0000-0000-000000000000'::uuid),
    CONSTRAINT project_tasks_not_self_parent
        CHECK (parent_task_id IS NULL OR parent_task_id <> id),
    CONSTRAINT project_tasks_title_content CHECK (
        title = btrim(title)
        AND btrim(title) <> ''
        AND char_length(title) <= 500
    ),
    CONSTRAINT project_tasks_description_length
        CHECK (char_length(description) <= 100000),
    CONSTRAINT project_tasks_positive_version CHECK (version > 0),
    CONSTRAINT project_tasks_timestamp_order CHECK (updated_at >= created_at)
);

COMMENT ON TABLE commit.project_tasks IS
    'Project-centered tasks. The composite parent foreign key makes every subtask project-local.';

CREATE TRIGGER project_tasks_touch_version
BEFORE UPDATE ON commit.project_tasks
FOR EACH ROW EXECUTE FUNCTION commit_private.touch_versioned_row();

CREATE FUNCTION commit_private.prevent_project_task_reparenting()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.id <> OLD.id
       OR NEW.organization_id <> OLD.organization_id
       OR NEW.project_id <> OLD.project_id
       OR NEW.parent_task_id IS DISTINCT FROM OLD.parent_task_id
       OR NEW.created_by_principal_id <> OLD.created_by_principal_id
       OR NEW.created_at <> OLD.created_at THEN
        RAISE EXCEPTION 'project task identity, parent, creator, and creation time are immutable'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER project_tasks_prevent_reparenting
BEFORE UPDATE ON commit.project_tasks
FOR EACH ROW EXECUTE FUNCTION commit_private.prevent_project_task_reparenting();

CREATE INDEX project_tasks_project_created_idx
    ON commit.project_tasks (organization_id, project_id, created_at, id);
CREATE INDEX project_tasks_parent_created_idx
    ON commit.project_tasks (
        organization_id,
        project_id,
        parent_task_id,
        created_at,
        id
    );

CREATE TABLE commit.project_entries (
    id uuid PRIMARY KEY,
    organization_id uuid NOT NULL,
    project_id uuid NOT NULL,
    entry_type commit.project_entry_type NOT NULL,
    title text NOT NULL,
    description text NOT NULL,
    blocker_status commit.blocker_status,
    created_by_principal_id uuid NOT NULL,
    created_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    UNIQUE (organization_id, id),
    CONSTRAINT project_entries_project_fk
        FOREIGN KEY (organization_id, project_id)
        REFERENCES commit.projects (organization_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT project_entries_created_by_fk
        FOREIGN KEY (organization_id, created_by_principal_id)
        REFERENCES commit.actor_projection (organization_id, principal_id)
        ON DELETE RESTRICT,
    CONSTRAINT project_entries_non_nil_id
        CHECK (id <> '00000000-0000-0000-0000-000000000000'::uuid),
    CONSTRAINT project_entries_title_content CHECK (
        title = btrim(title)
        AND btrim(title) <> ''
        AND char_length(title) <= 500
    ),
    CONSTRAINT project_entries_description_length
        CHECK (char_length(description) <= 100000),
    CONSTRAINT project_entries_blocker_status_consistency CHECK (
        (entry_type = 'blocker' AND blocker_status IS NOT NULL)
        OR (entry_type <> 'blocker' AND blocker_status IS NULL)
    )
);

COMMENT ON TABLE commit.project_entries IS
    'Append-only blocker, milestone update, and completion statements.';

CREATE UNIQUE INDEX project_entries_one_completion_idx
    ON commit.project_entries (organization_id, project_id)
    WHERE entry_type = 'completion';

CREATE INDEX project_entries_project_created_idx
    ON commit.project_entries (
        organization_id,
        project_id,
        created_at DESC,
        id DESC
    );

CREATE INDEX project_entries_open_blockers_idx
    ON commit.project_entries (
        organization_id,
        project_id,
        created_at DESC,
        id DESC
    )
    WHERE entry_type = 'blocker' AND blocker_status = 'open';

-- Participant replacement is a multi-row operation, so this invariant must be
-- checked at transaction commit rather than after each individual row change.
CREATE FUNCTION commit_private.assert_project_participation()
RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    target_organization_id uuid;
    target_project_id uuid;
    creator_principal_id uuid;
    active_participant_count integer;
BEGIN
    IF TG_TABLE_NAME = 'projects' THEN
        target_organization_id := NEW.organization_id;
        target_project_id := NEW.id;
    ELSE
        target_organization_id := COALESCE(NEW.organization_id, OLD.organization_id);
        target_project_id := COALESCE(NEW.project_id, OLD.project_id);
    END IF;

    SELECT project.created_by_principal_id
    INTO creator_principal_id
    FROM commit.projects AS project
    WHERE project.organization_id = target_organization_id
      AND project.id = target_project_id;

    IF NOT FOUND THEN
        RETURN NULL;
    END IF;

    SELECT count(*)
    INTO active_participant_count
    FROM commit.project_participants AS participant
    WHERE participant.organization_id = target_organization_id
      AND participant.project_id = target_project_id
      AND participant.removed_at IS NULL;

    IF active_participant_count = 0 THEN
        RAISE EXCEPTION 'project % must retain at least one active Silicon participant',
            target_project_id
            USING ERRCODE = '23514';
    END IF;

    IF active_participant_count > 100 THEN
        RAISE EXCEPTION 'project % exceeds the 100 participant database limit',
            target_project_id
            USING ERRCODE = '23514';
    END IF;

    IF NOT EXISTS (
        SELECT 1
        FROM commit.project_participants AS participant
        WHERE participant.organization_id = target_organization_id
          AND participant.project_id = target_project_id
          AND participant.silicon_principal_id = creator_principal_id
    ) THEN
        RAISE EXCEPTION 'project creator must be retained in participant history'
            USING ERRCODE = '23514';
    END IF;

    RETURN NULL;
END;
$$;

CREATE CONSTRAINT TRIGGER projects_require_participation
AFTER INSERT OR UPDATE ON commit.projects
DEFERRABLE INITIALLY DEFERRED
FOR EACH ROW EXECUTE FUNCTION commit_private.assert_project_participation();

CREATE CONSTRAINT TRIGGER project_participants_require_participation
AFTER INSERT OR UPDATE OR DELETE ON commit.project_participants
DEFERRABLE INITIALLY DEFERRED
FOR EACH ROW EXECUTE FUNCTION commit_private.assert_project_participation();

-- A completion statement and the completed state must first appear in the same
-- transaction. A later explicit reopening remains possible, as required by v1.
CREATE FUNCTION commit_private.assert_project_completion()
RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    target_organization_id uuid;
    target_project_id uuid;
    current_status commit.project_status;
BEGIN
    target_organization_id := NEW.organization_id;
    IF TG_TABLE_NAME = 'projects' THEN
        target_project_id := NEW.id;
    ELSE
        target_project_id := NEW.project_id;
    END IF;

    SELECT project.status
    INTO current_status
    FROM commit.projects AS project
    WHERE project.organization_id = target_organization_id
      AND project.id = target_project_id;

    IF NOT FOUND THEN
        RETURN NULL;
    END IF;

    IF current_status = 'completed' AND NOT EXISTS (
        SELECT 1
        FROM commit.project_entries AS entry
        WHERE entry.organization_id = target_organization_id
          AND entry.project_id = target_project_id
          AND entry.entry_type = 'completion'
    ) THEN
        RAISE EXCEPTION 'a completed project requires a completion statement'
            USING ERRCODE = '23514';
    END IF;

    IF TG_TABLE_NAME = 'project_entries' THEN
        IF NEW.entry_type = 'completion' AND current_status <> 'completed' THEN
            RAISE EXCEPTION 'a completion statement must atomically complete its project'
                USING ERRCODE = '23514';
        END IF;
    END IF;

    RETURN NULL;
END;
$$;

CREATE CONSTRAINT TRIGGER projects_require_completion_statement
AFTER INSERT OR UPDATE ON commit.projects
DEFERRABLE INITIALLY DEFERRED
FOR EACH ROW EXECUTE FUNCTION commit_private.assert_project_completion();

CREATE CONSTRAINT TRIGGER project_entries_require_completed_project
AFTER INSERT ON commit.project_entries
DEFERRABLE INITIALLY DEFERRED
FOR EACH ROW EXECUTE FUNCTION commit_private.assert_project_completion();

CREATE FUNCTION commit_private.preserve_project_entries()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    RAISE EXCEPTION 'project entries are append-only'
        USING ERRCODE = '23514';
    RETURN OLD;
END;
$$;

CREATE TRIGGER project_entries_preserve_append_only
BEFORE UPDATE OR DELETE ON commit.project_entries
FOR EACH ROW EXECUTE FUNCTION commit_private.preserve_project_entries();
