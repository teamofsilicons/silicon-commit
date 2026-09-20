#!/usr/bin/env python3
"""Deployment ordering and failure recovery; all external processes are mocked."""
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch


spec = importlib.util.spec_from_file_location('commit_bootstrap', Path(__file__).with_name('bootstrap.py'))
bootstrap = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bootstrap)


class BootstrapOrderingTests(unittest.TestCase):
    def exercise(self, running=('commit-api', 'commit-worker'), fail=None, missing_grants=False):
        events = []
        secret = {
            'db_api_password': 'test-api', 'db_worker_password': 'test-worker',
            'db_migrator_password': 'test-migrator', 'db_admin_password': 'test-admin',
            'COMMIT_IAM_APP_SECRET': 'test-iam',
            'COMMIT_TEST_ENVIRONMENT_ENCRYPTION_KEY': 'stable-test-key',
        }

        def check_output(args, **kwargs):
            if args[:3] == ['aws', 'secretsmanager', 'get-secret-value']:
                events.append(('secret', args))
                return json.dumps({'SecretString': json.dumps(secret)}).encode()
            if args[:3] == ['aws', 'ecr', 'get-login-password']:
                events.append(('registry', args))
                return b'test-registry-password'
            if args[:2] == ['docker', 'ps']:
                events.append(('running', args))
                return '\n'.join((*running, 'unrelated-service'))
            raise AssertionError(f'unexpected check_output: {args}')

        def run(args, **kwargs):
            if args[:2] == ['docker', 'pull']:
                stage = 'pull'
            elif args[:2] == ['docker', 'stop']:
                stage = 'stop:' + args[-1]
            elif args[:2] == ['docker', 'start']:
                stage = 'restore'
            elif args[-1] == 'commit-migrate':
                stage = 'migration'
            elif kwargs.get('input') == b'GRANTS':
                stage = 'grants'
            elif kwargs.get('input') == b'CHECKS':
                stage = 'checks'
            elif kwargs.get('input', b'').startswith(b"SELECT 'CREATE ROLE"):
                stage = 'roles'
            elif args[:3] == ['docker', 'run', '-d']:
                stage = 'new:' + args[args.index('--name') + 1]
            else:
                stage = args[1]
            events.append((stage, list(args)))
            if stage == fail:
                raise subprocess.CalledProcessError(1, args)
            return subprocess.CompletedProcess(args, 0)

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            if not missing_grants:
                (root/'postgres_runtime_grants.sql').write_bytes(b'GRANTS')
            (root/'test_runtime_grants.sql').write_bytes(b'CHECKS')
            with patch.object(bootstrap.subprocess, 'check_output', side_effect=check_output), \
                 patch.object(bootstrap.subprocess, 'run', side_effect=run), \
                 patch.object(bootstrap.urllib.request, 'urlopen', return_value=io.BytesIO(b'test-ca')), \
                 patch.object(bootstrap.os, 'chown'), patch.object(bootstrap.os, 'umask'), \
                 patch('sys.stdout', new_callable=io.StringIO):
                error = None
                try:
                    bootstrap.main('test-secret', 'test-db', 'registry.test/commit@sha256:test', root=root)
                except (subprocess.CalledProcessError, FileNotFoundError) as failure:
                    error = failure
            temporary_credentials = [name for name in ('admin.env', 'migrator.env') if (root/name).exists()]
        return events, error, temporary_credentials

    def test_both_old_services_stop_before_migration_and_new_services_follow_grants(self):
        events, error, credentials = self.exercise()
        self.assertIsNone(error)
        stages = [stage for stage, _ in events]
        for before, after in zip(
            ['roles', 'stop:commit-api', 'stop:commit-worker', 'migration', 'grants', 'checks', 'new:commit-api'],
            ['stop:commit-api', 'stop:commit-worker', 'migration', 'grants', 'checks', 'new:commit-api', 'new:commit-worker'],
        ):
            self.assertLess(stages.index(before), stages.index(after))
        self.assertLess(max(i for i, stage in enumerate(stages) if stage == 'pull'), stages.index('roles'))
        self.assertNotIn('restore', stages)
        self.assertEqual(credentials, [])

    def test_prepare_failures_leave_existing_services_running(self):
        for stage in ('pull', 'roles'):
            with self.subTest(stage=stage):
                events, error, _ = self.exercise(fail=stage)
                self.assertIsInstance(error, subprocess.CalledProcessError)
                self.assertFalse(any(name.startswith(('stop:', 'new:')) or name in ('migration', 'restore') for name, _ in events))

    def test_missing_grant_script_fails_before_drain_and_cleans_credentials(self):
        events, error, credentials = self.exercise(missing_grants=True)
        self.assertIsInstance(error, FileNotFoundError)
        self.assertFalse(any(name.startswith('stop:') or name == 'migration' for name, _ in events))
        self.assertEqual(credentials, [])

    def test_failed_second_stop_restores_previously_running_services(self):
        events, error, credentials = self.exercise(fail='stop:commit-worker')
        self.assertIsInstance(error, subprocess.CalledProcessError)
        self.assertIn(('restore', ['docker', 'start', 'commit-api', 'commit-worker']), events)
        self.assertNotIn('migration', [name for name, _ in events])
        self.assertEqual(credentials, [])

    def test_restore_never_starts_a_previously_stopped_service(self):
        events, error, _ = self.exercise(running=('commit-api',), fail='stop:commit-api')
        self.assertIsInstance(error, subprocess.CalledProcessError)
        self.assertIn(('restore', ['docker', 'start', 'commit-api']), events)
        self.assertFalse(any('commit-worker' in args for stage, args in events if stage == 'restore' or stage.startswith('stop:')))

    def test_migration_or_grant_failures_never_restart_old_binaries(self):
        for stage in ('migration', 'grants', 'checks'):
            with self.subTest(stage=stage):
                events, error, credentials = self.exercise(fail=stage)
                self.assertIsInstance(error, subprocess.CalledProcessError)
                stages = [name for name, _ in events]
                self.assertIn('stop:commit-api', stages)
                self.assertIn('stop:commit-worker', stages)
                self.assertFalse(any(name == 'restore' or name.startswith('new:') for name in stages))
                self.assertEqual(credentials, [])

    def test_first_deployment_has_no_old_services_to_stop(self):
        events, error, _ = self.exercise(running=())
        self.assertIsNone(error)
        self.assertFalse(any(stage.startswith('stop:') or stage == 'restore' for stage, _ in events))
        self.assertIn('new:commit-api', [stage for stage, _ in events])
        self.assertIn('new:commit-worker', [stage for stage, _ in events])


if __name__ == '__main__':
    unittest.main()
