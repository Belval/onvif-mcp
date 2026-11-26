"""Alternate prepared releases on deterministic camera-free production workloads."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess

parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--baseline',required=True,type=Path)
parser.add_argument('--candidate',required=True,type=Path)
parser.add_argument('--output',required=True,type=Path)
args=parser.parse_args()
results=[]
for index,variant in enumerate(('baseline','candidate','candidate','baseline','baseline','candidate')):
    binary=getattr(args,variant).resolve()
    env=os.environ.copy();env['CAMERA_METRICS']='1'
    print('Pipeline block',index+1,variant,flush=True)
    result=subprocess.run([str(binary),'--benchmark-pipeline'],env=env,stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,check=True,text=True)
    record={'block':index+1,'variant':variant,'binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(),'measurements':json.loads(result.stdout)}
    assert record['measurements']['metrics_enabled']
    results.append(record)
    args.output.parent.mkdir(parents=True,exist_ok=True)
    args.output.write_text(json.dumps({'protocol':'A/B/B/A/A/B; tracing enabled; fresh frame caches at each iteration; three warm-up iterations discarded, thirty measured; frame construction outside clock; all translated points checked to 0.01 normalized radius','results':results},indent=2)+'\n')
    print([(r['workload'],round(r['median_ms'],3)) for r in record['measurements']['results']],flush=True)
