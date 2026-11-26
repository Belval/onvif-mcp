"""Rebuild latency history from committed benchmarks, excluding agent time.

Requires matplotlib 3.10.x. Run from anywhere; output assets are sibling files.
"""
from pathlib import Path
import csv
import json
import statistics
import subprocess
import matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt
from matplotlib.ticker import FuncFormatter

ROOT = Path(__file__).resolve().parents[2]
BENCH = ROOT/'benchmarks/i-spy'
OUT = Path(__file__).resolve().parent/'assets'
OUT.mkdir(exist_ok=True)
TOKENS = json.loads((OUT/'brand-tokens.json').read_text())['colors']
COLORS = {key:TOKENS[key] for key in ('paper','ink','muted','sage','return')}
COLORS.update(line=TOKENS['border'],accent=TOKENS['flow'])

def percentile(values, q):
    values = sorted(values)
    index = (len(values)-1)*q
    lo = int(index)
    hi = min(lo+1,len(values)-1)
    return values[lo]+(values[hi]-values[lo])*(index-lo)

def recorded_in(path):
    return subprocess.check_output(['git','log','--diff-filter=A','--format=%h','--',str(path.relative_to(ROOT))],cwd=ROOT,text=True).strip()

snapshots=[]
for file,label,runtime,prototype in [
    ('stationary-baseline','JPEG baseline\n93b6533','93b6533',False),
    ('stationary-stream-recovery','Frame recovery\n3847e1b','3847e1b',False),
    ('stationary-h264-preview','H.264 prototype\nuncommitted','3847e1b + prototype',True),
    ('stationary-h264-simd','SIMD + Stop fix\n978d768','978d768',False),
]:
    source=BENCH/(file+'.json')
    values=[r['wall_ms'] for r in json.loads(source.read_text())['rows']]
    snapshots.append(dict(label=label,median_ms=statistics.median(values),p95_ms=percentile(values,.95),max_ms=max(values),n=len(values),source=str(source.relative_to(ROOT)),revision=runtime,prototype=prototype,recorded_in=recorded_in(source),operation='warm MCP snapshot, client wall'))

motion=[]
for file,label,status,revision in [
    ('framing-baseline','300 ms\n2 reports','reference','978d768 evidence'),
    ('framing-fast-zero','0 ms\n1 report','rejected','978d768 evidence'),
    ('framing-fast100','100 ms\n1 report','exploratory','978d768 evidence'),
    ('framing-fast50','50 ms\n1 report','exploratory','978d768 evidence'),
    ('framing-two-zero','0 ms\n2 reports','rejected','978d768 evidence'),
    ('framing-final-matched-baseline','Baseline\nreplay','reference','9c83345'),
    ('framing-final-matched-candidate','Final\n50 ms / 2','accepted','9c83345'),
]:
    source=BENCH/(file+'.json');data=json.loads(source.read_text())
    values=[r['wall_ms'] for r in data['samples']]
    motion.append(dict(label=label,median_ms=statistics.median(values),p95_ms=percentile(values,.95),max_ms=max(values),n=len(values),source=str(source.relative_to(ROOT)),revision=revision,recorded_in=recorded_in(source),status=status,geometry_passes=sum(r['geometry_pass'] for r in data['samples']),operation='native goto + observe, no late-reference evaluation',parameters=data['parameters'],origin=data['origin']))

data={'snapshot':snapshots,'motion':motion,'scope':['Tool/controller wall times only: no model, reasoning, challenge delivery or judging time.','Snapshot values are client wall times, not the smaller server-only summary values.','Snapshot median and observed maximum are distinct; maximum is over 200 calls, not a guarantee.','Motion shows selected chronological tuning iterations, not every experiment or one controlled cross-commit regression test.','Sweep origin differs from final replay origin; only the final baseline/candidate pair is a same-origin final comparison.','Evidence commit does not establish the exact runtime commit; those entries are explicitly labeled evidence.','Zero-wait single-report variant failed geometry and smear; two-report zero-wait later failed a real scan revisit.','H.264 prototype was uncommitted; it is not mislabeled as a released commit.']}
(OUT/'latency-history.json').write_text(json.dumps(data,indent=2)+'\n')
with (OUT/'latency-history.csv').open('w',newline='') as stream:
    fields=['operation','revision','recorded_in','label','n','median_ms','p95_ms','max_ms','source']
    writer=csv.DictWriter(stream,fields,extrasaction='ignore',lineterminator='\n');writer.writeheader()
    writer.writerows(snapshots+motion)

plt.rcParams.update({'font.family':'DejaVu Sans','font.size':16,'text.color':COLORS['ink'],
    'axes.labelcolor':COLORS['ink'],'xtick.color':COLORS['muted'],'ytick.color':COLORS['muted'],
    'figure.facecolor':COLORS['paper'],'axes.facecolor':COLORS['paper'],
    'savefig.facecolor':COLORS['paper'],'svg.fonttype':'none','svg.hashsalt':'peek-seek-rsi'})
fig,axes=plt.subplots(2,1,figsize=(15.2,12.6),dpi=100)
fig.subplots_adjust(left=.10,right=.96,top=.83,bottom=.14,hspace=.80)
fig.text(.048,.955,'04 / BEFORE AND AFTER',fontsize=15,color=COLORS['muted'])
fig.text(.048,.905,'Faster tools. Agent time excluded.',fontfamily='DejaVu Serif',fontsize=31)
for ax in axes:
    ax.spines[['top','right']].set_visible(False)
    ax.spines[['left','bottom']].set_color(COLORS['line'])
    ax.grid(axis='y',color=COLORS['line'],linewidth=1)
    ax.set_axisbelow(True)

ax=axes[0];xs=list(range(len(snapshots)))
ax.set_title('Selected snapshot checkpoints (200 calls per setting)',loc='left',fontsize=19,pad=22)
ax.plot(xs,[r['max_ms'] for r in snapshots],color=COLORS['ink'],marker='o',linewidth=3,label='Observed maximum (200 calls)')
ax.plot(xs,[r['median_ms'] for r in snapshots],color=COLORS['return'],marker='o',linewidth=2,linestyle='--',label='Median')
ax.set_yscale('log');ax.set_ylim(.45,15000)
ax.set_ylabel('Milliseconds · log scale',fontsize=15)
ax.set_xticks(xs,[r['label'] for r in snapshots],fontsize=14)
ax.yaxis.set_major_formatter(FuncFormatter(lambda value,pos:f'{value:,.0f}' if value>=1 else f'{value:g}'))
for x,row in enumerate(snapshots):
    ax.annotate(f'{row["max_ms"]:,.2f} ms',(x,row['max_ms']),xytext=(0,13),textcoords='offset points',ha='center',fontsize=14,fontweight='bold')
ax.legend(loc='upper right',frameon=False,fontsize=14)

ax=axes[1]
ax.set_title('Move + receive an image: the final matched comparison',loc='left',fontsize=19,pad=22)
pair=motion[-2:]
for y,row,color in [(1,pair[0],COLORS['return']),(0,pair[1],COLORS['ink'])]:
    ax.barh(y,row['median_ms'],height=.43,color=color)
    ax.text(row['median_ms']+22,y,f'{row["median_ms"]:.0f} ms',va='center',fontsize=21,fontweight='bold')
ax.set_ylim(-.6,1.6);ax.set_xlim(0,1200)
ax.set_yticks([]);ax.set_ylabel('')
ax.grid(False);ax.grid(axis='x',color=COLORS['line'])
ax.text(0,1.38,'BEFORE · 300 ms settling / 60 ms polling',fontsize=15,color=COLORS['muted'])
ax.text(0,.38,'AFTER · 50 ms settling / 15 ms polling',fontsize=15,color=COLORS['muted'])
ax.set_xlabel('Median milliseconds per move + image call · lower is better',fontsize=15,labelpad=12)
ax.spines['left'].set_visible(False)
fig.text(.10,.025,'34% faster · same starting pose · 18 moves per setting · 18/18 geometry passes for both',fontsize=13,color=COLORS['muted'])
fig.savefig(OUT/'latency-history.png',dpi=160)
fig.savefig(OUT/'latency-history.svg',metadata={'Date':None})
svg=OUT/'latency-history.svg'
svg.write_text('\n'.join(line.rstrip() for line in svg.read_text().splitlines())+'\n')

print('Wrote latency summary and source data; no agent timing fields used.')
