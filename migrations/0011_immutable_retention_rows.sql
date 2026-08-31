-- Keep append-only and delete-only records immutable while permitting workers
-- to take row locks through a narrowly scoped column privilege.

CREATE FUNCTION commit_private.reject_immutable_row_update()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    RAISE EXCEPTION USING
        ERRCODE = '55000',
        MESSAGE = pg_catalog.format(
            'rows in %I.%I are immutable',
            TG_TABLE_SCHEMA,
            TG_TABLE_NAME
        );
END;
$$;

REVOKE ALL ON FUNCTION commit_private.reject_immutable_row_update() FROM PUBLIC;

CREATE TRIGGER todo_attachments_reject_update
BEFORE UPDATE ON commit.todo_attachments
FOR EACH ROW
EXECUTE FUNCTION commit_private.reject_immutable_row_update();

CREATE TRIGGER todo_notes_reject_update
BEFORE UPDATE ON commit.todo_notes
FOR EACH ROW
EXECUTE FUNCTION commit_private.reject_immutable_row_update();

CREATE TRIGGER idempotency_records_reject_update
BEFORE UPDATE ON commit.idempotency_records
FOR EACH ROW
EXECUTE FUNCTION commit_private.reject_immutable_row_update();

CREATE TRIGGER audit_events_reject_update
BEFORE UPDATE ON commit.audit_events
FOR EACH ROW
EXECUTE FUNCTION commit_private.reject_immutable_row_update();

COMMENT ON FUNCTION commit_private.reject_immutable_row_update() IS
    'Rejects mutation of append-only/delete-only records while SELECT FOR UPDATE remains available to retention.';

CREATE FUNCTION commit_private.enforce_todo_activity_redaction()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.id IS DISTINCT FROM OLD.id
       OR NEW.organization_id IS DISTINCT FROM OLD.organization_id
       OR NEW.todo_id IS DISTINCT FROM OLD.todo_id
       OR NEW.activity_type IS DISTINCT FROM OLD.activity_type
       OR NEW.actor_principal_id IS DISTINCT FROM OLD.actor_principal_id
       OR NEW.request_id IS DISTINCT FROM OLD.request_id
       OR NEW.created_at IS DISTINCT FROM OLD.created_at
       OR NEW.retain_until IS DISTINCT FROM OLD.retain_until
       OR NEW.changes IS DISTINCT FROM '{}'::jsonb THEN
        RAISE EXCEPTION USING
            ERRCODE = '55000',
            MESSAGE = 'todo activity permits only canonical changes redaction';
    END IF;
    RETURN NEW;
END;
$$;

REVOKE ALL ON FUNCTION commit_private.enforce_todo_activity_redaction() FROM PUBLIC;

CREATE TRIGGER todo_activity_enforce_redaction
BEFORE UPDATE ON commit.todo_activity
FOR EACH ROW
EXECUTE FUNCTION commit_private.enforce_todo_activity_redaction();

COMMENT ON FUNCTION commit_private.enforce_todo_activity_redaction() IS
    'Allows only changes-to-empty-object redaction while preserving immutable activity metadata.';
