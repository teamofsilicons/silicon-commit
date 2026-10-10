#!/usr/bin/env python3
"""The host-side cutover helper (queue check and identity linking); every external process is mocked."""
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch


spec = importlib.util.spec_from_file_location('commit_cutover', Path(__file__).with_name('cutover.py'))
cutover = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cutover)

IMAGE = '234951665042.dkr.ecr.us-east-1.amazonaws.com/silicon-commit@sha256:' + 'a' * 64


class CutoverTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp())
        (self.root / 'rds-ca.pem').write_text('test-ca')
        self.mapping = self.root / 'mapping.csv'
        self.mapping.write_text('iam_principal_id,accounts_uuid,org_id\nc:ada,zQo,tos\n')
        self.secret = {'db_migrator_password': 'test-migrator', 'COMMIT_APP_SECRET': 'test-app-secret',
                       'ACCOUNTS_URL': 'https://accounts.example.test', 'db_api_password': 'never-copied',
                       'COMMIT_IAM_APP_SECRET': 'never-copied'}
        self.runs = []

    def exercise(self, *arguments, fail=False):
        def check_output(args, **kwargs):
            self.assertEqual(args[:3], ['aws', 'secretsmanager', 'get-secret-value'])
            return json.dumps({'SecretString': json.dumps(self.secret)}).encode()

        def run(args, **kwargs):
            envfile = Path(args[args.index('--env-file') + 1])
            self.runs.append((list(args), envfile.read_text(), oct(envfile.stat().st_mode & 0o777)))
            if fail:
                raise subprocess.CalledProcessError(1, args)

        with patch.object(cutover.subprocess, 'check_output', side_effect=check_output), \
                patch.object(cutover.subprocess, 'run', side_effect=run):
            cutover.main('arn:test', 'db.example.test', *arguments, root=self.root)

    def test_queues_lists_waiting_work_as_the_migrator_without_the_app_secret(self):
        self.exercise('queues')
        args, environment, mode = self.runs[0]
        self.assertEqual(args[args.index('postgres:18') + 1:args.index('-c')], ['psql', '-X', '-v', 'ON_ERROR_STOP=1'])
        query = args[-1]
        self.assertIn("FROM commit.outbox_events WHERE status IN ('pending', 'in_flight')", query)
        self.assertIn("FROM commit.email_jobs WHERE status = 'pending'", query)
        self.assertEqual(mode, '0o600')
        self.assertIn('PGUSER=commit_migrator\nPGPASSWORD=test-migrator\nPGSSLMODE=verify-full\n', environment)
        self.assertNotIn('test-app-secret', environment)
        self.assertFalse((self.root / 'cutover.env').exists())

    def test_plan_runs_the_template_command_with_lookups(self):
        self.exercise(IMAGE, 'plan')
        args, environment, mode = self.runs[0]
        self.assertEqual(args[-3:], ['commit-migrate', 'link-identities', '--plan'])
        self.assertNotIn('/mapping.csv:ro', ' '.join(args))
        self.assertEqual(mode, '0o600')
        self.assertIn('COMMIT_APP_SECRET=test-app-secret\n', environment)
        self.assertIn('ACCOUNTS_URL=https://accounts.example.test\n', environment)
        self.assertIn('COMMIT_MIGRATOR_DATABASE_URL=postgres://commit_migrator:test-migrator@db.example.test:5432/'
                      'silicon_commit?sslmode=verify-full&sslrootcert=/rds-ca.pem\n', environment)
        for leaked in ('never-copied', 'db_api_password', 'COMMIT_IAM'):
            self.assertNotIn(leaked, environment)
        self.assertFalse((self.root / 'cutover.env').exists())

    def test_dry_run_and_apply_mount_the_mapping_read_only(self):
        self.exercise(IMAGE, 'dry-run', str(self.mapping))
        self.exercise(IMAGE, 'apply', str(self.mapping))
        (dry, _, _), (apply, _, _) = self.runs
        self.assertIn(f'{self.mapping.resolve()}:/mapping.csv:ro', dry)
        self.assertEqual(dry[-5:], ['commit-migrate', 'link-identities', '--dry-run', '--file', '/mapping.csv'])
        self.assertEqual(apply[-4:], ['commit-migrate', 'link-identities', '--file', '/mapping.csv'])
        self.assertIn(IMAGE, apply)

    def test_the_environment_file_is_removed_when_the_command_fails(self):
        with self.assertRaises(subprocess.CalledProcessError):
            self.exercise(IMAGE, 'apply', str(self.mapping), fail=True)
        self.assertEqual(len(self.runs), 1)
        self.assertFalse((self.root / 'cutover.env').exists())

    def test_refuses_unsafe_or_incomplete_requests_before_reading_secrets(self):
        for arguments, message in [
            ((IMAGE, 'apply'), 'needs the mapping file'),
            ((IMAGE, 'apply', str(self.root / 'missing.csv')), 'is not a file'),
            ((IMAGE, 'plan', str(self.mapping)), 'takes no mapping file'),
            ((IMAGE, 'link'), 'Unknown action'),
            (('silicon-commit:latest', 'plan'), 'immutable image reference'),
            (('queues', 'extra'), 'takes no other arguments'),
            ((), 'Name an action'),
            ((IMAGE,), 'Usage'),
        ]:
            with self.subTest(arguments=arguments), \
                    patch.object(cutover.subprocess, 'check_output') as check_output, \
                    self.assertRaisesRegex(ValueError, message):
                cutover.main('arn:test', 'db.example.test', *arguments, root=self.root)
            check_output.assert_not_called()

    def test_refuses_a_secret_without_the_app_secret_when_linking(self):
        del self.secret['COMMIT_APP_SECRET']
        with self.assertRaisesRegex(ValueError, 'lacks COMMIT_APP_SECRET'):
            self.exercise(IMAGE, 'plan')
        self.assertEqual(self.runs, [])
        self.exercise('queues')  # the queue check needs only the migrator password
        self.assertEqual(len(self.runs), 1)

    def test_requires_the_bootstrap_certificate(self):
        (self.root / 'rds-ca.pem').unlink()
        for arguments in (('queues',), (IMAGE, 'plan')):
            with self.subTest(arguments=arguments), self.assertRaisesRegex(ValueError, 'run bootstrap.py first'):
                self.exercise(*arguments)


if __name__ == '__main__':
    unittest.main()
