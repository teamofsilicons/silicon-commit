-- Coordinated IAM public ID cutover. No UUID, ciphertext, receipt, or history rewrite.
-- Stop API/worker writes, drain deliveries and wait for replay expiry first.
SET CONSTRAINTS ALL IMMEDIATE;
CREATE TEMP TABLE public_id_security ON COMMIT DROP AS
SELECT oid, relrowsecurity, relforcerowsecurity FROM pg_class
WHERE relnamespace = 'commit'::regnamespace AND relkind IN ('r','p');
DO $$ DECLARE r record; BEGIN
 FOR r IN SELECT * FROM public_id_security ORDER BY oid LOOP
  EXECUTE format('LOCK TABLE ONLY %s IN ACCESS EXCLUSIVE MODE', r.oid::regclass);
  IF r.relrowsecurity THEN EXECUTE format('ALTER TABLE ONLY %s DISABLE ROW LEVEL SECURITY',r.oid::regclass); END IF;
 END LOOP;
 IF (SELECT count(*) FROM pg_trigger WHERE
  (tgrelid='commit.projects'::regclass AND tgname IN ('projects_preserve_identity','projects_touch_version') OR
   tgrelid='commit.actor_projection'::regclass AND tgname='actor_projection_preserve_identity') AND tgenabled='O')<>3 THEN
  RAISE EXCEPTION 'public ID cutover requires the three identity/version guards in their ordinary enabled mode; inspect unexpected trigger state';
 END IF;
 IF EXISTS(SELECT 1 FROM commit.idempotency_records WHERE expires_at > clock_timestamp()) THEN
  RAISE EXCEPTION 'public ID cutover requires expired idempotency replay windows; stop writes and wait, never delete live replay records';
 END IF;
 IF EXISTS(SELECT 1 FROM commit.outbox_events WHERE status IN ('pending','in_flight')) THEN
  RAISE EXCEPTION 'public ID cutover requires drained outbox deliveries';
 END IF;
 IF EXISTS(SELECT 1 FROM commit.honeycomb_environments WHERE state='pending') THEN
  RAISE EXCEPTION 'public ID cutover requires completed Honeycomb operations';
 END IF;
END $$;
CREATE TABLE commit_private.public_id_schema_map (
 organization_id uuid NOT NULL, principal_id uuid NOT NULL,
 environment_id uuid, actor_type commit.actor_type NOT NULL,
 old_id text NOT NULL, new_id text NOT NULL,
 old_membership_id text NOT NULL, new_membership_id text NOT NULL,
 PRIMARY KEY(organization_id, principal_id)
);
REVOKE ALL ON commit_private.public_id_schema_map FROM PUBLIC;
INSERT INTO commit_private.public_id_schema_map
SELECT a.organization_id,a.principal_id,o.environment_id,a.actor_type,a.actor_id,
 CASE WHEN a.actor_type='carbon' THEN CASE WHEN a.actor_id LIKE 'c:%' THEN a.actor_id ELSE 'c:'||a.actor_id END
 ELSE CASE WHEN a.actor_id LIKE 'si:%' THEN a.actor_id
 WHEN a.actor_id ~ '^[a-z0-9_-]{3,50}:[a-z0-9_-]{3,50}$' THEN 'si:'||split_part(a.actor_id,':',1) ELSE '' END END,
 a.membership_id,''
FROM commit.actor_projection a JOIN commit.organization_projection o USING(organization_id);
UPDATE commit_private.public_id_schema_map m SET new_membership_id=m.new_id||'['||o.org_id||']'
FROM commit.organization_projection o WHERE o.organization_id=m.organization_id;
DO $$ BEGIN
 IF EXISTS(SELECT 1 FROM commit_private.public_id_schema_map WHERE
   (actor_type='carbon' AND new_id !~ '^c:[a-z0-9_-]{3,30}$') OR
   (actor_type='silicon' AND new_id !~ '^si:[a-z0-9_-]{3,50}$')) THEN
  RAISE EXCEPTION 'invalid legacy actor; resolve against the authoritative IAM migration map before cutover';
 END IF;
 IF EXISTS(SELECT 1 FROM commit_private.public_id_schema_map
  GROUP BY environment_id,actor_type,new_id HAVING count(DISTINCT old_id)>1) THEN
  RAISE EXCEPTION 'public actor ID collision; resolve against the authoritative IAM migration map, never merge or rename automatically';
 END IF;
 IF EXISTS(SELECT 1 FROM commit.projects p JOIN commit_private.public_id_schema_map m USING(organization_id)
  WHERE m.principal_id=p.created_by_principal_id
  AND p.uid !~ ('^'||p.slug||':'||m.old_id||':[0-9]+$')) THEN
  RAISE EXCEPTION 'project UID does not match its recorded creator; resolve before cutover';
 END IF;
 IF EXISTS(SELECT 1 FROM commit.testing_environments WHERE iam_app_id IS NOT NULL
  AND iam_app_id !~ '^([a-z0-9_-]{3,50}>)?[a-z][a-z0-9_-]{0,79}$') OR
  EXISTS(SELECT 1 FROM commit.honeycomb_environments WHERE
  app_id !~ '^([a-z0-9_-]{3,50}>)?[a-z][a-z0-9_-]{0,79}$'
  OR (position('>' IN app_id)>0 AND split_part(app_id,'>',1)<>org_id)) THEN
  RAISE EXCEPTION 'invalid application identity or ownership; resolve against IAM/Honeycomb inventory';
 END IF;
END $$;
-- These tables are migration evidence, not accepted aliases for actor authentication.
CREATE TABLE commit_private.public_id_application_map AS
 SELECT 'testing'::text source, environment_id,iam_app_id old_id,
 CASE WHEN position('>' IN iam_app_id)>0 THEN split_part(iam_app_id,'>',2) ELSE iam_app_id END new_id
 FROM commit.testing_environments WHERE iam_app_id IS NOT NULL
 UNION ALL SELECT 'honeycomb',environment_id,app_id,
 CASE WHEN position('>' IN app_id)>0 THEN split_part(app_id,'>',2) ELSE app_id END
 FROM commit.honeycomb_environments;
REVOKE ALL ON commit_private.public_id_application_map FROM PUBLIC;
DO $$ BEGIN
 IF EXISTS(SELECT 1 FROM commit_private.public_id_application_map
  GROUP BY environment_id,new_id HAVING count(DISTINCT old_id) FILTER(WHERE position('>' IN old_id)>0)>1) THEN
  RAISE EXCEPTION 'public application ID collision; coordinate authoritative IAM mapping before cutover';
 END IF;
END $$;
ALTER TABLE commit.projects ADD COLUMN legacy_uid text;
CREATE UNIQUE INDEX projects_legacy_uid_idx ON commit.projects(organization_id,legacy_uid) WHERE legacy_uid IS NOT NULL;
COMMENT ON COLUMN commit.projects.legacy_uid IS 'Immutable pre-public-ID-cutover URL alias; only project lookup accepts it.';
ALTER TABLE commit.projects DISABLE TRIGGER projects_preserve_identity;
ALTER TABLE commit.projects DISABLE TRIGGER projects_touch_version;
UPDATE commit.projects p SET legacy_uid=p.uid,
 uid=p.slug||':'||m.new_id||':'||substring(p.uid FROM '([0-9]+)$')
FROM commit_private.public_id_schema_map m
WHERE m.organization_id=p.organization_id AND m.principal_id=p.created_by_principal_id AND m.old_id<>m.new_id;
ALTER TABLE commit.projects ENABLE TRIGGER projects_touch_version;
ALTER TABLE commit.projects ENABLE TRIGGER projects_preserve_identity;
ALTER TABLE commit.actor_projection DISABLE TRIGGER actor_projection_preserve_identity;
UPDATE commit.actor_projection a SET actor_id=m.new_id,membership_id=m.new_membership_id
FROM commit_private.public_id_schema_map m WHERE m.organization_id=a.organization_id AND m.principal_id=a.principal_id;
ALTER TABLE commit.actor_projection ENABLE TRIGGER actor_projection_preserve_identity;
UPDATE commit.testing_environments e SET iam_app_id=m.new_id
FROM commit_private.public_id_application_map m WHERE m.source='testing' AND m.environment_id=e.environment_id;
UPDATE commit.honeycomb_environments e SET app_id=m.new_id
FROM commit_private.public_id_application_map m WHERE m.source='honeycomb' AND m.environment_id=e.environment_id;
ALTER TABLE commit.testing_environments DROP CONSTRAINT testing_iam_application_credentials_complete;
ALTER TABLE commit.testing_environments ADD CONSTRAINT testing_iam_application_credentials_complete CHECK(
 (iam_app_id IS NULL AND iam_app_secret_ciphertext IS NULL) OR
 (iam_app_id IS NOT NULL AND iam_app_id ~ '^[a-z][a-z0-9_-]{0,79}$'
  AND iam_app_secret_ciphertext IS NOT NULL AND octet_length(iam_app_secret_ciphertext)>=28));
ALTER TABLE commit.honeycomb_environments ADD CONSTRAINT honeycomb_app_id_format CHECK(app_id ~ '^[a-z][a-z0-9_-]{0,79}$');
CREATE OR REPLACE FUNCTION commit_private.prevent_project_identity_change()
RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN
 IF NEW.id<>OLD.id OR NEW.organization_id<>OLD.organization_id OR NEW.slug<>OLD.slug OR NEW.uid<>OLD.uid
 OR NEW.legacy_uid IS DISTINCT FROM OLD.legacy_uid OR NEW.created_by_principal_id<>OLD.created_by_principal_id
 OR NEW.created_at<>OLD.created_at THEN
  RAISE EXCEPTION 'project tenant, identifiers, creator, and creation time are immutable' USING ERRCODE='23514';
 END IF;
 RETURN NEW;
END $$;
DO $$ DECLARE r record; BEGIN
 FOR r IN SELECT * FROM public_id_security ORDER BY oid LOOP
  IF r.relrowsecurity THEN EXECUTE format('ALTER TABLE ONLY %s ENABLE ROW LEVEL SECURITY',r.oid::regclass); END IF;
 END LOOP;
END $$;
