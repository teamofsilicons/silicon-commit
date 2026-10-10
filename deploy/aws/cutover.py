#!/usr/bin/env python3
"""Cutover steps that need the production database. Run on the Commit host via SSM as root, after bootstrap.py.

    python3 /opt/commit/cutover.py SECRET_ARN DATABASE_HOST queues
    python3 /opt/commit/cutover.py SECRET_ARN DATABASE_HOST IMAGE plan
    python3 /opt/commit/cutover.py SECRET_ARN DATABASE_HOST IMAGE dry-run /opt/commit/mapping.csv
    python3 /opt/commit/cutover.py SECRET_ARN DATABASE_HOST IMAGE apply /opt/commit/mapping.csv

The production database only accepts the Commit host, so these run here. `queues` lists webhook deliveries and
emails still waiting (read-only), to drain them before migration 0033. `plan`, `dry-run` and `apply` run
`commit-migrate link-identities` in the image the deployment runs (IMAGE is the immutable repository@sha256
reference given to bootstrap.py): `plan` prints the mapping template (CSV), the others print the JSON report.
The migrator password and Commit's app secret come from the deployment secret on the server; they go into a
root-only environment file for one container run, removed afterwards, also on failure. See
docs/migration/cutover.md.
"""
import json
import os
from pathlib import Path
import subprocess
import sys

LINK_ACTIONS = {'plan': ['--plan'], 'dry-run': ['--dry-run'], 'apply': []}
PASSED_THROUGH = ('ACCOUNTS_URL', 'ACCOUNTS_API_URL', 'COMMIT_APP_ID')
PSQL_IMAGE = 'postgres:18'
QUEUES = """
SELECT 'webhook' AS queue, status::text AS status, count(*) AS waiting, min(available_at) AS next_attempt
  FROM commit.outbox_events WHERE status IN ('pending', 'in_flight') GROUP BY status
UNION ALL
SELECT 'email', status, count(*), min(next_attempt_at)
  FROM commit.email_jobs WHERE status = 'pending' GROUP BY status
ORDER BY 1, 2;
"""


def read_secret(secret_arn, required):
    secret = json.loads(json.loads(subprocess.check_output([
        'aws', 'secretsmanager', 'get-secret-value', '--region', 'us-east-1',
        '--secret-id', secret_arn, '--output', 'json']))['SecretString'])
    missing = [name for name in required if not secret.get(name)]
    if missing:
        raise ValueError('The deployment secret lacks ' + ', '.join(missing))
    return secret


def run_with_environment(root, values, command_for):
    """Write a root-only env file, run the command that uses it, and always remove the file."""
    if any('\n' in str(value) or '\r' in str(value) for value in values.values()):
        raise ValueError('Environment values must be single line')
    os.umask(0o077)
    envfile = root / 'cutover.env'
    try:
        envfile.write_text(''.join(f'{key}={value}\n' for key, value in values.items()))
        subprocess.run(command_for(envfile), check=True)
    finally:
        envfile.unlink(missing_ok=True)


def queues(secret_arn, db_host, *, root):
    ca = certificate(root)
    secret = read_secret(secret_arn, ('db_migrator_password',))
    values = {'PGHOST': db_host, 'PGDATABASE': 'silicon_commit', 'PGUSER': 'commit_migrator',
              'PGPASSWORD': secret['db_migrator_password'], 'PGSSLMODE': 'verify-full',
              'PGSSLROOTCERT': '/rds-ca.pem'}
    run_with_environment(root, values, lambda envfile: [
        'docker', 'run', '--rm', '--network', 'host', '--env-file', str(envfile), '-v', f'{ca}:/rds-ca.pem:ro',
        PSQL_IMAGE, 'psql', '-X', '-v', 'ON_ERROR_STOP=1', '-c', QUEUES])


def link(secret_arn, db_host, image, action, mapping=None, *, root):
    if action not in LINK_ACTIONS:
        raise ValueError(f'Unknown action {action!r}; use queues, plan, dry-run or apply')
    if '@sha256:' not in image:
        raise ValueError('Pass the immutable image reference (repository@sha256:...) that bootstrap.py deployed')
    if action == 'plan' and mapping is not None:
        raise ValueError('plan prints a template and takes no mapping file')
    if action != 'plan':
        if mapping is None:
            raise ValueError(f'{action} needs the mapping file (iam_principal_id,accounts_uuid[,org_id])')
        mapping = Path(mapping).resolve()
        if not mapping.is_file():
            raise ValueError(f'{mapping} is not a file; copy the reviewed mapping to the host first')
    ca = certificate(root)
    secret = read_secret(secret_arn, ('db_migrator_password', 'COMMIT_APP_SECRET'))
    values = {
        'COMMIT_ENVIRONMENT': 'production',
        'COMMIT_LOG': 'silicon_commit=warn',
        'COMMIT_SCHEMA_OWNER': 'commit_migrator',
        'COMMIT_MIGRATOR_DATABASE_URL': f"postgres://commit_migrator:{secret['db_migrator_password']}@{db_host}:5432/"
                                        'silicon_commit?sslmode=verify-full&sslrootcert=/rds-ca.pem',
        # Read-only lookups that check each account's kind and current id at Silicon Accounts.
        'COMMIT_APP_SECRET': secret['COMMIT_APP_SECRET'],
        **{name: secret[name] for name in PASSED_THROUGH if secret.get(name)},
    }

    def command(envfile):
        parts = ['docker', 'run', '--rm', '--network', 'host', '--env-file', str(envfile),
                 '-v', f'{ca}:/rds-ca.pem:ro']
        if mapping is not None:
            parts += ['-v', f'{mapping}:/mapping.csv:ro']
        parts += [image, 'commit-migrate', 'link-identities', *LINK_ACTIONS[action]]
        return parts + (['--file', '/mapping.csv'] if mapping is not None else [])

    run_with_environment(root, values, command)


def certificate(root):
    ca = root / 'rds-ca.pem'
    if not ca.is_file():
        raise ValueError(f'{ca} is missing; run bootstrap.py first')
    return ca


def main(secret_arn, db_host, *rest, root=Path('/opt/commit')):
    if not rest:
        raise ValueError('Name an action: queues, plan, dry-run or apply')
    if rest[0] == 'queues':
        if len(rest) != 1:
            raise ValueError('queues takes no other arguments')
        return queues(secret_arn, db_host, root=root)
    if len(rest) < 2 or len(rest) > 3:
        raise ValueError('Usage: SECRET_ARN DATABASE_HOST IMAGE plan|dry-run|apply [MAPPING.csv]')
    return link(secret_arn, db_host, *rest, root=root)


if __name__ == '__main__':
    try:
        main(*sys.argv[1:])
    except (TypeError, ValueError) as error:
        sys.exit(f'cutover.py: {error}\n\n{__doc__}')
