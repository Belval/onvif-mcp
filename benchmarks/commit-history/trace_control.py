"""Alternate tracing on/off at one fixed revision, without camera reconfiguration."""
import argparse
import json
from pathlib import Path
import benchmark
from stream_profile import connect

parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--host',required=True)
args=parser.parse_args()
row=benchmark.MANIFEST['commits'][-1]
command=[str(benchmark.WORK/'binaries'/row['commit'][:7])]
camera,media=connect(args.host)
profiles=media.GetProfiles()
minor=next(p for p in profiles if p.Name=='minorStream')
cfg=media.GetVideoEncoderConfiguration({'ConfigurationToken':minor.VideoEncoderConfiguration.token})
stream={'width':cfg.Resolution.Width,'height':cfg.Resolution.Height,'fps':cfg.RateControl.FrameRateLimit}
results=[]
for index,enabled in enumerate((True,False,False,True,True,False)):
    print('Trace control block',index+1,'enabled',enabled,flush=True)
    value=benchmark.measure(row,command,100,50,'tracing-control-'+str(index+1),host=args.host,post_detail_samples=0,metrics_enabled=enabled)
    results.append(value)
    print('Median',round(value['median_ms'],3),'ms',flush=True)
out={'scope':'Tracing overhead only: same final source/binary; six alternating on/off blocks; 100 warm calls each, 50ms interval; no camera configuration changes. Separate conditions from the 640px historical-source suite.',
     'minor_stream':stream,'results':results}
Path(__file__).with_name('tracing-control-2026-11-26.json').write_text(json.dumps(out,indent=2)+'\n')
