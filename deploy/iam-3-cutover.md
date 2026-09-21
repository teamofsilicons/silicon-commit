# IAM 3 consumer cutover

Commit uses `silicon-iam-client` 3.0.0 and authenticates Carbon/Silicon identities by immutable `public_id`. The adapter accepts current IAM 2 responses that include the old extra `principal_id`, and IAM 3 responses without it. A disclosed canonical membership and current authorization snapshot are still required. UUID-only historical membership responses remain unsupported, as in Commit 0.2.1.

`ScopedIdentity` resolves the canonical identity, actor type, membership and selected organization to the existing `actor_projection` row. The UUID in `principal_id` is now a private Commit storage key: it is neither trusted from IAM nor sent to IAM. Existing todos, projects, assignment history and idempotency records retain their row keys; new identities receive private UUIDs. Testing organizations retain their isolated storage namespace. Concurrent first-use requests reserve one row key.

Run migration 0031 before starting this backend. It changes only webhook aggregate metadata to text and documents the private row-key semantics. It does not rewrite event payloads, signatures, hashes, ownership, or credentials. IAM resource UUID aggregate IDs and canonical identity aggregate IDs are both accepted.

Deploy this consumer before IAM 3. Keep this compatible consumer if IAM itself is rolled back. Rolling the consumer back to an older binary after canonical IAM is active is unsupported. An inverse webhook-column migration is only safe before any non-UUID aggregates have arrived, and is unnecessary when retaining this compatible binary.

Validation: IAM adapter tests cover current IAM responses and canonical actor consistency, OBO binding/replay, disclosure, audience and testing isolation. `iam_api_workflow` seeds retained ownership keys and performs authenticated todo/project/task operations and exact idempotent replay through canonical IAM responses. Enable that database test with `COMMIT_TEST_DATABASE_URL` pointing to a disposable PostgreSQL database.
