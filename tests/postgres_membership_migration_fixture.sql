-- The same public actors/org exist in production and two independent sandboxes.
DO $fixture$
DECLARE
    plane integer;
    org uuid;
    environment uuid;
    carbon uuid;
    silicon uuid;
    todo uuid;
BEGIN
    FOR plane IN 0..2 LOOP
        org := gen_random_uuid();
        carbon := gen_random_uuid();
        silicon := gen_random_uuid();
        todo := gen_random_uuid();
        environment := NULL;
        IF plane > 0 THEN
            environment := gen_random_uuid();
            INSERT INTO commit.testing_environments (
                environment_id, organization_id, creator_principal_id, name,
                iam_test_key_digest, key_digest, iam_test_key_ciphertext
            ) VALUES (
                environment, gen_random_uuid(), carbon, 'migration-' || plane,
                repeat(plane::text, 64), repeat(plane::text, 64), ''::bytea
            );
            INSERT INTO commit.testing_organizations (
                environment_id, iam_organization_id, storage_organization_id
            ) VALUES (environment, gen_random_uuid(), org);
        END IF;
        INSERT INTO commit.organization_projection (organization_id, org_id, environment_id)
        VALUES (org, 'shared', environment);
        INSERT INTO commit.actor_projection (
            organization_id, principal_id, membership_id, actor_type, actor_id
        ) VALUES
            (org, carbon, gen_random_uuid(), 'carbon', 'alice'),
            (org, silicon, gen_random_uuid(), 'silicon', 'chef:shared');
        INSERT INTO commit.todos (
            id, organization_id, title, assigned_by_principal_id, assigned_to_principal_id
        ) VALUES (todo, org, 'Retain this work', carbon, silicon);
        INSERT INTO commit.todo_notes (id, organization_id, todo_id, author_principal_id, body)
        VALUES (gen_random_uuid(), org, todo, carbon, 'Retain this history');
        INSERT INTO commit.idempotency_records (
            id, organization_id, todo_id, actor_principal_id, operation,
            resource_path, idempotency_key, request_fingerprint, response_status, response_body
        ) VALUES (
            gen_random_uuid(), org, todo, carbon, 'createTodo', '/todos', 'migration-replay',
            decode(repeat('c', 64), 'hex'), 201, jsonb_build_object('id', todo, 'title', 'Retain this work')
        );
    END LOOP;
END;
$fixture$;
