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
fig,axes=plt.subplots(2,1,figsize=(12,17),dpi=140)
fig.subplots_adjust(left=.25,right=.92,top=.85,bottom=.115,hspace=.32)
fig.text(.045,.965,'Where camera time goes',fontfamily='DejaVu Serif',fontsize=28)
fig.text(.045,.938,'14 camera actions · current CUDA release · server wall time',fontsize=14)
ys=np.arange(len(data))
for index,(ax,normalized) in enumerate(zip(axes,[False,True])):
    left=np.zeros(len(data))
    for category,color in zip(categories,colors):
        values=np.array([r['breakdown_ms'][category] for r in data])
        if normalized:values=values/totals*100
        ax.barh(ys,values,left=left,color=color,height=.68,label=category)
        left+=values
    ax.set_yticks(ys,labels);ax.invert_yaxis()
    ax.set_ylim(len(data)-.4,-.8)
    ax.spines[['top','right','left']].set_visible(False)
    ax.spines['bottom'].set_color(brand['border'])
    ax.tick_params(axis='y',length=0,pad=10)
    ax.set_axisbelow(True)
    ax.grid(axis='x',color=brand['border'],linewidth=.6)
    ax.set_title('Share of each action' if normalized else 'Total time and its components',loc='left',fontsize=18,pad=16)
    if normalized:
        ax.set_xlim(0,117);ax.set_xticks([0,25,50,75,100],['0%','25%','50%','75%','100%'])
        ax.set_xlabel('Percentage of server wall time',labelpad=10)
        for y,total in zip(ys,totals):ax.text(102,y,f'{total:,.2f} ms',va='center',fontsize=12)
    else:
        ax.set_xlim(0,max(totals)*1.18);ax.set_xlabel('Milliseconds',labelpad=10)
        for y,total in zip(ys,totals):ax.text(total+max(totals)*.015,y,f'{total:,.2f}',va='center',fontsize=12)
handles,legend_labels=axes[0].get_legend_handles_labels()
fig.legend(handles,legend_labels,loc='upper left',bbox_to_anchor=(.035,.919),ncol=3,frameon=False,fontsize=13)
fig.text(.045,.037,'Median-duration trace of 3 trials per action; cold startup has 1 sample.',fontsize=12)
fig.text(.045,.021,'Nested spans counted once. Agent/client time excluded. Controller waits are not pure motor time.',fontsize=11)
fig.savefig(HERE/'camera-action-costs.png')
plt.close(fig)
