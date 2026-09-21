-- IAM Carbon/Silicon/application aggregate IDs now use immutable public handles.
-- Existing inbox event bytes and deduplication hashes are retained unchanged.
ALTER TABLE commit.iam_webhook_events
    ALTER COLUMN aggregate_id TYPE text USING aggregate_id::text;
COMMENT ON COLUMN commit.actor_projection.principal_id IS
    'Private Commit row key retained for local relationships; never read from or sent to IAM.';
COMMENT ON COLUMN commit.actor_projection.actor_id IS
    'Immutable canonical IAM identity used to resolve online-verified actors to private row keys.';
