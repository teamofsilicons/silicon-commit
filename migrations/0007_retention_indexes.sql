-- Deterministic bounded-retention scans used by the maintenance worker.

-- A soft-delete is a domain mutation and advances the aggregate version. The
-- later retention-only redaction is not: it must leave tombstone metadata
-- byte-for-byte stable while the tombstone guard permits only the canonical
-- title/description erasure.
DROP TRIGGER todos_touch_version ON commit.todos;
CREATE TRIGGER todos_touch_version
BEFORE UPDATE ON commit.todos
FOR EACH ROW
WHEN (OLD.deleted_at IS NULL)
EXECUTE FUNCTION commit_private.touch_versioned_row();

CREATE INDEX todos_deleted_retention_idx
    ON commit.todos (content_retain_until, organization_id, id)
    WHERE deleted_at IS NOT NULL;

CREATE INDEX todo_activity_retention_idx
    ON commit.todo_activity (retain_until, organization_id, id);

CREATE INDEX outbox_events_delivered_retention_idx
    ON commit.outbox_events (purge_after, id)
    WHERE status IN ('delivered', 'dead_letter');

COMMENT ON INDEX commit.todos_deleted_retention_idx IS
    'Supports bounded tombstone content redaction by its frozen deadline.';
COMMENT ON INDEX commit.todo_activity_retention_idx IS
    'Supports bounded deletion of activity metadata by its frozen audit deadline.';
COMMENT ON INDEX commit.outbox_events_delivered_retention_idx IS
    'Supports bounded deletion of terminal notifications by their frozen deadline.';
