"""Build a checksum-pinned installer source archive without local state/secrets."""
from pathlib import Path
import tarfile,hashlib,subprocess
root=Path(__file__).resolve().parent.parent
out=root/'docs-site/dist/releases';out.mkdir(parents=True,exist_ok=True)
archive=out/'commit-0.2.0.tar.gz'
allowed=['Cargo.toml','Cargo.lock','rust-toolchain.toml','LICENSE','cli','client']
with tarfile.open(archive,'w:gz') as tar:
 for relative in allowed:
  p=root/relative
  if not p.exists():continue
  for f in ([p] if p.is_file() else sorted(p.rglob('*'))):
   if f.is_file() and not any(x in f.parts for x in ['target','node_modules','.work']):tar.add(f,arcname='silicon-commit/'+str(f.relative_to(root)))
 # Cargo must see every workspace member, but CLI installation never compiles the backend.
 for f in ['src/lib.rs','src/bin/commit_api.rs','src/bin/commit_worker.rs','src/bin/commit_migrate.rs']:
  p=root/f
  if p.exists():tar.add(p,arcname='silicon-commit/'+f)
(out/'commit-0.2.0.sha256').write_text(hashlib.sha256(archive.read_bytes()).hexdigest()+'  commit-0.2.0.tar.gz\n')
print('Built installer source archive and checksum')
