-- IAM-era data on schema 0032, shaped like production: one organization (`tos`) with two
-- Carbons and two Silicons, plus a former Honeycomb testing environment whose sandbox reused
-- the same public ids. Fixed UUIDs keep the assertions readable.
DO $fixture$
DECLARE
    tos uuid := '11111111-1111-4111-8111-111111111111';
    sandbox uuid := '22222222-2222-4222-8222-222222222222';
    environment uuid := '33333333-3333-4333-8333-333333333333';
    saket uuid := 'a0000000-0000-4000-8000-000000000001';
    shubham uuid := 'a0000000-0000-4000-8000-000000000002';
    chef uuid := 'a0000000-0000-4000-8000-000000000003';
    scout uuid := 'a0000000-0000-4000-8000-000000000004';
    sandbox_saket uuid := 'b0000000-0000-4000-8000-000000000001';
    sandbox_chef uuid := 'b0000000-0000-4000-8000-000000000003';
    ship uuid := 'c0000000-0000-4000-8000-000000000001';
    personal uuid := 'c0000000-0000-4000-8000-000000000002';
    scouting uuid := 'c0000000-0000-4000-8000-000000000003';
    sandbox_todo uuid := 'c0000000-0000-4000-8000-000000000004';
    copy_todo uuid := 'c0000000-0000-4000-8000-000000000005';
    launch uuid := 'd0000000-0000-4000-8000-000000000001';
    secret uuid := 'd0000000-0000-4000-8000-000000000002';
    copy_task uuid := 'e0000000-0000-4000-8000-000000000001';
BEGIN
    INSERT INTO commit.testing_environments (
        environment_id, organization_id, creator_principal_id, name,
        iam_test_key_digest, key_digest, iam_test_key_ciphertext
    ) VALUES (
        environment, gen_random_uuid(), sandbox_saket, 'sandbox',
        repeat('1', 64), repeat('1', 64), ''::bytea
    );
    INSERT INTO commit.testing_organizations (environment_id, iam_organization_id, storage_organization_id)
    VALUES (environment, gen_random_uuid(), sandbox);
    INSERT INTO commit.organization_projection (organization_id, org_id, environment_id, first_seen_at)
    VALUES (tos, 'tos', NULL, '2026-08-01 09:00 UTC'), (sandbox, 'tos', environment, '2026-08-02 09:00 UTC');
    INSERT INTO commit.actor_projection (organization_id, principal_id, membership_id, actor_type, actor_id, first_seen_at)
    VALUES
        (tos, saket, 'c:saket[tos]', 'carbon', 'c:saket', '2026-08-01 09:00 UTC'),
        (tos, shubham, 'c:shubham[tos]', 'carbon', 'c:shubham', '2026-08-01 09:01 UTC'),
        (tos, chef, 'si:chef[tos]', 'silicon', 'si:chef', '2026-08-01 09:02 UTC'),
        (tos, scout, 'si:scout[tos]', 'silicon', 'si:scout', '2026-08-01 09:03 UTC'),
        (sandbox, sandbox_saket, 'c:saket[tos]', 'carbon', 'c:saket', '2026-08-02 09:00 UTC'),
        (sandbox, sandbox_chef, 'si:chef[tos]', 'silicon', 'si:chef', '2026-08-02 09:01 UTC');

    INSERT INTO commit.todos (
        id, organization_id, title, description, assigned_by_principal_id, assigned_to_principal_id,
        status, created_at, updated_at
    ) VALUES
        (ship, tos, 'Ship the release', E'Keep this **Markdown**.\n\n- one', saket, chef,
         'in_progress', '2026-09-01 10:00 UTC', '2026-09-02 10:00 UTC'),
        (personal, tos, 'Personal list item', NULL, chef, chef,
         'yet_to_do', '2026-09-03 10:00 UTC', '2026-09-03 10:00 UTC'),
        (scouting, tos, 'Scout the market', NULL, shubham, scout,
         'completed', '2026-09-04 10:00 UTC', '2026-09-05 10:00 UTC'),
        (sandbox_todo, sandbox, 'Sandbox work', NULL, sandbox_saket, sandbox_chef,
         'yet_to_do', '2026-09-06 10:00 UTC', '2026-09-06 10:00 UTC');
    INSERT INTO commit.todo_notes (id, organization_id, todo_id, author_principal_id, body, created_at)
    VALUES (gen_random_uuid(), tos, ship, saket, 'Keep this history', '2026-09-01 11:00 UTC');
    INSERT INTO commit.todo_activity (
        id, organization_id, todo_id, activity_type, actor_principal_id, request_id, changes, created_at
    ) VALUES (
        gen_random_uuid(), tos, ship, 'created', saket, 'req-fixture-ship',
        '{"fields":["title","assigned_to"]}', '2026-09-01 10:00 UTC'
    );
    INSERT INTO commit.todo_attachments (organization_id, todo_id, position, url)
    VALUES (tos, ship, 0, 'https://briefcase.example/api/v1/entries/018f268d-715a-7b72-8f0f-41f16f9af553');
    INSERT INTO commit.idempotency_records (
        id, organization_id, todo_id, actor_principal_id, operation, resource_path, idempotency_key,
        request_fingerprint, response_status, response_body
    ) VALUES (
        gen_random_uuid(), tos, ship, saket, 'createTodo', '/todos', 'fixture-replay-key',
        decode(repeat('ab', 32), 'hex'), 201, jsonb_build_object('id', ship, 'title', 'Ship the release')
    );

    -- A public project of si:chef with si:scout, and a private, tagged project of si:scout.
    INSERT INTO commit.projects (
        id, organization_id, name, slug, uid, created_by_principal_id, status, private, tags,
        description, created_at, updated_at
    ) VALUES
        (launch, tos, 'Launch', 'launch', 'launch:si:chef:1788000000000', chef, 'in_progress', false,
         '{}', 'Public launch', '2026-09-07 10:00 UTC', '2026-09-07 10:00 UTC'),
        (secret, tos, 'Secret', 'secret', 'secret:si:scout:1788000000001', scout, 'yet_to_start', true,
         '{ops}', '', '2026-09-08 10:00 UTC', '2026-09-08 10:00 UTC');
    INSERT INTO commit.project_participants (
        id, organization_id, project_id, silicon_principal_id, added_by_principal_id, added_at
    ) VALUES
        (gen_random_uuid(), tos, launch, chef, chef, '2026-09-07 10:00 UTC'),
        (gen_random_uuid(), tos, launch, scout, chef, '2026-09-07 10:05 UTC'),
        (gen_random_uuid(), tos, secret, scout, scout, '2026-09-08 10:00 UTC');
    INSERT INTO commit.todos (
        id, organization_id, title, description, assigned_by_principal_id, assigned_to_principal_id,
        project_id, created_at, updated_at
    ) VALUES (
        copy_todo, tos, 'Write the copy', '', chef, scout, launch,
        '2026-09-07 11:00 UTC', '2026-09-07 11:00 UTC'
    );
    INSERT INTO commit.project_tasks (
        id, organization_id, project_id, title, created_by_principal_id, assigned_to_principal_id, todo_id,
        created_at, updated_at
    ) VALUES (
        copy_task, tos, launch, 'Write the copy', chef, scout, copy_todo,
        '2026-09-07 11:00 UTC', '2026-09-07 11:00 UTC'
    );
    INSERT INTO commit.project_entries (
        id, organization_id, project_id, entry_type, title, description, created_by_principal_id, created_at
    ) VALUES (
        gen_random_uuid(), tos, launch, 'update', 'Kickoff', 'Work started.', chef, '2026-09-07 12:00 UTC'
    );
    UPDATE commit.project_diaries SET markdown = '# Launch notes', updated_by_principal_id = scout
     WHERE project_id = launch;
    INSERT INTO commit.audit_events (
        id, organization_id, actor_principal_id, action, resource_type, resource_id, request_id, occurred_at
    ) VALUES
        (gen_random_uuid(), tos, chef, 'project.created', 'project', launch, 'req-launch', '2026-09-07 10:00 UTC'),
        (gen_random_uuid(), tos, scout, 'project.created', 'project', secret, 'req-secret', '2026-09-08 10:00 UTC'),
        (gen_random_uuid(), tos, saket, 'todo.created', 'todo', ship, 'req-ship', '2026-09-01 10:00 UTC');

    -- si:chef follows the work it delegated.
    INSERT INTO commit.silicon_notification_settings (organization_id, silicon_principal_id, webhook_url, todo_list_scope)
    VALUES (tos, chef, 'https://hook.example.com/silicon/si:chef/A1B2', 'any_update');
    INSERT INTO commit.todo_notification_subscriptions (organization_id, silicon_principal_id, todo_id, scope)
    VALUES (tos, chef, copy_todo, 'status_updates');
    INSERT INTO commit.outbox_events (id, organization_id, todo_id, recipient_silicon_principal_id, event_type, payload)
    VALUES (gen_random_uuid(), tos, copy_todo, chef, 'todo.updated', jsonb_build_object('todo_id', copy_todo));

    -- Email: production is the newer preference; the sandbox kept an older one for the same person.
    INSERT INTO commit.email_preferences (organization_id, principal_id, email, updated_at)
    VALUES (tos, saket, 'saket@example.test', '2026-09-10 10:00 UTC'),
           (sandbox, sandbox_saket, 'sandbox@example.test', '2026-09-01 10:00 UTC');
    INSERT INTO commit.email_jobs (
        organization_id, principal_id, kind, recipient, subject, body, status, report_key, request_hash
    ) VALUES (
        tos, saket, 'bug_report', 'bugs@example.test', 'Commit bug report', 'Steps to reproduce',
        'delivered', 'fixture-report', repeat('a', 64)
    );
END;
$fixture$;
