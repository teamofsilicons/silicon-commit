-- Board administrators inherit through the board tree. Personal and team roots are unique.
CREATE TABLE commit.boards (
 id uuid PRIMARY KEY, parent_id uuid REFERENCES commit.boards(id), name text NOT NULL,
 owner_account text REFERENCES commit.accounts(uuid), home_account text UNIQUE REFERENCES commit.accounts(uuid),
 home_team text UNIQUE, progress_mode text NOT NULL DEFAULT 'count' CHECK (progress_mode IN ('count','time','hidden')),
 revision bigint NOT NULL DEFAULT 1, created_at timestamptz NOT NULL DEFAULT now(),
 CHECK (home_team IS NULL OR (owner_account IS NULL AND parent_id IS NULL))
);
CREATE INDEX boards_parent ON commit.boards(parent_id);
CREATE TABLE commit.board_members (
 board_id uuid NOT NULL REFERENCES commit.boards(id), subject text NOT NULL,
 role text NOT NULL CHECK(role IN ('member','admin')), PRIMARY KEY(board_id,subject)
);
CREATE TABLE commit.board_invitations (
 id uuid PRIMARY KEY, board_id uuid NOT NULL REFERENCES commit.boards(id), sender text NOT NULL REFERENCES commit.accounts(uuid),
 recipient text NOT NULL REFERENCES commit.accounts(uuid), created_at timestamptz NOT NULL DEFAULT clock_timestamp()
);
CREATE INDEX board_invites_sent ON commit.board_invitations(sender,recipient);
CREATE INDEX board_invites_received ON commit.board_invitations(recipient,sender);
CREATE TABLE commit.board_categories (
 id uuid PRIMARY KEY, board_id uuid NOT NULL REFERENCES commit.boards(id), name text NOT NULL
);
CREATE UNIQUE INDEX board_categories_names ON commit.board_categories(board_id,lower(name));
CREATE TABLE commit.board_operations (
 actor text NOT NULL, operation_key text NOT NULL, input jsonb NOT NULL, result uuid NOT NULL REFERENCES commit.boards(id),
 PRIMARY KEY(actor,operation_key)
);
ALTER TABLE commit.projects ADD COLUMN board_id uuid REFERENCES commit.boards(id);
ALTER TABLE commit.todos ADD COLUMN board_id uuid REFERENCES commit.boards(id),
 ADD COLUMN project_ids uuid[] NOT NULL DEFAULT '{}',
 ADD COLUMN ideal_minutes integer CHECK(ideal_minutes BETWEEN 1 AND 525600),
 ADD COLUMN progress smallint CHECK(progress BETWEEN 0 AND 100);
ALTER TABLE commit.project_tasks ADD COLUMN ideal_minutes integer CHECK(ideal_minutes BETWEEN 1 AND 525600),
 ADD COLUMN progress smallint CHECK(progress BETWEEN 0 AND 100);
CREATE INDEX projects_board ON commit.projects(board_id) WHERE deleted_at IS NULL;
CREATE INDEX todos_board ON commit.todos(board_id) WHERE deleted_at IS NULL;
CREATE INDEX todos_projects ON commit.todos USING gin(project_ids) WHERE deleted_at IS NULL;
-- Preserve the existing single-project field for older clients and linked task synchronization.
UPDATE commit.todos SET project_ids=ARRAY[project_id] WHERE project_id IS NOT NULL;
CREATE FUNCTION commit_private.todo_project_links() RETURNS trigger LANGUAGE plpgsql SET search_path=pg_catalog,pg_temp AS $$
BEGIN
 IF (TG_OP='INSERT' OR NEW.project_id IS DISTINCT FROM OLD.project_id) AND
    (TG_OP='INSERT' OR NEW.project_ids IS NOT DISTINCT FROM OLD.project_ids) THEN
    IF TG_OP='UPDATE' AND OLD.project_id IS NOT NULL THEN NEW.project_ids=array_remove(NEW.project_ids,OLD.project_id); END IF;
    IF NEW.project_id IS NOT NULL AND NOT (NEW.project_id=ANY(NEW.project_ids)) THEN NEW.project_ids=array_prepend(NEW.project_id,NEW.project_ids); END IF;
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER todo_project_links BEFORE INSERT OR UPDATE OF project_id,project_ids ON commit.todos
FOR EACH ROW EXECUTE FUNCTION commit_private.todo_project_links();

CREATE FUNCTION commit.board_role(board uuid, account text, groups text[]) RETURNS integer
LANGUAGE sql STABLE SET search_path=pg_catalog,pg_temp AS $$
 WITH RECURSIVE parents AS (
  SELECT b.* FROM commit.boards b WHERE b.id=board
  UNION ALL SELECT b.* FROM commit.boards b JOIN parents child ON child.parent_id=b.id
 )
 SELECT coalesce(max(greatest(
   CASE WHEN p.owner_account=account OR ('admin:'||p.home_team)=ANY(groups) THEN 2 WHEN p.home_team=ANY(groups) THEN 1 ELSE 0 END,
   coalesce((SELECT max(CASE WHEN m.role='admin' THEN 2 ELSE 1 END) FROM commit.board_members m
      WHERE m.board_id=p.id AND (m.subject=account OR m.subject=ANY(groups))),0)
 )),0) FROM parents p
$$;
CREATE OR REPLACE FUNCTION commit.project_writable(uuid,text,text[]) RETURNS boolean LANGUAGE sql STABLE SET search_path=pg_catalog,pg_temp AS $$
 SELECT commit.project_writable($1,$2) OR EXISTS(SELECT 1 FROM commit.projects WHERE id=$1 AND deleted_at IS NULL AND
   (team_ids && $3 OR (board_id IS NOT NULL AND commit.board_role(board_id,$2,$3)>0)))
$$;
CREATE OR REPLACE FUNCTION commit.project_access(uuid,text,text[]) RETURNS boolean LANGUAGE sql STABLE SET search_path=pg_catalog,pg_temp AS $$
 SELECT commit.project_access($1,$2) OR commit.project_writable($1,$2,$3)
$$;
CREATE OR REPLACE FUNCTION commit.todo_access(uuid,text,text[]) RETURNS boolean LANGUAGE sql STABLE SET search_path=pg_catalog,pg_temp AS $$
 SELECT commit.todo_access($1,$2) OR EXISTS(SELECT 1 FROM commit.todos t WHERE t.id=$1 AND t.deleted_at IS NULL AND
   ((t.board_id IS NOT NULL AND commit.board_role(t.board_id,$2,$3)>0) OR EXISTS(
   SELECT 1 FROM unnest(t.project_ids) pid WHERE commit.project_access(pid,$2,$3))))
$$;

CREATE FUNCTION commit.rename_category(labels text[],previous text,replacement text) RETURNS text[] LANGUAGE sql IMMUTABLE AS $$
 SELECT coalesce(array_agg(label ORDER BY seq),'{}') FROM (
   SELECT DISTINCT ON (lower(label)) label,seq FROM (
    SELECT CASE WHEN lower(n)=lower(previous) THEN replacement ELSE n END label,seq
    FROM unnest(labels) WITH ORDINALITY category(n,seq)
   ) x WHERE label IS NOT NULL ORDER BY lower(label),seq
 ) deduplicated
$$;
CREATE FUNCTION commit_private.sync_work_estimate() RETURNS trigger LANGUAGE plpgsql SET search_path=pg_catalog,pg_temp AS $$
BEGIN
 IF TG_TABLE_NAME='project_tasks' THEN
   UPDATE commit.todos SET ideal_minutes=NEW.ideal_minutes, progress=NEW.progress WHERE id=NEW.todo_id
    AND (ideal_minutes,progress) IS DISTINCT FROM (NEW.ideal_minutes,NEW.progress);
 ELSIF TG_TABLE_NAME='todos' THEN
   UPDATE commit.project_tasks SET ideal_minutes=NEW.ideal_minutes, progress=NEW.progress WHERE todo_id=NEW.id
    AND (ideal_minutes,progress) IS DISTINCT FROM (NEW.ideal_minutes,NEW.progress);
 END IF; RETURN NEW;
END $$;
CREATE TRIGGER task_work_estimate AFTER UPDATE OF ideal_minutes,progress ON commit.project_tasks
 FOR EACH ROW EXECUTE FUNCTION commit_private.sync_work_estimate();
CREATE TRIGGER todo_work_estimate AFTER UPDATE OF ideal_minutes,progress ON commit.todos
 FOR EACH ROW EXECUTE FUNCTION commit_private.sync_work_estimate();

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
   'description', p.description, 'attachments', p.attachments, 'private', p.private, 'board_id',p.board_id, 'categories', p.categories, 'team_ids', p.team_ids,
   'owner', (SELECT jsonb_build_object('type', a.kind, 'id', a.public_id, 'uuid', a.uuid)
               FROM commit.accounts a WHERE a.uuid = p.owner_account),
   'participants', (SELECT coalesce(jsonb_agg(jsonb_build_object('type', a.kind, 'id', a.public_id, 'uuid', a.uuid)
                                               ORDER BY a.public_id, a.uuid), '[]')
                      FROM commit.project_participants m JOIN commit.accounts a ON a.uuid = m.participant_account
                     WHERE m.project_id = p.id AND m.removed_at IS NULL),
   'diary', (SELECT markdown FROM commit.project_diaries WHERE project_id = p.id),
   'tasks', (SELECT coalesce(jsonb_agg(jsonb_build_object(
                 'id', t.id, 'parent_task_id', t.parent_task_id, 'title', t.title, 'description', t.description,
                 'status', t.status, 'assigned_to', a.public_id, 'assigned_to_uuid', a.uuid, 'ideal_minutes',t.ideal_minutes,'progress',t.progress,'assigned_team', t.assigned_team, 'todo_id', t.todo_id)
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
