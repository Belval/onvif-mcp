"""Render the action timing stacks as a standalone image."""
from pathlib import Path
import json
import numpy as np
import matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt

HERE=Path(__file__).resolve().parent
ROOT=HERE.parents[1]
data=json.loads((HERE/'action-breakdown.json').read_text())['rows']
brand=json.loads((ROOT/'docs/blog/assets/brand-tokens.json').read_text())['colors']
categories=['ONVIF','Controller waits','Frame wait','Tracking','Response','Other MCP']
colors=[brand['ink'],brand['return'],brand['sage'],brand['flow'],brand['warm'],brand['border']]
names={'Go to view · .05 pan':'Go to saved view','Clear views · 2':'Clear two views'}
labels=[names.get(r['action'],r['action']) for r in data]
totals=np.array([r['server_ms'] for r in data])
plt.rcParams.update({'font.family':'DejaVu Sans','font.size':14,'text.color':brand['ink'],
    'axes.labelcolor':brand['ink'],'xtick.color':brand['muted'],'ytick.color':brand['ink'],
    'figure.facecolor':brand['paper'],'axes.facecolor':brand['paper'],
    'savefig.facecolor':brand['paper']})
summary = json.loads((HERE / 'summary.json').read_text())
by_name = {row['action']: row for row in data}
groups = [
    ('1   Aim and move', [4, 5, 6, 7, 9, 10], 1100),
    ('2   Acquire an image', [0, 2, 1], 270),
    ('3   Query and manage views', [3, 11, 8, 12, 13], 19),
]
assert sorted(i for _, indices, _ in groups for i in indices) == list(range(len(data)))
fig, axes = plt.subplots(3, 1, figsize=(11, 13), dpi=150,
                         gridspec_kw={'height_ratios': [6, 3, 5]})
fig.subplots_adjust(left=.24, right=.94, top=.79, bottom=.17, hspace=.64)
fig.text(.045, .953, 'Where can we save the next 100 ms?', fontsize=25, family='DejaVu Serif')
default = by_name['Center · default']
precise = by_name['Center · precise']
delta = precise['server_ms'] - default['server_ms']
move_delta = sum(precise['breakdown_ms'][key] - default['breakdown_ms'][key]
                 for key in ['ONVIF', 'Controller waits'])
fig.text(.045, .91, f'Precise centering took {delta:.0f} ms longer than default. Controller waits and ONVIF calls')
fig.text(.045, .885, f'accounted for {move_delta:.0f} ms of that gap. Both modes reported the target centered.')
for ax, (heading, indices, limit) in zip(axes, groups):
    rows = [data[i] for i in indices]
    y = np.arange(len(rows))
    left = np.zeros(len(rows))
    for category, color in zip(categories, colors):
        widths = np.array([r['breakdown_ms'][category] for r in rows])
        ax.barh(y, widths, left=left, height=.64, color=color, label=category)
        left += widths
    for pos, total in zip(y, left):
        label = f'{total:.0f}' if total >= 100 else f'{total:.2f}'
        ax.text(total + limit * .012, pos, label, va='center', fontsize=11)
    ax.set_yticks(y, [labels[i] for i in indices])
    ax.invert_yaxis()
    ax.set_xlim(0, limit)
    ax.set_title(heading + '  ·  milliseconds', loc='left', fontsize=15)
    ax.spines[:].set_visible(False)
    ax.tick_params(length=0)
    ax.grid(axis='x', color=brand['border'])
    ax.set_axisbelow(True)
handles, _ = axes[0].get_legend_handles_labels()
fig.legend(handles, categories, loc='upper left', bbox_to_anchor=(.04,.868),
           ncol=3, frameon=False, fontsize=12)
baseline = summary['variants']['baseline']['median_ms']
final = summary['variants']['candidate']['median_ms']
fig.text(.045,.095, f'Separate live A/B test · Warm preview fell from {baseline:.2f} to {final:.2f} ms (600 requests per release).', fontsize=13)
fig.text(.045,.062, 'Bars show server time for one median-duration request of three, except cold startup (one trial).', fontsize=10)
fig.text(.045,.043, 'Each panel has its own scale. The A/B test measures client wall time. All measurements exclude agent time.', fontsize=10)
fig.savefig(HERE / 'camera-action-costs.png')
plt.close(fig)
