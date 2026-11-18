WITH visible_projects AS MATERIALIZED (
  SELECT p.* FROM commit.projects p WHERE p.deleted_at IS NULL AND commit.project_access(p.id,$1,$2)
), visible_todos AS MATERIALIZED (
  SELECT t.* FROM commit.todos t WHERE t.deleted_at IS NULL AND commit.todo_access(t.id,$1,$2)
), work AS (
  SELECT 'project' AS kind,p.id,p.id AS project_id,p.name AS title,p.status::text AS status,p.owner_account AS owner,
         NULL::text AS assignee,NULL::text AS assigned_team,p.categories,ARRAY[p.board_id] AS boards,
         NULL::integer AS ideal_minutes,NULL::smallint AS progress,p.updated_at,
         p.owner_account=$1 OR p.team_ids && $2 OR EXISTS (
           SELECT 1 FROM commit.project_participants m WHERE m.project_id=p.id AND m.participant_account=$1 AND m.removed_at IS NULL)
         OR EXISTS(SELECT 1 FROM commit.project_tasks t WHERE t.project_id=p.id AND t.deleted_at IS NULL
                    AND (t.assigned_to_account=$1 OR t.assigned_team=ANY($2))) AS mine,
         false AS unit,false AS linked_todo
  FROM visible_projects p
  UNION ALL
  SELECT 'task',k.id,k.project_id,k.title,k.status::text,k.created_by_account,k.assigned_to_account,k.assigned_team,
         p.categories,ARRAY[p.board_id],k.ideal_minutes,k.progress,k.updated_at,
         k.created_by_account=$1 OR k.assigned_to_account=$1 OR k.assigned_team=ANY($2),true,false
  FROM commit.project_tasks k JOIN visible_projects p ON p.id=k.project_id WHERE k.deleted_at IS NULL
  UNION ALL
  SELECT 'todo',t.id,t.project_id,t.title,t.status::text,t.assigned_by_account,t.assigned_to_account,NULL,t.categories,
         array_prepend(t.board_id,ARRAY(SELECT p.board_id FROM visible_projects p WHERE p.id=ANY(t.project_ids))),
         t.ideal_minutes,t.progress,t.updated_at,t.assigned_to_account=$1 OR t.assigned_by_account=$1,
         k.id IS NULL,k.id IS NOT NULL
  FROM visible_todos t LEFT JOIN commit.project_tasks k ON k.todo_id=t.id AND k.deleted_at IS NULL
), scoped AS MATERIALIZED (
 SELECT w.*, o.public_id AS owner_id,a.public_id AS assignee_id FROM work w JOIN commit.accounts o ON o.uuid=w.owner
 LEFT JOIN commit.accounts a ON a.uuid=w.assignee
 WHERE ($3::uuid IS NULL OR $3=ANY(w.boards)) AND (NOT $4 OR w.mine)
 AND ($5::text IS NULL OR $5=w.owner OR lower($5)=lower(o.public_id))
 AND ($6::text IS NULL OR $6=w.assignee OR lower($6)=lower(a.public_id) OR $6=w.assigned_team)
 AND ($7::text IS NULL OR EXISTS(SELECT 1 FROM unnest(w.categories) c WHERE lower(c)=lower($7)))
), filtered AS MATERIALIZED (
 SELECT * FROM scoped WHERE ($9::text IS NULL OR kind=$9)
 AND ($8::text IS NULL OR CASE $8 WHEN 'open' THEN status NOT IN ('completed','canceled')
     WHEN 'active' THEN status='in_progress' ELSE status=$8 END)
 AND ($9 IS NOT NULL OR NOT linked_todo OR $4)
), page AS (
 SELECT * FROM filtered ORDER BY (status='in_progress') DESC,updated_at DESC,id,kind LIMIT $10 OFFSET $11
), config AS (
 SELECT coalesce((SELECT progress_mode FROM commit.boards WHERE id=$3),'count') AS mode
), units AS (
 SELECT s.status,CASE WHEN s.status='completed' THEN 100 ELSE coalesce(s.progress,0) END AS pct,
   CASE WHEN c.mode='time' THEN coalesce(s.ideal_minutes,1) ELSE 1 END AS weight
 FROM scoped s CROSS JOIN config c WHERE s.unit AND s.status<>'canceled'
)
SELECT jsonb_build_object('total',(SELECT count(*) FROM filtered),'items',
 (SELECT coalesce(jsonb_agg(to_jsonb(p)-'boards'-'unit'-'mine'-'linked_todo' ORDER BY (status='in_progress') DESC,updated_at DESC,id,kind),'[]') FROM page p),
 'summary',(SELECT jsonb_build_object('open',count(*) FILTER(WHERE status<>'completed'),'active',count(*) FILTER(WHERE status='in_progress'),
 'completed',count(*) FILTER(WHERE status='completed'),'total',count(*),'mode',(SELECT mode FROM config),
 'percent',CASE WHEN (SELECT mode FROM config)='hidden' THEN NULL ELSE coalesce(round(sum(pct::numeric*weight)/nullif(sum(weight),0)),0) END) FROM units))

