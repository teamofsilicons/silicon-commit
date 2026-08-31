-- Organization-visible personal and delegated work.

CREATE TABLE commit.todos (
    id uuid PRIMARY KEY,
    organization_id uuid NOT NULL,
    title text NOT NULL,
    description text,
    assigned_by_principal_id uuid NOT NULL,
    assigned_to_principal_id uuid NOT NULL,
    status commit.todo_status NOT NULL DEFAULT 'yet_to_do',
    version bigint NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    deleted_at timestamptz,
    content_retain_until timestamptz,
    deleted_by_principal_id uuid,
    UNIQUE (organization_id, id),
    CONSTRAINT todos_organization_fk
        FOREIGN KEY (organization_id)
        REFERENCES commit.organization_projection (organization_id)
        ON DELETE RESTRICT,
    CONSTRAINT todos_assigned_by_actor_fk
        FOREIGN KEY (organization_id, assigned_by_principal_id)
        REFERENCES commit.actor_projection (organization_id, principal_id)
        ON DELETE RESTRICT,
    CONSTRAINT todos_assigned_to_actor_fk
        FOREIGN KEY (organization_id, assigned_to_principal_id)
        REFERENCES commit.actor_projection (organization_id, principal_id)
        ON DELETE RESTRICT,
    CONSTRAINT todos_deleted_by_actor_fk
        FOREIGN KEY (organization_id, deleted_by_principal_id)
        REFERENCES commit.actor_projection (organization_id, principal_id)
        ON DELETE RESTRICT,
    CONSTRAINT todos_non_nil_id
        CHECK (id <> '00000000-0000-0000-0000-000000000000'::uuid),
    CONSTRAINT todos_title_content CHECK (
        title = btrim(title)
        AND btrim(title) <> ''
        AND char_length(title) <= 500
    ),
    CONSTRAINT todos_description_length
        CHECK (description IS NULL OR char_length(description) <= 100000),
    CONSTRAINT todos_positive_version CHECK (version > 0),
    CONSTRAINT todos_timestamp_order CHECK (
        updated_at >= created_at
        AND (deleted_at IS NULL OR deleted_at >= created_at)
    ),
    CONSTRAINT todos_deletion_actor_consistency CHECK (
        (
            deleted_at IS NULL
            AND content_retain_until IS NULL
            AND deleted_by_principal_id IS NULL
        )
        OR (
            deleted_at IS NOT NULL
            AND content_retain_until IS NOT NULL
            AND content_retain_until > deleted_at
            AND deleted_by_principal_id IS NOT NULL
        )
    )
);

COMMENT ON TABLE commit.todos IS
    'Organization-visible todos. DELETE is represented by deleted_at so operational history remains intact.';
COMMENT ON COLUMN commit.todos.assigned_by_principal_id IS
    'Derived from the IAM-authenticated or OBO-represented actor; never accepted from the request body.';
COMMENT ON COLUMN commit.todos.version IS
    'Internal monotonic aggregate version used by audit and notification producers.';
COMMENT ON COLUMN commit.todos.content_retain_until IS
    'Immutable deletion-time deadline before user-authored todo content may be redacted.';

CREATE TRIGGER todos_touch_version
BEFORE UPDATE ON commit.todos
FOR EACH ROW EXECUTE FUNCTION commit_private.touch_versioned_row();

CREATE FUNCTION commit_private.preserve_todo_tombstone()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'todos must be soft-deleted'
            USING ERRCODE = '23514';
    END IF;

    IF OLD.deleted_at IS NOT NULL THEN
        -- D-025 permits the retention worker to erase user-authored content,
        -- but a tombstone's identity, assignment, status, and deletion history
        -- remain immutable. This exact terminal representation also prevents a
        -- caller from replacing deleted content with arbitrary text.
        IF NEW.id IS DISTINCT FROM OLD.id
            OR NEW.organization_id IS DISTINCT FROM OLD.organization_id
            OR NEW.assigned_by_principal_id IS DISTINCT FROM OLD.assigned_by_principal_id
            OR NEW.assigned_to_principal_id IS DISTINCT FROM OLD.assigned_to_principal_id
            OR NEW.status IS DISTINCT FROM OLD.status
            OR NEW.version IS DISTINCT FROM OLD.version
            OR NEW.created_at IS DISTINCT FROM OLD.created_at
            OR NEW.updated_at IS DISTINCT FROM OLD.updated_at
            OR NEW.deleted_at IS DISTINCT FROM OLD.deleted_at
            OR NEW.content_retain_until IS DISTINCT FROM OLD.content_retain_until
            OR NEW.deleted_by_principal_id IS DISTINCT FROM OLD.deleted_by_principal_id
            OR NEW.title IS DISTINCT FROM '[deleted]'
            OR NEW.description IS NOT NULL
        THEN
            RAISE EXCEPTION 'deleted todo tombstones only permit retention redaction'
                USING ERRCODE = '23514';
        END IF;
    END IF;

    RETURN NEW;
END;
$$;

CREATE TRIGGER todos_preserve_tombstones
BEFORE UPDATE OR DELETE ON commit.todos
FOR EACH ROW EXECUTE FUNCTION commit_private.preserve_todo_tombstone();

-- Supports the unfiltered organization list and its stable keyset cursor.
CREATE INDEX todos_org_created_idx
    ON commit.todos (organization_id, created_at DESC, id DESC)
    WHERE deleted_at IS NULL;

CREATE INDEX todos_org_status_created_idx
    ON commit.todos (organization_id, status, created_at DESC, id DESC)
    WHERE deleted_at IS NULL;

CREATE INDEX todos_assigned_to_created_idx
    ON commit.todos (
        organization_id,
        assigned_to_principal_id,
        created_at DESC,
        id DESC
    )
    WHERE deleted_at IS NULL;

CREATE INDEX todos_assigned_to_status_created_idx
    ON commit.todos (
        organization_id,
        assigned_to_principal_id,
        status,
        created_at DESC,
        id DESC
    )
    WHERE deleted_at IS NULL;

CREATE INDEX todos_assigned_by_created_idx
    ON commit.todos (
        organization_id,
        assigned_by_principal_id,
        created_at DESC,
        id DESC
    )
    WHERE deleted_at IS NULL
      AND assigned_by_principal_id <> assigned_to_principal_id;

CREATE INDEX todos_assigned_by_status_created_idx
    ON commit.todos (
        organization_id,
        assigned_by_principal_id,
        status,
        created_at DESC,
        id DESC
    )
    WHERE deleted_at IS NULL
      AND assigned_by_principal_id <> assigned_to_principal_id;

CREATE TABLE commit.todo_attachments (
    organization_id uuid NOT NULL,
    todo_id uuid NOT NULL,
    position smallint NOT NULL,
    permanent_url text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    PRIMARY KEY (organization_id, todo_id, position),
    UNIQUE (organization_id, todo_id, permanent_url),
    CONSTRAINT todo_attachments_todo_fk
        FOREIGN KEY (organization_id, todo_id)
        REFERENCES commit.todos (organization_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT todo_attachments_position_range CHECK (position BETWEEN 0 AND 99),
    CONSTRAINT todo_attachments_permanent_https_url CHECK (
        permanent_url = btrim(permanent_url)
        AND char_length(permanent_url) BETWEEN 9 AND 2048
        AND permanent_url ~ '^https://'
    )
);

COMMENT ON TABLE commit.todo_attachments IS
    'Ordered, unique canonical permanent Briefcase URLs; temporary CDN URLs are never persisted.';
COMMENT ON COLUMN commit.todo_attachments.position IS
    'Zero-based response order. The database permits at most 100 attachments; runtime policy may be stricter.';

CREATE TABLE commit.todo_notes (
    id uuid PRIMARY KEY,
    organization_id uuid NOT NULL,
    todo_id uuid NOT NULL,
    author_principal_id uuid NOT NULL,
    body text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    UNIQUE (organization_id, id),
    CONSTRAINT todo_notes_todo_fk
        FOREIGN KEY (organization_id, todo_id)
        REFERENCES commit.todos (organization_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT todo_notes_author_fk
        FOREIGN KEY (organization_id, author_principal_id)
        REFERENCES commit.actor_projection (organization_id, principal_id)
        ON DELETE RESTRICT,
    CONSTRAINT todo_notes_non_nil_id
        CHECK (id <> '00000000-0000-0000-0000-000000000000'::uuid),
    CONSTRAINT todo_notes_body_content CHECK (
        btrim(body) <> ''
        AND char_length(body) <= 100000
    )
);

COMMENT ON TABLE commit.todo_notes IS
    'Append-only notes whose visibility and lifecycle follow their organization-qualified todo.';

CREATE INDEX todo_notes_todo_created_idx
    ON commit.todo_notes (organization_id, todo_id, created_at, id);

CREATE TABLE commit.todo_activity (
    id uuid PRIMARY KEY,
    organization_id uuid NOT NULL,
    todo_id uuid NOT NULL,
    activity_type commit.todo_activity_type NOT NULL,
    actor_principal_id uuid NOT NULL,
    request_id text NOT NULL,
    changes jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    retain_until timestamptz NOT NULL DEFAULT (
        transaction_timestamp() + interval '2555 days'
    ),
    UNIQUE (organization_id, id),
    CONSTRAINT todo_activity_todo_fk
        FOREIGN KEY (organization_id, todo_id)
        REFERENCES commit.todos (organization_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT todo_activity_actor_fk
        FOREIGN KEY (organization_id, actor_principal_id)
        REFERENCES commit.actor_projection (organization_id, principal_id)
        ON DELETE RESTRICT,
    CONSTRAINT todo_activity_non_nil_id
        CHECK (id <> '00000000-0000-0000-0000-000000000000'::uuid),
    CONSTRAINT todo_activity_request_id_format CHECK (
        request_id = btrim(request_id)
        AND octet_length(request_id) BETWEEN 1 AND 255
    ),
    CONSTRAINT todo_activity_changes_object
        CHECK (jsonb_typeof(changes) = 'object'),
    CONSTRAINT todo_activity_retention_order
        CHECK (retain_until > created_at)
);

COMMENT ON TABLE commit.todo_activity IS
    'Internal append-only todo history. It is intentionally not exposed by the v1 HTTP contract.';
COMMENT ON COLUMN commit.todo_activity.changes IS
    'Minimal non-secret change metadata; request bodies and credentials must never be copied here.';
COMMENT ON COLUMN commit.todo_activity.retain_until IS
    'Immutable-by-policy audit deadline selected when activity is appended.';

CREATE INDEX todo_activity_todo_created_idx
    ON commit.todo_activity (organization_id, todo_id, created_at DESC, id DESC);
