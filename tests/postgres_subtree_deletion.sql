CREATE TEMP TABLE subtree_fixture (
 label text, organization_id uuid, actor_id uuid, project_id uuid,
 parent_id uuid, child_id uuid, grandchild_id uuid, sibling_id uuid,
 parent_todo uuid, grandchild_todo uuid, sibling_todo uuid
) ON COMMIT DROP;
INSERT INTO subtree_fixture
SELECT label,gen_random_uuid(),gen_random_uuid(),gen_random_uuid(),
 gen_random_uuid(),gen_random_uuid(),gen_random_uuid(),gen_random_uuid(),
 gen_random_uuid(),gen_random_uuid(),gen_random_uuid()
FROM unnest(ARRAY['delete','legacy','unassign','protected']) AS label;

DO $$
DECLARE fixture record;
BEGIN
 FOR fixture IN SELECT * FROM subtree_fixture LOOP
   INSERT INTO commit.organization_projection(organization_id,org_id)
   VALUES(fixture.organization_id,'subtree-'||fixture.organization_id);
   INSERT INTO commit.actor_projection(organization_id,principal_id,membership_id,actor_type,actor_id)
   VALUES(fixture.organization_id,fixture.actor_id,'worker[subtree-'||fixture.organization_id||']','silicon','worker');
   INSERT INTO commit.projects(id,organization_id,name,slug,uid,created_by_principal_id)
   VALUES(fixture.project_id,fixture.organization_id,'Subtree','subtree','subtree:worker:'||fixture.project_id,fixture.actor_id);
   INSERT INTO commit.project_participants(id,organization_id,project_id,silicon_principal_id,added_by_principal_id)
   VALUES(gen_random_uuid(),fixture.organization_id,fixture.project_id,fixture.actor_id,fixture.actor_id);
   INSERT INTO commit.todos(id,organization_id,title,assigned_by_principal_id,assigned_to_principal_id,project_id)
   VALUES(fixture.parent_todo,fixture.organization_id,'Parent',fixture.actor_id,fixture.actor_id,fixture.project_id),
         (fixture.grandchild_todo,fixture.organization_id,'Grandchild',fixture.actor_id,fixture.actor_id,fixture.project_id),
         (fixture.sibling_todo,fixture.organization_id,'Sibling',fixture.actor_id,fixture.actor_id,fixture.project_id);
   INSERT INTO commit.project_tasks(id,organization_id,project_id,parent_task_id,title,created_by_principal_id,assigned_to_principal_id,todo_id)
   VALUES(fixture.parent_id,fixture.organization_id,fixture.project_id,NULL,'Parent',fixture.actor_id,fixture.actor_id,fixture.parent_todo),
         (fixture.child_id,fixture.organization_id,fixture.project_id,fixture.parent_id,'Unassigned child',fixture.actor_id,NULL,NULL),
         (fixture.grandchild_id,fixture.organization_id,fixture.project_id,fixture.child_id,'Grandchild',fixture.actor_id,fixture.actor_id,fixture.grandchild_todo),
         (fixture.sibling_id,fixture.organization_id,fixture.project_id,NULL,'Sibling',fixture.actor_id,fixture.actor_id,fixture.sibling_todo);
   INSERT INTO commit.audit_events(id,organization_id,actor_principal_id,action,resource_type,resource_id,request_id)
   VALUES(gen_random_uuid(),fixture.organization_id,fixture.actor_id,'project.created','project',fixture.project_id,'subtree-fixture');
 END LOOP;
END;
$$;

-- Deleting through the todo interface must mirror the entire task tree, retain
-- attribution/content, leave the sibling and other organizations untouched,
-- and capture the final state in the operation's single project revision.
DO $$
DECLARE fixture subtree_fixture; count_active bigint; snapshot_tasks integer;
BEGIN
 SELECT * INTO fixture FROM subtree_fixture WHERE label='delete';
 UPDATE commit.todos SET deleted_at=clock_timestamp(),content_retain_until=clock_timestamp()+interval '7 days',deleted_by_principal_id=fixture.actor_id
 WHERE id=fixture.parent_todo;
 INSERT INTO commit.audit_events(id,organization_id,actor_principal_id,action,resource_type,resource_id,request_id,change_summary)
 VALUES(gen_random_uuid(),fixture.organization_id,fixture.actor_id,'todo.deleted','todo',fixture.parent_todo,'subtree-delete','{"fields":["deleted_at"]}');
 SELECT count(*) INTO count_active FROM commit.project_tasks WHERE project_id=fixture.project_id AND deleted_at IS NULL;
 ASSERT count_active=1, 'deleting a task-backed todo must remove assigned and unassigned descendants';
 SELECT count(*) INTO count_active FROM commit.todos WHERE project_id=fixture.project_id AND deleted_at IS NULL;
 ASSERT count_active=1, 'descendant personal todos must disappear with their tasks';
 ASSERT EXISTS(SELECT 1 FROM commit.project_tasks WHERE id=fixture.sibling_id AND deleted_at IS NULL), 'unrelated sibling must survive';
 ASSERT EXISTS(SELECT 1 FROM commit.todos WHERE id=fixture.grandchild_todo AND title='Grandchild' AND version=2 AND deleted_by_principal_id=fixture.actor_id AND content_retain_until-deleted_at BETWEEN interval '7 days' AND interval '7 days 1 second'), 'cascade must preserve content and version each tombstone once';
 SELECT jsonb_array_length(snapshot->'tasks') INTO snapshot_tasks FROM commit.project_versions WHERE project_id=fixture.project_id ORDER BY version DESC LIMIT 1;
 ASSERT snapshot_tasks=1, 'deletion revision must omit the entire deleted subtree';
 ASSERT (SELECT count(*)=2 FROM commit.project_versions WHERE project_id=fixture.project_id), 'one deletion must append only one revision';
 UPDATE commit.todos SET deleted_at=clock_timestamp(),content_retain_until=clock_timestamp()+interval '7 days',deleted_by_principal_id=fixture.actor_id
 WHERE id=fixture.parent_todo AND deleted_at IS NULL;
 ASSERT EXISTS(SELECT 1 FROM commit.todos WHERE id=fixture.grandchild_todo AND version=2), 'repeated deletion must preserve descendant tombstones';
 ASSERT (SELECT count(*)=4 FROM commit.project_tasks WHERE project_id=(SELECT project_id FROM subtree_fixture WHERE label='protected') AND deleted_at IS NULL), 'cascade must stay within the selected project and organization';
END;
$$;

-- Unassigning deletes the detached personal todo while keeping the task tree.
DO $$
DECLARE fixture subtree_fixture;
BEGIN
 SELECT * INTO fixture FROM subtree_fixture WHERE label='unassign';
 UPDATE commit.project_tasks SET assigned_to_principal_id=NULL,todo_id=NULL WHERE id=fixture.parent_id;
 UPDATE commit.todos SET deleted_at=clock_timestamp(),content_retain_until=clock_timestamp()+interval '7 days',deleted_by_principal_id=fixture.actor_id WHERE id=fixture.parent_todo;
 ASSERT (SELECT count(*)=4 FROM commit.project_tasks WHERE project_id=fixture.project_id AND deleted_at IS NULL), 'unassignment must not delete tasks';
 ASSERT EXISTS(SELECT 1 FROM commit.todos WHERE id=fixture.grandchild_todo AND deleted_at IS NULL), 'unassignment must preserve descendant assignments';
END;
$$;

-- Reproduce the old mirror's outcome under the migration owner's table lock;
-- all changes, including trigger definitions, are rolled back by the test.
ALTER TABLE commit.todos DISABLE TRIGGER todos_sync_project;
UPDATE commit.todos SET deleted_at=clock_timestamp(),content_retain_until=clock_timestamp()+interval '7 days',deleted_by_principal_id=f.actor_id
FROM subtree_fixture f WHERE f.label='legacy' AND commit.todos.id=f.parent_todo;
UPDATE commit.project_tasks SET deleted_at=clock_timestamp()
FROM subtree_fixture f WHERE f.label='legacy' AND commit.project_tasks.id=f.parent_id;
ALTER TABLE commit.todos ENABLE TRIGGER todos_sync_project;
INSERT INTO commit.audit_events(id,organization_id,actor_principal_id,action,resource_type,resource_id,request_id,change_summary)
SELECT gen_random_uuid(),organization_id,actor_id,'todo.deleted','todo',parent_todo,'legacy-subtree-delete','{"fields":["deleted_at"]}'
FROM subtree_fixture WHERE label='legacy';
INSERT INTO commit.email_preferences(organization_id,principal_id,email,project_updates)
SELECT organization_id,actor_id,'subtree-regression@example.invalid',true FROM subtree_fixture WHERE label='legacy';
