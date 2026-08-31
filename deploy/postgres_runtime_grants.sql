\set ON_ERROR_STOP on

-- Run with psql after every migration rollout as a deployment database
-- administrator that controls the database and may alter the schema owner's
-- default privileges:
--
-- psql "$COMMIT_MIGRATOR_DATABASE_URL" \
--   --set=database_name=silicon_commit \
--   --set=schema_owner=silicon_commit_migrator \
--   --set=api_role=silicon_commit_api_production \
--   --set=worker_role=silicon_commit_worker_production \
--   --file=deploy/postgres_runtime_grants.sql
--
-- The named roles must already exist. This application-owned script never
-- creates, alters, or assigns membership to cluster roles.

\if :{?database_name}
\else
  \echo 'database_name is required'
  \quit 3
\endif
\if :{?schema_owner}
\else
  \echo 'schema_owner is required'
  \quit 3
\endif
\if :{?api_role}
\else
  \echo 'api_role is required'
  \quit 3
\endif
\if :{?worker_role}
\else
  \echo 'worker_role is required'
  \quit 3
\endif

SELECT current_database() = :'database_name'
       AND :'schema_owner' <> :'api_role'
       AND :'schema_owner' <> :'worker_role'
       AND :'api_role' <> :'worker_role'
       AND (
           SELECT count(*) = 3
           FROM pg_catalog.pg_roles
           WHERE rolname IN (:'schema_owner', :'api_role', :'worker_role')
       )
       AND (
           SELECT count(*) = 2
           FROM pg_catalog.pg_roles AS runtime_role
           WHERE runtime_role.rolname IN (:'api_role', :'worker_role')
             AND NOT runtime_role.rolsuper
             AND NOT runtime_role.rolcreatedb
             AND NOT runtime_role.rolcreaterole
             AND NOT runtime_role.rolreplication
             AND NOT runtime_role.rolbypassrls
             AND runtime_role.oid <> (
                 SELECT database.datdba
                 FROM pg_catalog.pg_database AS database
                 WHERE database.datname = current_database()
             )
             AND NOT EXISTS (
                 SELECT 1
                 FROM pg_catalog.pg_auth_members AS membership
                 WHERE membership.member = runtime_role.oid
             )
       )
       AND (
           SELECT count(*) = 2
           FROM pg_catalog.pg_namespace AS namespace
           JOIN pg_catalog.pg_roles AS owner
             ON owner.oid = namespace.nspowner
           WHERE namespace.nspname IN ('commit', 'commit_private')
             AND owner.rolname = :'schema_owner'
       )
       AND NOT EXISTS (
           SELECT 1
           FROM pg_catalog.pg_class AS relation
           JOIN pg_catalog.pg_namespace AS namespace
             ON namespace.oid = relation.relnamespace
           WHERE (
               namespace.nspname IN ('commit', 'commit_private')
               OR (
                   namespace.nspname = 'public'
                   AND relation.relname = '_sqlx_migrations'
               )
           )
             AND pg_catalog.pg_get_userbyid(relation.relowner) <> :'schema_owner'
       )
       AND NOT EXISTS (
           SELECT 1
           FROM pg_catalog.pg_type AS app_type
           JOIN pg_catalog.pg_namespace AS namespace
             ON namespace.oid = app_type.typnamespace
           WHERE namespace.nspname IN ('commit', 'commit_private')
             AND app_type.typisdefined
             AND pg_catalog.pg_get_userbyid(app_type.typowner) <> :'schema_owner'
       )
       AND NOT EXISTS (
           SELECT 1
           FROM pg_catalog.pg_proc AS routine
           JOIN pg_catalog.pg_namespace AS namespace
             ON namespace.oid = routine.pronamespace
           WHERE namespace.nspname IN ('commit', 'commit_private')
             AND pg_catalog.pg_get_userbyid(routine.proowner) <> :'schema_owner'
       ) AS grant_preconditions_met
\gset

\if :grant_preconditions_met
\else
  \echo 'grant preconditions failed: verify database, complete application-object ownership, and dedicated unprivileged runtime roles'
  \quit 3
\endif

BEGIN;

-- A dedicated Commit database does not rely on PostgreSQL's implicit PUBLIC
-- CONNECT/TEMPORARY grants. The migrator retains CONNECT for future releases.
REVOKE CONNECT, TEMPORARY ON DATABASE :"database_name" FROM PUBLIC;
GRANT CONNECT ON DATABASE :"database_name"
    TO :"schema_owner", :"api_role", :"worker_role";

-- SQLx keeps the migration ledger in public. Only the deployment-owned schema
-- owner needs that namespace; runtime pools use commit,pg_catalog exclusively.
-- This assumes Commit owns a dedicated database, as required by deployment.
REVOKE ALL ON SCHEMA public FROM PUBLIC;
REVOKE ALL ON SCHEMA public FROM :"api_role", :"worker_role";
GRANT USAGE, CREATE ON SCHEMA public TO :"schema_owner";

REVOKE ALL ON SCHEMA commit FROM :"api_role", :"worker_role";
REVOKE ALL ON SCHEMA commit_private FROM :"api_role", :"worker_role";
GRANT USAGE ON SCHEMA commit TO :"api_role", :"worker_role";

REVOKE ALL ON ALL TABLES IN SCHEMA commit FROM :"api_role", :"worker_role";
REVOKE ALL ON ALL SEQUENCES IN SCHEMA commit FROM :"api_role", :"worker_role";
REVOKE ALL
    ON TYPE commit.actor_type,
            commit.todo_status,
            commit.project_status,
            commit.project_entry_type,
            commit.blocker_status,
            commit.todo_activity_type,
            commit.outbox_status
    FROM :"api_role", :"worker_role";
REVOKE ALL ON ALL FUNCTIONS IN SCHEMA commit FROM :"api_role", :"worker_role";
REVOKE ALL ON ALL FUNCTIONS IN SCHEMA commit_private FROM :"api_role", :"worker_role";

-- API: online identity projections and transactional product mutations.
GRANT SELECT, INSERT
    ON TABLE commit.organization_projection,
             commit.actor_projection
    TO :"api_role";
GRANT SELECT, INSERT, UPDATE
    ON TABLE commit.todos,
             commit.projects,
             commit.project_diaries,
             commit.project_tasks
    TO :"api_role";
GRANT SELECT, INSERT, DELETE
    ON TABLE commit.todo_attachments
    TO :"api_role";
GRANT SELECT, INSERT
    ON TABLE commit.todo_notes,
             commit.project_entries
    TO :"api_role";
GRANT SELECT, INSERT, UPDATE
    ON TABLE commit.project_participants
    TO :"api_role";
GRANT INSERT
    ON TABLE commit.todo_activity,
             commit.audit_events,
             commit.outbox_events
    TO :"api_role";
GRANT SELECT, INSERT, DELETE
    ON TABLE commit.idempotency_records
    TO :"api_role";

-- Worker: Hook queue state plus one owner-defined bounded retention
-- capability; no direct content deletion/redaction, project writes, identity
-- mutation, audit insertion, or new product-row creation.
GRANT SELECT
    ON TABLE commit.organization_projection,
             commit.actor_projection
    TO :"worker_role";
GRANT SELECT
    ON TABLE commit.outbox_events
    TO :"worker_role";
GRANT UPDATE (
        status,
        attempt_count,
        available_at,
        lease_owner,
        lease_expires_at,
        last_error_code,
        delivered_at,
        dead_lettered_at,
        purge_after
    )
    ON TABLE commit.outbox_events
    TO :"worker_role";
GRANT EXECUTE
    ON FUNCTION commit.run_retention_pass(integer)
    TO :"worker_role";

-- Future objects remain inaccessible until this explicit table map is updated
-- and the template is rerun. Default privileges prevent PUBLIC or stale role
-- grants from silently widening a later migration.
ALTER DEFAULT PRIVILEGES FOR ROLE :"schema_owner" IN SCHEMA commit
    REVOKE ALL ON TABLES FROM PUBLIC, :"api_role", :"worker_role";
ALTER DEFAULT PRIVILEGES FOR ROLE :"schema_owner" IN SCHEMA commit
    REVOKE ALL ON SEQUENCES FROM PUBLIC, :"api_role", :"worker_role";
-- Built-in PUBLIC USAGE/EXECUTE defaults are global. Schema-scoped revokes do
-- not override them, so the dedicated migration role must default closed for
-- every type and routine it creates.
ALTER DEFAULT PRIVILEGES FOR ROLE :"schema_owner"
    REVOKE USAGE ON TYPES FROM PUBLIC, :"api_role", :"worker_role";
ALTER DEFAULT PRIVILEGES FOR ROLE :"schema_owner"
    REVOKE EXECUTE ON FUNCTIONS FROM PUBLIC, :"api_role", :"worker_role";
ALTER DEFAULT PRIVILEGES FOR ROLE :"schema_owner" IN SCHEMA commit
    REVOKE USAGE ON TYPES FROM PUBLIC, :"api_role", :"worker_role";
ALTER DEFAULT PRIVILEGES FOR ROLE :"schema_owner" IN SCHEMA commit
    REVOKE EXECUTE ON FUNCTIONS FROM PUBLIC, :"api_role", :"worker_role";
ALTER DEFAULT PRIVILEGES FOR ROLE :"schema_owner" IN SCHEMA commit_private
    REVOKE EXECUTE ON FUNCTIONS FROM PUBLIC, :"api_role", :"worker_role";

-- Both processes must use the public enums in prepared statements. New enum
-- types stay inaccessible until this explicit contract is extended.
GRANT USAGE
    ON TYPE commit.actor_type,
            commit.todo_status,
            commit.project_status,
            commit.project_entry_type,
            commit.blocker_status,
            commit.todo_activity_type,
            commit.outbox_status
    TO :"api_role", :"worker_role";

COMMIT;
