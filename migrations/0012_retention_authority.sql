-- Keep retention authority narrower than ordinary table mutation authority.

CREATE OR REPLACE FUNCTION commit_private.preserve_todo_tombstone()
RETURNS trigger
LANGUAGE plpgsql
AS $function$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'todos must be soft-deleted'
            USING ERRCODE = '23514';
    END IF;

    IF OLD.deleted_at IS NOT NULL THEN
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

        IF (OLD.title IS DISTINCT FROM '[deleted]' OR OLD.description IS NOT NULL)
            AND (
                OLD.content_retain_until > transaction_timestamp()
                OR EXISTS (
                    SELECT 1
                    FROM commit.idempotency_records AS replay
                    WHERE replay.organization_id = OLD.organization_id
                      AND replay.todo_id = OLD.id
                      AND replay.expires_at > transaction_timestamp()
                )
            )
        THEN
            RAISE EXCEPTION 'todo content retention deadline has not elapsed'
                USING ERRCODE = '55000';
        END IF;
    END IF;

    RETURN NEW;
END;
$function$;

REVOKE ALL ON FUNCTION commit_private.preserve_todo_tombstone() FROM PUBLIC;

CREATE OR REPLACE FUNCTION commit_private.enforce_todo_activity_redaction()
RETURNS trigger
LANGUAGE plpgsql
AS $function$
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

    IF OLD.changes <> '{}'::jsonb
       AND NOT EXISTS (
           SELECT 1
           FROM commit.todos AS todo
           WHERE todo.organization_id = OLD.organization_id
             AND todo.id = OLD.todo_id
             AND todo.deleted_at IS NOT NULL
             AND todo.content_retain_until <= transaction_timestamp()
             AND NOT EXISTS (
                 SELECT 1
                 FROM commit.idempotency_records AS replay
                 WHERE replay.organization_id = todo.organization_id
                   AND replay.todo_id = todo.id
                   AND replay.expires_at > transaction_timestamp()
             )
       )
    THEN
        RAISE EXCEPTION USING
            ERRCODE = '55000',
            MESSAGE = 'todo activity content retention deadline has not elapsed';
    END IF;

    RETURN NEW;
END;
$function$;

REVOKE ALL ON FUNCTION commit_private.enforce_todo_activity_redaction() FROM PUBLIC;

-- A terminal delivery result is retained evidence, not queue state that can be
-- retried or rewritten. Reject every UPDATE until the owner-defined retention
-- routine deletes the row after its stored deadline.
DROP TRIGGER outbox_events_touch_updated_at ON commit.outbox_events;

CREATE TRIGGER outbox_events_touch_updated_at
BEFORE UPDATE ON commit.outbox_events
FOR EACH ROW
WHEN (OLD.status NOT IN ('delivered', 'dead_letter'))
EXECUTE FUNCTION commit_private.touch_updated_at();

CREATE FUNCTION commit_private.preserve_terminal_outbox_event()
RETURNS trigger
LANGUAGE plpgsql
AS $function$
BEGIN
    IF OLD.status IN ('delivered', 'dead_letter') THEN
        RAISE EXCEPTION USING
            ERRCODE = '55000',
            MESSAGE = 'terminal outbox events are immutable';
    END IF;

    RETURN NEW;
END;
$function$;

REVOKE ALL ON FUNCTION commit_private.preserve_terminal_outbox_event() FROM PUBLIC;

CREATE TRIGGER outbox_events_preserve_terminal_state
BEFORE UPDATE ON commit.outbox_events
FOR EACH ROW
EXECUTE FUNCTION commit_private.preserve_terminal_outbox_event();

COMMENT ON FUNCTION commit_private.preserve_terminal_outbox_event() IS
    'Rejects every update of delivered/dead-letter outbox evidence before retention deletion.';

CREATE FUNCTION commit.run_retention_pass(p_batch_size integer)
RETURNS TABLE (
    notes_purged bigint,
    attachments_purged bigint,
    activity_changes_redacted bigint,
    activity_purged bigint,
    todos_redacted bigint,
    idempotency_purged bigint,
    audit_purged bigint,
    delivered_outbox_purged bigint,
    dead_letter_outbox_purged bigint
)
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $function$
BEGIN
    IF p_batch_size < 1 OR p_batch_size > 10000 THEN
        RAISE EXCEPTION 'retention batch size must be between 1 and 10000'
            USING ERRCODE = '22023';
    END IF;
    WITH candidates AS MATERIALIZED (
        SELECT note.organization_id, note.id
        FROM commit.todo_notes AS note
        JOIN commit.todos AS todo
          ON todo.organization_id = note.organization_id
         AND todo.id = note.todo_id
        WHERE todo.content_retain_until <= transaction_timestamp()
          AND NOT EXISTS (
              SELECT 1
              FROM commit.idempotency_records AS replay
              WHERE replay.organization_id = todo.organization_id
                AND replay.todo_id = todo.id
                AND replay.expires_at > transaction_timestamp()
          )
        ORDER BY todo.content_retain_until, note.organization_id, note.id
        LIMIT p_batch_size
        FOR UPDATE OF note SKIP LOCKED
    )
    DELETE FROM commit.todo_notes AS note
    USING candidates
    WHERE note.organization_id = candidates.organization_id
      AND note.id = candidates.id;
    GET DIAGNOSTICS notes_purged = ROW_COUNT;

    WITH candidates AS MATERIALIZED (
        SELECT attachment.organization_id,
               attachment.todo_id,
               attachment.position
        FROM commit.todo_attachments AS attachment
        JOIN commit.todos AS todo
          ON todo.organization_id = attachment.organization_id
         AND todo.id = attachment.todo_id
        WHERE todo.content_retain_until <= transaction_timestamp()
          AND NOT EXISTS (
              SELECT 1
              FROM commit.idempotency_records AS replay
              WHERE replay.organization_id = todo.organization_id
                AND replay.todo_id = todo.id
                AND replay.expires_at > transaction_timestamp()
          )
        ORDER BY todo.content_retain_until,
                 attachment.organization_id,
                 attachment.todo_id,
                 attachment.position
        LIMIT p_batch_size
        FOR UPDATE OF attachment SKIP LOCKED
    )
    DELETE FROM commit.todo_attachments AS attachment
    USING candidates
    WHERE attachment.organization_id = candidates.organization_id
      AND attachment.todo_id = candidates.todo_id
      AND attachment.position = candidates.position;
    GET DIAGNOSTICS attachments_purged = ROW_COUNT;

    WITH candidates AS MATERIALIZED (
        SELECT activity.organization_id, activity.id
        FROM commit.todo_activity AS activity
        WHERE activity.retain_until <= transaction_timestamp()
        ORDER BY activity.retain_until, activity.organization_id, activity.id
        LIMIT p_batch_size
        FOR UPDATE OF activity SKIP LOCKED
    )
    DELETE FROM commit.todo_activity AS activity
    USING candidates
    WHERE activity.organization_id = candidates.organization_id
      AND activity.id = candidates.id;
    GET DIAGNOSTICS activity_purged = ROW_COUNT;

    WITH candidates AS MATERIALIZED (
        SELECT activity.organization_id, activity.id
        FROM commit.todo_activity AS activity
        JOIN commit.todos AS todo
          ON todo.organization_id = activity.organization_id
         AND todo.id = activity.todo_id
        WHERE activity.changes <> '{}'::jsonb
          AND todo.content_retain_until <= transaction_timestamp()
          AND NOT EXISTS (
              SELECT 1
              FROM commit.idempotency_records AS replay
              WHERE replay.organization_id = todo.organization_id
                AND replay.todo_id = todo.id
                AND replay.expires_at > transaction_timestamp()
          )
        ORDER BY todo.content_retain_until,
                 activity.created_at,
                 activity.organization_id,
                 activity.id
        LIMIT p_batch_size
        FOR UPDATE OF activity SKIP LOCKED
    )
    UPDATE commit.todo_activity AS activity
    SET changes = '{}'::jsonb
    FROM candidates
    WHERE activity.organization_id = candidates.organization_id
      AND activity.id = candidates.id;
    GET DIAGNOSTICS activity_changes_redacted = ROW_COUNT;

    WITH candidates AS MATERIALIZED (
        SELECT todo.organization_id, todo.id
        FROM commit.todos AS todo
        WHERE todo.content_retain_until <= transaction_timestamp()
          AND (todo.title <> '[deleted]' OR todo.description IS NOT NULL)
          AND NOT EXISTS (
              SELECT 1
              FROM commit.idempotency_records AS replay
              WHERE replay.organization_id = todo.organization_id
                AND replay.todo_id = todo.id
                AND replay.expires_at > transaction_timestamp()
          )
        ORDER BY todo.content_retain_until, todo.organization_id, todo.id
        LIMIT p_batch_size
        FOR UPDATE OF todo SKIP LOCKED
    )
    UPDATE commit.todos AS todo
    SET title = '[deleted]', description = NULL
    FROM candidates
    WHERE todo.organization_id = candidates.organization_id
      AND todo.id = candidates.id;
    GET DIAGNOSTICS todos_redacted = ROW_COUNT;

    WITH candidates AS MATERIALIZED (
        SELECT record.id
        FROM commit.idempotency_records AS record
        WHERE record.expires_at <= transaction_timestamp()
        ORDER BY record.expires_at, record.id
        LIMIT p_batch_size
        FOR UPDATE OF record SKIP LOCKED
    )
    DELETE FROM commit.idempotency_records AS record
    USING candidates
    WHERE record.id = candidates.id;
    GET DIAGNOSTICS idempotency_purged = ROW_COUNT;

    WITH candidates AS MATERIALIZED (
        SELECT event.id
        FROM commit.audit_events AS event
        WHERE event.retain_until <= transaction_timestamp()
        ORDER BY event.retain_until, event.id
        LIMIT p_batch_size
        FOR UPDATE OF event SKIP LOCKED
    )
    DELETE FROM commit.audit_events AS event
    USING candidates
    WHERE event.id = candidates.id;
    GET DIAGNOSTICS audit_purged = ROW_COUNT;

    WITH candidates AS MATERIALIZED (
        SELECT event.id
        FROM commit.outbox_events AS event
        WHERE event.status = 'delivered'
          AND event.purge_after <= transaction_timestamp()
        ORDER BY event.purge_after, event.id
        LIMIT p_batch_size
        FOR UPDATE OF event SKIP LOCKED
    )
    DELETE FROM commit.outbox_events AS event
    USING candidates
    WHERE event.id = candidates.id;
    GET DIAGNOSTICS delivered_outbox_purged = ROW_COUNT;

    WITH candidates AS MATERIALIZED (
        SELECT event.id
        FROM commit.outbox_events AS event
        WHERE event.status = 'dead_letter'
          AND event.purge_after <= transaction_timestamp()
        ORDER BY event.purge_after, event.id
        LIMIT p_batch_size
        FOR UPDATE OF event SKIP LOCKED
    )
    DELETE FROM commit.outbox_events AS event
    USING candidates
    WHERE event.id = candidates.id;
    GET DIAGNOSTICS dead_letter_outbox_purged = ROW_COUNT;

    RETURN NEXT;
END;
$function$;

REVOKE ALL
    ON FUNCTION commit.run_retention_pass(integer)
    FROM PUBLIC;

COMMENT ON FUNCTION commit.run_retention_pass(integer) IS
    'Owner-defined bounded retention capability; runtime workers receive EXECUTE without direct content DML.';
