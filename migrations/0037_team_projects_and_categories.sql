-- Commit stores stable team references. Membership comes from Organizations for each request.
ALTER TABLE commit.projects
  ADD COLUMN team_ids text[] NOT NULL DEFAULT '{}',
  ADD COLUMN categories text[] NOT NULL DEFAULT '{}';
ALTER TABLE commit.todos ADD COLUMN categories text[] NOT NULL DEFAULT '{}';
ALTER TABLE commit.project_tasks ADD COLUMN assigned_team text,
  ADD CONSTRAINT task_has_one_assignee CHECK (assigned_team IS NULL OR assigned_to_account IS NULL);
CREATE INDEX projects_team_ids ON commit.projects USING gin(team_ids) WHERE deleted_at IS NULL;
CREATE INDEX participants_invites_sent ON commit.project_participants (added_by_account, participant_account, added_at);
CREATE INDEX participants_invites_received ON commit.project_participants (participant_account, added_by_account, added_at);
CREATE TABLE commit.contact_connections (
  account_uuid text PRIMARY KEY REFERENCES commit.accounts(uuid) ON UPDATE CASCADE ON DELETE CASCADE
);

CREATE FUNCTION commit.project_writable(uuid, text, text[]) RETURNS boolean
LANGUAGE sql STABLE SET search_path = pg_catalog, pg_temp AS $$
  SELECT commit.project_writable($1,$2) OR EXISTS(
    SELECT 1 FROM commit.projects WHERE id=$1 AND deleted_at IS NULL AND team_ids && $3)
$$;
CREATE FUNCTION commit.project_access(uuid, text, text[]) RETURNS boolean
LANGUAGE sql STABLE SET search_path = pg_catalog, pg_temp AS $$
  SELECT commit.project_access($1,$2) OR EXISTS(
    SELECT 1 FROM commit.projects WHERE id=$1 AND deleted_at IS NULL AND team_ids && $3)
$$;
CREATE FUNCTION commit.todo_access(uuid, text, text[]) RETURNS boolean
LANGUAGE sql STABLE SET search_path = pg_catalog, pg_temp AS $$
  SELECT commit.todo_access($1,$2) OR EXISTS(
    SELECT 1 FROM commit.todos t JOIN commit.projects p ON p.id=t.project_id
     WHERE t.id=$1 AND t.deleted_at IS NULL AND p.deleted_at IS NULL AND p.team_ids && $3)
$$;

CREATE OR REPLACE FUNCTION commit_private.record_project_revision() RETURNS trigger
LANGUAGE plpgsql
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE pid uuid; v bigint; who jsonb; doc jsonb;
BEGIN
 IF NEW.resource_type IN ('project', 'project_diary') THEN pid := NEW.resource_id;
 ELSIF NEW.resource_type IN ('project_task', 'project_entry') THEN pid := (NEW.change_summary->>'project_id')::uuid;
 ELSIF NEW.resource_type = 'todo' THEN SELECT project_id INTO pid FROM commit.todos WHERE id = NEW.resource_id;
 ELSE RETURN NEW; END IF;
 IF pid IS NULL THEN RETURN NEW; END IF;
 PERFORM 1 FROM commit.projects WHERE id = pid FOR UPDATE;
 IF NOT FOUND THEN RETURN NEW; END IF;
 SELECT jsonb_build_object('type', kind, 'id', public_id, 'uuid', uuid) INTO who
   FROM commit.accounts WHERE uuid = NEW.actor_account;
 INSERT INTO commit.project_collaborators (project_id, account) VALUES (pid, NEW.actor_account)
 ON CONFLICT DO NOTHING;
 SELECT coalesce(max(version), 0) + 1 INTO v FROM commit.project_versions WHERE project_id = pid;
 SELECT jsonb_build_object(
   'id', p.id, 'name', p.name, 'slug', p.slug, 'uid', p.uid, 'status', p.status,
   'description', p.description, 'attachments', p.attachments, 'private', p.private, 'categories', p.categories, 'team_ids', p.team_ids,
   'owner', (SELECT jsonb_build_object('type', a.kind, 'id', a.public_id, 'uuid', a.uuid)
               FROM commit.accounts a WHERE a.uuid = p.owner_account),
   'participants', (SELECT coalesce(jsonb_agg(jsonb_build_object('type', a.kind, 'id', a.public_id, 'uuid', a.uuid)
                                               ORDER BY a.public_id, a.uuid), '[]')
                      FROM commit.project_participants m JOIN commit.accounts a ON a.uuid = m.participant_account
                     WHERE m.project_id = p.id AND m.removed_at IS NULL),
   'diary', (SELECT markdown FROM commit.project_diaries WHERE project_id = p.id),
   'tasks', (SELECT coalesce(jsonb_agg(jsonb_build_object(
                 'id', t.id, 'parent_task_id', t.parent_task_id, 'title', t.title, 'description', t.description,
                 'status', t.status, 'assigned_to', a.public_id, 'assigned_to_uuid', a.uuid, 'assigned_team', t.assigned_team, 'todo_id', t.todo_id)
               ORDER BY t.created_at, t.id), '[]')
               FROM commit.project_tasks t LEFT JOIN commit.accounts a ON a.uuid = t.assigned_to_account
              WHERE t.project_id = p.id AND t.deleted_at IS NULL),
   'entries', (SELECT coalesce(jsonb_agg(to_jsonb(e) - 'organization_id' - 'created_by_principal_id'
                                         ORDER BY e.created_at, e.id), '[]')
                 FROM commit.project_entries e WHERE e.project_id = p.id))
   INTO doc FROM commit.projects p WHERE p.id = pid;
 INSERT INTO commit.project_versions (project_id, version, actor, action, snapshot) VALUES (pid, v, who, NEW.action, doc);
 DELETE FROM commit.project_versions WHERE project_id = pid AND version <= v - 1000;
 RETURN NEW;
END;
$$;
