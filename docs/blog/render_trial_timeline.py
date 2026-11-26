"""Export all 216 measured controller trials, with per-trial timings and sources.

The archive has within-batch order and durations, not UTC start times. The X axis
is explicitly grouped trial sequence; active time is the sum of measured calls.
"""
import csv
from html import escape
import json
from pathlib import Path
import statistics
import xml.etree.ElementTree as ET

import matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt

ROOT=Path(__file__).resolve().parents[2]
BENCH=ROOT/'benchmarks/i-spy'
OUT=Path(__file__).resolve().parent/'assets'
C=json.loads((OUT/'brand-tokens.json').read_text())['colors']
specs=[(row['variant'],False) for row in json.loads((BENCH/'framing-summary.json').read_text())]
specs += [('framing-final-matched-baseline',True),('framing-final-matched-candidate',True)]
trials=[];batches=[];active_ms=0.0
for number,(name,final) in enumerate(specs,1):
    source=BENCH/(name+'.json');record=json.loads(source.read_text())
    setting=next(s['values'] for s in record['samples'][0]['trace']['spans'] if s['stage']=='motion.target')
    settle,poll,checks=(int(setting[k]) for k in ('settle_ms','poll_ms','confirmations'))
    main=record['parameters'].get('CAMERA_PREVIEW')=='h264-main'
    label=f'{settle} ms settle / {poll} ms poll / {checks} arrival '+('check' if checks==1 else 'checks')
    if main:label='Main stream: '+label
    if name=='framing-final-matched-baseline':label='Final matched baseline'
    if name=='framing-final-matched-candidate':label='Final controller'
    start=len(trials)+1
    for index,sample in enumerate(record['samples'],1):
        duration=sample['wall_ms']
        trials.append(dict(trial=len(trials)+1,batch=number,within_batch=index,setting=label,
            settle_ms=settle,poll_ms=poll,arrival_checks=checks,axis=sample['axis'],
            direction=sample['direction'],duration_ms=duration,geometry_pass=sample['geometry_pass'],
            active_start_ms=active_ms,active_end_ms=active_ms+duration,
            utc_start=None,source=str(source.relative_to(ROOT)),sample_index=index-1))
        active_ms+=duration
    batches.append(dict(batch=number,name=name,label=label,start=start,end=len(trials),
        n=len(record['samples']),median_ms=statistics.median(r['wall_ms'] for r in record['samples']),
        geometry_passes=sum(r['geometry_pass'] for r in record['samples']),
        final_matched_pair=final,origin=record['origin'],settle_ms=settle,poll_ms=poll,arrival_checks=checks))
assert len(trials)==216 and len(batches)==12
assert all(b['n']==18 for b in batches)
assert batches[-1]['origin']==batches[-2]['origin']
games=[json.loads(s) for s in (BENCH/'runs.jsonl').read_text().splitlines() if s.strip()]
coverage=dict(judged_games=len(games),timing_configurations=len(specs)-2,
    timing_sweep_trials=sum(b['n'] for b in batches[:-2]),final_comparison_trials=36,
    total_motion_trials=len(trials),active_controller_seconds=active_ms/1000,
    geometry_failures=sum(not r['geometry_pass'] for r in trials))
data=dict(coverage=coverage,batches=batches,trials=trials,
    time_basis='Recorded batch grouping; execution order within each batch. No absolute UTC timestamps were stored.',
    duration_basis='Native goto + observe wall time. Excludes agent reasoning, inter-trial gaps, restores, and delayed reference/evaluation.',
    quality_scope='Point crosses flag geometry failures only. Blur and scan-revisit rejection apply at variant level; see original experiment notes.')
(OUT/'motion-trials.json').write_text(json.dumps(data,indent=2)+'\n')
with (OUT/'motion-trials.csv').open('w',newline='') as stream:
    writer=csv.DictWriter(stream,fieldnames=list(trials[0]),lineterminator='\n')
    writer.writeheader();writer.writerows(trials)
(OUT/'experiment-coverage.json').write_text(json.dumps(coverage,indent=2)+'\n')

plt.rcParams.update({'font.family':'DejaVu Sans','font.size':15,'text.color':C['ink'],
    'axes.labelcolor':C['ink'],'xtick.color':C['muted'],'ytick.color':C['muted'],
    'figure.facecolor':C['paper'],'axes.facecolor':C['paper'],
    'savefig.facecolor':C['paper'],'svg.fonttype':'none','svg.hashsalt':'peek-seek-trials'})
fig=plt.figure(figsize=(16,16),dpi=100)
fig.text(.05,.958,'03 / THE WORK BEHIND THE RESULT',fontsize=15,color=C['muted'])
fig.text(.05,.902,'216 physical trials. Every one timed.',fontfamily='DejaVu Serif',fontsize=31)
fig.text(.05,.856,'7 judged games · 10 timing settings × 18 moves · 18 + 18 final comparison moves',fontsize=16)
ax=fig.add_axes([.075,.55,.89,.24])

def axes_style(axis):
    axis.spines[['top','right']].set_visible(False)
    axis.spines[['left','bottom']].set_color(C['border'])
    axis.grid(axis='y',color=C['border']);axis.set_axisbelow(True)
    axis.set_ylim(0,1500)

def draw_trial(axis,row):
    artist=axis.scatter([row['trial']],[row['duration_ms']],s=22 if row['geometry_pass'] else 46,
        marker='o' if row['geometry_pass'] else 'x',color=C['ink'] if row['geometry_pass'] else C['return'],zorder=3)
    artist.set_gid(f'trial-{row["trial"]:03d}')

axes_style(ax)
ax.set_title('Timing search: 180 trials, not a monotonic improvement curve',loc='left',fontsize=16,pad=22)
for b in batches[:-2]:
    fill=C['surround'] if b['batch']%2 else C['paper']
    ax.axvspan(b['start']-.5,b['end']+.5,facecolor=fill,zorder=0)
    ax.axvline(b['start']-.5,color=C['border'],linewidth=.8)
    ax.text((b['start']+b['end'])/2,1430,f'{b["batch"]:02d}',ha='center',fontsize=12,color=C['muted'])
    ax.hlines(b['median_ms'],b['start'],b['end'],color=C['ink'],linewidth=2,alpha=.6)
for row in trials[:180]:draw_trial(ax,row)
ax.set_xlim(.5,180.5)
ax.set_xticks([1,36,72,108,144,180])
ax.set_xlabel('Trial number · grouped by setting',fontsize=14,labelpad=12)
ax.set_ylabel('Move + image duration (ms)',fontsize=14,labelpad=12)
fig.text(.075,.465,'Separate validation: replaying old settings is a controlled comparison, not a regression.',fontsize=15)
for i,b in enumerate(batches[-2:]):
    mini=fig.add_axes([.075+i*.485,.285,.405,.135]);axes_style(mini)
    mini.set_title(('Old settings replay' if i==0 else 'Final settings')+f' · median {b["median_ms"]:.0f} ms',loc='left',fontsize=15,pad=12)
    for row in trials[b['start']-1:b['end']]:draw_trial(mini,row)
    mini.hlines(b['median_ms'],b['start'],b['end'],color=C['ink'],linewidth=2,alpha=.6)
    mini.set_xlim(b['start']-.5,b['end']+.5)
    mini.set_xticks([b['start'],(b['start']+b['end'])//2,b['end']])
    mini.set_xlabel('Trial number',fontsize=11)
    mini.tick_params(labelsize=11)
    if i==0:mini.set_ylabel('Duration (ms)',fontsize=11)
fig.text(.075,.235,'Dots: measured calls. Short rules: medians. ×: geometry failure. Both validation groups share an origin.',fontsize=12,color=C['muted'])
for i,b in enumerate(batches):
    column=i//6;row=i%6;x=.05+column*.49;y=.195-row*.026
    label=b['label'].replace('arrival checks','checks').replace('arrival check','check')
    fig.text(x,y,f'{b["batch"]:02d}  {label}',fontsize=12.5)
    fig.text(x,y-.012,f'18 moves · median {b["median_ms"]:.0f} ms · geometry {b["geometry_passes"]}/18',fontsize=10.5,color=C['muted'])
fig.text(.05,.028,f'{active_ms/1000:.1f} s of measured controller calls. Agent time and gaps excluded; UTC timestamps unavailable.',fontsize=12,color=C['muted'])
fig.savefig(OUT/'motion-trials.png',dpi=160)
svg_path=OUT/'motion-trials.svg'
fig.savefig(svg_path,metadata={'Date':None})
# Add exact timings and source references to each editable SVG point.
ns='http://www.w3.org/2000/svg';ET.register_namespace('',ns)
tree=ET.parse(svg_path)
by_id={node.attrib.get('id'):node for node in tree.getroot().iter()}
for row in trials:
    node=by_id[f'trial-{row["trial"]:03d}'];node.set('data-trial',str(row['trial']))
    label=f'Trial {row["trial"]}: {row["duration_ms"]:.2f} ms; batch {row["batch"]:02d}; {row["setting"]}; {row["axis"]}; geometry '+('pass' if row['geometry_pass'] else 'FAIL')
    node.set('tabindex','0');node.set('role','button');node.set('aria-label',label)
    ET.SubElement(node,'{'+ns+'}title').text=label
svg=ET.tostring(tree.getroot(),encoding='unicode')
svg_path.write_text('\n'.join(s.rstrip() for s in svg.splitlines())+'\n')

# Self-contained interactive companion; no network libraries or APIs.
template='''<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>Peek &amp; Seek: 216 measured trials</title><style>
:root{color-scheme:light}*{box-sizing:border-box}body{margin:0;padding:24px;background:#F5F3EC;color:#303D2C;font:16px/1.6 Arial,Helvetica,sans-serif}main{max-width:1280px;margin:auto}a{color:inherit}h1{font:36px/1.15 Georgia,serif;letter-spacing:-1px}label{display:block}input{width:100%;accent-color:#303D2C}output{display:block;min-height:82px;padding:14px 0;font-size:17px;font-variant-numeric:tabular-nums}.chart{overflow:auto}svg{display:block;width:100%;min-width:950px;height:auto}g[data-trial]{cursor:pointer}g[data-trial].selected use{stroke:#303D2C;stroke-width:3;fill:#BF956B}g[data-trial]:focus-visible{outline:2px solid #303D2C}p.note{font-size:14px;color:#626959}@media(max-width:600px){body{padding:18px}h1{font-size:30px}}
</style></head><body><main><a href="/writing/peek-seek-rsi/">← Read the experiment</a><h1>Every physical trial, inspectable.</h1><p>180 tuning trials, then a separate 36-trial before/after validation. Returning to old settings for that check is not a regression. Within-batch order is preserved; absolute UTC start times were not recorded.</p><label for="trial">Inspect trial <span id="number">1</span> of 216</label><input id="trial" type="range" min="1" max="216" step="1" value="1"><output id="detail" aria-live="polite"></output><div class="chart">__SVG__</div><p class="note">Timings exclude agent reasoning, inter-trial gaps, restores and delayed optical evaluation. Geometry is one quality check; blur and scan-revisit failures are documented separately. Active-call time is cumulative measured work, not session elapsed time.</p><p><a href="motion-trials.csv">Download all trial timings (CSV)</a> · <a href="motion-trials.json">Sources and conditions (JSON)</a></p></main><script id="data" type="application/json">__DATA__</script><script>
const records=JSON.parse(document.getElementById('data').textContent);const slider=document.getElementById('trial');const detail=document.getElementById('detail');let selected=null;
function show(n){const r=records[n-1];slider.value=n;document.getElementById('number').textContent=n;detail.textContent=`Trial ${n}: ${r.duration_ms.toFixed(2)} ms · batch ${String(r.batch).padStart(2,'0')} · ${r.setting} · ${r.axis} ${r.direction>0?'+':'−'} · geometry ${r.geometry_pass?'pass':'FAIL'} · cumulative active calls ${(r.active_end_ms/1000).toFixed(2)} s`;if(selected)selected.classList.remove('selected');selected=document.getElementById(`trial-${String(n).padStart(3,'0')}`);selected.classList.add('selected');}
slider.addEventListener('input',()=>show(Number(slider.value)));document.querySelectorAll('g[data-trial]').forEach(g=>{g.addEventListener('mouseenter',()=>show(Number(g.dataset.trial)));g.addEventListener('click',()=>show(Number(g.dataset.trial)));g.addEventListener('focus',()=>show(Number(g.dataset.trial)));});show(1);
</script></body></html>'''
(OUT/'motion-trials.html').write_text(template.replace('__SVG__',svg).replace('__DATA__',json.dumps(trials)))
print(json.dumps(coverage,indent=2))
