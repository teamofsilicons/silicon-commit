-- Prevent transaction-clock regression after row-lock waits.
--
-- transaction_timestamp() is fixed when a transaction starts. An older
-- transaction can wait behind, then overwrite, a row committed by a newer
-- transaction. Comparing the wall clock at trigger execution with the locked
-- row records the post-wait mutation while version counters still advance once.

CREATE OR REPLACE FUNCTION commit_private.touch_versioned_row()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    NEW.version := OLD.version + 1;
    NEW.updated_at := GREATEST(OLD.updated_at, clock_timestamp());
    RETURN NEW;
END;
$$;

CREATE OR REPLACE FUNCTION commit_private.touch_updated_at()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    NEW.updated_at := GREATEST(OLD.updated_at, clock_timestamp());
    RETURN NEW;
END;
$$;

CREATE OR REPLACE FUNCTION commit_private.touch_todo_version()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF OLD.deleted_at IS NOT NULL THEN
        NEW.version := OLD.version;
        NEW.updated_at := OLD.updated_at;
    ELSE
        NEW.version := OLD.version + 1;
        NEW.updated_at := GREATEST(
            OLD.updated_at,
            clock_timestamp(),
            COALESCE(NEW.deleted_at, '-infinity'::timestamptz)
        );
    END IF;
    RETURN NEW;
END;
$$;

REVOKE ALL
    ON FUNCTION commit_private.touch_versioned_row(),
                commit_private.touch_updated_at(),
                commit_private.touch_todo_version()
    FROM PUBLIC;

COMMENT ON FUNCTION commit_private.touch_versioned_row() IS
    'Advances aggregate version and preserves monotonic updated_at across row-lock waits.';
COMMENT ON FUNCTION commit_private.touch_updated_at() IS
    'Preserves monotonic updated_at across row-lock waits.';
COMMENT ON FUNCTION commit_private.touch_todo_version() IS
    'Versions active todo mutations monotonically while preserving retention-only tombstone metadata.';
