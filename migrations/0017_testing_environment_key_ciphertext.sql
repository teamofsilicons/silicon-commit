-- Encrypt the IAm root key at rest so Commit can safely forward it only to IAm.
ALTER TABLE commit.testing_environments
    ADD COLUMN iam_test_key_ciphertext bytea;

UPDATE commit.testing_environments
SET iam_test_key_ciphertext = decode('', 'hex')
WHERE iam_test_key_ciphertext IS NULL;

ALTER TABLE commit.testing_environments
    ALTER COLUMN iam_test_key_ciphertext SET NOT NULL;
