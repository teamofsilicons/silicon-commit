-- Carbon and Silicon collaboration; existing public projects keep their IDs.
ALTER TABLE commit.projects DROP CONSTRAINT projects_creator_silicon_fk;
ALTER TABLE commit.projects DROP COLUMN created_by_actor_type;
ALTER TABLE commit.projects ADD CONSTRAINT projects_creator_fk FOREIGN KEY (organization_id,created_by_principal_id) REFERENCES commit.actor_projection(organization_id,principal_id);
ALTER TABLE commit.project_participants DROP CONSTRAINT project_participants_silicon_fk;
ALTER TABLE commit.project_participants DROP COLUMN silicon_actor_type;
ALTER TABLE commit.project_participants ADD CONSTRAINT project_participants_actor_fk FOREIGN KEY (organization_id,silicon_principal_id) REFERENCES commit.actor_projection(organization_id,principal_id);
ALTER TABLE commit.projects
    ADD COLUMN description text NOT NULL DEFAULT '' CHECK(char_length(description)<=100000),
    ADD COLUMN attachments jsonb NOT NULL DEFAULT '[]' CHECK(jsonb_typeof(attachments)='array'),
    ADD COLUMN private boolean NOT NULL DEFAULT false,
    ADD COLUMN tags text[] NOT NULL DEFAULT '{}';

CREATE FUNCTION commit.project_access(p_org uuid,p_project uuid,p_actor uuid,p_tags text[]) RETURNS boolean
LANGUAGE sql STABLE SET search_path=pg_catalog,pg_temp AS $$
 SELECT EXISTS(SELECT 1 FROM commit.projects p WHERE p.organization_id=p_org AND p.id=p_project AND
   (NOT p.private OR p.created_by_principal_id=p_actor OR p.tags && p_tags OR EXISTS(
     SELECT 1 FROM commit.project_participants m WHERE m.organization_id=p_org AND m.project_id=p_project AND m.silicon_principal_id=p_actor AND m.removed_at IS NULL)));
$$;
REVOKE ALL ON FUNCTION commit.project_access(uuid,uuid,uuid,text[]) FROM PUBLIC;

ALTER TABLE commit.todos ADD COLUMN project_id uuid,
    ADD CONSTRAINT todos_project_fk FOREIGN KEY(organization_id,project_id) REFERENCES commit.projects(organization_id,id);
ALTER TABLE commit.project_tasks ADD COLUMN assigned_to_principal_id uuid,
    ADD COLUMN todo_id uuid,
    ADD COLUMN deleted_at timestamptz,
    ADD CONSTRAINT project_tasks_assignee_fk FOREIGN KEY(organization_id,assigned_to_principal_id) REFERENCES commit.actor_projection(organization_id,principal_id),
    ADD CONSTRAINT project_tasks_todo_fk FOREIGN KEY(organization_id,todo_id) REFERENCES commit.todos(organization_id,id),
    ADD CONSTRAINT project_tasks_todo_unique UNIQUE(organization_id,todo_id);
CREATE INDEX todos_project_idx ON commit.todos(organization_id,project_id) WHERE project_id IS NOT NULL;

CREATE TABLE commit.project_collaborators (
    organization_id uuid NOT NULL,
    project_id uuid NOT NULL,
    principal_id uuid NOT NULL,
    first_contributed_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY(organization_id,project_id,principal_id),
    FOREIGN KEY(organization_id,project_id) REFERENCES commit.projects(organization_id,id) ON DELETE CASCADE,
    FOREIGN KEY(organization_id,principal_id) REFERENCES commit.actor_projection(organization_id,principal_id)
);
CREATE TABLE commit.project_versions (
    organization_id uuid NOT NULL,
    project_id uuid NOT NULL,
    version bigint NOT NULL,
    actor jsonb NOT NULL,
    action text NOT NULL,
    snapshot jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY(organization_id,project_id,version),
    FOREIGN KEY(organization_id,project_id) REFERENCES commit.projects(organization_id,id) ON DELETE CASCADE
);
REVOKE ALL ON commit.project_collaborators,commit.project_versions FROM PUBLIC;

-- Changes from either interface share one task's title, description and status.
CREATE FUNCTION commit_private.sync_project_work() RETURNS trigger
LANGUAGE plpgsql SET search_path=pg_catalog,pg_temp AS $$
BEGIN
 IF pg_trigger_depth()>1 THEN RETURN NEW; END IF;
 IF TG_TABLE_NAME='todos' THEN
   UPDATE commit.project_tasks SET title=NEW.title,description=coalesce(NEW.description,''),status=NEW.status,
       assigned_to_principal_id=NEW.assigned_to_principal_id,deleted_at=NEW.deleted_at
     WHERE organization_id=NEW.organization_id AND todo_id=NEW.id;
 ELSE
   IF NEW.todo_id IS NOT NULL THEN
     UPDATE commit.todos SET title=NEW.title,description=NEW.description,status=NEW.status,
       assigned_to_principal_id=NEW.assigned_to_principal_id
       WHERE organization_id=NEW.organization_id AND id=NEW.todo_id AND deleted_at IS NULL;
   END IF;
 END IF;
 RETURN NEW;
END;
$$;
CREATE TRIGGER todos_sync_project AFTER UPDATE OF title,description,status,assigned_to_principal_id,deleted_at ON commit.todos FOR EACH ROW EXECUTE FUNCTION commit_private.sync_project_work();
CREATE TRIGGER tasks_sync_todo AFTER UPDATE OF title,description,status,assigned_to_principal_id ON commit.project_tasks FOR EACH ROW EXECUTE FUNCTION commit_private.sync_project_work();

-- The audit append is the end of each atomic workflow, so snapshots include
-- all child changes. Locking the project serializes versions and pruning.
CREATE FUNCTION commit_private.record_project_revision() RETURNS trigger
LANGUAGE plpgsql SET search_path=pg_catalog,pg_temp AS $$
DECLARE pid uuid; v bigint; who jsonb; doc jsonb;
BEGIN
 IF NEW.resource_type IN ('project','project_diary') THEN pid:=NEW.resource_id;
 ELSIF NEW.resource_type IN ('project_task','project_entry') THEN pid:=(NEW.change_summary->>'project_id')::uuid;
 ELSIF NEW.resource_type='todo' THEN SELECT project_id INTO pid FROM commit.todos WHERE organization_id=NEW.organization_id AND id=NEW.resource_id;
 ELSE RETURN NEW; END IF;
 IF pid IS NULL THEN RETURN NEW; END IF;
 PERFORM 1 FROM commit.projects WHERE organization_id=NEW.organization_id AND id=pid FOR UPDATE;
 IF NOT FOUND THEN RETURN NEW; END IF;
 SELECT jsonb_build_object('type',actor_type,'id',actor_id) INTO who FROM commit.actor_projection WHERE organization_id=NEW.organization_id AND principal_id=NEW.actor_principal_id;
 INSERT INTO commit.project_collaborators(organization_id,project_id,principal_id) VALUES(NEW.organization_id,pid,NEW.actor_principal_id) ON CONFLICT DO NOTHING;
 SELECT coalesce(max(version),0)+1 INTO v FROM commit.project_versions WHERE organization_id=NEW.organization_id AND project_id=pid;
 SELECT jsonb_build_object('id',p.id,'name',p.name,'slug',p.slug,'uid',p.uid,'status',p.status,'description',p.description,'attachments',p.attachments,'private',p.private,'tags',p.tags,
   'participants',(SELECT coalesce(jsonb_agg(jsonb_build_object('type',a.actor_type,'id',a.actor_id) ORDER BY a.actor_id),'[]') FROM commit.project_participants m JOIN commit.actor_projection a ON a.organization_id=m.organization_id AND a.principal_id=m.silicon_principal_id WHERE m.organization_id=p.organization_id AND m.project_id=p.id AND m.removed_at IS NULL),
   'diary',(SELECT markdown FROM commit.project_diaries WHERE organization_id=p.organization_id AND project_id=p.id),
   'tasks',(SELECT coalesce(jsonb_agg(jsonb_build_object('id',t.id,'parent_task_id',t.parent_task_id,'title',t.title,'description',t.description,'status',t.status,'assigned_to',a.actor_id,'todo_id',t.todo_id) ORDER BY t.created_at,t.id),'[]') FROM commit.project_tasks t LEFT JOIN commit.actor_projection a ON a.organization_id=t.organization_id AND a.principal_id=t.assigned_to_principal_id WHERE t.organization_id=p.organization_id AND t.project_id=p.id AND t.deleted_at IS NULL),
   'entries',(SELECT coalesce(jsonb_agg(to_jsonb(e)-'organization_id'-'created_by_principal_id' ORDER BY e.created_at,e.id),'[]') FROM commit.project_entries e WHERE e.organization_id=p.organization_id AND e.project_id=p.id))
 INTO doc FROM commit.projects p WHERE p.organization_id=NEW.organization_id AND p.id=pid;
 INSERT INTO commit.project_versions(organization_id,project_id,version,actor,action,snapshot) VALUES(NEW.organization_id,pid,v,who,NEW.action,doc);
 DELETE FROM commit.project_versions WHERE organization_id=NEW.organization_id AND project_id=pid AND version<=v-1000;
 RETURN NEW;
END;
$$;
CREATE TRIGGER audit_project_revision AFTER INSERT ON commit.audit_events FOR EACH ROW EXECUTE FUNCTION commit_private.record_project_revision();

-- New foreign keys require child cleanup before project/todo cleanup.
CREATE OR REPLACE FUNCTION commit_private.erase_testing_data(p_environment uuid) RETURNS void
LANGUAGE plpgsql SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE org uuid; t text;
BEGIN
 PERFORM set_config('commit.clean_environment',p_environment::text,true);
 FOR org IN SELECT storage_organization_id FROM commit.testing_organizations WHERE environment_id=p_environment LOOP
   FOREACH t IN ARRAY ARRAY['outbox_events','idempotency_records','audit_events','todo_notification_subscriptions','silicon_notification_settings','project_versions','project_collaborators','project_entries','project_tasks','project_diaries','project_participants','todo_activity','todo_notes','todo_attachments','todos','projects','actor_projection','organization_projection'] LOOP
     EXECUTE format('DELETE FROM commit.%I WHERE organization_id=$1',t) USING org;
   END LOOP;
 END LOOP;
 DELETE FROM commit.iam_webhook_events WHERE environment_id=p_environment;
 DELETE FROM commit.testing_organizations WHERE environment_id=p_environment;
 PERFORM set_config('commit.clean_environment','',true);
END;
$fn$;
