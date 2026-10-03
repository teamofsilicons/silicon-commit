# Public identifier cutover — 2026-09-23

Commit 0.3.0 API and worker are live at `f15f944932f5a85b89dcee7829ad228e88178b83`, image `sha256:b947fb4662b69788ece78e5f8beec784b24c9779b8e7e7cdcd203b180deb30f3`. Public readiness and version checks passed. Vercel deployment `dpl_6kMxaUidFdSw7BGWNJXtL6f8pQ8R` serves the production frontend. Its persistent production `COMMIT_APP_ID` is now `commit`.

A fresh stopped-service PostgreSQL 18 dump, container configuration, environment files, CA and runtime secret were uploaded to the private versioned backup bucket with AES256, length and SHA256 verification. Migration 32 ran only after exact replay-expiry rehearsal and the authorized expiry of frozen outstanding replay records. Original replay payloads and hashes remained unchanged; six production identity projections exactly matched IAM's final mapping. The migrator's temporary database privilege was restored to its prior state. Activation receipt: SSM `bc775023-cc50-4456-96c8-765eece7160e`; successful migration receipt: `e8f11c7f-fdc8-4bf5-b5fe-133e8185b106`.

Only the runtime secret's app ID and audience changed; the other 12 keys were retained. New Docker containers retain `unless-stopped` restart policies. The live CloudFormation user data only provisions Docker and directories, so it has no stale application image pointer. Do not start an old image against migration 32; recovery requires a coherent database/configuration backup.

Fresh production login authenticated `c:saket` in `tos`; an authenticated todo read passed. This does not claim the previously expired local CLI session was renewed. GitHub native CI 35858418656 and full CI 35858423788 passed. All four GitHub release asset digests were checked before publishing v0.3.0. Honeycomb prod release `93e39e91-5683-482a-bfbe-473e34a43463` has package SHA256 `0170b36b35de6f1cba28d7c8868bca290a58785ed38de2cacf9e9c2a7513911e`. An anonymous clean-home installation reported `commit 0.3.0`. The installation's temporary updater was stopped and removed after verification.

Both silicon-commit-client 0.3.0 and silicon-commit-cli 0.3.0 were published successfully to crates.io after Cargo package verification.
