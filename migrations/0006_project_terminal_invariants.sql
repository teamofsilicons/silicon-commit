-- Strengthen creator participation and completion into permanent invariants.

ALTER TABLE commit.projects
    DROP CONSTRAINT projects_uid_format,
    ADD CONSTRAINT projects_uid_format CHECK (
        uid = btrim(uid)
        AND octet_length(uid) BETWEEN 3 AND 2048
    );

-- Idempotency scopes embed exact project locators, so their storage must fit
-- the enlarged UID plus the operation's fixed path prefix.
ALTER TABLE commit.idempotency_records
    DROP CONSTRAINT idempotency_records_resource_path_format,
    ADD CONSTRAINT idempotency_records_resource_path_format CHECK (
        resource_path = btrim(resource_path)
        AND octet_length(resource_path) BETWEEN 1 AND 4096
        AND left(resource_path, 1) = '/'
    );

DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM commit.projects AS project
        WHERE NOT EXISTS (
            SELECT 1
            FROM commit.project_participants AS participant
            WHERE participant.organization_id = project.organization_id
              AND participant.project_id = project.id
              AND participant.silicon_principal_id = project.created_by_principal_id
              AND participant.removed_at IS NULL
        )
    ) THEN
        RAISE EXCEPTION 'cannot enforce active project creators while legacy violations exist'
            USING ERRCODE = '23514';
    END IF;

    IF EXISTS (
        SELECT 1
        FROM commit.projects AS project
        JOIN commit.project_entries AS entry
          ON entry.organization_id = project.organization_id
         AND entry.project_id = project.id
         AND entry.entry_type = 'completion'
        WHERE project.status <> 'completed'
    ) THEN
        RAISE EXCEPTION 'cannot enforce terminal completion while reopened legacy projects exist'
            USING ERRCODE = '23514';
    END IF;
END;
$$;

CREATE OR REPLACE FUNCTION commit_private.assert_project_participation()
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
          AND participant.removed_at IS NULL
    ) THEN
        RAISE EXCEPTION 'project creator must remain an active participant'
            USING ERRCODE = '23514';
    END IF;

    RETURN NULL;
END;
$$;

CREATE FUNCTION commit_private.prevent_completed_project_reopening()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF OLD.status = 'completed' AND NEW.status <> 'completed' THEN
        RAISE EXCEPTION 'completed project status is terminal'
            USING ERRCODE = '23514';
    END IF;

    RETURN NEW;
END;
$$;

CREATE TRIGGER projects_keep_completed_status_terminal
BEFORE UPDATE ON commit.projects
FOR EACH ROW EXECUTE FUNCTION commit_private.prevent_completed_project_reopening();

REVOKE ALL
    ON FUNCTION commit_private.prevent_completed_project_reopening()
    FROM PUBLIC;

COMMENT ON FUNCTION commit_private.prevent_completed_project_reopening() IS
    'Rejects every completed-to-non-completed project status transition.';
