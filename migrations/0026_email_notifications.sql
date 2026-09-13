CREATE TABLE commit.email_preferences (
 organization_id uuid NOT NULL, principal_id uuid NOT NULL,
 email text NOT NULL CHECK(length(email)<=254), enabled boolean NOT NULL DEFAULT true,
 project_completed boolean NOT NULL DEFAULT true, project_updates boolean NOT NULL DEFAULT false,
 task_completed boolean NOT NULL DEFAULT false, task_assigned boolean NOT NULL DEFAULT false,
 updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 PRIMARY KEY(organization_id,principal_id),
 FOREIGN KEY(organization_id,principal_id) REFERENCES commit.actor_projection(organization_id,principal_id) ON DELETE CASCADE
);
CREATE TABLE commit.email_jobs (
 id uuid PRIMARY KEY DEFAULT gen_random_uuid(), organization_id uuid NOT NULL REFERENCES commit.organization_projection(organization_id) ON DELETE CASCADE,
 principal_id uuid, project_id uuid, kind text NOT NULL, recipient text NOT NULL,
 subject text NOT NULL, body text NOT NULL, status text NOT NULL DEFAULT 'pending' CHECK(status IN('pending','delivered','simulated','suppressed','failed')),
 attempts integer NOT NULL DEFAULT 0, next_attempt_at timestamptz NOT NULL DEFAULT clock_timestamp(), created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 report_key text, request_hash text,
 UNIQUE(organization_id,principal_id,report_key)
);
CREATE INDEX email_jobs_pending_idx ON commit.email_jobs(next_attempt_at) WHERE status='pending';
REVOKE ALL ON commit.email_preferences,commit.email_jobs FROM PUBLIC;
CREATE FUNCTION commit_private.queue_project_email() RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $$
DECLARE pid uuid; kind text; v_title text; assignee uuid;
BEGIN
 IF NEW.resource_type='project' THEN pid:=NEW.resource_id;
 ELSIF NEW.change_summary ? 'project_id' THEN pid:=(NEW.change_summary->>'project_id')::uuid;
 ELSIF NEW.resource_type='todo' THEN SELECT t.project_id,t.assigned_to_principal_id,t.title INTO pid,assignee,v_title FROM commit.todos t WHERE t.organization_id=NEW.organization_id AND t.id=NEW.resource_id;
 END IF;
 IF NEW.action='project.completed' THEN kind:='project_completed';
 ELSIF NEW.action IN('project.task.created','project.task.claimed','todo.created','todo.reassigned') THEN kind:='task_assigned';
 ELSIF NEW.action='todo.updated' AND (NEW.change_summary->'fields') ? 'assigned_to' THEN kind:='task_assigned';
 ELSIF NEW.action='todo.updated' AND (NEW.change_summary->'fields') ? 'status' AND EXISTS(SELECT 1 FROM commit.todos WHERE organization_id=NEW.organization_id AND id=NEW.resource_id AND status='completed') THEN kind:='task_completed';
 ELSIF NEW.action='project.task.updated' AND coalesce((NEW.change_summary->>'assignment_changed')::boolean,false) THEN kind:='task_assigned';
 ELSIF NEW.action='project.task.updated' AND coalesce((NEW.change_summary->>'status_changed')::boolean,false) AND EXISTS(SELECT 1 FROM commit.project_tasks WHERE organization_id=NEW.organization_id AND id=NEW.resource_id AND status='completed') THEN kind:='task_completed';
 ELSIF NEW.action='todo.status_changed' AND EXISTS(SELECT 1 FROM commit.todos WHERE organization_id=NEW.organization_id AND id=NEW.resource_id AND status='completed') THEN kind:='task_completed';
 ELSIF pid IS NOT NULL THEN kind:='project_updates'; ELSE RETURN NEW; END IF;
 IF pid IS NOT NULL THEN SELECT name INTO v_title FROM commit.projects WHERE organization_id=NEW.organization_id AND id=pid; END IF;
 IF NEW.resource_type='project_task' THEN SELECT assigned_to_principal_id INTO assignee FROM commit.project_tasks WHERE organization_id=NEW.organization_id AND id=NEW.resource_id; END IF;
 INSERT INTO commit.email_jobs(organization_id,principal_id,project_id,kind,recipient,subject,body)
 SELECT e.organization_id,e.principal_id,pid,kind,e.email,'Commit: '||replace(kind,'_',' '),coalesce(v_title,'Work item')||E'\nEvent: '||NEW.action||E'\nOpen Commit: https://commit.teamofsilicons.com\nManage delivery in organization email settings.'
 FROM commit.email_preferences e WHERE e.organization_id=NEW.organization_id AND e.enabled AND e.email<>'' AND
 CASE kind WHEN 'project_completed' THEN e.project_completed WHEN 'project_updates' THEN e.project_updates WHEN 'task_completed' THEN e.task_completed WHEN 'task_assigned' THEN e.task_assigned AND e.principal_id=assignee ELSE false END
 AND (pid IS NULL OR commit.project_access(NEW.organization_id,pid,e.principal_id,'{}'))
 AND (e.principal_id=assignee OR (pid IS NOT NULL AND (EXISTS(SELECT 1 FROM commit.project_participants m WHERE m.organization_id=e.organization_id AND m.project_id=pid AND m.silicon_principal_id=e.principal_id AND m.removed_at IS NULL) OR EXISTS(SELECT 1 FROM commit.project_collaborators c WHERE c.organization_id=e.organization_id AND c.project_id=pid AND c.principal_id=e.principal_id))));
 IF NEW.action='project.created' THEN
  INSERT INTO commit.email_jobs(organization_id,principal_id,project_id,kind,recipient,subject,body)
  SELECT e.organization_id,e.principal_id,pid,'task_assigned',e.email,'Commit: task assigned',t.title||E'\nProject: '||v_title||E'\nOpen Commit: https://commit.teamofsilicons.com\nManage delivery in organization email settings.'
  FROM commit.project_tasks t JOIN commit.email_preferences e ON e.organization_id=t.organization_id AND e.principal_id=t.assigned_to_principal_id
  WHERE t.organization_id=NEW.organization_id AND t.project_id=pid AND t.deleted_at IS NULL AND e.enabled AND e.task_assigned AND e.email<>'' AND commit.project_access(e.organization_id,pid,e.principal_id,'{}');
 END IF;
 RETURN NEW;
END $$;
REVOKE ALL ON FUNCTION commit_private.queue_project_email() FROM PUBLIC;
CREATE TRIGGER project_email AFTER INSERT ON commit.audit_events FOR EACH ROW EXECUTE FUNCTION commit_private.queue_project_email();
CREATE FUNCTION commit.claim_email() RETURNS SETOF jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $$
DECLARE job commit.email_jobs; preference commit.email_preferences; sandbox uuid; allowed boolean:=true;
BEGIN
 SELECT * INTO job FROM commit.email_jobs WHERE status='pending' AND next_attempt_at<=clock_timestamp() ORDER BY next_attempt_at LIMIT 1 FOR UPDATE SKIP LOCKED;
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
REVOKE ALL ON FUNCTION commit.claim_email() FROM PUBLIC;
-- Child email state must be erased before deleting an isolated identity plane.
CREATE FUNCTION commit_private.clear_testing_email() RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $$
BEGIN
 DELETE FROM commit.email_jobs WHERE organization_id=OLD.storage_organization_id;
 DELETE FROM commit.email_preferences WHERE organization_id=OLD.storage_organization_id;
 RETURN OLD;
END $$;
REVOKE ALL ON FUNCTION commit_private.clear_testing_email() FROM PUBLIC;
CREATE TRIGGER clear_testing_email BEFORE DELETE ON commit.testing_organizations FOR EACH ROW EXECUTE FUNCTION commit_private.clear_testing_email();
