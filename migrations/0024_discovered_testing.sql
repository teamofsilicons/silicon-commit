ALTER TABLE commit.testing_environments
 ADD COLUMN iam_environment_id uuid UNIQUE,
 ADD COLUMN iam_control_version bigint,
 ADD COLUMN iam_cleaned_at timestamptz,
 ADD COLUMN iam_owner_org_id text,
 ADD COLUMN iam_creator_id text;
CREATE UNIQUE INDEX testing_environment_key_digest_unique ON commit.testing_environments(key_digest);
CREATE FUNCTION commit.reset_discovered_testing_environment(p_environment uuid,p_version bigint) RETURNS void
LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $$
BEGIN
 PERFORM 1 FROM commit.testing_environments WHERE environment_id=p_environment AND version=p_version AND iam_environment_id IS NOT NULL AND status='active' FOR UPDATE;
 IF NOT FOUND THEN RAISE EXCEPTION 'stale testing environment' USING ERRCODE='28000'; END IF;
 PERFORM commit_private.erase_testing_data(p_environment);
END;
$$;
REVOKE ALL ON FUNCTION commit.reset_discovered_testing_environment(uuid,bigint) FROM PUBLIC;
-- Keep sandbox lifecycle maintenance behind the same owner-defined boundary as
-- the rest of retention. The worker must not receive direct UPDATE/DELETE
-- privileges on testing-environment metadata.
CREATE OR REPLACE FUNCTION commit.run_testing_environment_retention(p_limit integer)
RETURNS TABLE (expired bigint, purged bigint)
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $fn$
BEGIN
    IF p_limit NOT BETWEEN 1 AND 10000 THEN
        RAISE EXCEPTION 'invalid batch size';
    END IF;

    UPDATE commit.testing_environments
       SET status = 'deleted',
           deleted_at = clock_timestamp(),
           purge_after = clock_timestamp() + interval '30 days',
           version = version + 1,
           updated_at = clock_timestamp()
     WHERE status = 'active' AND iam_environment_id IS NULL
       AND last_activity_at < clock_timestamp() - interval '15 days';
    GET DIAGNOSTICS expired = ROW_COUNT;

    purged := commit.purge_testing_environments(p_limit);
    RETURN NEXT;
END;
$fn$;
REVOKE ALL ON FUNCTION commit.run_testing_environment_retention(integer) FROM PUBLIC;
