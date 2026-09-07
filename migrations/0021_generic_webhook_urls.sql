-- Webhook destinations are arbitrary HTTPS endpoints; Commit delivers directly.
ALTER TABLE commit.silicon_notification_settings
    DROP CONSTRAINT silicon_notification_settings_webhook_format,
    ADD CONSTRAINT silicon_notification_settings_webhook_format CHECK (
        webhook_url IS NULL OR (
            webhook_url = btrim(webhook_url)
            AND octet_length(webhook_url) BETWEEN 1 AND 2048
            AND webhook_url ~ '^https://[^/?#@]+(/[^?#]*)?$'
        )
    );

ALTER TABLE commit.outbox_events
    DROP CONSTRAINT outbox_events_routing_snapshot_consistency,
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
            AND webhook_url ~ '^https://[^/?#@]+(/[^?#]*)?$'
            AND destination_version > 0
            AND subscription_level IS NOT NULL
            AND subscription_scope IS NOT NULL
            AND subscription_version > 0
        )
    );
