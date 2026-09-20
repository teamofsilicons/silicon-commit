-- IAM 2 exposes memberships as actor_id[org_id]. Resource history and
-- idempotency remain keyed by the original organization/principal UUIDs.
-- The join includes every production and testing organization projection;
-- sandbox storage UUIDs must never be substituted for the public org handle.

ALTER TABLE commit.actor_projection
    DISABLE TRIGGER actor_projection_preserve_identity;

ALTER TABLE commit.actor_projection
    DROP CONSTRAINT actor_projection_non_nil_membership_id,
    ALTER COLUMN membership_id TYPE text USING membership_id::text;

UPDATE commit.actor_projection AS actor
SET membership_id = actor.actor_id || '[' || organization.org_id || ']'
FROM commit.organization_projection AS organization
WHERE organization.organization_id = actor.organization_id;

ALTER TABLE commit.actor_projection
    ADD CONSTRAINT actor_projection_membership_id_format CHECK (
        char_length(membership_id) BETWEEN char_length(actor_id) + 3 AND 512
        AND left(membership_id, char_length(actor_id) + 1) = actor_id || '['
        AND right(membership_id, 1) = ']'
    ),
    ENABLE TRIGGER actor_projection_preserve_identity;

COMMENT ON COLUMN commit.actor_projection.membership_id IS
    'Canonical IAM membership ID, actor_id[org_id]; active status is always checked online.';
