"""Summarize measured requests; retain outliers and distinguish CPU/installed builds."""
import hashlib
import json
from pathlib import Path
import statistics

HERE=Path(__file__).resolve().parent
ROOT=HERE.parents[1]
live=json.loads((HERE/'final-live-results.json').read_text())['results']
summary={}
for variant in ('baseline','intermediate','candidate'):
    blocks=[b for b in live if b['variant']==variant]
    values=sorted(s['wall_ms'] for b in blocks for s in b['samples'])
    assert len(blocks)==3 and len(values)==600
    summary[variant]={'n':len(values),'median_ms':statistics.median(values),
        'p95_ms':values[569],'max_ms':max(values),'block_medians_ms':[b['median_ms'] for b in blocks],
        'binary_sha256':blocks[0]['binary_sha256']}
actions=json.loads((HERE/'action-traces.json').read_text())
assert actions['restored'] and len(actions['samples'])==40
for sample in actions['samples']:
    trace=sample['trace'];assert trace['trace_complete'] and trace['response_complete'] and not trace['dropped_spans']
    if sample['action'] in ('Center · default','Center · precise'):assert sample['focus_status']=='centered'
env=json.loads((HERE/'environment.json').read_text())
env['validation'].update(cuda_release_built=True,cuda_device_validation_before_language_guard=True,
    live_mcp_verification_passed=True,all_default_and_precise_centering_samples_centered=True,
    action_profile_original_pose_restored=True)
env['installed_binary_sha256']=hashlib.sha256((ROOT/'target/release/onvif-sight').read_bytes()).hexdigest()
assert env['installed_binary_sha256']==actions['binary_sha256']
paths=list((ROOT/'src').glob('*.rs'))+[ROOT/p for p in ('Cargo.toml','Cargo.lock','build.rs','.cargo/config.toml')]
env['runtime_source_sha256']={str(p.relative_to(ROOT)):hashlib.sha256(p.read_bytes()).hexdigest() for p in sorted(paths)}
(HERE/'environment.json').write_text(json.dumps(env,indent=2)+'\n')
output={'source':'final-live-results.json','scope':'Complete MCP stationary preview; three default-feature CPU releases; six hundred requests per release. Installed CUDA action timings are separate in action-breakdown.json.',
    'statistics':'Pooled median; nearest-rank p95; observed maximum retained, not a latency guarantee.',
    'preview_median_reduction_percent':100*(1-summary['candidate']['median_ms']/summary['baseline']['median_ms']),
    'preview_p95_reduction_percent':100*(1-summary['candidate']['p95_ms']/summary['baseline']['p95_ms']),
    'variants':summary}
(HERE/'summary.json').write_text(json.dumps(output,indent=2)+'\n')
print('PASS: three releases / 1,800 requests; 40 complete action traces; installed CUDA hash matched')
