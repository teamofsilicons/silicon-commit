#!/usr/bin/env python3
"""Copy a file to the Commit host, or run a command there as root, through AWS Systems Manager.

    python3 deploy/aws/host.py copy LOCAL_FILE /opt/commit/NAME [--mode 600]
    python3 deploy/aws/host.py run 'COMMAND' [--timeout SECONDS]

Uses the AWS profile silicon-production (AWS_PROFILE overrides it) and the instance named in
deploy/aws/README.md (COMMIT_INSTANCE_ID overrides it). `run` prints the command's output and exits 1 when it
fails. `copy` sends the file in base64 chunks, as deploy_docs.py does, checks its SHA-256 on the host and
installs it root-owned under /opt/commit. Never put a secret in a command: bootstrap.py and link_identities.py
read theirs from Secrets Manager on the host.
"""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shlex
import subprocess
import sys
import time

INSTANCE = os.environ.get('COMMIT_INSTANCE_ID', 'i-0bd8d9688a5a5d252')
AWS = ['aws', '--profile', os.environ.get('AWS_PROFILE', 'silicon-production'), '--region', 'us-east-1']
CHUNK = 40000
HOST_DIRECTORY = PurePosixPath('/opt/commit')


class HostError(Exception):
    """The SSM command failed or timed out; the message carries its output."""


def run(script, timeout=180, sleep=time.sleep):
    """Run a shell script as root on the host and return its standard output."""
    payload = json.dumps({'commands': [script], 'executionTimeout': [str(timeout)]})
    sent = json.loads(subprocess.check_output(AWS + [
        'ssm', 'send-command', '--instance-ids', INSTANCE, '--document-name', 'AWS-RunShellScript',
        '--parameters', payload, '--output', 'json']))
    command_id = sent['Command']['CommandId']
    deadline = time.monotonic() + timeout + 60
    while time.monotonic() < deadline:
        sleep(3)
        answer = subprocess.run(AWS + ['ssm', 'get-command-invocation', '--command-id', command_id,
                                       '--instance-id', INSTANCE, '--output', 'json'],
                                capture_output=True, text=True, check=False)
        if answer.returncode:
            continue  # the invocation is not registered yet
        result = json.loads(answer.stdout)
        if result['Status'] in ('Pending', 'InProgress', 'Delayed'):
            continue
        if result['Status'] != 'Success':
            raise HostError(f"{result['Status']} (SSM command {command_id}):\n"
                            f"{result.get('StandardOutputContent', '')}{result.get('StandardErrorContent', '')}")
        return result.get('StandardOutputContent', '')
    raise HostError(f'SSM command {command_id} did not finish within {timeout + 60} seconds')


def copy(local, remote, mode='600', sleep=time.sleep):
    """Install a local file at REMOTE (under /opt/commit) with the given octal mode, checked by SHA-256."""
    remote = PurePosixPath(remote)
    if remote.parent != HOST_DIRECTORY or not re.fullmatch(r'[A-Za-z0-9_-][A-Za-z0-9._-]*', remote.name):
        raise HostError(f'{remote} must be a file directly under {HOST_DIRECTORY} (letters, digits, . _ -)')
    if not (len(mode) == 3 and all(c in '01234567' for c in mode)):
        raise HostError(f'--mode {mode!r} must be three octal digits')
    data = Path(local).read_bytes()
    encoded = base64.b64encode(data).decode()
    digest = hashlib.sha256(data).hexdigest()
    upload = shlex.quote(f'{remote}.upload')
    run(f'umask 077; : > {upload}.b64', sleep=sleep)
    for index in range(0, len(encoded), CHUNK):
        run(f"printf %s '{encoded[index:index + CHUNK]}' >> {upload}.b64", sleep=sleep)
    target = shlex.quote(str(remote))
    run(f'set -eu; umask 077; base64 -d {upload}.b64 > {upload}; rm {upload}.b64; '
        f"echo '{digest}  {remote}.upload' | sha256sum -c - >/dev/null; "
        f'chown root:root {upload}; chmod {mode} {upload}; mv {upload} {target}', sleep=sleep)
    return digest


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest='command', required=True)
    copying = commands.add_parser('copy', help='install a local file under /opt/commit')
    copying.add_argument('local', type=Path)
    copying.add_argument('remote')
    copying.add_argument('--mode', default='600', help='octal mode on the host (default 600)')
    running = commands.add_parser('run', help='run a shell command as root and print its output')
    running.add_argument('script')
    running.add_argument('--timeout', type=int, default=1800, help='seconds (default 1800)')
    args = parser.parse_args(argv)
    try:
        if args.command == 'copy':
            print(f'{args.remote}: sha256 {copy(args.local, args.remote, args.mode)}')
        else:
            print(run(args.script, args.timeout), end='')
    except (HostError, OSError, subprocess.CalledProcessError) as error:
        print(f'host.py: {error}', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
