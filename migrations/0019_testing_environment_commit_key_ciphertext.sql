-- Keep the generated Commit test key recoverable for authorized operators.
ALTER TABLE commit.testing_environments
    ADD COLUMN key_ciphertext bytea;
