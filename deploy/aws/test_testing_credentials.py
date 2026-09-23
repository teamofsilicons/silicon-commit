"""Offline checks: no AWS calls and no real secrets."""
import copy
import json
import unittest
from testing_credentials import desired_credentials, TOKEN, REGISTRY

class Provisioning(unittest.TestCase):
    def fixture(self):
        return ({"backend": {REGISTRY:json.dumps([{"app_id":"other","base_url":"https://other.example","token_env":"OTHER_SERVICE_TOKEN"}])}},
                {"COMMIT_IAM_APP_ID":"commit","COMMIT_HONEYCOMB_URL":"https://honeycomb.example","COMMIT_PUBLIC_BASE_URL":"https://commit.example/api/v1/"})
    def test_retry_preserves_token_other_participants_and_input(self):
        honeycomb, commit = self.fixture()
        before = copy.deepcopy((honeycomb, commit))
        first = desired_credentials(honeycomb, commit)
        self.assertEqual((honeycomb, commit), before)
        self.assertEqual(desired_credentials(*first), first)
        self.assertEqual(first[0]["backend"][TOKEN],first[1][TOKEN])
        self.assertEqual(len(json.loads(first[0]["backend"][REGISTRY])),2)
        self.assertEqual(desired_credentials(honeycomb, first[1]), first)
    def test_conflicting_credentials_and_destinations_fail_closed(self):
        honeycomb, commit = desired_credentials(*self.fixture())
        commit[TOKEN]="different-credential-which-is-long-enough"
        with self.assertRaises(ValueError): desired_credentials(honeycomb,commit)
        honeycomb,commit=self.fixture()
        commit["COMMIT_PUBLIC_BASE_URL"]="https://user:secret@commit.example"
        with self.assertRaises(ValueError): desired_credentials(honeycomb,commit)
        honeycomb,commit=self.fixture()
        honeycomb["backend"][REGISTRY]=json.dumps([{"app_id":"other","base_url":"https://other.example","token_env":TOKEN}])
        with self.assertRaises(ValueError): desired_credentials(honeycomb,commit)

if __name__ == "__main__": unittest.main()
