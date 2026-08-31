-- Versioned Silicon notification configuration and per-todo overrides.

CREATE TYPE commit.notification_scope AS ENUM (
    'any_update',
    'status_updates',
    'specific_statuses'
);

CREATE FUNCTION commit_private.todo_status_array_is_unique(
    statuses commit.todo_status[]
)
RETURNS boolean
LANGUAGE sql
IMMUTABLE
STRICT
PARALLEL SAFE
SET search_path = pg_catalog
AS $function$
    SELECT array_position($1, NULL) IS NULL
       AND cardinality($1) = (
           SELECT count(DISTINCT status)
           FROM unnest($1) AS status
       )
$function$;

COMMENT ON FUNCTION commit_private.todo_status_array_is_unique(commit.todo_status[]) IS
    'Checks that a todo-status array contains neither duplicate nor NULL elements.';

CREATE TABLE commit.silicon_notification_settings (
    organization_id uuid NOT NULL,
    silicon_principal_id uuid NOT NULL,
    silicon_actor_type commit.actor_type
        GENERATED ALWAYS AS ('silicon'::commit.actor_type) STORED,
    webhook_url text,
    todo_list_scope commit.notification_scope,
    todo_list_statuses commit.todo_status[] NOT NULL DEFAULT '{}',
    version bigint NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    PRIMARY KEY (organization_id, silicon_principal_id),
    CONSTRAINT silicon_notification_settings_actor_fk
        FOREIGN KEY (
            organization_id,
            silicon_principal_id,
            silicon_actor_type
        )
        REFERENCES commit.actor_projection (
            organization_id,
            principal_id,
            actor_type
        )
        ON DELETE RESTRICT,
    CONSTRAINT silicon_notification_settings_webhook_format CHECK (
        webhook_url IS NULL
        OR (
            webhook_url = btrim(webhook_url)
            AND octet_length(webhook_url) BETWEEN 1 AND 2048
            AND webhook_url ~ '^https://[^/?#@]+/silicon/[^/?#]+/[0-9A-F]{6}$'
        )
    ),
    CONSTRAINT silicon_notification_settings_rule_consistency CHECK (
        (
            todo_list_scope IS NULL
            AND cardinality(todo_list_statuses) = 0
        )
        OR (
            todo_list_scope IN ('any_update', 'status_updates')
            AND cardinality(todo_list_statuses) = 0
        )
        OR (
            todo_list_scope = 'specific_statuses'
            AND cardinality(todo_list_statuses) BETWEEN 1 AND 5
            AND commit_private.todo_status_array_is_unique(todo_list_statuses)
        )
    ),
    CONSTRAINT silicon_notification_settings_positive_version CHECK (version > 0),
    CONSTRAINT silicon_notification_settings_timestamp_order CHECK (
        updated_at >= created_at
    )
);

COMMENT ON TABLE commit.silicon_notification_settings IS
    'Versioned Silicon-owned Hook endpoint and list-wide todo subscription.';
COMMENT ON COLUMN commit.silicon_notification_settings.webhook_url IS
    'Canonical actor-bound Silicon Hook public ingress URL; never copied into audit metadata.';
COMMENT ON COLUMN commit.silicon_notification_settings.todo_list_scope IS
    'NULL means no list-wide subscription. A per-todo non-NULL rule overrides this value.';

CREATE TRIGGER silicon_notification_settings_touch_version
BEFORE UPDATE ON commit.silicon_notification_settings
FOR EACH ROW EXECUTE FUNCTION commit_private.touch_versioned_row();

CREATE FUNCTION commit_private.preserve_silicon_notification_settings_identity()
RETURNS trigger
LANGUAGE plpgsql
AS $function$
BEGIN
    IF NEW.organization_id IS DISTINCT FROM OLD.organization_id
        OR NEW.silicon_principal_id IS DISTINCT FROM OLD.silicon_principal_id
        OR NEW.created_at IS DISTINCT FROM OLD.created_at
    THEN
        RAISE EXCEPTION 'Silicon notification settings identity is immutable'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$function$;

CREATE TRIGGER silicon_notification_settings_preserve_identity
BEFORE UPDATE ON commit.silicon_notification_settings
FOR EACH ROW EXECUTE FUNCTION commit_private.preserve_silicon_notification_settings_identity();

ALTER TABLE commit.todos
    ADD CONSTRAINT todos_assigned_by_notification_key
    UNIQUE (organization_id, id, assigned_by_principal_id);

CREATE TABLE commit.todo_notification_subscriptions (
    organization_id uuid NOT NULL,
    silicon_principal_id uuid NOT NULL,
    silicon_actor_type commit.actor_type
        GENERATED ALWAYS AS ('silicon'::commit.actor_type) STORED,
    todo_id uuid NOT NULL,
    scope commit.notification_scope,
    statuses commit.todo_status[] NOT NULL DEFAULT '{}',
    version bigint NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    PRIMARY KEY (organization_id, silicon_principal_id, todo_id),
    CONSTRAINT todo_notification_subscriptions_actor_fk
        FOREIGN KEY (
            organization_id,
            silicon_principal_id,
            silicon_actor_type
        )
        REFERENCES commit.actor_projection (
            organization_id,
            principal_id,
            actor_type
        )
        ON DELETE RESTRICT,
    CONSTRAINT todo_notification_subscriptions_todo_assigner_fk
        FOREIGN KEY (organization_id, todo_id, silicon_principal_id)
        REFERENCES commit.todos (
            organization_id,
            id,
            assigned_by_principal_id
        )
        ON DELETE RESTRICT,
    CONSTRAINT todo_notification_subscriptions_rule_consistency CHECK (
        (
            scope IS NULL
            AND cardinality(statuses) = 0
        )
        OR (
            scope IN ('any_update', 'status_updates')
            AND cardinality(statuses) = 0
        )
        OR (
            scope = 'specific_statuses'
            AND cardinality(statuses) BETWEEN 1 AND 5
            AND commit_private.todo_status_array_is_unique(statuses)
        )
    ),
    CONSTRAINT todo_notification_subscriptions_positive_version CHECK (version > 0),
    CONSTRAINT todo_notification_subscriptions_timestamp_order CHECK (
        updated_at >= created_at
    )
);

COMMENT ON TABLE commit.todo_notification_subscriptions IS
    'Versioned per-todo Silicon override resources; a retained NULL scope is an unsubscribe tombstone.';
COMMENT ON COLUMN commit.todo_notification_subscriptions.scope IS
    'A non-NULL rule overrides the list rule. NULL falls back to the list rule while retaining version history.';

CREATE TRIGGER todo_notification_subscriptions_touch_version
BEFORE UPDATE ON commit.todo_notification_subscriptions
FOR EACH ROW EXECUTE FUNCTION commit_private.touch_versioned_row();

CREATE FUNCTION commit_private.preserve_todo_notification_subscription_identity()
RETURNS trigger
LANGUAGE plpgsql
AS $function$
BEGIN
    IF NEW.organization_id IS DISTINCT FROM OLD.organization_id
        OR NEW.silicon_principal_id IS DISTINCT FROM OLD.silicon_principal_id
        OR NEW.todo_id IS DISTINCT FROM OLD.todo_id
        OR NEW.created_at IS DISTINCT FROM OLD.created_at
    THEN
        RAISE EXCEPTION 'todo notification subscription identity is immutable'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$function$;

CREATE TRIGGER todo_notification_subscriptions_preserve_identity
BEFORE UPDATE ON commit.todo_notification_subscriptions
FOR EACH ROW EXECUTE FUNCTION commit_private.preserve_todo_notification_subscription_identity();

-- Migration 0005 installed owner-scoped closed defaults. Repeat explicit
-- revokes here so the security posture remains evident and locally verifiable.
REVOKE ALL ON TYPE commit.notification_scope FROM PUBLIC;
REVOKE ALL ON TABLE
    commit.silicon_notification_settings,
    commit.todo_notification_subscriptions
FROM PUBLIC;
REVOKE ALL ON FUNCTION
    commit_private.todo_status_array_is_unique(commit.todo_status[]),
    commit_private.preserve_silicon_notification_settings_identity(),
    commit_private.preserve_todo_notification_subscription_identity()
FROM PUBLIC;
