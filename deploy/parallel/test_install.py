import importlib.util
from pathlib import Path
import unittest
import urllib.parse

spec = importlib.util.spec_from_file_location("installer", Path(__file__).with_name("install.py"))
installer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(installer)

class IsolationTests(unittest.TestCase):
    def fixture(self):
        app = installer.APP
        roles = {"dm": ["migrator", "runtime"], "extend": ["app"], "commit": ["migrator", "api", "worker"]}[app]
        return {"app_id": app, "database": "silicon_" + app + "_accounts",
                "database_host": "db.example.test", "role_passwords": {app + "_accounts_" + r: "a" * 48 for r in roles},
                "app_secret": "test-app-secret", "webhook_secret": "test-webhook-secret",
                "data_key": "test-data-key", "legacy_shared_service_settings": {}}

    def test_legacy_database_is_refused(self):
        value = self.fixture()
        value["database"] = "silicon_" + installer.APP
        with self.assertRaises(ValueError):
            installer.make_settings(value)

    def test_foreign_app_is_refused(self):
        value = self.fixture()
        value["app_id"] = "another-app"
        with self.assertRaises(ValueError):
            installer.make_settings(value)

    def test_legacy_role_is_refused(self):
        value = self.fixture()
        value["role_passwords"][installer.APP + "_api"] = "legacy-password"
        with self.assertRaises(ValueError):
            installer.make_settings(value)

    def test_every_connection_stays_inside_new_store_and_roles(self):
        value = self.fixture()
        api, worker, migration = installer.make_settings(value)
        for environment in [api, worker or {}, migration]:
            for key, item in environment.items():
                if key.endswith("DATABASE_URL"):
                    url = urllib.parse.urlparse(item)
                    self.assertEqual(url.path, "/" + value["database"])
                    self.assertTrue(url.username.startswith(installer.APP + "_accounts_"))
                    self.assertIn("sslmode=verify-full", url.query)
                self.assertNotIn("backend." + installer.APP + ".teamofsilicons.com", item)
        if installer.APP == "commit":
            self.assertNotIn("COMMIT_APP_SECRET", worker)
            self.assertNotIn("COMMIT_ACCOUNTS_WEBHOOK_SECRET", worker)

if __name__ == "__main__":
    unittest.main()
