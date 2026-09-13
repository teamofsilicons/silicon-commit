CREATE TABLE commit.telemetry_events (
 id uuid PRIMARY KEY DEFAULT gen_random_uuid(), environment_id uuid REFERENCES commit.testing_environments(environment_id) ON DELETE CASCADE,
 event jsonb NOT NULL CHECK(jsonb_typeof(event)='object'), created_at timestamptz NOT NULL DEFAULT clock_timestamp(), exported_at timestamptz
);
CREATE INDEX telemetry_export_idx ON commit.telemetry_events(created_at) WHERE exported_at IS NULL AND environment_id IS NULL;
REVOKE ALL ON commit.telemetry_events FROM PUBLIC;
CREATE FUNCTION commit_private.clear_testing_telemetry() RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $$
BEGIN DELETE FROM commit.telemetry_events WHERE environment_id=OLD.environment_id; RETURN OLD; END $$;
REVOKE ALL ON FUNCTION commit_private.clear_testing_telemetry() FROM PUBLIC;
CREATE TRIGGER clear_testing_telemetry BEFORE DELETE ON commit.testing_organizations FOR EACH ROW EXECUTE FUNCTION commit_private.clear_testing_telemetry();
-- Existing todo webhooks also obey a project's current visibility at dispatch.
CREATE FUNCTION commit.lock_notification_access(p_event uuid) RETURNS boolean LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $$
DECLARE org uuid; pid uuid; recipient uuid;
BEGIN
 SELECT e.organization_id,t.project_id,e.recipient_silicon_principal_id INTO org,pid,recipient FROM commit.outbox_events e JOIN commit.todos t ON t.organization_id=e.organization_id AND t.id=e.todo_id WHERE e.id=p_event;
 IF NOT FOUND THEN RETURN false; END IF;
 IF pid IS NULL THEN RETURN true; END IF;
 PERFORM 1 FROM commit.projects WHERE organization_id=org AND id=pid FOR SHARE;
 RETURN commit.project_access(org,pid,recipient,'{}');
END $$;
REVOKE ALL ON FUNCTION commit.lock_notification_access(uuid) FROM PUBLIC;

CREATE OR REPLACE FUNCTION commit_private.erase_testing_data(p_environment uuid) RETURNS void
LANGUAGE plpgsql SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE org uuid; t text;
BEGIN
 PERFORM set_config('commit.clean_environment',p_environment::text,true);
 FOR org IN SELECT storage_organization_id FROM commit.testing_organizations WHERE environment_id=p_environment LOOP
   FOREACH t IN ARRAY ARRAY['email_jobs','email_preferences','outbox_events','idempotency_records','audit_events','todo_notification_subscriptions','silicon_notification_settings','project_versions','project_collaborators','project_entries','project_tasks','project_diaries','project_participants','todo_activity','todo_notes','todo_attachments','todos','projects','actor_projection','organization_projection'] LOOP
     EXECUTE format('DELETE FROM commit.%I WHERE organization_id=$1',t) USING org;
   END LOOP;
 END LOOP;
 DELETE FROM commit.telemetry_events WHERE environment_id=p_environment;
 DELETE FROM commit.iam_webhook_events WHERE environment_id=p_environment;
 DELETE FROM commit.testing_organizations WHERE environment_id=p_environment;
 PERFORM set_config('commit.clean_environment','',true);
END;
$fn$;
