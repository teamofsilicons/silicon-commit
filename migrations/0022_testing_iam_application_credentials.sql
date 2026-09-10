-- Existing environments require explicit pairing with their imported IAM app.
-- Never copy the deployment's production application secret into a test plane.
ALTER TABLE commit.testing_environments
    ADD COLUMN iam_app_id text,
    ADD COLUMN iam_app_secret_ciphertext bytea,
    ADD CONSTRAINT testing_iam_application_credentials_complete CHECK (
        (iam_app_id IS NULL AND iam_app_secret_ciphertext IS NULL)
        OR (
            iam_app_id IS NOT NULL AND length(iam_app_id) BETWEEN 3 AND 80
            AND iam_app_secret_ciphertext IS NOT NULL
            AND octet_length(iam_app_secret_ciphertext) >= 28
        )
    );
