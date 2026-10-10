#!/usr/bin/env python3
"""Tests for the local Silicon Accounts development stack and the end-to-end harness: no network, no processes."""

import os
from pathlib import Path
import re
import sys
import unittest
from unittest import mock

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent / "tests" / "e2e"))
import dev_accounts  # noqa: E402

BASE_ENV = {"COMMIT_APP_SECRET": "sa_app_commit_test", "PATH": "/usr/bin:/bin", "HOME": "/tmp"}


def config(**overrides):
    with mock.patch.dict(os.environ, {**BASE_ENV, **overrides}, clear=True):
        return dev_accounts.load_config()


class Configuration(unittest.TestCase):
    def test_defaults_use_the_local_stack_and_the_port_block(self):
        cfg = config()
        self.assertEqual(cfg["accounts_url"], "http://localhost:9590")
        self.assertEqual(cfg["api_url"], "http://127.0.0.1:4141")
        self.assertEqual(cfg["webhook_url"], "http://127.0.0.1:4141/webhook/")
        self.assertEqual(cfg["database"], "commit_e2e")

    def test_a_deployed_silicon_accounts_is_refused(self):
        for name in ("ACCOUNTS_URL", "ACCOUNTS_API_URL"):
            with self.assertRaisesRegex(dev_accounts.Failure, "never talks to a deployed"):
                config(**{name: "https://accounts.teamofsilicons.com"})

    def test_a_remote_database_is_refused(self):
        with self.assertRaisesRegex(dev_accounts.Failure, "on this machine"):
            config(COMMIT_DEV_DATABASE_URL="postgres://commit@db.example.com:5432/commit_e2e")

    def test_without_a_secret_it_says_where_to_find_one(self):
        with mock.patch.dict(os.environ, {"PATH": "/usr/bin"}, clear=True):
            with self.assertRaisesRegex(dev_accounts.Failure, "COMMIT_TEST_STACK"):
                dev_accounts.load_config()

    def test_only_commit_databases_are_managed(self):
        cfg = config(COMMIT_DEV_DATABASE_URL="postgres://postgres@127.0.0.1:5460/remind_e2e")
        with self.assertRaisesRegex(dev_accounts.Failure, "commit_"):
            dev_accounts.ensure_database(cfg, fresh=True)

    def test_the_default_proof_issuers_match_the_cutover_runbook(self):
        runbook = (HERE.parent / "docs" / "migration" / "cutover.md").read_text()
        block = runbook[runbook.index('COMMIT_PROOF_ISSUERS=$(printf'):]
        block = block[:block.index("| sed")]
        listed = re.findall(r"commit\.[a-z_]+\.[a-z_]+", block)
        self.assertEqual(listed, list(dev_accounts.INTERFACE_ACTIONS))
        self.assertEqual(config()["proof_issuers"],
                         ",".join(f"{action}=interface" for action in listed))


class ServiceEnvironment(unittest.TestCase):
    def test_live_mail_and_telemetry_keys_never_reach_the_service(self):
        cfg = config(COMMIT_POSTMARK_SERVER_TOKEN="live-postmark", COMMIT_TELEMETRY_TABLE_KEY="table-committelemetry-x")
        with mock.patch.dict(os.environ, {**BASE_ENV, "COMMIT_POSTMARK_SERVER_TOKEN": "live-postmark",
                                          "COMMIT_TELEMETRY_TABLE_KEY": "table-committelemetry-x"}, clear=True):
            env = dev_accounts.service_env(cfg, "whsec_test")
        self.assertEqual(env["COMMIT_POSTMARK_SERVER_TOKEN"], "")
        self.assertEqual(env["COMMIT_TELEMETRY_TABLE_KEY"], "")
        self.assertEqual(env["COMMIT_TELEMETRY"], "off")
        self.assertEqual(env["COMMIT_ACCOUNTS_WEBHOOK_SECRET"], "whsec_test")
        self.assertNotIn("live-postmark", env.values())

    def test_the_migrator_gets_no_app_secret(self):
        env = dev_accounts.service_env(config(), "whsec_test", migrator=True)
        self.assertNotIn("COMMIT_APP_SECRET", env)
        self.assertNotIn("COMMIT_ACCOUNTS_WEBHOOK_SECRET", env)
        self.assertEqual(env["COMMIT_SCHEMA_OWNER"], "postgres")


class Redaction(unittest.TestCase):
    def test_tokens_never_reach_the_transcript(self):
        with mock.patch.dict(os.environ, BASE_ENV, clear=True):
            import accounts_e2e
        jwt = "eyJhbGciOiJFZERTQSJ9.eyJzdWIiOiJ6UW8ifQ.c2lnbmF0dXJl"
        record = {
            "access_token": jwt, "nested": [{"proof_token": "sap_abc", "note": f"Bearer {jwt} and sapr_def"}],
            "stk": "stk-1234", "text": "slt_xyz then whsec_abc and sar_123", "uuid": "zQo", "id": "c:ada",
            "stderr": "Wrong STK stk-3274ee6aa473 for si:scout (app secret sa_app_commit_abc)",
        }
        cleaned = accounts_e2e.redact(record)
        flat = repr(cleaned)
        for secret in (jwt, "sap_abc", "sapr_def", "stk-1234", "slt_xyz", "whsec_abc", "sar_123",
                       "stk-3274ee6aa473", "sa_app_commit_abc"):
            self.assertNotIn(secret, flat)
        self.assertEqual((cleaned["uuid"], cleaned["id"]), ("zQo", "c:ada"))


if __name__ == "__main__":
    unittest.main()
