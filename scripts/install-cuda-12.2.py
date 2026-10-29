#!/usr/bin/env python3
"""Install pinned NVIDIA development libraries locally; do not change the driver."""
import argparse
import concurrent.futures
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import urllib.request

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--prefix', type=Path, default=Path.home()/'.local/share/onvif-cuda-12.2')
args = parser.parse_args()
manifest = json.loads(Path(__file__).with_name('cuda-12.2-packages.json').read_text())
args.prefix.mkdir(parents=True, exist_ok=True)
with tempfile.TemporaryDirectory(prefix='onvif-cuda-') as temp:
    def download(item):
        name, meta = item
        path = Path(temp)/(name+'.tar.xz')
        with urllib.request.urlopen(manifest['base_url']+meta['relative_path'], timeout=60) as response, path.open('wb') as target:
            shutil.copyfileobj(response, target)
        digest = hashlib.sha256()
        with path.open('rb') as source:
            for block in iter(lambda: source.read(1024*1024), b''):
                digest.update(block)
        if digest.hexdigest() != meta['sha256']:
            raise RuntimeError('NVIDIA archive checksum mismatch: '+name)
        print(name+': verified', flush=True)
        return path
    with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
        archives = list(pool.map(download, manifest['packages'].items()))
    for archive in archives:
        subprocess.run(['tar','xf',str(archive),'--strip-components=1','-C',str(args.prefix)],check=True)
lib64 = args.prefix/'lib64'
if not lib64.exists():
    lib64.symlink_to('lib', target_is_directory=True)
subprocess.run([str(args.prefix/'bin/nvcc'),'--version'],check=True)
print('Installed at '+str(args.prefix))
