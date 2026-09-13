CREATE TABLE commit.contract_versions (
 version integer PRIMARY KEY CHECK(version>0),
 status text NOT NULL CHECK(status IN ('active','deprecated','sunset')),
 introduced_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 deprecated_at timestamptz,
 last_request_at timestamptz,
 sunset_at timestamptz,
 requests bigint NOT NULL DEFAULT 0 CHECK(requests>=0),
 CHECK(status='active' OR deprecated_at IS NOT NULL)
);
INSERT INTO commit.contract_versions(version,status) VALUES(1,'active');
REVOKE ALL ON commit.contract_versions FROM PUBLIC;
CREATE FUNCTION commit.admit_contract(p_version integer,p_testing boolean) RETURNS text
LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $$
DECLARE current_status text;
BEGIN
 SELECT status INTO current_status FROM commit.contract_versions WHERE version=p_version FOR UPDATE;
 IF NOT FOUND THEN RETURN 'unsupported'; END IF;
 IF current_status='deprecated' THEN
   UPDATE commit.contract_versions SET status='sunset',sunset_at=clock_timestamp()
   WHERE version=p_version AND greatest(coalesce(last_request_at,introduced_at),deprecated_at)<=clock_timestamp()-interval '7 days';
 END IF;
 UPDATE commit.contract_versions SET requests=requests+CASE WHEN p_testing THEN 0 ELSE 1 END,last_request_at=CASE WHEN p_testing THEN last_request_at ELSE clock_timestamp() END
 WHERE version=p_version AND status<>'sunset' RETURNING status INTO current_status;
 RETURN coalesce(current_status,'sunset');
END;
$$;
CREATE FUNCTION commit.sunset_idle_contracts() RETURNS void
LANGUAGE sql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $$
 UPDATE commit.contract_versions SET status='sunset',sunset_at=clock_timestamp() WHERE status='deprecated' AND greatest(coalesce(last_request_at,introduced_at),deprecated_at)<=clock_timestamp()-interval '7 days';
$$;
REVOKE ALL ON FUNCTION commit.admit_contract(integer,boolean),commit.sunset_idle_contracts() FROM PUBLIC;
