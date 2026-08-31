\set ON_ERROR_STOP on

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

SELECT has_schema_privilege(:'api_role', 'commit', 'USAGE')
       AND NOT has_schema_privilege(:'api_role', 'public', 'USAGE')
       AND has_table_privilege(:'api_role', 'commit.todos', 'SELECT')
       AND has_table_privilege(:'api_role', 'commit.projects', 'SELECT')
       AND NOT has_table_privilege(:'api_role', 'commit.outbox_events', 'UPDATE')
       AND NOT has_schema_privilege(:'api_role', 'commit_private', 'USAGE')
       AS api_grants_match
\gset

\if :api_grants_match
\else
  \echo 'API runtime grants do not match the reviewed contract'
  \quit 3
\endif

SELECT has_schema_privilege(:'worker_role', 'commit', 'USAGE')
       AND NOT has_schema_privilege(:'worker_role', 'public', 'USAGE')
       AND has_table_privilege(:'worker_role', 'commit.outbox_events', 'SELECT')
       AND has_column_privilege(
           :'worker_role',
           'commit.outbox_events',
           'status',
           'UPDATE'
       )
       AND has_column_privilege(
           :'worker_role',
           'commit.outbox_events',
           'purge_after',
           'UPDATE'
       )
       AND NOT has_column_privilege(
           :'worker_role',
           'commit.outbox_events',
           'payload',
           'UPDATE'
       )
       AND NOT has_column_privilege(
           :'worker_role',
           'commit.outbox_events',
           'updated_at',
           'UPDATE'
       )
       AND NOT has_table_privilege(:'worker_role', 'commit.todos', 'SELECT')
       AND NOT has_column_privilege(:'worker_role', 'commit.todos', 'title', 'UPDATE')
       AND NOT has_table_privilege(:'worker_role', 'commit.todo_activity', 'SELECT')
       AND NOT has_table_privilege(:'worker_role', 'commit.todo_notes', 'DELETE')
       AND NOT has_table_privilege(:'worker_role', 'commit.todo_attachments', 'DELETE')
       AND NOT has_table_privilege(:'worker_role', 'commit.audit_events', 'DELETE')
       AND NOT has_table_privilege(:'worker_role', 'commit.idempotency_records', 'DELETE')
       AND NOT has_table_privilege(:'worker_role', 'commit.outbox_events', 'DELETE')
       AND has_function_privilege(
           :'worker_role',
           'commit.run_retention_pass(integer)',
           'EXECUTE'
       )
       AND NOT has_table_privilege(:'worker_role', 'commit.projects', 'SELECT')
       AND NOT has_schema_privilege(:'worker_role', 'commit_private', 'USAGE')
       AS worker_grants_match
\gset

\if :worker_grants_match
\else
  \echo 'worker runtime grants do not match the reviewed contract'
  \quit 3
\endif

SELECT NOT EXISTS (
           SELECT 1
           FROM pg_catalog.pg_namespace AS namespace
           JOIN pg_catalog.pg_roles AS owner ON owner.oid = namespace.nspowner
           WHERE namespace.nspname IN ('commit', 'commit_private')
             AND owner.rolname IN (:'api_role', :'worker_role')
       )
       AND NOT EXISTS (
           SELECT 1
           FROM pg_catalog.pg_class AS relation
           JOIN pg_catalog.pg_namespace AS namespace
             ON namespace.oid = relation.relnamespace
           JOIN pg_catalog.pg_roles AS owner ON owner.oid = relation.relowner
           WHERE (
               namespace.nspname IN ('commit', 'commit_private')
               OR (
                   namespace.nspname = 'public'
                   AND relation.relname = '_sqlx_migrations'
               )
           )
             AND owner.rolname IN (:'api_role', :'worker_role')
       )
       AND NOT EXISTS (
           SELECT 1
           FROM pg_catalog.pg_type AS app_type
           JOIN pg_catalog.pg_namespace AS namespace
             ON namespace.oid = app_type.typnamespace
           JOIN pg_catalog.pg_roles AS owner ON owner.oid = app_type.typowner
           WHERE namespace.nspname IN ('commit', 'commit_private')
             AND app_type.typisdefined
             AND owner.rolname IN (:'api_role', :'worker_role')
       )
       AND NOT EXISTS (
           SELECT 1
           FROM pg_catalog.pg_proc AS routine
           JOIN pg_catalog.pg_namespace AS namespace
             ON namespace.oid = routine.pronamespace
           JOIN pg_catalog.pg_roles AS owner ON owner.oid = routine.proowner
           WHERE namespace.nspname IN ('commit', 'commit_private')
             AND owner.rolname IN (:'api_role', :'worker_role')
       ) AS runtime_roles_own_no_application_objects
\gset

\if :runtime_roles_own_no_application_objects
\else
  \echo 'a runtime role owns a Commit application object'
  \quit 3
\endif

BEGIN;
SET LOCAL ROLE :"api_role";
SET LOCAL search_path TO commit, pg_catalog;
SELECT count(*) FROM commit.todos;
ROLLBACK;

BEGIN;
SET LOCAL ROLE :"worker_role";
SET LOCAL search_path TO commit, pg_catalog;
SELECT count(*) FROM commit.outbox_events;
SELECT * FROM commit.run_retention_pass(1);
ROLLBACK;

\set ON_ERROR_STOP off
BEGIN;
SET LOCAL ROLE :"worker_role";
UPDATE commit.todos SET title = title WHERE false;
\set worker_todo_update_sqlstate :SQLSTATE
ROLLBACK;

BEGIN;
SET LOCAL ROLE :"worker_role";
UPDATE commit.todo_activity SET changes = '{}'::jsonb WHERE false;
\set worker_activity_update_sqlstate :SQLSTATE
ROLLBACK;
\set ON_ERROR_STOP on

SELECT :'worker_todo_update_sqlstate' = '42501'
       AND :'worker_activity_update_sqlstate' = '42501'
       AS worker_direct_content_mutation_denied
\gset

\if :worker_direct_content_mutation_denied
\else
  \echo 'worker direct todo/activity mutation was not denied'
  \quit 3
\endif

-- Seed one valid terminal row as the schema owner, then prove that the worker's
-- queue-transition columns cannot reopen it or rewrite its stored purge
-- deadline. The surrounding transaction keeps this contract test repeatable.
SELECT pg_catalog.gen_random_uuid() AS terminal_fixture_organization_id,
       pg_catalog.gen_random_uuid() AS terminal_fixture_membership_id,
       pg_catalog.gen_random_uuid() AS terminal_fixture_principal_id,
       pg_catalog.gen_random_uuid() AS terminal_fixture_todo_id,
       pg_catalog.gen_random_uuid() AS terminal_fixture_event_id
\gset

BEGIN;
INSERT INTO commit.organization_projection (organization_id, org_id)
VALUES (
    :'terminal_fixture_organization_id'::uuid,
    'runtime-grants-' || :'terminal_fixture_organization_id'
);
INSERT INTO commit.actor_projection (
    organization_id,
    principal_id,
    membership_id,
    actor_type,
    actor_id
)
VALUES (
    :'terminal_fixture_organization_id'::uuid,
    :'terminal_fixture_principal_id'::uuid,
    :'terminal_fixture_membership_id'::uuid,
    'silicon'::commit.actor_type,
    'runtime-grants-silicon-' || :'terminal_fixture_principal_id'
);
INSERT INTO commit.todos (
    id,
    organization_id,
    title,
    assigned_by_principal_id,
    assigned_to_principal_id
)
VALUES (
    :'terminal_fixture_todo_id'::uuid,
    :'terminal_fixture_organization_id'::uuid,
    'runtime grant terminal fixture',
    :'terminal_fixture_principal_id'::uuid,
    :'terminal_fixture_principal_id'::uuid
);
INSERT INTO commit.outbox_events (
    id,
    organization_id,
    todo_id,
    recipient_silicon_principal_id,
    event_type,
    payload,
    status,
    attempt_count,
    delivered_at,
    purge_after
)
VALUES (
    :'terminal_fixture_event_id'::uuid,
    :'terminal_fixture_organization_id'::uuid,
    :'terminal_fixture_todo_id'::uuid,
    :'terminal_fixture_principal_id'::uuid,
    'todo.runtime_grant_test',
    '{}'::jsonb,
    'delivered'::commit.outbox_status,
    1,
    transaction_timestamp(),
    transaction_timestamp() + interval '30 days'
);

SET LOCAL ROLE :"worker_role";
SAVEPOINT before_terminal_reopen;
\set ON_ERROR_STOP off
UPDATE commit.outbox_events
SET status = 'pending',
    delivered_at = NULL,
    purge_after = NULL
WHERE id = :'terminal_fixture_event_id'::uuid;
\set worker_terminal_reopen_sqlstate :SQLSTATE
\set ON_ERROR_STOP on
ROLLBACK TO SAVEPOINT before_terminal_reopen;

SAVEPOINT before_terminal_deadline_rewrite;
\set ON_ERROR_STOP off
UPDATE commit.outbox_events
SET purge_after = purge_after + interval '1 day'
WHERE id = :'terminal_fixture_event_id'::uuid;
\set worker_terminal_deadline_sqlstate :SQLSTATE
\set ON_ERROR_STOP on
ROLLBACK TO SAVEPOINT before_terminal_deadline_rewrite;

SAVEPOINT before_terminal_noop;
\set ON_ERROR_STOP off
UPDATE commit.outbox_events
SET status = status
WHERE id = :'terminal_fixture_event_id'::uuid;
\set worker_terminal_noop_sqlstate :SQLSTATE
\set ON_ERROR_STOP on
ROLLBACK TO SAVEPOINT before_terminal_noop;
ROLLBACK;

SELECT :'worker_terminal_reopen_sqlstate' = '55000'
       AND :'worker_terminal_deadline_sqlstate' = '55000'
       AND :'worker_terminal_noop_sqlstate' = '55000'
       AS worker_terminal_mutation_denied
\gset

\if :worker_terminal_mutation_denied
\else
  \echo 'worker could reopen or rewrite terminal outbox evidence'
  \quit 3
\endif
