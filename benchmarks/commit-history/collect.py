"""Promote a complete, restored-camera run to credential-free benchmark evidence."""
import argparse
import json
from pathlib import Path
import subprocess
import benchmark

parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--block',default='forward-100')
args=parser.parse_args()
work=benchmark.WORK
results=json.loads((work/'results'/('suite-'+args.block+'.json')).read_text())
assert [r['commit'] for r in results]==[r['commit'] for r in benchmark.MANIFEST['commits']]
assert all(r.get('source_clean') and r.get('n',0)>0 for r in results),'Incomplete or dirty-source run'
backup=json.loads((work/'stream-backup.json').read_text())
assert backup['restored'],'Restore and verify the camera configuration first'
device=json.loads((work/'device.json').read_text());device.pop('host',None)
data={
    'measured_on':'2026-11-26','historical':False,
    'protocol':'README.md','block':args.block,
    'camera':device,'original_stream_configuration_restored':True,
    'rustc':subprocess.check_output([str(benchmark.CARGO.parent/'rustc'),'--version'],text=True).strip(),
    'nasm':subprocess.check_output([str(Path.home()/'.local/bin/nasm'),'-v'],text=True).strip(),
    'conditions':[
        'One sequential block per commit; release builds completed before measurement.',
        'Fresh measurements of exact October source; camera firmware differs from the original archive.',
        'MCP request wall time excludes agent time and image validation; cold time includes initialization.',
        'Native-only prototype is not comparable to MCP and has only five printed samples.',
        'H264 minor stream temporarily 640x360/15fps; JPEG advertises 640x360/1fps (not independently verified).',
        'All fresh native builds use NASM; final provenance commit has no runtime code change.',
        'No camera images, credentials or stream URLs are retained in this artifact.'
    ],
    'results':results,
}
destination=Path(__file__).with_name('results-2026-11-26.json')
destination.write_text(json.dumps(data,indent=2)+'\n')
print('Published evidence for',len(results),'exact-source checkpoints')
