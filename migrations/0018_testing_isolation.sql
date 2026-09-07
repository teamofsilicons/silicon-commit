
CREATE TABLE commit.testing_organizations (
    environment_id uuid NOT NULL REFERENCES commit.testing_environments(environment_id) ON DELETE CASCADE,
    iam_organization_id uuid NOT NULL,
    storage_organization_id uuid NOT NULL UNIQUE,
    PRIMARY KEY (environment_id, iam_organization_id),
    UNIQUE (environment_id, storage_organization_id)
);
ALTER TABLE commit.organization_projection ADD COLUMN environment_id uuid;
ALTER TABLE commit.organization_projection DROP CONSTRAINT organization_projection_org_id_key;
ALTER TABLE commit.organization_projection ADD CONSTRAINT organization_projection_testing_fk
    FOREIGN KEY (environment_id, organization_id)
    REFERENCES commit.testing_organizations(environment_id, storage_organization_id)
    ON DELETE CASCADE;
CREATE UNIQUE INDEX organization_projection_public_scope_key
    ON commit.organization_projection (org_id, environment_id) NULLS NOT DISTINCT;
GRANT ALL ON TABLE commit.testing_organizations, commit.testing_environments, commit.organization_projection, commit.iam_webhook_events TO CURRENT_USER;

-- Preserve production retention/append-only invariants. Only the dedicated
-- cleanup routine may erase rows belonging to its exact sandbox mapping.
DO $migration$
DECLARE n text; definition text;
BEGIN
  FOREACH n IN ARRAY ARRAY['preserve_todo_tombstone','prevent_participant_history_rewrite','preserve_project_diary_identity','preserve_project_entries'] LOOP
    SELECT pg_get_functiondef(to_regprocedure('commit_private.' || n || '()')) INTO definition;
    definition := replace(definition, E'BEGIN\n', E'BEGIN\n    IF TG_OP = ''DELETE'' AND EXISTS (SELECT 1 FROM commit.testing_organizations m WHERE m.storage_organization_id=OLD.organization_id AND m.environment_id::text=current_setting(''commit.clean_environment'',true)) THEN RETURN OLD; END IF;\n');
    EXECUTE definition;
  END LOOP;
END;
$migration$;

CREATE FUNCTION commit_private.erase_testing_data(p_environment uuid) RETURNS void
LANGUAGE plpgsql SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE org uuid; t text;
BEGIN
  PERFORM set_config('commit.clean_environment',p_environment::text,true);
  FOR org IN SELECT storage_organization_id FROM commit.testing_organizations WHERE environment_id=p_environment LOOP
    FOREACH t IN ARRAY ARRAY['outbox_events','idempotency_records','audit_events','todo_notification_subscriptions','silicon_notification_settings','project_entries','project_tasks','project_diaries','project_participants','todo_activity','todo_notes','todo_attachments','projects','todos','actor_projection','organization_projection'] LOOP
      EXECUTE format('DELETE FROM commit.%I WHERE organization_id=$1',t) USING org;
    END LOOP;
  END LOOP;
  DELETE FROM commit.iam_webhook_events WHERE environment_id=p_environment;
  DELETE FROM commit.testing_organizations WHERE environment_id=p_environment;
  PERFORM set_config('commit.clean_environment','',true);
END;
$fn$;
REVOKE ALL ON FUNCTION commit_private.erase_testing_data(uuid) FROM PUBLIC;

ALTER TABLE commit.iam_webhook_events ADD COLUMN environment_id uuid REFERENCES commit.testing_environments(environment_id);
ALTER TABLE commit.iam_webhook_events DROP CONSTRAINT iam_webhook_events_pkey;
CREATE UNIQUE INDEX iam_webhook_events_scope_key ON commit.iam_webhook_events(environment_id,event_id) NULLS NOT DISTINCT;

CREATE FUNCTION commit.clean_testing_environment(p_environment uuid,p_digest text) RETURNS void
LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
BEGIN
  PERFORM 1 FROM commit.testing_environments WHERE environment_id=p_environment AND key_digest=p_digest AND status='active' FOR UPDATE;
  IF NOT FOUND THEN RAISE EXCEPTION 'invalid testing environment' USING ERRCODE='28000'; END IF;
  PERFORM commit_private.erase_testing_data(p_environment);
  UPDATE commit.testing_environments SET version=version+1,last_activity_at=clock_timestamp(),updated_at=clock_timestamp() WHERE environment_id=p_environment;
END;
$fn$;
REVOKE ALL ON FUNCTION commit.clean_testing_environment(uuid,text) FROM PUBLIC;

CREATE FUNCTION commit.purge_testing_environments(p_limit integer) RETURNS bigint
LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE env uuid; total bigint := 0;
BEGIN
 IF p_limit NOT BETWEEN 1 AND 10000 THEN RAISE EXCEPTION 'invalid batch size'; END IF;
 FOR env IN SELECT environment_id FROM commit.testing_environments WHERE status='deleted' AND purge_after<=clock_timestamp() LIMIT p_limit FOR UPDATE SKIP LOCKED LOOP
   PERFORM commit_private.erase_testing_data(env);
   DELETE FROM commit.testing_environments WHERE environment_id=env;
   total:=total+1;
 END LOOP;
 RETURN total;
END;
$fn$;
REVOKE ALL ON FUNCTION commit.purge_testing_environments(integer) FROM PUBLIC;
