-- Silicon Commit foundational schema and retained IAM identity projections.
--
-- Commit verifies identity and membership online with IAM. These projections are
-- deliberately not an authorization cache: they retain stable internal IDs and
-- immutable public handles so domain ownership and historical attribution remain
-- meaningful after the request that created them.

CREATE SCHEMA commit;
CREATE SCHEMA commit_private;

CREATE TYPE commit.actor_type AS ENUM ('carbon', 'silicon');

CREATE TYPE commit.todo_status AS ENUM (
    'completed',
    'canceled',
    'in_progress',
    'blocked',
    'yet_to_do'
);

CREATE TYPE commit.project_status AS ENUM (
    'completed',
    'blocked',
    'canceled',
    'in_progress',
    'yet_to_start'
);

CREATE TYPE commit.project_entry_type AS ENUM ('blocker', 'update', 'completion');
CREATE TYPE commit.blocker_status AS ENUM ('open', 'resolved');

CREATE TYPE commit.todo_activity_type AS ENUM (
    'created',
    'updated',
    'status_changed',
    'reassigned',
    'note_added',
    'deleted'
);

CREATE TYPE commit.outbox_status AS ENUM (
    'pending',
    'in_flight',
    'delivered',
    'dead_letter'
);

CREATE FUNCTION commit_private.touch_versioned_row()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    NEW.version := OLD.version + 1;
    NEW.updated_at := transaction_timestamp();
    RETURN NEW;
END;
$$;

COMMENT ON FUNCTION commit_private.touch_versioned_row() IS
    'Advances an aggregate version exactly once for every persisted row update.';

CREATE FUNCTION commit_private.touch_updated_at()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    NEW.updated_at := transaction_timestamp();
    RETURN NEW;
END;
$$;

COMMENT ON FUNCTION commit_private.touch_updated_at() IS
    'Sets updated_at from the database transaction clock on every row update.';

CREATE FUNCTION commit_private.prevent_organization_projection_identity_change()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.organization_id <> OLD.organization_id
       OR NEW.org_id <> OLD.org_id
       OR NEW.first_seen_at <> OLD.first_seen_at THEN
        RAISE EXCEPTION 'organization projection identity is immutable'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;

CREATE FUNCTION commit_private.prevent_actor_projection_identity_change()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.organization_id <> OLD.organization_id
       OR NEW.principal_id <> OLD.principal_id
       OR NEW.membership_id <> OLD.membership_id
       OR NEW.actor_type <> OLD.actor_type
       OR NEW.actor_id <> OLD.actor_id
       OR NEW.first_seen_at <> OLD.first_seen_at THEN
        RAISE EXCEPTION 'actor projection identity is immutable'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TABLE commit.organization_projection (
    organization_id uuid PRIMARY KEY,
    org_id text NOT NULL,
    first_seen_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    UNIQUE (organization_id, org_id),
    UNIQUE (org_id),
    CONSTRAINT organization_projection_non_nil_id
        CHECK (organization_id <> '00000000-0000-0000-0000-000000000000'::uuid),
    CONSTRAINT organization_projection_org_id_format CHECK (
        org_id = btrim(org_id)
        AND octet_length(org_id) BETWEEN 1 AND 255
    )
);

COMMENT ON TABLE commit.organization_projection IS
    'Retained IAM organization identity mapping; membership authorization is always checked online.';
COMMENT ON COLUMN commit.organization_projection.organization_id IS
    'Stable IAM internal organization UUID used as the tenant key in every domain relationship.';
COMMENT ON COLUMN commit.organization_projection.org_id IS
    'Immutable public organization handle supplied through X-Org-ID.';

CREATE TRIGGER organization_projection_preserve_identity
BEFORE UPDATE ON commit.organization_projection
FOR EACH ROW EXECUTE FUNCTION commit_private.prevent_organization_projection_identity_change();

CREATE TABLE commit.actor_projection (
    organization_id uuid NOT NULL,
    principal_id uuid NOT NULL,
    membership_id uuid NOT NULL,
    actor_type commit.actor_type NOT NULL,
    actor_id text NOT NULL,
    first_seen_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    PRIMARY KEY (organization_id, principal_id),
    UNIQUE (organization_id, membership_id),
    UNIQUE (organization_id, actor_type, actor_id),
    UNIQUE (organization_id, principal_id, actor_type),
    CONSTRAINT actor_projection_organization_fk
        FOREIGN KEY (organization_id)
        REFERENCES commit.organization_projection (organization_id)
        ON DELETE RESTRICT,
    CONSTRAINT actor_projection_non_nil_principal_id
        CHECK (principal_id <> '00000000-0000-0000-0000-000000000000'::uuid),
    CONSTRAINT actor_projection_non_nil_membership_id
        CHECK (membership_id <> '00000000-0000-0000-0000-000000000000'::uuid),
    CONSTRAINT actor_projection_actor_id_format CHECK (
        actor_id = btrim(actor_id)
        AND octet_length(actor_id) BETWEEN 1 AND 255
    )
);

COMMENT ON TABLE commit.actor_projection IS
    'Retained IAM principal, membership, type, and public-handle mapping; never used instead of online IAM authorization.';
COMMENT ON COLUMN commit.actor_projection.membership_id IS
    'Stable IAM membership UUID retained for traceability; active status is not inferred from this projection.';

CREATE TRIGGER actor_projection_preserve_identity
BEFORE UPDATE ON commit.actor_projection
FOR EACH ROW EXECUTE FUNCTION commit_private.prevent_actor_projection_identity_change();

CREATE INDEX actor_projection_public_lookup_idx
    ON commit.actor_projection (organization_id, actor_id, actor_type);
