-- A project task and its personal todo are two views of the same work. Deleting
-- either view must also remove its descendants, including their linked todos.
CREATE OR REPLACE FUNCTION commit_private.sync_project_work() RETURNS trigger
LANGUAGE plpgsql SET search_path=pg_catalog,pg_temp AS $$
DECLARE descendants uuid[];
BEGIN
 IF pg_trigger_depth()>1 THEN RETURN NEW; END IF;
 IF TG_TABLE_NAME='todos' THEN
   UPDATE commit.project_tasks SET title=NEW.title,description=coalesce(NEW.description,''),status=NEW.status,
       assigned_to_principal_id=NEW.assigned_to_principal_id,deleted_at=NEW.deleted_at
     WHERE organization_id=NEW.organization_id AND todo_id=NEW.id;
   IF OLD.deleted_at IS NULL AND NEW.deleted_at IS NOT NULL THEN
     WITH RECURSIVE subtree AS (
       SELECT id,project_id,deleted_at FROM commit.project_tasks
        WHERE organization_id=NEW.organization_id AND todo_id=NEW.id
       UNION
       SELECT child.id,child.project_id,child.deleted_at FROM commit.project_tasks child
       JOIN subtree parent ON child.parent_task_id=parent.id AND child.project_id=parent.project_id
        WHERE child.organization_id=NEW.organization_id
     ) SELECT array_agg(id) INTO descendants FROM subtree WHERE deleted_at IS NULL;

     -- Nested mirror triggers deliberately do not recurse. Update both tables
     -- explicitly so assigned and unassigned descendants have one lifecycle.
     UPDATE commit.todos SET
       deleted_at=greatest(updated_at,NEW.deleted_at,clock_timestamp()),
       content_retain_until=greatest(updated_at,NEW.deleted_at,clock_timestamp())
         +(NEW.content_retain_until-NEW.deleted_at),
       deleted_by_principal_id=NEW.deleted_by_principal_id
      WHERE organization_id=NEW.organization_id AND deleted_at IS NULL
        AND id IN (SELECT todo_id FROM commit.project_tasks
                    WHERE organization_id=NEW.organization_id AND id=ANY(descendants));
     UPDATE commit.project_tasks SET deleted_at=greatest(updated_at,NEW.deleted_at,clock_timestamp())
      WHERE organization_id=NEW.organization_id AND id=ANY(descendants) AND deleted_at IS NULL;
   END IF;
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

-- Repairs are recorded in project history, without sending historical changes
-- as new email notifications to project participants.
DROP TRIGGER project_email ON commit.audit_events;
CREATE TRIGGER project_email AFTER INSERT ON commit.audit_events FOR EACH ROW
WHEN (NEW.action <> 'project.task.subtree_repaired')
EXECUTE FUNCTION commit_private.queue_project_email();

-- Repair descendants left active by the old todo-deletion path. Keep existing
-- tombstones immutable, and use the original deletion actor and retention span
-- for newly removed descendants. All tenant and sandbox keys remain unchanged.
DO $$
DECLARE root record; descendants uuid[]; repaired integer; repaired_todos integer;
BEGIN
 FOR root IN
   SELECT task.id,task.organization_id,task.project_id,todo.deleted_at,
          todo.deleted_by_principal_id,todo.content_retain_until-todo.deleted_at AS retention
   FROM commit.project_tasks task JOIN commit.todos todo
     ON todo.organization_id=task.organization_id AND todo.id=task.todo_id
   WHERE task.deleted_at IS NOT NULL AND todo.deleted_at IS NOT NULL
   ORDER BY todo.deleted_at,task.id
 LOOP
   WITH RECURSIVE subtree AS (
     SELECT id,project_id FROM commit.project_tasks
      WHERE organization_id=root.organization_id AND id=root.id
     UNION
     SELECT child.id,child.project_id FROM commit.project_tasks child
     JOIN subtree parent ON child.parent_task_id=parent.id AND child.project_id=parent.project_id
      WHERE child.organization_id=root.organization_id
   ) SELECT array_agg(id) INTO descendants FROM subtree WHERE id<>root.id;

   -- Mark tasks first; their matching todo updates then see no active subtree
   -- to cascade into while this repair updates several todos in one statement.
   UPDATE commit.project_tasks SET deleted_at=greatest(updated_at,clock_timestamp())
    WHERE organization_id=root.organization_id AND id=ANY(descendants) AND deleted_at IS NULL;
   GET DIAGNOSTICS repaired = ROW_COUNT;
   UPDATE commit.todos SET
     deleted_at=greatest(updated_at,clock_timestamp()),
     content_retain_until=greatest(updated_at,clock_timestamp())+root.retention,
     deleted_by_principal_id=root.deleted_by_principal_id
    WHERE organization_id=root.organization_id AND deleted_at IS NULL
      AND id IN (SELECT todo_id FROM commit.project_tasks
                  WHERE organization_id=root.organization_id AND id=ANY(descendants));
   GET DIAGNOSTICS repaired_todos = ROW_COUNT;
   IF repaired+repaired_todos>0 THEN
     INSERT INTO commit.audit_events(id,organization_id,actor_principal_id,action,resource_type,resource_id,request_id,change_summary)
     VALUES(gen_random_uuid(),root.organization_id,root.deleted_by_principal_id,
       'project.task.subtree_repaired','project_task',root.id,'migration-0030-task-subtree-deletion',
       jsonb_build_object('project_id',root.project_id,'tasks_removed',repaired,'todos_removed',repaired_todos));
   END IF;
 END LOOP;
END;
$$;
