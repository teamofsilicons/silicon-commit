-- Evolve attachment storage from Briefcase-only URLs to canonical HTTPS URLs
-- supplied by any image provider. Provider classification remains an
-- application-domain concern because PostgreSQL is not a WHATWG URL parser.

ALTER TABLE commit.todo_attachments
    DROP CONSTRAINT todo_attachments_permanent_https_url;

ALTER TABLE commit.todo_attachments
    RENAME COLUMN permanent_url TO url;

ALTER TABLE commit.todo_attachments
    RENAME CONSTRAINT todo_attachments_organization_id_todo_id_permanent_url_key
    TO todo_attachments_organization_id_todo_id_url_key;

ALTER TABLE commit.todo_attachments
    ADD CONSTRAINT todo_attachments_https_url CHECK (
        url = btrim(url)
        AND octet_length(url) BETWEEN 9 AND 2048
        AND url ~ '^https://'
        AND url !~ '[[:cntrl:]]'
        AND strpos(url, '#') = 0
    );

COMMENT ON TABLE commit.todo_attachments IS
    'Ordered, unique canonical HTTPS attachment URLs from any image provider.';
COMMENT ON COLUMN commit.todo_attachments.url IS
    'Provider-neutral HTTPS URL; only configured Briefcase entry URLs support temporary-URL generation.';
