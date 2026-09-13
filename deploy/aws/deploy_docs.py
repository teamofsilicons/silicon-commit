#!/usr/bin/env python3
"""Publish a built docs-site/dist over SSM, preserving the existing API proxy."""
import base64,json,subprocess,time,tarfile,io,hashlib
from pathlib import Path
root=Path(__file__).resolve().parents[2]
aws=['aws','--profile','silicon-production','--region','us-east-1']
instance='i-0bd8d9688a5a5d252'
def command(script):
 payload=json.dumps({'commands':[script],'executionTimeout':['180']})
 result=json.loads(subprocess.check_output(aws+['ssm','send-command','--instance-ids',instance,'--document-name','AWS-RunShellScript','--parameters',payload]))
 cid=result['Command']['CommandId']
 for _ in range(90):
  time.sleep(2)
  p=subprocess.run(aws+['ssm','get-command-invocation','--command-id',cid,'--instance-id',instance],capture_output=True,text=True)
  if p.returncode: continue
  r=json.loads(p.stdout)
  if r['Status'] in ['Pending','InProgress','Delayed']:continue
  if r['Status']!='Success':raise RuntimeError(r.get('StandardErrorContent','')+' '+r.get('StandardOutputContent',''))
  return r.get('StandardOutputContent','')
 raise RuntimeError('SSM command timeout: '+cid)
buf=io.BytesIO()
with tarfile.open(fileobj=buf,mode='w:gz') as tar:
 for p in sorted((root/'docs-site/dist').rglob('*')):
  if p.is_file():tar.add(p,arcname=str(p.relative_to(root/'docs-site/dist')))
raw=buf.getvalue();encoded=base64.b64encode(raw).decode();digest=hashlib.sha256(raw).hexdigest()
command('umask 077; : > /opt/commit/docs-upload.b64')
for index in range(0,len(encoded),40000):
 command("printf %s '"+encoded[index:index+40000]+"' >> /opt/commit/docs-upload.b64")
 print('Transferred documentation chunk',index//40000+1,flush=True)
script="""set -eu
base64 -d /opt/commit/docs-upload.b64 > /opt/commit/docs-upload.tar.gz
echo 'DIGEST  /opt/commit/docs-upload.tar.gz' | sha256sum -c -
mkdir -p /opt/commit/docs-release
find /opt/commit/docs-release -mindepth 1 -maxdepth 1 -exec rm -rf {} +
tar -xzf /opt/commit/docs-upload.tar.gz -C /opt/commit/docs-release
chmod -R a+rX /opt/commit/docs-release
docker cp /opt/commit/docs-release/. commit-caddy:/config/commit-docs-next
cat > /opt/commit/docs.caddy <<'CADDY'
docs.commit.teamofsilicons.com {
    root * /config/commit-docs
    encode zstd gzip
    file_server
    header X-Content-Type-Options nosniff
    header Referrer-Policy strict-origin-when-cross-origin
}
CADDY
cp /opt/commit/Caddyfile /opt/commit/Caddyfile.before-docs
python3 - <<'REMOTE'
from pathlib import Path
p=Path('/opt/commit/Caddyfile')
s=p.read_text()
if 'docs.commit.teamofsilicons.com' not in s:p.write_text(s+'\\n'+Path('/opt/commit/docs.caddy').read_text())
REMOTE
docker exec commit-caddy caddy validate --config /etc/caddy/Caddyfile --adapter caddyfile
docker exec commit-caddy sh -c 'rm -rf /config/commit-docs-previous; if [ -d /config/commit-docs ]; then mv /config/commit-docs /config/commit-docs-previous; fi; mv /config/commit-docs-next /config/commit-docs'
docker exec commit-caddy caddy reload --config /etc/caddy/Caddyfile --adapter caddyfile
rm /opt/commit/docs-upload.b64 /opt/commit/docs-upload.tar.gz
printf 'Docs published; previous files retained in the Caddy config volume.\\n'
""".replace('DIGEST',digest)
print(command(script))
