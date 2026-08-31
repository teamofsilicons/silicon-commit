-- Immutable notification-destination decisions for durable Hook delivery.

CREATE TYPE commit.notification_subscription_level AS ENUM (
    'list',
    'todo'
);

ALTER TABLE commit.outbox_events
    ADD COLUMN webhook_url text,
    ADD COLUMN destination_version bigint,
    ADD COLUMN subscription_level commit.notification_subscription_level,
    ADD COLUMN subscription_scope commit.notification_scope,
    ADD COLUMN subscription_version bigint,
    ADD CONSTRAINT outbox_events_routing_snapshot_consistency CHECK (
        (
            payload_version = 1
            AND webhook_url IS NULL
            AND destination_version IS NULL
            AND subscription_level IS NULL
            AND subscription_scope IS NULL
            AND subscription_version IS NULL
        )
        OR (
            payload_version >= 2
            AND webhook_url IS NOT NULL
            AND webhook_url = btrim(webhook_url)
            AND octet_length(webhook_url) BETWEEN 1 AND 2048
            AND webhook_url ~ '^https://[^/?#@]+/silicon/[^/?#]+/[0-9A-F]{6}$'
            AND destination_version > 0
            AND subscription_level IS NOT NULL
            AND subscription_scope IS NOT NULL
            AND subscription_version > 0
        )
    );

COMMENT ON COLUMN commit.outbox_events.webhook_url IS
    'Immutable actor-bound Hook endpoint selected transactionally when the event was committed; NULL only on legacy payload-v1 rows.';
COMMENT ON COLUMN commit.outbox_events.destination_version IS
    'Silicon notification-settings version which supplied webhook_url.';
COMMENT ON COLUMN commit.outbox_events.subscription_level IS
    'Whether the effective rule came from the list-wide subscription or a todo-specific override.';
COMMENT ON COLUMN commit.outbox_events.subscription_scope IS
    'Effective notification scope which selected this event.';
COMMENT ON COLUMN commit.outbox_events.subscription_version IS
    'Version of the subscription resource which supplied the effective rule.';

CREATE FUNCTION commit_private.preserve_outbox_routing_snapshot()
RETURNS trigger
LANGUAGE plpgsql
SET search_path = pg_catalog
AS $function$
BEGIN
    IF NEW.webhook_url IS DISTINCT FROM OLD.webhook_url
        OR NEW.destination_version IS DISTINCT FROM OLD.destination_version
        OR NEW.subscription_level IS DISTINCT FROM OLD.subscription_level
        OR NEW.subscription_scope IS DISTINCT FROM OLD.subscription_scope
        OR NEW.subscription_version IS DISTINCT FROM OLD.subscription_version
        OR NEW.payload_version IS DISTINCT FROM OLD.payload_version
    THEN
        RAISE EXCEPTION 'outbox routing snapshots are immutable'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$function$;

CREATE TRIGGER outbox_events_preserve_routing_snapshot
BEFORE UPDATE ON commit.outbox_events
FOR EACH ROW EXECUTE FUNCTION commit_private.preserve_outbox_routing_snapshot();

-- Migration 0005 installed owner-scoped closed defaults. Repeat explicit
-- revokes for the new type and routine so fresh and upgraded databases share
-- the same least-privilege posture before runtime grants are applied.
REVOKE ALL ON TYPE commit.notification_subscription_level FROM PUBLIC;
REVOKE ALL ON TABLE commit.outbox_events FROM PUBLIC;
REVOKE ALL ON FUNCTION commit_private.preserve_outbox_routing_snapshot() FROM PUBLIC;
