"""Partition completed trace wall time once, then build the action chart data."""
from collections import defaultdict
import heapq
import json
from pathlib import Path

HERE=Path(__file__).resolve().parent
ROOT=HERE.parents[1]
data=json.loads((HERE/'action-traces.json').read_text())
assert data['restored'],'Camera pose must be restored before publishing measurements'
CATEGORIES=['ONVIF','Controller waits','Frame wait','Tracking','Response','Other MCP']
def category(stage):
    if stage.startswith('onvif.'):return 'ONVIF'
    if stage in ('motion.poll_interval','motion.settling_wait','motion.velocity_interval'):return 'Controller waits'
    if stage.startswith('video.') or stage=='camera.capture':return 'Frame wait'
    if stage.startswith('vision.'):return 'Tracking'
    if stage.startswith('response.') or stage in ('server.response_content','transport.response_prepare','transport.response_write','transport.write_poll'):return 'Response'
    return None

def partition(trace):
    total=round(trace['elapsed_ms']*1_000_000)
    records={s['id']:s for s in trace['spans']}
    depths={}
    def depth(ident):
        if ident is None or ident not in records:return 0
        if ident not in depths:depths[ident]=1+depth(records[ident]['parent'])
        return depths[ident]
    add=defaultdict(list);remove=defaultdict(list)
    for ident,span in records.items():
        cat=category(span['stage'])
        if not cat or not span['duration_ns']:continue
        start=max(0,min(total,span['start_ns']));end=max(start,min(total,span['start_ns']+span['duration_ns']))
        if end==start:continue
        add[start].append((-depth(ident),CATEGORIES.index(cat),ident,cat));remove[end].append(ident)
    times=sorted({0,total,*add,*remove});active=set();heap=[];parts=dict.fromkeys(CATEGORIES,0)
    for start,end in zip(times,times[1:]):
        for ident in remove[start]:active.discard(ident)
        for entry in add[start]:active.add(entry[2]);heapq.heappush(heap,entry)
        while heap and heap[0][2] not in active:heapq.heappop(heap)
        parts[heap[0][3] if heap else 'Other MCP']+=end-start
    assert sum(parts.values())==total
    return {k:v/1_000_000 for k,v in parts.items()}

groups=defaultdict(list)
for sample in data['samples']:groups[sample['action']].append(sample)
rows=[]
for action,samples in groups.items():
    ranked=sorted(samples,key=lambda s:s['trace']['elapsed_ms'])
    selected=ranked[len(ranked)//2];trace=selected['trace'];parts=partition(trace)
    assert abs(sum(parts.values())-trace['elapsed_ms'])<.000001
    rows.append({'action':action,'n':len(samples),'server_ms':trace['elapsed_ms'],
        'client_wall_ms':selected['client_wall_ms'],'focus_status':selected['focus_status'],
        'request_id':trace['request_id'],'breakdown_ms':parts})
assert len(rows)==14 and len(data['samples'])==40
assert all(s['focus_status']=='centered' for s in data['samples'] if s['action'] in ('Center · default','Center · precise'))
output={'source':'action-traces.json','binary_sha256':data['binary_sha256'],'build_features':'cuda',
    'method':'Representative median-duration server trace per action (three samples; cold one). Sweep all span boundaries; assign each interval once to its deepest active categorized span, breaking equal-depth overlaps in category order. Unclassified/uncovered time is Other MCP. Does not sum overlapping spans or background video work.',
    'rows':rows}
(HERE/'action-breakdown.json').write_text(json.dumps(output,indent=2)+'\n')
short={'Snapshot · cold':'Cold snapshot','Snapshot · warm':'Warm snapshot','Detail image':'Detail image',
    'Center · rough':'Rough centering','Center · default':'Default centering','Center · precise':'Precise centering',
    'Scan · 3 views':'Scan · 3 views','Go to view · .05 pan':'Go to saved view','Move · 150 ms':'Velocity · 150 ms',
    'Stop · idle':'Stop · idle','Clear views · 2':'Clear · 2 views'}
chart_rows=[]
for row in rows:
    number=f'{row["server_ms"]:.2f}' if row['server_ms']<10 else f'{row["server_ms"]:.1f}'
    label=short.get(row['action'],row['action'])+' · '+number+' ms'
    chart_rows.append([label]+[row['breakdown_ms'][cat] for cat in CATEGORIES])
configs={}
for mode in ('share','absolute'):
    configs[mode]={'id':'camera-cost-'+mode,'title':'Camera action breakdown','showHeader':False,
        'description':'Current CUDA release. One median-duration trace of three per action; cold snapshot one sample. Server pipe completion only; excludes client parsing, agent time, setup and restoration.',
        'height':760,'spec':{'type':'horizontalStackedBar100' if mode=='share' else 'horizontalStackedBar',
            'y':'Action','x':CATEGORIES,'xLabel':'Share of server wall time' if mode=='share' else 'Server wall time (ms)',
            'sortOrder':'original','showLegend':True},'columns':['Action']+CATEGORIES,'rows':chart_rows}
fragment=(HERE/'action-chart.template.html').read_text()
for mode,config in configs.items():fragment=fragment.replace('__'+mode.upper()+'_CONFIG__',json.dumps(config,ensure_ascii=False).replace('<','\\u003c'))
destination=ROOT/'captures/mcp-optimization-2026-11-26/camera-action-costs.html'
destination.write_text(fragment)
print(destination)
print('PASS: 14 bars, 40 completed traces; all stack totals equal server wall time')
