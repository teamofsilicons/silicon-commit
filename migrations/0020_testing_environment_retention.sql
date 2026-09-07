-- Keep sandbox lifecycle maintenance behind the same owner-defined boundary as
-- the rest of retention. The worker must not receive direct UPDATE/DELETE
-- privileges on testing-environment metadata.
CREATE FUNCTION commit.run_testing_environment_retention(p_limit integer)
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
     WHERE status = 'active'
       AND last_activity_at < clock_timestamp() - interval '15 days';
    GET DIAGNOSTICS expired = ROW_COUNT;

    purged := commit.purge_testing_environments(p_limit);
    RETURN NEXT;
END;
$fn$;
REVOKE ALL ON FUNCTION commit.run_testing_environment_retention(integer) FROM PUBLIC;
