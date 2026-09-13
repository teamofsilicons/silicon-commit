#!/usr/bin/env python3
"""Run via SSM as root; arguments are secret ARN, DB endpoint, and immutable image."""
import json
import os
from pathlib import Path
import subprocess
import sys
import urllib.request

os.umask(0o077)
secret_arn, db_host, image = sys.argv[1:]
root = Path('/opt/commit')
root.mkdir(exist_ok=True)

def run(args, **kwargs):
    return subprocess.run(args, check=True, **kwargs)

secret = json.loads(json.loads(subprocess.check_output([
    'aws', 'secretsmanager', 'get-secret-value', '--region', 'us-east-1',
    '--secret-id', secret_arn, '--output', 'json']))['SecretString'])
ca = root / 'rds-ca.pem'
ca.write_bytes(urllib.request.urlopen('https://truststore.pki.rds.amazonaws.com/global/global-bundle.pem').read())
ca.chmod(0o644)
login = subprocess.check_output(['aws', 'ecr', 'get-login-password', '--region', 'us-east-1'])
run(['docker', 'login', '--username', 'AWS', '--password-stdin', image.split('/')[0]], input=login)
for img in [image, 'postgres:18', 'caddy:2']:
    run(['docker', 'pull', img])

def envfile(name, values):
    path = root / name
    if any('\n' in str(v) or '\r' in str(v) for v in values.values()):
        raise ValueError('Environment values must be single line')
    path.write_text(''.join(f'{k}={v}\n' for k, v in values.items()))
    return str(path)

def dburl(role, password):
    return f'postgres://{role}:{password}@{db_host}:5432/silicon_commit?sslmode=verify-full&sslrootcert=/rds-ca.pem'

base = {'COMMIT_ENVIRONMENT': 'production', 'COMMIT_DATABASE_MAX_CONNECTIONS': '8',
        'COMMIT_LOG': 'silicon_commit=info,tower_http=info'}
api = {**base, **{k:v for k,v in secret.items() if k.startswith('COMMIT_') and k not in ['COMMIT_POSTMARK_SERVER_TOKEN','COMMIT_TELEMETRY_TABLE_KEY','COMMIT_TELEMETRY_HOME']},
       'COMMIT_AUTH_MODE':'iam', 'COMMIT_BIND_ADDR':'127.0.0.1:8080',
       'COMMIT_PUBLIC_BASE_URL':'https://backend.commit.teamofsilicons.com/api/v1/',
       'COMMIT_DATABASE_URL':dburl('commit_api', secret['db_api_password'])}
worker = {**base, **{k:v for k,v in secret.items() if k in ['COMMIT_POSTMARK_SERVER_TOKEN','COMMIT_TELEMETRY','COMMIT_TELEMETRY_TABLE_KEY']}, 'COMMIT_TELEMETRY_HOME':'/var/lib/commit/telemetry', 'COMMIT_DATABASE_URL':dburl('commit_worker', secret['db_worker_password'])}
envfile('api.env', api)
envfile('worker.env', worker)
envfile('migrator.env', {**base,'COMMIT_SCHEMA_OWNER':'commit_migrator',
    'COMMIT_MIGRATOR_DATABASE_URL':dburl('commit_migrator',secret['db_migrator_password'])})
envfile('admin.env', {'PGHOST':db_host, 'PGDATABASE':'silicon_commit', 'PGUSER':'commit_admin',
    'PGPASSWORD':secret['db_admin_password'], 'PGSSLMODE':'verify-full','PGSSLROOTCERT':'/rds-ca.pem'})
psql=['docker','run','--rm','-i','--network','host','--env-file',str(root/'admin.env'),
      '-v',str(ca)+':/rds-ca.pem:ro','postgres:18','psql','-X','-v','ON_ERROR_STOP=1']
# Fresh dedicated database; never reset passwords or modify existing application rows.
roles=''
for role, key in [('commit_migrator','db_migrator_password'),('commit_api','db_api_password'),('commit_worker','db_worker_password')]:
    roles+=f"SELECT 'CREATE ROLE {role} LOGIN PASSWORD ''{secret[key]}''' WHERE NOT EXISTS (SELECT FROM pg_roles WHERE rolname='{role}')\\gexec\n"
roles+='GRANT CREATE ON DATABASE silicon_commit TO commit_migrator;\nGRANT USAGE, CREATE ON SCHEMA public TO commit_migrator;\nGRANT commit_migrator, commit_api, commit_worker TO commit_admin;\n'
try:
    run(psql, input=roles.encode(), stdout=subprocess.DEVNULL)
    run(['docker','run','--rm','--network','host','--env-file',str(root/'migrator.env'),
        '-v',str(ca)+':/rds-ca.pem:ro', image,'commit-migrate'])
    variables=['-v','database_name=silicon_commit','-v','schema_owner=commit_migrator',
               '-v','api_role=commit_api','-v','worker_role=commit_worker']
    run(psql+variables, input=(root/'postgres_runtime_grants.sql').read_bytes(), stdout=subprocess.DEVNULL)
    # Full role contract includes transactional checks of denied writes; all fixtures roll back.
    run(psql+variables, input=(root/'test_runtime_grants.sql').read_bytes())
finally:
    (root/'admin.env').unlink(missing_ok=True)
    (root/'migrator.env').unlink(missing_ok=True)

telemetry_home = root / 'telemetry'
telemetry_home.mkdir(exist_ok=True)
os.chown(telemetry_home, 10001, 10001)
telemetry_home.chmod(0o700)
for name, binary in [('api','commit-api'),('worker','commit-worker')]:
    # The first deployment creates containers; subsequent invocations replace only these services.
    subprocess.run(['docker','stop','--time','30','commit-'+name], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    subprocess.run(['docker','rm','commit-'+name], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    run(['docker','run','-d','--name','commit-'+name,'--restart','unless-stopped',
         '--network','host','--read-only','--cap-drop','ALL','--security-opt','no-new-privileges',
         '--log-opt','max-size=10m','--log-opt','max-file=3','--env-file',str(root/(name+'.env')),
         *(['-v',str(telemetry_home)+':/var/lib/commit/telemetry'] if name == 'worker' else []), '-v',str(ca)+':/rds-ca.pem:ro', image,binary])
caddy=root/'Caddyfile'
caddy.write_text('backend.commit.teamofsilicons.com {\n    reverse_proxy 127.0.0.1:8080\n}\n' + ((root/'docs.caddy').read_text() if (root/'docs.caddy').exists() else ''))
caddy.chmod(0o644)
subprocess.run(['docker','rm','-f','commit-caddy'], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
run(['docker','run','-d','--name','commit-caddy','--restart','unless-stopped','--network','host',
     '--log-opt','max-size=10m','--log-opt','max-file=3',
     '-v',str(caddy)+':/etc/caddy/Caddyfile:ro','-v','commit-caddy-data:/data',
     '-v','commit-caddy-config:/config','caddy:2'])
print('Commit containers started; verify HTTPS health and readiness.')
