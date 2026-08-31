-- Schema-owner security baseline.
--
-- Runtime roles are deployment resources: their names, login policy, and
-- membership differ per environment and must not be created or granted by an
-- application migration. The deployment grant template applies explicit API
-- and worker privileges after migrations finish.

REVOKE ALL ON SCHEMA commit FROM PUBLIC;
REVOKE ALL ON SCHEMA commit_private FROM PUBLIC;

REVOKE ALL ON ALL TABLES IN SCHEMA commit FROM PUBLIC;
REVOKE ALL ON ALL SEQUENCES IN SCHEMA commit FROM PUBLIC;
REVOKE ALL
    ON TYPE commit.actor_type,
            commit.todo_status,
            commit.project_status,
            commit.project_entry_type,
            commit.blocker_status,
            commit.todo_activity_type,
            commit.outbox_status
    FROM PUBLIC;
REVOKE ALL ON ALL FUNCTIONS IN SCHEMA commit FROM PUBLIC;
REVOKE ALL ON ALL FUNCTIONS IN SCHEMA commit_private FROM PUBLIC;

-- These defaults belong to the role executing migrations. Runtime privileges
-- remain opt-in per object; a new table cannot silently become writable by a
-- production process merely because a migration created it.
ALTER DEFAULT PRIVILEGES IN SCHEMA commit
    REVOKE ALL ON TABLES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES IN SCHEMA commit
    REVOKE ALL ON SEQUENCES FROM PUBLIC;
-- PostgreSQL's built-in PUBLIC defaults for types and routines are global;
-- schema-scoped revokes cannot override them. The migrator is a dedicated role,
-- so its future types and routines default closed in every namespace it owns.
ALTER DEFAULT PRIVILEGES
    REVOKE USAGE ON TYPES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES
    REVOKE EXECUTE ON FUNCTIONS FROM PUBLIC;
ALTER DEFAULT PRIVILEGES IN SCHEMA commit
    REVOKE USAGE ON TYPES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES IN SCHEMA commit
    REVOKE EXECUTE ON FUNCTIONS FROM PUBLIC;
ALTER DEFAULT PRIVILEGES IN SCHEMA commit_private
    REVOKE EXECUTE ON FUNCTIONS FROM PUBLIC;
