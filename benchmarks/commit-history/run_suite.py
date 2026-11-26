"""Run prepared historical binaries on one fixed camera/stream, then restore it.

Use the historical repository .venv: it supplies onvif-zeep for stream setup.
Build every checkpoint first; this runner does not compile during measurements.
"""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time

import benchmark

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--host',required=True)
    parser.add_argument('--samples',type=int,default=100)
    parser.add_argument('--interval-ms',type=int,default=200)
    parser.add_argument('--block',default='forward-100')
    args=parser.parse_args()
    script=Path(__file__).with_name('stream_profile.py')
    entries=benchmark.MANIFEST['commits']
    commands={}
    for row in entries:
        c=row['commit'][:7]
        command=[str(benchmark.ROOT/'.venv/bin/python'),str(benchmark.WORK/'worktrees'/c/'camera_mcp.py')] if row['runtime']=='python-mcp' else [str(benchmark.WORK/'binaries'/c)]
        if not Path(command[-1]).is_file():raise RuntimeError('Missing prepared checkpoint '+c)
        commands[c]=command
    out=benchmark.WORK/'results';out.mkdir(exist_ok=True)
    # Always attempt restoration, including failed readback/setup after mutation.
    results=[];backup=benchmark.WORK/'stream-backup.json'
    try:
        subprocess.run([sys.executable,str(script),'prepare','--host',args.host],check=True)
        time.sleep(2)
        for row in entries:
            short=row['commit'][:7];print(f'{short}: measuring {args.samples} requests',flush=True)
            try:
                record=benchmark.measure(row,commands[short],args.samples,args.interval_ms,args.block,host=args.host)
                print(f'{short}: n={record["n"]}, median {record["median_ms"]:.3f} ms; p95 {record["p95_ms"]:.3f} ms; max {record["max_ms"]:.3f} ms',flush=True)
            except Exception as e:
                record=dict(commit=row['commit'],changes=row['changes'],status='failed',error_type=type(e).__name__,block=args.block,historical=False,measured_at=datetime.now(timezone.utc).isoformat())
                print(f'{short}: failed ({type(e).__name__}); no latency assigned',flush=True)
            results.append(record)
            (out/(short+'-'+args.block+'.json')).write_text(json.dumps(record,indent=2)+'\n')
            (out/('suite-'+args.block+'.json')).write_text(json.dumps(results,indent=2)+'\n')
            time.sleep(2)
    finally:
        if backup.exists() and not json.loads(backup.read_text()).get('restored'):
            subprocess.run([sys.executable,str(script),'restore','--host',args.host],check=True)
    print('Suite complete; camera stream restored.',flush=True)

if __name__=='__main__':main()
