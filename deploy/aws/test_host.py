#!/usr/bin/env python3
"""The SSM helper; the AWS CLI is mocked."""
import base64
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch


spec = importlib.util.spec_from_file_location('commit_host', Path(__file__).with_name('host.py'))
host = importlib.util.module_from_spec(spec)
spec.loader.exec_module(host)


class FakeSsm:
    def __init__(self, statuses=('InProgress', 'Success'), output='done\n'):
        self.scripts, self.statuses, self.output, self.polls = [], list(statuses), output, 0

    def check_output(self, args, **kwargs):
        assert args[:5] == ['aws', '--profile', 'silicon-production', '--region', 'us-east-1'], args
        assert args[5:7] == ['ssm', 'send-command'], args
        self.scripts.append(json.loads(args[args.index('--parameters') + 1])['commands'][0])
        return json.dumps({'Command': {'CommandId': f'cmd-{len(self.scripts)}'}}).encode()

    def run(self, args, **kwargs):
        self.polls += 1
        if self.polls == 1:
            return subprocess.CompletedProcess(args, 255, '', 'InvocationDoesNotExist')
        status = self.statuses.pop(0) if len(self.statuses) > 1 else self.statuses[0]
        body = {'Status': status, 'StandardOutputContent': self.output, 'StandardErrorContent': 'boom\n'}
        return subprocess.CompletedProcess(args, 0, json.dumps(body), '')


class HostTests(unittest.TestCase):
    def fake(self, **kwargs):
        ssm = FakeSsm(**kwargs)
        patches = [patch.object(host.subprocess, 'check_output', side_effect=ssm.check_output),
                   patch.object(host.subprocess, 'run', side_effect=ssm.run)]
        for item in patches:
            item.start()
            self.addCleanup(item.stop)
        return ssm

    def test_run_waits_for_the_result_and_returns_its_output(self):
        ssm = self.fake()
        self.assertEqual(host.run('docker ps', sleep=lambda _: None), 'done\n')
        self.assertEqual(ssm.scripts, ['docker ps'])

    def test_a_failed_command_raises_with_its_output(self):
        self.fake(statuses=('Failed',))
        with self.assertRaisesRegex(host.HostError, '(?s)Failed .*boom'):
            host.run('false', sleep=lambda _: None)

    def test_copy_sends_chunks_and_installs_after_checking_the_digest(self):
        ssm = self.fake()
        data = bytes(range(256)) * 400  # more than one chunk once encoded
        with tempfile.NamedTemporaryFile() as local:
            local.write(data)
            local.flush()
            with patch.object(host, 'CHUNK', 40000):
                digest = host.copy(local.name, '/opt/commit/mapping.csv', sleep=lambda _: None)
        self.assertEqual(digest, hashlib.sha256(data).hexdigest())
        chunks = [script.split("'")[1] for script in ssm.scripts[1:-1]]
        self.assertGreater(len(chunks), 1)
        self.assertEqual(base64.b64decode(''.join(chunks)), data)
        final = ssm.scripts[-1]
        self.assertIn(f"echo '{digest}  /opt/commit/mapping.csv.upload' | sha256sum -c -", final)
        self.assertTrue(final.endswith('chmod 600 /opt/commit/mapping.csv.upload; '
                                       'mv /opt/commit/mapping.csv.upload /opt/commit/mapping.csv'))

    def test_copy_refuses_paths_outside_the_commit_directory_and_bad_modes(self):
        self.fake()
        with tempfile.NamedTemporaryFile() as local:
            for remote in ('/etc/cron.d/commit', '/opt/commit/sub/file', '/opt/commit/..', "/opt/commit/a'b"):
                with self.subTest(remote=remote), self.assertRaisesRegex(host.HostError, 'directly under'):
                    host.copy(local.name, remote, sleep=lambda _: None)
            with self.assertRaisesRegex(host.HostError, 'three octal digits'):
                host.copy(local.name, '/opt/commit/bootstrap.py', mode='0644; reboot', sleep=lambda _: None)


if __name__ == '__main__':
    unittest.main()
