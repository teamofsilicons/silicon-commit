-- Durable service control state survives content cleanup and permanent removal.
-- Counters belong to Honeycomb, independently of IAM and local write versions.
CREATE TABLE commit.honeycomb_environments (
 environment_id uuid PRIMARY KEY,
 org_id text NOT NULL,
 app_id text NOT NULL,
 environment_revision bigint NOT NULL CHECK(environment_revision>0),
 generation bigint NOT NULL CHECK(generation>0),
 key_version bigint NOT NULL CHECK(key_version>0),
 state text NOT NULL CHECK(state IN ('pending','active','disabled','purged','retired','failed')),
 operation_id uuid NOT NULL,
 resume_state text NOT NULL DEFAULT 'active',
 root_key_ciphertext bytea NOT NULL,
 root_key_digest text NOT NULL,
 iam_cleaned_before timestamptz,
 cleared_at timestamptz,
 require_iam_clean boolean NOT NULL DEFAULT false,
 last_activity_at timestamptz,
 activity_reported_at timestamptz,
 updated_at timestamptz NOT NULL DEFAULT clock_timestamp()
);
CREATE TABLE commit.honeycomb_operations (
 environment_id uuid NOT NULL REFERENCES commit.honeycomb_environments(environment_id),
 operation_id uuid NOT NULL,
 request_hash text NOT NULL,
 receipt jsonb NOT NULL,
 PRIMARY KEY(environment_id,operation_id)
);
REVOKE ALL ON commit.honeycomb_environments,commit.honeycomb_operations FROM PUBLIC;

-- Called inside the participant transaction; deletion authority remains owner-only.
CREATE FUNCTION commit.finish_honeycomb_operation(p_environment uuid,p_operation uuid,p_action text,p_retired boolean)
RETURNS void LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $$
DECLARE h commit.honeycomb_environments; target_state text;
BEGIN
 SELECT * INTO h FROM commit.honeycomb_environments WHERE environment_id=p_environment FOR UPDATE;
 IF NOT FOUND OR h.operation_id<>p_operation OR h.state<>'pending' THEN
  RAISE EXCEPTION 'stale lifecycle operation' USING ERRCODE='28000';
 END IF;
 PERFORM 1 FROM commit.testing_environments WHERE environment_id=p_environment FOR UPDATE;
 IF p_action IN ('clean','purge') OR p_retired THEN
  PERFORM commit_private.erase_testing_data(p_environment);
 END IF;
 target_state:=CASE WHEN p_action='purge' THEN 'purged' WHEN p_retired THEN 'retired'
  WHEN p_action='disable' THEN 'disabled' WHEN p_action IN ('clean','rotate-key','refresh-import','retire-applications') THEN h.resume_state ELSE 'active' END;
 UPDATE commit.testing_environments SET status=CASE WHEN target_state='active' THEN 'active' ELSE 'deleted' END,
  version=version+1,purge_after=NULL,deleted_at=CASE WHEN target_state='active' THEN NULL ELSE clock_timestamp() END,
  iam_test_key_ciphertext=CASE WHEN target_state IN ('purged','retired') THEN ''::bytea ELSE iam_test_key_ciphertext END,
  iam_app_secret_ciphertext=CASE WHEN target_state IN ('purged','retired') THEN NULL ELSE iam_app_secret_ciphertext END,
  key_ciphertext=CASE WHEN target_state IN ('purged','retired') THEN NULL ELSE key_ciphertext END,
  updated_at=clock_timestamp()
 WHERE environment_id=p_environment;
 UPDATE commit.honeycomb_environments SET state=target_state,
  root_key_ciphertext=CASE WHEN target_state IN ('purged','retired') THEN ''::bytea ELSE root_key_ciphertext END,
  updated_at=clock_timestamp() WHERE environment_id=p_environment;
 UPDATE commit.honeycomb_operations SET receipt=jsonb_set(receipt,'{state}','"completed"')
 WHERE environment_id=p_environment AND operation_id=p_operation;
END;
$$;
REVOKE ALL ON FUNCTION commit.finish_honeycomb_operation(uuid,uuid,text,boolean) FROM PUBLIC;

-- Shared environments are never independently retired or purged by Commit.
CREATE OR REPLACE FUNCTION commit.purge_testing_environments(p_limit integer) RETURNS bigint
LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $$
DECLARE env uuid; total bigint := 0;
BEGIN
 IF p_limit NOT BETWEEN 1 AND 10000 THEN RAISE EXCEPTION 'invalid batch size'; END IF;
 FOR env IN SELECT environment_id FROM commit.testing_environments e
  WHERE status='deleted' AND purge_after<=clock_timestamp() AND iam_environment_id IS NULL
  AND NOT EXISTS(SELECT 1 FROM commit.honeycomb_environments h WHERE h.environment_id=e.environment_id)
  LIMIT p_limit FOR UPDATE SKIP LOCKED LOOP
  PERFORM commit_private.erase_testing_data(env);
  DELETE FROM commit.testing_environments WHERE environment_id=env;
  total:=total+1;
 END LOOP;
 RETURN total;
END;
$$;

-- Generation-pinned activity, never carried across a clean or key rotation.
CREATE FUNCTION commit.mark_honeycomb_activity(p_environment uuid,p_version bigint) RETURNS void
LANGUAGE sql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $$
 UPDATE commit.honeycomb_environments h SET last_activity_at=clock_timestamp()
 WHERE environment_id=p_environment AND state='active' AND EXISTS(
 SELECT 1 FROM commit.testing_environments e WHERE e.environment_id=p_environment AND e.version=p_version AND e.status='active');
$$;
REVOKE ALL ON FUNCTION commit.mark_honeycomb_activity(uuid,bigint) FROM PUBLIC;
CREATE FUNCTION commit.honeycomb_activity_outbox()
RETURNS TABLE(environment_id uuid,app_id text,generation bigint,key_version bigint,root_key_ciphertext bytea,last_activity_at timestamptz)
LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $$
 SELECT e.environment_id,e.app_id,e.generation,e.key_version,e.root_key_ciphertext,e.last_activity_at
 FROM commit.honeycomb_environments e WHERE e.state='active' AND e.last_activity_at IS NOT NULL
 AND (e.activity_reported_at IS NULL OR e.last_activity_at>e.activity_reported_at)
 ORDER BY e.last_activity_at LIMIT 25;
$$;
CREATE FUNCTION commit.ack_honeycomb_activity(p_environment uuid,p_generation bigint,p_key bigint,p_at timestamptz)
RETURNS void LANGUAGE sql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $$
 UPDATE commit.honeycomb_environments SET activity_reported_at=GREATEST(activity_reported_at,p_at)
 WHERE environment_id=p_environment AND generation=p_generation AND key_version=p_key AND state='active';
$$;
REVOKE ALL ON FUNCTION commit.honeycomb_activity_outbox(),commit.ack_honeycomb_activity(uuid,bigint,bigint,timestamptz) FROM PUBLIC;

CREATE FUNCTION commit.testing_delivery_allowed(p_environment uuid) RETURNS boolean
LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $$
 SELECT p_environment IS NULL OR EXISTS(SELECT 1 FROM commit.testing_environments e
 WHERE e.environment_id=p_environment AND e.status='active'
 AND NOT EXISTS(SELECT 1 FROM commit.honeycomb_environments h WHERE h.environment_id=e.environment_id AND h.state<>'active'));
$$;
REVOKE ALL ON FUNCTION commit.testing_delivery_allowed(uuid) FROM PUBLIC;
CREATE OR REPLACE FUNCTION commit.claim_email() RETURNS SETOF jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $$
DECLARE job commit.email_jobs; preference commit.email_preferences; sandbox uuid; allowed boolean:=true;
BEGIN
 SELECT j.* INTO job FROM commit.email_jobs j JOIN commit.organization_projection o USING(organization_id) WHERE j.status='pending' AND j.next_attempt_at<=clock_timestamp() AND commit.testing_delivery_allowed(o.environment_id) ORDER BY j.next_attempt_at LIMIT 1 FOR UPDATE OF j SKIP LOCKED;
 IF NOT FOUND THEN RETURN; END IF;
 SELECT environment_id INTO sandbox FROM commit.organization_projection WHERE organization_id=job.organization_id;
 IF job.project_id IS NOT NULL THEN
  PERFORM 1 FROM commit.projects WHERE organization_id=job.organization_id AND id=job.project_id FOR SHARE;
  allowed:=commit.project_access(job.organization_id,job.project_id,job.principal_id,'{}');
 END IF;
 IF job.kind<>'bug_report' THEN
  SELECT * INTO preference FROM commit.email_preferences WHERE organization_id=job.organization_id AND principal_id=job.principal_id FOR SHARE;
  allowed:=allowed AND FOUND AND preference.enabled AND preference.email=job.recipient AND coalesce((to_jsonb(preference)->>job.kind)::boolean,false);
 END IF;
 IF sandbox IS NOT NULL OR NOT allowed THEN
  UPDATE commit.email_jobs SET status=CASE WHEN sandbox IS NOT NULL THEN 'simulated' ELSE 'suppressed' END WHERE id=job.id;
  RETURN NEXT jsonb_build_object('simulated',true); RETURN;
 END IF;
 RETURN NEXT to_jsonb(job)||jsonb_build_object('simulated',false);
END $$;

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

    UPDATE commit.testing_environments AS e
       SET status = 'deleted',
           deleted_at = clock_timestamp(),
           purge_after = clock_timestamp() + interval '30 days',
           version = version + 1,
           updated_at = clock_timestamp()
     WHERE status = 'active' AND iam_environment_id IS NULL
       AND NOT EXISTS(SELECT 1 FROM commit.honeycomb_environments h WHERE h.environment_id=e.environment_id)
       AND last_activity_at < clock_timestamp() - interval '15 days';
    GET DIAGNOSTICS expired = ROW_COUNT;

    purged := commit.purge_testing_environments(p_limit);
    RETURN NEXT;
END;
$fn$;
REVOKE ALL ON FUNCTION commit.run_testing_environment_retention(integer) FROM PUBLIC;
