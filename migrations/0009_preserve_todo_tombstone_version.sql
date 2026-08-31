-- Content redaction is retention maintenance, not a new aggregate mutation.
-- Preserve the terminal todo version and public update timestamp once a todo
-- has been deleted while retaining ordinary versioning for the delete itself.

CREATE FUNCTION commit_private.touch_todo_version()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF OLD.deleted_at IS NOT NULL THEN
        NEW.version := OLD.version;
        NEW.updated_at := OLD.updated_at;
    ELSE
        NEW.version := OLD.version + 1;
        NEW.updated_at := transaction_timestamp();
    END IF;
    RETURN NEW;
END;
$$;

DROP TRIGGER todos_touch_version ON commit.todos;

CREATE TRIGGER todos_touch_version
BEFORE UPDATE ON commit.todos
FOR EACH ROW EXECUTE FUNCTION commit_private.touch_todo_version();

REVOKE ALL
    ON FUNCTION commit_private.touch_todo_version()
    FROM PUBLIC;

COMMENT ON FUNCTION commit_private.touch_todo_version() IS
    'Versions active todo mutations while preserving a deleted tombstone version during retention redaction.';
