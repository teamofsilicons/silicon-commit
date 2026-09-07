-- Transactional request replay, audit history, and durable Hook notifications.

CREATE TABLE commit.idempotency_records (
    id uuid PRIMARY KEY,
    organization_id uuid NOT NULL,
    todo_id uuid,
    actor_principal_id uuid NOT NULL,
    operation text NOT NULL,
    resource_path text NOT NULL,
    idempotency_key text NOT NULL,
    request_fingerprint bytea NOT NULL,
    response_status smallint NOT NULL,
    response_body jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    expires_at timestamptz NOT NULL DEFAULT (
        transaction_timestamp() + interval '24 hours'
    ),
    UNIQUE (
        organization_id,
        actor_principal_id,
        operation,
        resource_path,
        idempotency_key
    ),
    CONSTRAINT idempotency_records_organization_fk
        FOREIGN KEY (organization_id)
        REFERENCES commit.organization_projection (organization_id)
        ON DELETE RESTRICT,
    CONSTRAINT idempotency_records_todo_fk
        FOREIGN KEY (organization_id, todo_id)
        REFERENCES commit.todos (organization_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT idempotency_records_actor_fk
        FOREIGN KEY (organization_id, actor_principal_id)
        REFERENCES commit.actor_projection (organization_id, principal_id)
        ON DELETE RESTRICT,
    CONSTRAINT idempotency_records_non_nil_id
        CHECK (id <> '00000000-0000-0000-0000-000000000000'::uuid),
    CONSTRAINT idempotency_records_operation_format CHECK (
        operation = btrim(operation)
        AND octet_length(operation) BETWEEN 1 AND 128
        AND operation ~ '^[A-Za-z][A-Za-z0-9_.:-]*$'
    ),
    CONSTRAINT idempotency_records_resource_path_format CHECK (
        resource_path = btrim(resource_path)
        AND octet_length(resource_path) BETWEEN 1 AND 1024
        AND left(resource_path, 1) = '/'
    ),
    CONSTRAINT idempotency_records_key_length
        CHECK (octet_length(idempotency_key) BETWEEN 8 AND 255),
    CONSTRAINT idempotency_records_sha256_fingerprint
        CHECK (octet_length(request_fingerprint) = 32),
    CONSTRAINT idempotency_records_response_status
        CHECK (response_status BETWEEN 200 AND 599),
    CONSTRAINT idempotency_records_todo_resource_link CHECK (
        (todo_id IS NOT NULL) = (
            resource_path = '/todos'
            OR resource_path LIKE '/todos/%'
        )
    ),
    CONSTRAINT idempotency_records_expiry_order CHECK (expires_at > created_at)
);

COMMENT ON TABLE commit.idempotency_records IS
    'Replayable committed mutation responses, scoped to tenant, actor, operation, path, and caller key.';
COMMENT ON COLUMN commit.idempotency_records.request_fingerprint IS
    'Exactly 32 raw bytes containing the SHA-256 digest of the canonical request representation.';
COMMENT ON COLUMN commit.idempotency_records.response_body IS
    'The complete JSON body returned for a matching retry; it commits with the domain transaction.';
COMMENT ON COLUMN commit.idempotency_records.todo_id IS
    'Todo whose representation may be replayed; NULL for non-todo operations.';

CREATE INDEX idempotency_records_expiry_idx
    ON commit.idempotency_records (expires_at, id);

CREATE INDEX idempotency_records_todo_expiry_idx
    ON commit.idempotency_records (organization_id, todo_id, expires_at)
    WHERE todo_id IS NOT NULL;

COMMENT ON INDEX commit.idempotency_records_todo_expiry_idx IS
    'Prevents todo content retention from overtaking a still-live replay response.';

CREATE TABLE commit.audit_events (
    id uuid PRIMARY KEY,
    organization_id uuid NOT NULL,
    actor_principal_id uuid NOT NULL,
    action text NOT NULL,
    resource_type text NOT NULL,
    resource_id uuid NOT NULL,
    request_id text NOT NULL,
    change_summary jsonb NOT NULL DEFAULT '{}'::jsonb,
    occurred_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    retain_until timestamptz NOT NULL DEFAULT (
        transaction_timestamp() + interval '2555 days'
    ),
    UNIQUE (organization_id, id),
    CONSTRAINT audit_events_organization_fk
        FOREIGN KEY (organization_id)
        REFERENCES commit.organization_projection (organization_id)
        ON DELETE RESTRICT,
    CONSTRAINT audit_events_actor_fk
        FOREIGN KEY (organization_id, actor_principal_id)
        REFERENCES commit.actor_projection (organization_id, principal_id)
        ON DELETE RESTRICT,
    CONSTRAINT audit_events_non_nil_id
        CHECK (id <> '00000000-0000-0000-0000-000000000000'::uuid),
    CONSTRAINT audit_events_non_nil_resource_id
        CHECK (resource_id <> '00000000-0000-0000-0000-000000000000'::uuid),
    CONSTRAINT audit_events_action_format CHECK (
        action = btrim(action)
        AND octet_length(action) BETWEEN 1 AND 128
        AND action ~ '^[a-z][a-z0-9_.:-]*$'
    ),
    CONSTRAINT audit_events_resource_type_format CHECK (
        resource_type = btrim(resource_type)
        AND octet_length(resource_type) BETWEEN 1 AND 64
        AND resource_type ~ '^[a-z][a-z0-9_]*$'
    ),
    CONSTRAINT audit_events_request_id_format CHECK (
        request_id = btrim(request_id)
        AND octet_length(request_id) BETWEEN 1 AND 255
    ),
    CONSTRAINT audit_events_change_summary_object
        CHECK (jsonb_typeof(change_summary) = 'object'),
    CONSTRAINT audit_events_retention_order CHECK (retain_until > occurred_at)
);

COMMENT ON TABLE commit.audit_events IS
    'Append-only mutation audit records retained for 2,555 days by default and not exposed by API v1.';
COMMENT ON COLUMN commit.audit_events.change_summary IS
    'Minimal non-secret field-level summary; raw request bodies and credentials are prohibited.';

CREATE INDEX audit_events_org_time_idx
    ON commit.audit_events (organization_id, occurred_at DESC, id DESC);
CREATE INDEX audit_events_resource_idx
    ON commit.audit_events (
        organization_id,
        resource_type,
        resource_id,
        occurred_at DESC,
        id DESC
    );
CREATE INDEX audit_events_retention_idx
    ON commit.audit_events (retain_until, id);

CREATE TABLE commit.outbox_events (
    id uuid PRIMARY KEY,
    organization_id uuid NOT NULL,
    todo_id uuid NOT NULL,
    recipient_silicon_principal_id uuid NOT NULL,
    recipient_actor_type commit.actor_type
        GENERATED ALWAYS AS ('silicon'::commit.actor_type) STORED,
    event_type text NOT NULL,
    payload_version smallint NOT NULL DEFAULT 1,
    payload jsonb NOT NULL,
    status commit.outbox_status NOT NULL DEFAULT 'pending',
    attempt_count integer NOT NULL DEFAULT 0,
    available_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    lease_owner text,
    lease_expires_at timestamptz,
    last_error_code text,
    created_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    delivered_at timestamptz,
    dead_lettered_at timestamptz,
    purge_after timestamptz,
    UNIQUE (organization_id, id),
    CONSTRAINT outbox_events_todo_fk
        FOREIGN KEY (organization_id, todo_id)
        REFERENCES commit.todos (organization_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT outbox_events_recipient_silicon_fk
        FOREIGN KEY (
            organization_id,
            recipient_silicon_principal_id,
            recipient_actor_type
        )
        REFERENCES commit.actor_projection (
            organization_id,
            principal_id,
            actor_type
        )
        ON DELETE RESTRICT,
    CONSTRAINT outbox_events_non_nil_id
        CHECK (id <> '00000000-0000-0000-0000-000000000000'::uuid),
    CONSTRAINT outbox_events_type_format CHECK (
        event_type = btrim(event_type)
        AND octet_length(event_type) BETWEEN 1 AND 128
        AND event_type ~ '^todo\.[a-z][a-z0-9_.-]*$'
    ),
    CONSTRAINT outbox_events_positive_payload_version CHECK (payload_version > 0),
    CONSTRAINT outbox_events_payload_object CHECK (jsonb_typeof(payload) = 'object'),
    CONSTRAINT outbox_events_nonnegative_attempt_count CHECK (attempt_count >= 0),
    CONSTRAINT outbox_events_lease_owner_format CHECK (
        lease_owner IS NULL
        OR (
            lease_owner = btrim(lease_owner)
            AND octet_length(lease_owner) BETWEEN 1 AND 255
        )
    ),
    CONSTRAINT outbox_events_last_error_code_format CHECK (
        last_error_code IS NULL
        OR (
            last_error_code = btrim(last_error_code)
            AND octet_length(last_error_code) BETWEEN 1 AND 255
        )
    ),
    CONSTRAINT outbox_events_state_consistency CHECK (
        (
            status = 'pending'
            AND lease_owner IS NULL
            AND lease_expires_at IS NULL
            AND delivered_at IS NULL
            AND dead_lettered_at IS NULL
            AND purge_after IS NULL
        )
        OR (
            status = 'in_flight'
            AND lease_owner IS NOT NULL
            AND lease_expires_at IS NOT NULL
            AND delivered_at IS NULL
            AND dead_lettered_at IS NULL
            AND purge_after IS NULL
        )
        OR (
            status = 'delivered'
            AND lease_owner IS NULL
            AND lease_expires_at IS NULL
            AND delivered_at IS NOT NULL
            AND dead_lettered_at IS NULL
            AND purge_after IS NOT NULL
        )
        OR (
            status = 'dead_letter'
            AND lease_owner IS NULL
            AND lease_expires_at IS NULL
            AND delivered_at IS NULL
            AND dead_lettered_at IS NOT NULL
            AND purge_after IS NOT NULL
        )
    ),
    CONSTRAINT outbox_events_timestamp_order CHECK (
        updated_at >= created_at
        AND available_at >= created_at
        AND (lease_expires_at IS NULL OR lease_expires_at > created_at)
        AND (delivered_at IS NULL OR delivered_at >= created_at)
        AND (dead_lettered_at IS NULL OR dead_lettered_at >= created_at)
        AND (
            delivered_at IS NULL
            OR purge_after >= delivered_at + interval '30 days'
        )
        AND (
            dead_lettered_at IS NULL
            OR purge_after >= dead_lettered_at + interval '90 days'
        )
    )
);

COMMENT ON TABLE commit.outbox_events IS
    'At-least-once webhook notification queue committed atomically with delegated todo mutations.';
COMMENT ON COLUMN commit.outbox_events.payload IS
    'Versioned non-secret event body containing stable event and todo IDs for consumer deduplication.';
COMMENT ON COLUMN commit.outbox_events.last_error_code IS
    'Sanitized bounded classification only; provider response bodies must not be persisted.';
COMMENT ON COLUMN commit.outbox_events.purge_after IS
    'Immutable terminal-state retention deadline, with database-enforced 30/90-day minima.';

CREATE TRIGGER outbox_events_touch_updated_at
BEFORE UPDATE ON commit.outbox_events
FOR EACH ROW EXECUTE FUNCTION commit_private.touch_updated_at();

CREATE INDEX outbox_events_claim_idx
    ON commit.outbox_events (available_at, created_at, id)
    INCLUDE (attempt_count)
    WHERE status = 'pending';

CREATE INDEX outbox_events_expired_lease_idx
    ON commit.outbox_events (lease_expires_at, id)
    WHERE status = 'in_flight';

CREATE INDEX outbox_events_todo_idx
    ON commit.outbox_events (organization_id, todo_id, created_at DESC, id DESC);

CREATE INDEX outbox_events_dead_letter_idx
    ON commit.outbox_events (dead_lettered_at, id)
    WHERE status = 'dead_letter';
