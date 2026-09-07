-- Verified events only: never store raw test envelopes or their root keys.
CREATE TABLE commit.iam_webhook_events (
    event_id uuid PRIMARY KEY,
    event_type text NOT NULL,
    organization_id uuid,
    aggregate_id uuid NOT NULL,
    aggregate_version bigint NOT NULL CHECK (aggregate_version > 0),
    payload jsonb NOT NULL CHECK (jsonb_typeof(payload) = 'object'),
    payload_sha256 text NOT NULL CHECK (length(payload_sha256) = 64),
    received_at timestamptz NOT NULL DEFAULT clock_timestamp()
);
CREATE INDEX iam_webhook_events_aggregate ON commit.iam_webhook_events (aggregate_id, aggregate_version DESC);
REVOKE ALL ON commit.iam_webhook_events FROM PUBLIC;

CREATE TABLE commit.testing_environments (
    environment_id uuid PRIMARY KEY,
    organization_id uuid NOT NULL,
    creator_principal_id uuid NOT NULL,
    name text NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 200),
    description text,
    iam_test_key_digest text NOT NULL CHECK (length(iam_test_key_digest) = 64),
    key_digest text NOT NULL CHECK (length(key_digest) = 64),
    status text NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'deleted')),
    version bigint NOT NULL DEFAULT 1 CHECK (version > 0),
    last_activity_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    deleted_at timestamptz,
    purge_after timestamptz,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    UNIQUE (organization_id, name)
);
CREATE INDEX testing_environments_org_status ON commit.testing_environments (organization_id, status, created_at DESC);
REVOKE ALL ON commit.testing_environments FROM PUBLIC;
