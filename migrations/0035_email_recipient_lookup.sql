-- Resolve preferences only for recipients, not every Carbon in Commit.
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
   FROM (SELECT assignee AS account WHERE assignee IS NOT NULL
         UNION SELECT m.participant_account FROM commit.project_participants m WHERE m.project_id=pid AND m.removed_at IS NULL
         UNION SELECT c.account FROM commit.project_collaborators c WHERE c.project_id=pid) recipients
   JOIN commit.effective_email_preferences e ON e.account=recipients.account
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
