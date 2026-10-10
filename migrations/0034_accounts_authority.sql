-- Persist lifecycle events even before the first API request. No FK: the account may be unknown.
CREATE TABLE commit.account_lifecycle (
 uuid text PRIMARY KEY,
 revoked_before timestamptz,
 access_removed_at timestamptz,
 deleted_at timestamptz,
 CHECK (revoked_before IS NULL OR isfinite(revoked_before)),
 CHECK (access_removed_at IS NULL OR isfinite(access_removed_at)),
 CHECK (deleted_at IS NULL OR isfinite(deleted_at))
);
INSERT INTO commit.account_lifecycle(uuid, revoked_before, deleted_at)
 SELECT uuid, revoked_before, CASE WHEN status='deleted' THEN refreshed_at END
 FROM commit.accounts WHERE revoked_before IS NOT NULL OR status='deleted';

-- Custody is an authorization fact, valid only for a bounded time without a fresh Accounts answer.
CREATE FUNCTION commit.current_custodian(p_uuid text) RETURNS text
LANGUAGE sql STABLE SET search_path = pg_catalog, pg_temp AS $$
 SELECT custodian_uuid FROM commit.accounts WHERE uuid=p_uuid AND kind='silicon' AND status='active'
 AND refreshed_at >= transaction_timestamp() - interval '10 minutes'
$$;
CREATE OR REPLACE FUNCTION commit.in_circle(p_a text,p_b text) RETURNS boolean
LANGUAGE sql STABLE SET search_path = pg_catalog, pg_temp AS $$
 SELECT EXISTS(SELECT 1 FROM commit.accounts a JOIN commit.accounts b ON b.uuid=p_b
 WHERE a.uuid=p_a AND a.status='active' AND b.status='active' AND
 (p_a=p_b OR commit.current_custodian(p_b)=p_a OR commit.current_custodian(p_a)=p_b
 OR (commit.current_custodian(p_a) IS NOT NULL AND commit.current_custodian(p_a)=commit.current_custodian(p_b))))
$$;
CREATE OR REPLACE FUNCTION commit.circle_of(p_account text) RETURNS text[]
LANGUAGE sql STABLE SET search_path = pg_catalog, pg_temp AS $$
 SELECT coalesce(array_agg(uuid ORDER BY (uuid=p_account) DESC,uuid),'{}') FROM commit.accounts
 WHERE commit.in_circle(p_account,uuid)
$$;
CREATE OR REPLACE FUNCTION commit.project_writable(p_project uuid, p_account text) RETURNS boolean
LANGUAGE sql STABLE
SET search_path = pg_catalog, pg_temp
AS $$
    SELECT EXISTS (
        SELECT 1
          FROM commit.project_participants AS member
          JOIN commit.projects AS project ON project.id = member.project_id AND project.deleted_at IS NULL
          LEFT JOIN commit.accounts AS member_account ON member_account.uuid = member.participant_account
         WHERE member.project_id = p_project
           AND member.removed_at IS NULL
           AND (
               member.participant_account = p_account
               OR commit.current_custodian(member_account.uuid) = p_account
           )
    )
$$;

CREATE FUNCTION commit.clear_deleted_custodian() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp AS $$
BEGIN IF NEW.status='deleted' THEN NEW.custodian_uuid := NULL; END IF; RETURN NEW; END $$;
CREATE TRIGGER accounts_clear_deleted_custodian BEFORE INSERT OR UPDATE ON commit.accounts
 FOR EACH ROW EXECUTE FUNCTION commit.clear_deleted_custodian();
UPDATE commit.accounts SET custodian_uuid=NULL WHERE status='deleted';

-- An unsaved preference uses the Carbon's shared email and the same defaults as GET email-settings.
CREATE VIEW commit.effective_email_preferences AS
 SELECT a.uuid AS account, coalesce(p.email,a.email,'') AS email,
 coalesce(p.enabled,true) AS enabled,coalesce(p.project_completed,true) AS project_completed,
 coalesce(p.project_updates,false) AS project_updates,coalesce(p.task_completed,false) AS task_completed,
 coalesce(p.task_assigned,false) AS task_assigned
 FROM commit.accounts a LEFT JOIN commit.email_preferences p ON p.account=a.uuid
 WHERE a.kind='carbon' AND a.status='active';
CREATE OR REPLACE FUNCTION commit_private.queue_project_email() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE pid uuid; v_kind text; v_title text; assignee text;
BEGIN
 IF NEW.resource_type = 'project' THEN pid := NEW.resource_id;
 ELSIF NEW.change_summary ? 'project_id' THEN pid := (NEW.change_summary->>'project_id')::uuid;
 ELSIF NEW.resource_type = 'todo' THEN
   SELECT t.project_id, t.assigned_to_account, t.title INTO pid, assignee, v_title FROM commit.todos t WHERE t.id = NEW.resource_id;
 END IF;
 IF NEW.action = 'project.completed' THEN v_kind := 'project_completed';
 ELSIF NEW.action IN ('project.task.created', 'project.task.claimed', 'todo.created', 'todo.reassigned') THEN v_kind := 'task_assigned';
 ELSIF NEW.action = 'todo.updated' AND (NEW.change_summary->'fields') ? 'assigned_to' THEN v_kind := 'task_assigned';
 ELSIF NEW.action = 'todo.updated' AND (NEW.change_summary->'fields') ? 'status'
       AND EXISTS (SELECT 1 FROM commit.todos WHERE id = NEW.resource_id AND status = 'completed') THEN v_kind := 'task_completed';
 ELSIF NEW.action = 'project.task.updated' AND coalesce((NEW.change_summary->>'assignment_changed')::boolean, false) THEN v_kind := 'task_assigned';
 ELSIF NEW.action = 'project.task.updated' AND coalesce((NEW.change_summary->>'status_changed')::boolean, false)
       AND EXISTS (SELECT 1 FROM commit.project_tasks WHERE id = NEW.resource_id AND status = 'completed') THEN v_kind := 'task_completed';
 ELSIF NEW.action = 'todo.status_changed'
       AND EXISTS (SELECT 1 FROM commit.todos WHERE id = NEW.resource_id AND status = 'completed') THEN v_kind := 'task_completed';
 ELSIF pid IS NOT NULL THEN v_kind := 'project_updates';
 ELSE RETURN NEW; END IF;
 IF pid IS NOT NULL THEN SELECT name INTO v_title FROM commit.projects WHERE id = pid; END IF;
 IF NEW.resource_type = 'project_task' THEN
   SELECT assigned_to_account INTO assignee FROM commit.project_tasks WHERE id = NEW.resource_id;
 END IF;
 INSERT INTO commit.email_jobs (account, project_id, kind, recipient, subject, body)
 SELECT e.account, pid, v_kind, e.email, 'Commit: ' || replace(v_kind, '_', ' '),
        coalesce(v_title, 'Work item') || E'\nEvent: ' || NEW.action
          || E'\nOpen Commit: https://commit.teamofsilicons.com\nManage delivery in Commit email settings.'
   FROM commit.effective_email_preferences e
   JOIN commit.accounts a ON a.uuid = e.account AND a.status = 'active'
  WHERE e.enabled AND e.email <> ''
    AND CASE v_kind
          WHEN 'project_completed' THEN e.project_completed
          WHEN 'project_updates' THEN e.project_updates
          WHEN 'task_completed' THEN e.task_completed
          WHEN 'task_assigned' THEN e.task_assigned AND e.account = assignee
          ELSE false END
    AND (pid IS NULL OR commit.project_access(pid, e.account))
    AND (e.account = assignee OR (pid IS NOT NULL AND (
          EXISTS (SELECT 1 FROM commit.project_participants m
                   WHERE m.project_id = pid AND m.participant_account = e.account AND m.removed_at IS NULL)
          OR EXISTS (SELECT 1 FROM commit.project_collaborators c WHERE c.project_id = pid AND c.account = e.account))));
 IF NEW.action = 'project.created' THEN
  INSERT INTO commit.email_jobs (account, project_id, kind, recipient, subject, body)
  SELECT e.account, pid, 'task_assigned', e.email, 'Commit: task assigned',
         t.title || E'\nProject: ' || v_title
           || E'\nOpen Commit: https://commit.teamofsilicons.com\nManage delivery in Commit email settings.'
    FROM commit.project_tasks t
    JOIN commit.effective_email_preferences e ON e.account = t.assigned_to_account
    JOIN commit.accounts a ON a.uuid = e.account AND a.status = 'active'
   WHERE t.project_id = pid AND t.deleted_at IS NULL AND e.enabled AND e.task_assigned AND e.email <> ''
     AND commit.project_access(pid, e.account);
 END IF;
 RETURN NEW;
END $$;
CREATE OR REPLACE FUNCTION commit.claim_email() RETURNS SETOF jsonb
LANGUAGE plpgsql SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE job commit.email_jobs; preference commit.email_preferences; allowed boolean := true;
BEGIN
 SELECT j.* INTO job FROM commit.email_jobs j
  WHERE j.status = 'pending' AND j.next_attempt_at <= clock_timestamp()
    AND NOT EXISTS(SELECT 1 FROM commit.accounts a WHERE a.uuid=j.account AND a.status='unlinked')
  ORDER BY j.next_attempt_at LIMIT 1 FOR UPDATE OF j SKIP LOCKED;
 IF NOT FOUND THEN RETURN; END IF;
 IF job.kind <> 'bug_report' THEN
  allowed := EXISTS (SELECT 1 FROM commit.accounts WHERE uuid = job.account AND status = 'active');
  IF job.project_id IS NOT NULL THEN
   PERFORM 1 FROM commit.projects WHERE id = job.project_id FOR SHARE;
   allowed := allowed AND commit.project_access(job.project_id, job.account);
  END IF;
  SELECT * INTO preference FROM commit.email_preferences WHERE account = job.account FOR SHARE;
  IF NOT FOUND THEN
   SELECT email INTO preference.email FROM commit.accounts WHERE uuid=job.account AND kind='carbon' AND status='active' FOR SHARE;
   preference.enabled := true; preference.project_completed := true;
   preference.project_updates := false; preference.task_completed := false; preference.task_assigned := false;
  END IF;
  allowed := allowed AND preference.enabled AND preference.email = job.recipient
             AND coalesce((to_jsonb(preference)->>job.kind)::boolean, false);
 END IF;
 IF NOT allowed THEN
  UPDATE commit.email_jobs SET status = 'suppressed' WHERE id = job.id;
  RETURN NEXT jsonb_build_object('simulated', true);
  RETURN;
 END IF;
 RETURN NEXT to_jsonb(job) || jsonb_build_object('simulated', false);
END $$;
