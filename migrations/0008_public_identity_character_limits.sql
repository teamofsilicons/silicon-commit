-- Keep retained IAM public identifiers aligned with the Rust/OpenAPI contract.
--
-- The application bounds these opaque identifiers by Unicode scalar count.
-- The original octet limit rejected valid multi-byte identifiers after they
-- had already passed domain validation.

ALTER TABLE commit.organization_projection
    DROP CONSTRAINT organization_projection_org_id_format,
    ADD CONSTRAINT organization_projection_org_id_format CHECK (
        org_id = btrim(org_id)
        AND char_length(org_id) BETWEEN 1 AND 255
    );

ALTER TABLE commit.actor_projection
    DROP CONSTRAINT actor_projection_actor_id_format,
    ADD CONSTRAINT actor_projection_actor_id_format CHECK (
        actor_id = btrim(actor_id)
        AND char_length(actor_id) BETWEEN 1 AND 255
    );
