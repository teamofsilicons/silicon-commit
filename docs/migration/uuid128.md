# Accounts UUID128 cutover consumer

The service accepts canonical lowercase UUIDs alongside existing legacy Accounts keys. Accounts owns issuance and exports one immutable CSV with exactly `old_uuid,new_uuid,kind`; `kind` is `carbon` or `silicon`. Use the same export for every app. Public c:/si: IDs and app resource IDs do not change.

## Operator sequence

1. Deploy the compatibility schema and code before moving data. Stop every API, worker, CLI maintenance process and incoming webhook writer for the coordinated apply. Back up the database and keep the original application encryption keys.
2. Install `scripts/accounts_uuid128.requirements.txt` in an isolated Python environment. Supply `DATABASE_URL` through the environment. DM also requires its original `DM_DATA_KEY` for held Ting proofs; Extend requires its original `EXTEND_DELEGATION_ENCRYPTION_KEY` for held delegation proofs.
3. Run `python scripts/accounts_uuid128.py --file /absolute/path/accounts-uuid128.csv --dry-run`. This executes and validates the entire transaction, including real foreign keys, then rolls back.
4. Run the identical file with `--apply --writers-stopped`. Check the reported mapping hash against the Accounts export. Rerunning the same file is idempotent; conflicting sources, targets, kinds, chains and account merges are refused.
5. Start the migrated services only after all linked apps and Accounts are ready. Old JWT/proof subjects listed in the ledger are refused before any account can be recreated. Sign in again with the new identity and check existing resources. Native device IDs, credentials and provider URLs remain stable.

The exact scalar and JSON manifest is `scripts/accounts_uuid128_manifest.json`; no blanket string replacement is performed. Approved JSON rewrites identity fields and typed account references only, never prose. The persisted `commit.accounts_uuid128_map` is both the replay ledger and retired-subject fence. Mapping is one transaction with exclusive table locks, temporary deferred foreign keys, restoration of trigger/constraint flags and a final check that no old identities remain in live scalar columns.

DM authenticates and reseals held Ting proof grants with the new account AAD, then rebuilds direct-conversation participant seals. Extend authenticates and reseals both delegation proof tokens and refresh grants. The original secrets and plaintext survive. Signed outgoing request bodies, archive payloads, foreign provider identifiers, resource keys and object paths remain untouched. Their recipient metadata may change; a previously signed body never changes under the same event ID.

Rollback requires the coordinated pre-cutover database backup and matching Accounts/app state. Do not invert the mapping into a live system or restart an old binary against partially migrated data. Production has not been changed by this work.

## Local evidence, 10 October 2026

A populated PostgreSQL clone with 307 cached accounts passed rollback-only dry-run, apply, and idempotent reapply. 0 held proof grants were resealed; decrypted values matched their originals. Private resource IDs, provider/artifact paths, native credential digests, and signed outgoing/archive bodies were compared before and after and preserved. A conflicting mapping was refused in the shared contract tests. Sanitized reports are kept locally in Commit's `.mig/uuid-proof/results.json` and `.mig/uuid-preservation.log`; maps, database snapshots, keys and tokens are intentionally untracked.

The pure contract suite covers exact CSV shape, canonical UUIDv4, duplicate/collision rejection, typed JSON boundaries and manifest-controlled unknown-kind tombstones. Service regressions exercise canonical UUID accounts and rejection of retired subjects without inserting an old account.
