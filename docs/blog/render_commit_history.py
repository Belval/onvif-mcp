"""Render the controlled historical-source rerun. Requires matplotlib."""
from pathlib import Path
import csv
import json
import matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt
from matplotlib.ticker import FuncFormatter

ROOT=Path(__file__).resolve().parents[2]
BENCH=ROOT/'benchmarks/commit-history'
OUT=Path(__file__).resolve().parent/'assets'
rows=json.loads((BENCH/'results-2026-11-26.json').read_text())['results']
assert len(rows)==10 and all('median_ms' in r for r in rows)
points=[r for r in rows if r['mcp_comparable']]
colors=json.loads((OUT/'brand-tokens.json').read_text())['colors']
plt.rcParams.update({'font.family':'DejaVu Sans','font.size':13,'text.color':colors['ink'],
    'axes.labelcolor':colors['ink'],'xtick.color':colors['muted'],'ytick.color':colors['muted'],
    'figure.facecolor':colors['paper'],'axes.facecolor':colors['paper'],
    'savefig.facecolor':colors['paper'],'svg.fonttype':'none','svg.hashsalt':'commit-history'})
fig,axes=plt.subplots(3,1,figsize=(16,15.5),dpi=120)
fig.subplots_adjust(left=.09,right=.97,top=.81,bottom=.14,hspace=.95)
fig.text(.055,.95,'PYTHON → RUST / EXACT SOURCE CHECKPOINTS',fontsize=14,color=colors['muted'])
fig.text(.055,.915,'Rebuilt. Measured. Every commit.',fontsize=31,fontfamily='DejaVu Serif')
fig.text(.055,.88,'26 Nov 2026 rerun · same C210 · 100 warm MCP requests per commit · agent time excluded',fontsize=14)
xs=list(range(len(points)))
labels=['Python\n640 px','Rust\nMCP','CPU worker\nisolation','Lazy\nconnections','Full-path\ntracing','Frame-local\nrecovery','H.264 +\nTurboJPEG','SIMD\nresize','ASM build\nverification']
for ax in axes:
    ax.spines[['top','right']].set_visible(False)
    ax.spines[['left','bottom']].set_color(colors['border'])
    ax.grid(axis='y',color=colors['border']);ax.set_axisbelow(True)
    ax.set_xticks(xs,[label+'\n'+r['commit'][:7] for label,r in zip(labels,points)],fontsize=11)
    ax.set_xlim(-.45,len(points)-.55)
ax=axes[0]
ax.set_title('Warm image response · median and p95',loc='left',pad=23,fontsize=18)
ax.plot(xs,[r['p95_ms'] for r in points],color=colors['muted'],marker='s',linestyle='--',label='p95')
ax.plot(xs,[r['median_ms'] for r in points],color=colors['ink'],marker='o',linewidth=2.5,label='Median')
ax.set_yscale('log');ax.set_ylabel('Milliseconds · log scale')
ax.yaxis.set_major_formatter(FuncFormatter(lambda v,p:f'{v:g}'))
ax.set_ylim(min(r['median_ms'] for r in points)*.5,max(r['p95_ms'] for r in points)*4)
for x,r in zip(xs,points):
    ax.annotate(f'{r["median_ms"]:,.2f}',(x,r['median_ms']),xytext=(0,17),textcoords='offset points',ha='center',fontweight='bold',fontsize=12)
ax.legend(frameon=False,loc='upper right')
ax=axes[1]
ax.set_title('Cold start · process launch to first complete image',loc='left',pad=23,fontsize=18)
values=[r['cold_first_ms']/1000 for r in points]
ax.bar(xs,values,color=colors['ink'],width=.5)
ax.set_ylabel('Seconds');ax.set_ylim(0,max(values)*1.25)
for x,v in zip(xs,values):ax.text(x,v+max(values)*.035,f'{v:.2f}',ha='center',fontsize=12)
ax=axes[2]
ax.set_title('After one detail image · worst of 20 subsequent preview requests',loc='left',pad=23,fontsize=18)
detail_points=[(x,max(v['client_wall_ms'] for v in r['post_detail_samples'])) for x,r in zip(xs,points) if r.get('post_detail_samples')]
ax.plot([x for x,v in detail_points],[v for x,v in detail_points],color=colors['ink'],marker='o',linewidth=2.5)
ax.set_yscale('log');ax.set_ylabel('Milliseconds · log scale')
ax.set_ylim(min(v for x,v in detail_points)*.45,max(v for x,v in detail_points)*3)
ax.yaxis.set_major_formatter(FuncFormatter(lambda v,p:f'{v:g}'))
for x,v in detail_points:ax.annotate(f'{v:.2f}',(x,v),xytext=(0,12),textcoords='offset points',ha='center',fontsize=12,fontweight='bold')
fig.text(.055,.083,'4eeaf12 is native ingestion only: no MCP server, so it is excluded from these comparable points.',fontsize=12)
fig.text(.055,.055,'640 px outputs. Python decodes the main stream; Rust uses JPEG (advertised 1 fps), then H.264 (15 fps).',fontsize=12)
fig.text(.055,.027,'978d768 and eca26f7 have identical runtime source; both reruns use NASM. Small differences are not isolated gains.',fontsize=12,color=colors['muted'])
for extension in ('png','svg'):
    path=OUT/('commit-history.'+extension)
    fig.savefig(path,metadata={'Date':None} if extension=='svg' else {})
    if extension=='svg':path.write_text('\n'.join(line.rstrip() for line in path.read_text().splitlines())+'\n')
plt.close(fig)
with (OUT/'commit-history.csv').open('w',newline='') as stream:
    fields=['commit','mcp_comparable','n','cold_first_ms','median_ms','p95_ms','max_ms','detail_call_ms','measured_at']
    writer=csv.DictWriter(stream,fields,extrasaction='ignore',lineterminator='\n');writer.writeheader();writer.writerows(rows)
