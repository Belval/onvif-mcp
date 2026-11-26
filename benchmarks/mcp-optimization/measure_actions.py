"""Trace bounded camera actions; isolate saved views and always restore the initial pose."""
import argparse
from datetime import datetime,timezone
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import time

ROOT=Path(__file__).resolve().parents[2]
def module(name,path):
    spec=importlib.util.spec_from_file_location(name,path);value=importlib.util.module_from_spec(spec);spec.loader.exec_module(value);return value
history=module('history',ROOT/'benchmarks/commit-history/benchmark.py')
stream=module('stream',ROOT/'benchmarks/commit-history/stream_profile.py')
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--host',required=True)
args=parser.parse_args()
work=ROOT/'captures/mcp-optimization-2026-11-26/action-profile';work.mkdir(parents=True,exist_ok=True)
if not (work/'.env').exists():(work/'.env').symlink_to(ROOT/'.env')
shutil.copy2(ROOT/'calibration.json',work/'calibration.json')
device,media=stream.connect(args.host);ptz=device.create_ptz_service()
profile=next(p for p in media.GetProfiles() if p.PTZConfiguration is not None)
def position():
    p=ptz.GetStatus({'ProfileToken':profile.token}).Position.PanTilt
    return [float(p.x),float(p.y)]
def stop():ptz.Stop({'ProfileToken':profile.token,'PanTilt':True})
def goto(pose):
    try:
        ptz.AbsoluteMove({'ProfileToken':profile.token,'Position':{'PanTilt':{'x':pose[0],'y':pose[1]}}})
        deadline=time.monotonic()+12;stable=0
        while stable<2:
            if time.monotonic()>deadline:raise TimeoutError('Pose restoration timed out')
            time.sleep(.06);now=position()
            stable=stable+1 if max(abs(a-b) for a,b in zip(now,pose))<.003 else 0
    finally:stop()
    time.sleep(.25)
origin=position();(work/'origin.json').write_text(json.dumps(origin)+'\n')
binary=ROOT/'target/release/onvif-sight'
env=history.environment();env.update(CAMERA_CONFIG_DIR=str(work),CAMERA_HOST=args.host,CAMERA_METRICS='1')
client=history.Client([str(binary)],work,env)
out=Path(__file__).with_name('action-traces.json')
info=device.devicemgmt.GetDeviceInformation()
data={'measured_at':datetime.now(timezone.utc).isoformat(),'binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(),
      'build_features':'cuda','device':{'model':info.Model,'firmware':info.FirmwareVersion},
      'protocol':'Three samples per action except cold snapshot (one). Small bounded moves; setups/restores and diagnostic trace reads outside action clocks. Isolated saved-view store. Original pose restored in finally. Server traces end at completed pipe write; client wall measured separately. No images retained.',
      'origin':origin,'restored':False,'samples':[]}
def save():out.write_text(json.dumps(data,indent=2)+'\n')
def metadata(result):
    return [json.loads(b['text']) for b in result.get('content',[]) if b['type']=='text']
def call(name,arguments=None,label=None):
    result,wall=client.request('tools/call',{'name':name,'arguments':arguments or {}})
    if result.get('isError'):raise RuntimeError('Camera action failed: '+name)
    for b in result.get('content',[]):
        if b['type']=='image':history.image_info({'content':[b]})
    if label:
        ident=result['_meta']['camera_metrics']['request_id']
        trace,_=client.request('tools/call',{'name':'camera_metrics','arguments':{'request_id':ident,'full':True}})
        trace=metadata(trace)[0]
        assert trace['response_complete'] and trace['trace_complete'] and trace['dropped_spans']==0
        # Trace values hold timing/poses only; image blocks and arguments are excluded.
        data['samples'].append({'action':label,'tool':name,'client_wall_ms':wall,
            'focus_status':next((m.get('focus_status') for m in metadata(result) if 'focus_status' in m),None),'trace':trace})
        save();print(label,round(wall,2),'ms',data['samples'][-1]['focus_status'] or '',flush=True)
    return result
try:
    client.request('initialize',{'protocolVersion':'2024-11-05','capabilities':{},'clientInfo':{'name':'action-breakdown','version':'1'}})
    client.send({'jsonrpc':'2.0','method':'notifications/initialized'})
    call('camera_snapshot',label='Snapshot · cold')
    for _ in range(3):
        time.sleep(.08);call('camera_snapshot',label='Snapshot · warm')
    for _ in range(3):call('camera_snapshot',{'detail':True},'Detail image')
    for _ in range(3):call('camera_status',label='Status')
    for rough,precise,label in [(True,False,'Center · rough'),(False,False,'Center · default'),(False,True,'Center · precise')]:
        for _ in range(3):
            goto(origin)
            seen=metadata(call('camera_snapshot'))[0]
            call('camera_center',{'x':.65,'y':.45,'frame_id':seen['frame_id'],'rough':rough,'precise':precise},label)
    for _ in range(3):
        goto(origin);call('camera_scan',{'direction':'left' if origin[0]<.8 else 'right','views':3,'step':.05},'Scan · 3 views')
    goto(origin)
    for _ in range(3):call('camera_save_view',{'name':'benchmark-origin'},'Save view')
    for _ in range(3):
        goto([origin[0]+(.05 if origin[0]<.8 else -.05),origin[1]])
        call('camera_goto_view',{'name':'benchmark-origin'},'Go to view · .05 pan')
    for _ in range(3):
        goto(origin);call('camera_move',{'pan':.2,'tilt':0.,'seconds':.15},'Move · 150 ms')
    goto(origin)
    for _ in range(3):call('camera_stop',label='Stop · idle')
    for _ in range(3):
        call('camera_save_view',{'name':'benchmark-origin'})
        call('camera_delete_view',{'name':'benchmark-origin'},'Delete view')
    for _ in range(3):
        call('camera_save_view',{'name':'benchmark-a'});call('camera_save_view',{'name':'benchmark-b'})
        call('camera_clear_views',label='Clear views · 2')
finally:
    try:
        try:call('camera_stop')
        finally:client.close()
    finally:
        goto(origin);final=position();data['restored']=max(abs(a-b) for a,b in zip(origin,final))<.003
        data['final_pose']=final;save()
        print('Original pose restored:',data['restored'],flush=True)
