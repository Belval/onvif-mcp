"""Alternate two prepared releases on the same camera; never compile or move it."""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import statistics
import time

ROOT=Path(__file__).resolve().parents[2]
spec=importlib.util.spec_from_file_location('history',ROOT/'benchmarks/commit-history/benchmark.py')
history=importlib.util.module_from_spec(spec);spec.loader.exec_module(history)
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--baseline',required=True,type=Path)
parser.add_argument('--candidate',required=True,type=Path)
parser.add_argument('--intermediate',type=Path,help='Optional third release; use balanced three-variant blocks')
parser.add_argument('--host',required=True)
parser.add_argument('--output',required=True,type=Path)
args=parser.parse_args()
results=[]
order=('baseline','intermediate','candidate','intermediate','candidate','baseline','candidate','baseline','intermediate') if args.intermediate else ('baseline','candidate','candidate','baseline','baseline','candidate')
for index,variant in enumerate(order):
    binary=getattr(args,variant).resolve()
    env=history.environment();env.update(CAMERA_CONFIG_DIR=str(ROOT),CAMERA_HOST=args.host,CAMERA_METRICS='1')
    client=history.Client([str(binary)],ROOT,env)
    print('Block',index+1,variant,flush=True)
    try:
        client.request('initialize',{'protocolVersion':'2024-11-05','capabilities':{},'clientInfo':{'name':'mcp-optimization-benchmark','version':'1'}})
        client.send({'jsonrpc':'2.0','method':'notifications/initialized'})
        listing,_=client.request('tools/list',{})
        assert len(listing['tools'])==16
        first,_=client.request('tools/call',{'name':'camera_snapshot','arguments':{}})
        history.image_info(first)
        rows=[]
        for sample in range(200):
            time.sleep(.05)
            result,elapsed=client.request('tools/call',{'name':'camera_snapshot','arguments':{}})
            rows.append({'wall_ms':elapsed,**history.image_info(result)})
        metric,_=client.request('tools/call',{'name':'camera_metrics','arguments':{}})
        metrics=next(json.loads(b['text']) for b in metric['content'] if b['type']=='text')
        detail,detail_ms=client.request('tools/call',{'name':'camera_snapshot','arguments':{'detail':True}})
        detail_info=history.image_info(detail)
        values=sorted(r['wall_ms'] for r in rows)
        record={'block':index+1,'variant':variant,'binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(),
            'samples':rows,'median_ms':statistics.median(values),'p95_ms':values[189],'max_ms':max(values),
            'detail_ms':detail_ms,'detail_image':detail_info,'durations':metrics['durations'],
            'counters':metrics['counters'],'host_load':os.getloadavg()}
        results.append(record)
        args.output.parent.mkdir(parents=True,exist_ok=True)
        args.output.write_text(json.dumps({'protocol':'200 preview calls per block; 50ms pause; tracing enabled in all variants; JPEG validation outside request timer; no camera configuration/motion changes','variant_order':order,'results':results},indent=2)+'\n')
        print('Median',round(record['median_ms'],3),'p95',round(record['p95_ms'],3),flush=True)
    finally:client.close()
