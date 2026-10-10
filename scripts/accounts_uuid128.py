#!/usr/bin/env python3
"""Checked Accounts UUID backfill. Run only with all app writers stopped.

Dependencies: scripts/accounts_uuid128.requirements.txt. Default is a rollback-only dry run.
The adjacent manifest owns the exact scalar/structured columns, preserving public IDs,
private resource keys, provider keys, prose, signed events and archive payloads.
"""
import argparse
import base64
import csv
import hashlib
import io
import json
import os
from pathlib import Path
import re
import sys
import uuid


def read_mapping(path):
    raw = Path(path).read_bytes()
    rows = csv.DictReader(io.StringIO(raw.decode("utf-8")))
    if rows.fieldnames != ["old_uuid", "new_uuid", "kind"]:
        raise ValueError("CSV header must be exactly old_uuid,new_uuid,kind")
    mapping, targets, kinds = {}, set(), {}
    for line, row in enumerate(rows, 2):
        if None in row or any(row.get(k) is None for k in rows.fieldnames):
            raise ValueError(f"line {line}: expected exactly three fields")
        old, new, kind = (row[k] for k in rows.fieldnames)
        if not old or old != old.strip() or any(c.isspace() or c in ':,/' for c in old):
            raise ValueError(f"line {line}: invalid source ID")
        try:
            parsed = uuid.UUID(new)
        except ValueError as error:
            raise ValueError(f"line {line}: invalid target UUID") from error
        if str(parsed) != new or parsed.version != 4 or parsed.variant != uuid.RFC_4122:
            raise ValueError(f"line {line}: target must be a canonical lowercase UUIDv4")
        if kind not in ("carbon", "silicon") or old == new:
            raise ValueError(f"line {line}: invalid kind or identity mapping")
        if old in mapping or new in targets:
            raise ValueError(f"line {line}: duplicate source or target (merges are forbidden)")
        mapping[old], kinds[old] = new, kind
        targets.add(new)
    if not mapping or set(mapping) & targets:
        raise ValueError("mapping is empty or chains/cycles into another source")
    return mapping, kinds, hashlib.sha256(raw).hexdigest()


# Only identity-bearing keys in explicitly approved structured columns are rewritten.
IDENTITY_KEYS = {"uuid", "account_uuid", "custodian_uuid", "actor_account", "owner_account", "assigned_by_account", "assigned_to_account", "participant_account", "silicon_account", "allowed_account", "created_by_account", "updated_by_account", "added_by_account", "removed_by_account", "deleted_by_account"}


def rewrite_identity_json(value, mapping):
    if isinstance(value, list):
        return [rewrite_identity_json(item, mapping) for item in value]
    if not isinstance(value, dict):
        return value
    typed = value.get("type", value.get("kind")) in ("carbon", "silicon")
    result = {}
    for key, item in value.items():
        if isinstance(item, str) and (key in IDENTITY_KEYS or (typed and key == "id")):
            result[key] = mapping.get(item, item)
        elif key == "membership_id" and isinstance(item, str) and ":" in item:
            app, old = item.split(":", 1)
            result[key] = f"{app}:{mapping[old]}" if old in mapping else item
        else:
            result[key] = rewrite_identity_json(item, mapping)
    return result


def kind_matches(actual, expected, allow_unknown=False):
    return actual == expected or (allow_unknown and actual is None)


def qualify(sql, dotted):
    return sql.SQL(".").join(sql.Identifier(part) for part in dotted.split("."))


def reseal(cur, app, mapping, sql):
    """Authenticate old ciphertext and reseal with the new account identity as AAD."""
    from cryptography.hazmat.primitives.ciphers.aead import AESGCM
    count = 0
    if app == "dm":
        cur.execute("SELECT ctid::text,app_id,actor_id,purpose,nonce,ciphertext FROM dm.ting_proof_grants WHERE actor_id=ANY(%s)", (list(mapping),))
        rows = cur.fetchall()
        if not rows:
            return 0
        import blake3
        try:
            key = base64.urlsafe_b64decode(os.environ["DM_DATA_KEY"].strip().rstrip("=") + "===")
            if len(key) != 32:
                raise ValueError()
        except (KeyError, ValueError) as error:
            raise ValueError("DM_DATA_KEY must be present and decode to32 bytes to migrate held proofs") from error
        cipher = AESGCM(blake3.blake3(key, derive_key_context="silicon-dm ting proof refresh tokens v1").digest())
        for ctid, app_id, old, purpose, nonce, encrypted in rows:
            plain = cipher.decrypt(bytes(nonce), bytes(encrypted), f"{app_id}\x1f{old}\x1f{purpose}".encode())
            nonce = os.urandom(12)
            encrypted = cipher.encrypt(nonce, plain, f"{app_id}\x1f{mapping[old]}\x1f{purpose}".encode())
            cur.execute("UPDATE dm.ting_proof_grants SET nonce=%s,ciphertext=%s WHERE ctid=%s::tid", (nonce, encrypted, ctid))
            count += 1
    elif app == "extend":
        cur.execute("SELECT ctid::text,account_uuid,receiving_app,scopes,token_cipher,refresh_cipher FROM extend.proof_grants WHERE account_uuid=ANY(%s)", (list(mapping),))
        rows = cur.fetchall()
        if not rows:
            return 0
        try:
            key = base64.urlsafe_b64decode(os.environ["EXTEND_DELEGATION_ENCRYPTION_KEY"].strip().rstrip("=") + "===")
            if len(key) != 32:
                raise ValueError()
        except (KeyError, ValueError) as error:
            raise ValueError("EXTEND_DELEGATION_ENCRYPTION_KEY must be present and decode to32 bytes to migrate held proofs") from error
        cipher = AESGCM(key)
        for ctid, old, receiver, scopes, token, refresh in rows:
            sealed = []
            for value in (token, refresh):
                if value is None:
                    sealed.append(None)
                    continue
                value = bytes(value)
                plain = cipher.decrypt(value[:12], value[12:], f"extend-proof:{old}:{receiver}:{scopes}".encode())
                nonce = os.urandom(12)
                sealed.append(nonce + cipher.encrypt(nonce, plain, f"extend-proof:{mapping[old]}:{receiver}:{scopes}".encode()))
            cur.execute("UPDATE extend.proof_grants SET token_cipher=%s,refresh_cipher=%s WHERE ctid=%s::tid", (*sealed, ctid))
            count += 1
    return count


def migrate(database_url, manifest, mapping, kinds, digest, apply=False):
    import psycopg
    from psycopg import sql
    from psycopg.types.json import Jsonb
    app = manifest["app"]
    schema = manifest["schema"]
    report = {"app": app, "mapping_sha256": digest, "dry_run": not apply, "mapping_rows": len(mapping), "changed": {}, "resealed_proofs": 0}
    with psycopg.connect(database_url) as conn:
        with conn.cursor() as cur:
            cur.execute("SET LOCAL lock_timeout='10s'")
            cur.execute("SET LOCAL statement_timeout='5min'")
            cur.execute("SELECT pg_advisory_xact_lock(hashtextextended(%s,0))", (f"{app}:accounts-uuid128",))
            ledger = qualify(sql, f"{schema}.accounts_uuid128_map")
            cur.execute(sql.SQL("CREATE TABLE IF NOT EXISTS {} (old_uuid text PRIMARY KEY,new_uuid text UNIQUE NOT NULL,kind text NOT NULL CHECK(kind IN ('carbon','silicon')),mapping_sha256 text NOT NULL,applied_at timestamptz NOT NULL DEFAULT clock_timestamp())").format(ledger))
            tables = sorted({key.rsplit(".", 1)[0] for key in manifest["columns"] + manifest["json_columns"]} | {f"{schema}.accounts_uuid128_map"})
            cur.execute(sql.SQL("LOCK TABLE {} IN ACCESS EXCLUSIVE MODE").format(sql.SQL(",").join(qualify(sql, table) for table in tables)))
            cur.execute(sql.SQL("SELECT old_uuid,new_uuid,kind FROM {}").format(ledger))
            prior = {old:(new,kind) for old,new,kind in cur.fetchall()}
            for old,new in mapping.items():
                if old in prior and prior[old] != (new,kinds[old]):
                    raise ValueError("mapping conflicts with the persisted app mapping")
            if any(new in set(mapping.values()) and old not in mapping for old,(new,_) in prior.items()):
                raise ValueError("a target already belongs to a different persisted mapping")
            cur.execute(sql.SQL("SELECT uuid,kind::text FROM {} WHERE uuid=ANY(%s)").format(qualify(sql,f"{schema}.accounts")), (list(mapping)+list(mapping.values()),))
            accounts = dict(cur.fetchall())
            for old,new in mapping.items():
                if old in accounts and not kind_matches(accounts[old], kinds[old], manifest.get("allow_unknown_kind", False)):
                    raise ValueError("mapping kind contradicts the existing account")
                if new in accounts and (old in accounts or old not in prior or not kind_matches(accounts[new], kinds[old], manifest.get("allow_unknown_kind", False))):
                    raise ValueError("target account already exists; merging accounts is forbidden")
            cur.execute("CREATE TEMP TABLE uuid_map(old_uuid text PRIMARY KEY,new_uuid text UNIQUE NOT NULL) ON COMMIT DROP")
            cur.executemany("INSERT INTO uuid_map VALUES(%s,%s)", mapping.items())
            # All changes are checked by deferred real FKs before restoring original flags.
            cur.execute("SELECT n.nspname,c.relname,k.conname,k.condeferrable,k.condeferred FROM pg_constraint k JOIN pg_class c ON c.oid=k.conrelid JOIN pg_namespace n ON n.oid=c.relnamespace WHERE k.contype='f' AND n.nspname=ANY(%s)", (manifest["schemas"],))
            constraints=cur.fetchall()
            for ns,table,name,_,_ in constraints:
                cur.execute(sql.SQL("ALTER TABLE {} ALTER CONSTRAINT {} DEFERRABLE INITIALLY DEFERRED").format(qualify(sql,f"{ns}.{table}"),sql.Identifier(name)))
            cur.execute("SET CONSTRAINTS ALL DEFERRED")
            cur.execute("SELECT n.nspname,c.relname,t.tgname,t.tgenabled FROM pg_trigger t JOIN pg_class c ON c.oid=t.tgrelid JOIN pg_namespace n ON n.oid=c.relnamespace WHERE NOT t.tgisinternal AND n.nspname=ANY(%s)", (manifest["schemas"],))
            triggers=cur.fetchall()
            for ns,table,name,_ in triggers:
                cur.execute(sql.SQL("ALTER TABLE {} DISABLE TRIGGER {}").format(qualify(sql,f"{ns}.{table}"),sql.Identifier(name)))
            report["resealed_proofs"] = reseal(cur,app,mapping,sql)
            for dotted in manifest["columns"]:
                table,column=dotted.rsplit(".",1)
                cur.execute(sql.SQL("UPDATE {} t SET {}=m.new_uuid FROM uuid_map m WHERE t.{}=m.old_uuid").format(qualify(sql,table),sql.Identifier(column),sql.Identifier(column)))
                if cur.rowcount:
                    report["changed"][dotted]=cur.rowcount
            for dotted in manifest["json_columns"]:
                table,column=dotted.rsplit(".",1)
                cur.execute(sql.SQL("SELECT ctid::text,{} FROM {} WHERE {} IS NOT NULL").format(sql.Identifier(column),qualify(sql,table),sql.Identifier(column)))
                changed=0
                for ctid,value in cur.fetchall():
                    updated=rewrite_identity_json(value,mapping)
                    if updated != value:
                        cur.execute(sql.SQL("UPDATE {} SET {}=%s WHERE ctid=%s::tid").format(qualify(sql,table),sql.Identifier(column)),(Jsonb(updated),ctid))
                        changed+=1
                if changed:
                    report["changed"][dotted]=changed
            if app == "dm":
                # Restore conversation seals using the exact production framing; group hashes stay stable.
                cur.execute("""WITH canonical AS (
                  SELECT conversation_id,jsonb_agg(jsonb_build_object('type',actor_kind::text,'id',actor_id) ORDER BY actor_kind::text COLLATE "C",actor_id COLLATE "C") AS participants,
                  sha256(string_agg(int8send(octet_length(actor_kind::text)::bigint)||convert_to(actor_kind::text,'UTF8')||int8send(octet_length(actor_id)::bigint)||convert_to(actor_id,'UTF8'),''::bytea ORDER BY actor_kind::text COLLATE "C",actor_id COLLATE "C")) AS hash
                  FROM dm.conversation_participants WHERE conversation_id IN(SELECT DISTINCT p.conversation_id FROM dm.conversation_participants p JOIN uuid_map m ON m.new_uuid=p.actor_id)
                  GROUP BY conversation_id)
                  UPDATE dm.conversations c SET participant_set=x.participants,participant_set_fingerprint=sha256(convert_to(x.participants::text,'UTF8')),participant_set_hash=CASE WHEN c.is_group THEN c.participant_set_hash ELSE x.hash END FROM canonical x WHERE c.id=x.conversation_id AND NOT c.is_group AND c.participant_set IS DISTINCT FROM x.participants""")
                report["conversation_seals"] = cur.rowcount
            # No source identity can remain in a live identity column.
            for dotted in manifest["columns"]:
                table,column=dotted.rsplit(".",1)
                cur.execute(sql.SQL("SELECT EXISTS(SELECT 1 FROM {} t JOIN uuid_map m ON t.{}=m.old_uuid)").format(qualify(sql,table),sql.Identifier(column)))
                if cur.fetchone()[0]:
                    raise ValueError(f"unmigrated identity remains in {dotted}")
            cur.execute("SET CONSTRAINTS ALL IMMEDIATE")
            for ns,table,name,deferrable,deferred in constraints:
                flags="DEFERRABLE INITIALLY DEFERRED" if deferred else "DEFERRABLE INITIALLY IMMEDIATE" if deferrable else "NOT DEFERRABLE"
                cur.execute(sql.SQL("ALTER TABLE {} ALTER CONSTRAINT {} "+flags).format(qualify(sql,f"{ns}.{table}"),sql.Identifier(name)))
            for ns,table,name,enabled in triggers:
                action={"O":"ENABLE", "D":"DISABLE", "R":"ENABLE REPLICA", "A":"ENABLE ALWAYS"}[enabled]
                cur.execute(sql.SQL("ALTER TABLE {} "+action+" TRIGGER {}").format(qualify(sql,f"{ns}.{table}"),sql.Identifier(name)))
            cur.executemany(sql.SQL("INSERT INTO {}(old_uuid,new_uuid,kind,mapping_sha256) VALUES(%s,%s,%s,%s) ON CONFLICT(old_uuid) DO NOTHING").format(ledger),[(old,new,kinds[old],digest) for old,new in mapping.items()])
        if not apply:
            conn.rollback()
    return report


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--file", required=True)
    parser.add_argument("--manifest", default=str(Path(__file__).with_name("accounts_uuid128_manifest.json")))
    parser.add_argument("--database-env",default="DATABASE_URL",help="Name of the environment variable holding the target PostgreSQL DSN")
    parser.add_argument("--apply",action="store_true")
    parser.add_argument("--dry-run",action="store_true")
    parser.add_argument("--writers-stopped",action="store_true",help="Confirm all app API/worker/maintenance writers are stopped for apply")
    args=parser.parse_args()
    if args.apply and (args.dry_run or not args.writers_stopped):
        parser.error("--apply requires --writers-stopped and cannot be combined with --dry-run")
    if args.database_env not in os.environ:
        parser.error(f"{args.database_env} is not set")
    mapping,kinds,digest=read_mapping(args.file)
    manifest=json.loads(Path(args.manifest).read_text())
    report=migrate(os.environ[args.database_env],manifest,mapping,kinds,digest,args.apply)
    print(json.dumps(report,indent=2,sort_keys=True))

if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        # SQL/provider errors may include row values and encrypted tokens: keep diagnostics redacted.
        message=str(error) if isinstance(error,ValueError) else f"{type(error).__name__}: transaction rolled back; inspect the local database constraints and migration configuration"
        print(f"accounts-uuid128: {message}",file=sys.stderr)
        sys.exit(1)
