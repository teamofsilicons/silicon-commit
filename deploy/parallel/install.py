#!/usr/bin/env python3
"""Stage an independent Accounts app beside its unchanged IAM deployment.

The public proxy remains in maintenance. Only explicit accounts-prefixed stores,
roles, files and container names are used. No existing service is stopped.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tarfile
import time
import urllib.parse
import urllib.request

APP = 'commit'
ROOT = Path('/opt/silicon-accounts-apps') / APP
BUCKET_PREFIX = f's3://silicon-hook-standalone-artifacts-lxpfsbc0jpuk/parallel-accounts-20261010/{APP}'


def run(args, **kwargs):
    result = subprocess.run(args, capture_output=True, text=True, **kwargs)
    if result.returncode:
        raise RuntimeError(f'{args[0]} failed with exit {result.returncode}')
    return result.stdout


def write_env(path, values):
    if any('\n' in str(v) or '\r' in str(v) for v in values.values()):
        raise ValueError('Environment values must be single line')
    path.write_text(''.join(f'{k}={v}\n' for k, v in values.items()))
    path.chmod(0o600)


def make_settings(secret):
    database = 'silicon_' + APP + '_accounts'
    if secret['app_id'] != APP or secret['database'] != database:
        raise ValueError('Refusing any database outside this Accounts deployment')
    passwords = secret['role_passwords']
    if not all(re.fullmatch(APP + r'_accounts_[a-z]+', role) for role in passwords):
        raise ValueError('Refusing legacy or unrelated roles')

    def dburl(role):
        password = urllib.parse.quote(passwords[role], safe='')
        return f'postgres://{role}:{password}@{secret["database_host"]}:5432/{database}?sslmode=verify-full&sslrootcert=/rds-ca.pem'

    accounts = {'ACCOUNTS_URL': 'https://accounts.teamofsilicons.com',
                APP.upper() + '_APP_SECRET': secret['app_secret'],
                APP.upper() + '_ACCOUNTS_WEBHOOK_SECRET': secret['webhook_secret']}
    shared = secret['legacy_shared_service_settings']
    if APP == 'commit':
        base = {'COMMIT_ENVIRONMENT': 'production', 'COMMIT_DATABASE_MAX_CONNECTIONS': '4',
                'COMMIT_DATABASE_MIN_CONNECTIONS': '0', 'COMMIT_LOG': 'silicon_commit=info'}
        api = {**base, **accounts, 'COMMIT_BIND_ADDR': '127.0.0.1:8081',
               'COMMIT_PUBLIC_BASE_URL': 'https://api.commit.teamofsilicons.com/api/v1/',
               'COMMIT_DATABASE_URL': dburl('commit_accounts_api')}
        worker = {**base, 'ACCOUNTS_URL': accounts['ACCOUNTS_URL'],
                  'COMMIT_DATABASE_URL': dburl('commit_accounts_worker'),
                  'COMMIT_TELEMETRY_HOME': '/var/lib/commit/telemetry',
                  **{k: v for k, v in shared.items() if k == 'COMMIT_POSTMARK_SERVER_TOKEN'}}
        migrator = {**base, 'COMMIT_SCHEMA_OWNER': 'commit_accounts_migrator',
                    'COMMIT_MIGRATOR_DATABASE_URL': dburl('commit_accounts_migrator')}
    elif APP == 'extend':
        api = {**accounts, **shared, 'EXTEND_ENVIRONMENT': 'production',
               'EXTEND_BIND': '0.0.0.0:8080', 'EXTEND_PUBLIC_URL': 'https://api.extend.teamofsilicons.com',
               'EXTEND_DATABASE_URL': dburl('extend_accounts_app'), 'EXTEND_DATA_DIR': '/var/lib/extend/data',
               'EXTEND_DELEGATION_ENCRYPTION_KEY': secret['data_key'], 'EXTEND_FILES_MODE': 'briefcase',
               'EXTEND_BRIEFCASE_URL': 'https://api.briefcase.teamofsilicons.com',
               'EXTEND_BRIEFCASE_WEB_URL': 'https://briefcase.teamofsilicons.com',
               'EXTEND_LOG_FORMAT': 'json'}
        worker = None
        migrator = dict(api)
    else:
        api = {**accounts, **shared, 'DM_ENVIRONMENT': 'production',
               'DM_BIND_ADDR': '0.0.0.0:8081', 'DM_PUBLIC_BASE_URL': 'https://api.dm.teamofsilicons.com/api/v1/',
               'DM_DATABASE_URL': dburl('dm_accounts_runtime'), 'DM_DATABASE_MAX_CONNECTIONS': '4',
               'DM_DATABASE_MIN_CONNECTIONS': '0', 'DM_DATA_KEY': secret['data_key'],
               'DM_TELEMETRY_ENABLED': 'false', 'DM_LOG_FILTER': 'silicon_dm=info',
               'DM_PROOF_ISSUERS': 'dm.conversations.read=interface,dm.messages.read=interface,dm.messages.write=interface,dm.receipts.write=interface,dm.drafts.write=interface'}
        worker = dict(api)
        migrator = {'DM_ENVIRONMENT': 'production', 'DM_DATABASE_URL': dburl('dm_accounts_migrator')}
    return api, worker, migrator


def install(archive, checksum, source, grant_file):
    if os.geteuid() != 0 or not re.fullmatch(r'[a-f0-9]{40}', source):
        raise ValueError('Root and a full source revision are required')
    if hashlib.sha256(archive.read_bytes()).hexdigest() != checksum:
        raise ValueError('Backend archive checksum mismatch')
    with tarfile.open(archive) as bundle:
        manifest = json.load(bundle.extractfile('manifest.json'))
        config = json.load(bundle.extractfile(manifest[0]['Config']))
        if config['architecture'] != 'arm64' or config['os'] != 'linux':
            raise ValueError('Backend must be Linux ARM64')
        image = 'sha256:' + manifest[0]['Config'].rsplit('/', 1)[-1].removesuffix('.json')
    secret = json.loads(json.loads(run(['aws', '--region', 'us-east-1', 'secretsmanager',
        'get-secret-value', '--secret-id', 'silicon-' + APP + '/accounts-production/runtime']))['SecretString'])
    api, worker, migration = make_settings(secret)
    names = [APP + '-accounts-' + role for role in (['api', 'worker'] if worker else ['api'])]
    existing = run(['docker', 'ps', '-a', '--format', '{{.Names}}']).splitlines()
    if set(existing) & set(names):
        raise ValueError('A new Accounts container already exists; inspect it rather than replacing it')
    ROOT.mkdir(parents=True, mode=0o700, exist_ok=True)
    ROOT.chmod(0o700)
    config_dir = ROOT / 'config'
    config_dir.mkdir(mode=0o700, exist_ok=True)
    for role, values in [('api', api), ('worker', worker), ('migration', migration)]:
        if values is not None:
            write_env(config_dir / (role + '.env'), values)
    state = ROOT / 'state'
    state.mkdir(mode=0o700, exist_ok=True)
    os.chown(state, 10001, 10001)
    ca = ROOT / 'rds-ca.pem'
    if not ca.is_file():
        raise ValueError('Expected the separately staged RDS CA')
    run(['docker', 'load', '--input', str(archive)])
    network = 'extend' if APP == 'extend' else 'host'
    common = ['--network', network, '--read-only', '--cap-drop', 'ALL', '--security-opt', 'no-new-privileges',
              '--tmpfs', '/tmp:rw,noexec,nosuid,size=64m', '--mount', f'type=bind,source={ca},target=/rds-ca.pem,readonly']
    migrate_args = ['migrate'] if APP == 'extend' else [APP + '-migrate']
    migrate = run(['docker', 'run', '--rm', *common, '--env-file', str(config_dir / 'migration.env'), image, *migrate_args])
    (ROOT / 'migration.log').write_text(migrate)
    (ROOT / 'migration.log').chmod(0o600)
    if APP != 'extend':
        if grant_file is None:
            raise ValueError('Runtime grants are required')
        owner = APP + '_accounts_migrator'
        pg_env = {**os.environ, 'PGHOST': secret['database_host'], 'PGDATABASE': secret['database'],
                  'PGUSER': owner, 'PGPASSWORD': secret['role_passwords'][owner],
                  'PGSSLMODE': 'verify-full', 'PGSSLROOTCERT': '/rds-ca.pem'}
        client = next(x for x in run(['docker', 'images', '--format', '{{.Repository}}:{{.Tag}}']).splitlines()
                      if x.startswith('postgres:') and '<none>' not in x)
        args = ['docker', 'run', '--rm', '-i', '--network', 'host', '--read-only', '--cap-drop', 'ALL',
                '--mount', f'type=bind,source={ca},target=/rds-ca.pem,readonly']
        for key in ['PGHOST', 'PGDATABASE', 'PGUSER', 'PGPASSWORD', 'PGSSLMODE', 'PGSSLROOTCERT']:
            args += ['-e', key]
        args += [client, 'psql', '-X', '-v', 'ON_ERROR_STOP=1']
        variables = ({'runtime_role': 'dm_accounts_runtime'} if APP == 'dm' else {
            'database_name': 'silicon_commit_accounts', 'schema_owner': owner,
            'api_role': 'commit_accounts_api', 'worker_role': 'commit_accounts_worker'})
        for key, value in variables.items():
            args += ['-v', key + '=' + value]
        run(args, input=grant_file.read_text(), env=pg_env)
    for role, name in zip(['api', 'worker'], names):
        args = ['docker', 'run', '-d', '--restart', 'unless-stopped', '--name', name,
                '--memory', '768m' if APP == 'extend' else '384m', '--pids-limit', '256', *common,
                '--env-file', str(config_dir / (role + '.env'))]
        if APP == 'extend':
            args += ['--publish', '127.0.0.1:8481:8080', '--mount', f'type=bind,source={state},target=/var/lib/extend/data']
        elif APP == 'commit':
            args += ['--mount', f'type=bind,source={state},target=/var/lib/commit/telemetry']
        args += [image]
        if APP != 'extend':
            args += [APP + '-' + role]
        run(args)
    ready_url = ('http://127.0.0.1:8481/ready' if APP == 'extend' else
                 'http://127.0.0.1:8081/ready' if APP == 'dm' else 'http://127.0.0.1:8081/readyz')
    ready = False
    for _ in range(20):
        try:
            with urllib.request.urlopen(ready_url, timeout=2) as response:
                ready = 200 <= response.status < 300
            if ready:
                break
        except Exception:
            pass
        time.sleep(1)
    receipt = {'app': APP, 'source_revision': source, 'archive_sha256': checksum, 'image': image,
               'database': secret['database'], 'containers': names, 'local_ready': ready,
               'public_proxy': 'maintenance', 'iam_containers_preserved': all(x in run(
                   ['docker', 'ps', '--format', '{{.Names}}']).splitlines() for x in existing
                   if x in ['commit-api', 'commit-worker', 'extend-service', 'silicon-dm-gateway'])}
    (ROOT / 'install-receipt.json').write_text(json.dumps(receipt, indent=2))
    print(json.dumps(receipt))
    if not ready:
        raise RuntimeError('New local API readiness failed; IAM deployment remains untouched')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--archive', type=Path, required=True)
    parser.add_argument('--sha256', required=True)
    parser.add_argument('--source', required=True)
    parser.add_argument('--grants', type=Path)
    args = parser.parse_args()
    install(args.archive, args.sha256, args.source, args.grants)
