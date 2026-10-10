# Public ID migration (0032)

Coordinate this release with IAM 0118, Honeycomb, the SDK 4 clients, and other consumers. Carbon `alice` becomes `c:alice`; Silicon `chef:shared` becomes `si:chef`; `shared>commit` becomes `commit`. Organization ownership remains a separate value. Actor membership becomes `c:alice[shared]` or `si:chef[shared]`. Bundle IDs retain their existing namespace.

1. Snapshot the database, deployment configuration, encrypted-secret keys, and binaries. Inventory IAM's authoritative `iam_private.public_id_schema_map` export separately for production and each testing world. Match Commit projections to that inventory before starting. Formerly organization-scoped Silicon/application handles can collide; resolve those identities explicitly across services before migration. Never silently merge, truncate or suffix them.
2. Stop incoming writes and all workers. Drain pending/in-flight outbox deliveries, finish Honeycomb operations, and wait for the last idempotency replay window to expire (normally 24 hours). Migration 0032 rejects any remaining work. Do not delete live records to bypass these guards.
3. Run the normal `commit-migrate` command with the schema owner. The migration locks the tables and runs transactionally. It updates actor/membership projections and testing/Honeycomb app identifiers in production and all testing worlds. Any malformed source or collision aborts the transaction. The migration also rejects unexpected trigger modes; its three identity/version guards must already be ordinarily enabled and are restored in that same mode. Restricted `commit_private.public_id_schema_map` and `public_id_application_map` retain the exact mapping for audit and comparison.
4. Change `COMMIT_IAM_APP_ID` and Honeycomb participant registry app IDs to bare handles. Deploy the coordinated IAM/consumer versions and then restart API/workers. Existing private principal, organization, project, todo, participant and history UUIDs are preserved. Credentials retain their encrypted bytes: Commit encrypts these secrets without public app/actor IDs as associated data. Existing client secrets can be retained with the new app ID; refresh/reconnect clients to discard old identity-bearing session claims.
5. Verify online IAM authorization for Carbon and Silicon, project lookup by both new UID and retained old URL, testing environment access, and notification delivery. Project public UIDs gain the new creator ID; `legacy_uid` preserves each old URL as an immutable project-only lookup alias. Actor authentication accepts only new canonical IDs. History snapshots, webhook inbox bytes/hashes, Honeycomb receipts, and expired idempotency bytes remain historical evidence and are not rewritten.

Do not roll back only a binary after migration. Before traffic resumes, restore the coordinated database/configuration snapshots and old binaries together. After new writes, use a reviewed forward fix or an explicit mapping-aware recovery plan; new handles and newly created records may have no old equivalent. Keep the mapping and old encryption keys until the recovery window closes. Never infer an organization from a new Silicon or application ID.

Regression check (creates and removes an isolated database):

```sh
COMMIT_TEST_DATABASE_URL=postgresql://localhost/postgres cargo test --test postgres_public_id_migration
```

The test covers production plus two testing worlds, collision and live-replay rollback, exact retained keys/history/ciphertext/security, project aliases, and restored identity immutability.
