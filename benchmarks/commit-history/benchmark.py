"""Rebuild and measure historical commits without modifying production source.

Uses stdlib plus Pillow for untimed image validation. Artifacts stay in captures/.
Only snapshot and status tools are allowed. No motion or camera configuration.
"""
import argparse
import base64
from datetime import datetime, timezone
import hashlib
from io import BytesIO
import json
import os
from pathlib import Path
import platform
import select
import socket
import statistics
import subprocess
import time
from PIL import Image

ROOT=Path(__file__).resolve().parents[2]
MANIFEST=json.loads((Path(__file__).with_name('manifest.json')).read_text())
WORK=ROOT/'captures/commit-timings-2026-11-26'
CARGO=Path('/home/edouard/.cargo/bin/cargo')

def config():
    values={}
    for line in (ROOT/'.env').read_text().splitlines():
        if '=' in line and not line.startswith('#'):
            key,value=line.split('=',1);values[key]=value.strip().strip('"').strip("'")
    return values

def reachable(host=None):
    c=config()
    if host:c['CAMERA_HOST']=host
    for port in (int(c.get('CAMERA_PORT','2020')),554):
        with socket.create_connection((c.get('CAMERA_HOST','192.168.0.62'),port),timeout=2):pass

def environment():
    env=os.environ.copy()
    env['PATH']=str(CARGO.parent)+os.pathsep+str(Path.home()/'.local/bin')+os.pathsep+env.get('PATH','')
    # Avoid unrelated listener/metrics settings changing a historical workload.
    for key in list(env):
        if key.startswith('CAMERA_') and key not in ('CAMERA_HOST','CAMERA_PORT','CAMERA_USERNAME','CAMERA_PASSWORD'):
            del env[key]
    return env

def checkout(row):
    folder=WORK/'worktrees'/row['commit'][:7]
    folder.parent.mkdir(parents=True,exist_ok=True)
    if not folder.exists():
        subprocess.run(['git','worktree','add','--detach',str(folder),row['commit']],cwd=ROOT,check=True,stdout=subprocess.DEVNULL)
    actual=subprocess.check_output(['git','rev-parse','HEAD'],cwd=folder,text=True).strip()
    if actual!=row['commit']:raise RuntimeError('Unexpected checkout revision')
    envfile=folder/'.env'
    if not envfile.exists():envfile.symlink_to(ROOT/'.env')
    return folder

def build(row,no_asm=False):
    folder=checkout(row)
    if row['runtime']=='python-mcp':return [str(ROOT/'.venv/bin/python'),str(folder/'camera_mcp.py')]
    target=WORK/('target-no-asm' if no_asm else 'target-asm')
    env=environment()
    if no_asm:env['OPENH264_NO_ASM']='1'
    else:env.pop('OPENH264_NO_ASM',None)
    log=WORK/'logs'/(row['commit'][:7]+('-no-asm' if no_asm else '')+'-build.log')
    log.parent.mkdir(exist_ok=True)
    with log.open('wb') as stream:
        result=subprocess.run([str(CARGO),'build','--release','--locked','--target-dir',str(target),'-j','2'],cwd=folder,env=env,stdout=stream,stderr=subprocess.STDOUT)
    if result.returncode:raise RuntimeError('Build failed; inspect ignored build log')
    binary=WORK/'binaries'/(row['commit'][:7]+('-no-asm' if no_asm else ''))
    binary.parent.mkdir(exist_ok=True)
    import shutil
    shutil.copy2(target/'release/onvif-sight',binary)
    return [str(binary)]

class Client:
    def __init__(self,command,folder,env):
        self.started=time.perf_counter()
        self.process=subprocess.Popen(command,cwd=folder,env=env,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.DEVNULL)
        self.buffer=b'';self.serial=0
    def send(self,value):
        self.process.stdin.write((json.dumps(value)+'\n').encode());self.process.stdin.flush()
    def receive(self,deadline):
        while True:
            if b'\n' in self.buffer:
                line,self.buffer=self.buffer.split(b'\n',1)
                if line.strip():return json.loads(line)
            left=deadline-time.monotonic()
            if left<=0:raise TimeoutError('MCP response timeout')
            if not select.select([self.process.stdout],[],[],left)[0]:raise TimeoutError('MCP response timeout')
            data=os.read(self.process.stdout.fileno(),1024*1024)
            if not data:raise RuntimeError('Historical server exited')
            self.buffer+=data
    def request(self,method,params):
        self.serial+=1;ident=self.serial;start=time.perf_counter()
        self.send({'jsonrpc':'2.0','id':ident,'method':method,'params':params})
        deadline=time.monotonic()+45
        while True:
            response=self.receive(deadline)
            if response.get('id')!=ident:continue
            elapsed=(time.perf_counter()-start)*1000
            if 'error' in response:raise RuntimeError('Historical MCP request failed')
            return response['result'],elapsed
    def close(self):
        self.process.stdin.close()
        try:self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.terminate()
            try:self.process.wait(timeout=3)
            except subprocess.TimeoutExpired:self.process.kill();self.process.wait()

def image_info(result):
    if result.get('isError'):raise RuntimeError('Historical snapshot failed')
    image=next((block for block in result.get('content',[]) if block.get('type')=='image'),None)
    if not image:raise RuntimeError('Snapshot did not return an image')
    data=base64.b64decode(image['data'],validate=True)
    if not data.startswith(b'\xff\xd8') or not data.endswith(b'\xff\xd9'):raise RuntimeError('Invalid JPEG')
    with Image.open(BytesIO(data)) as decoded:
        decoded.load()
        actual_dimensions=decoded.size
    meta={}
    for block in result.get('content',[]):
        if block.get('type')=='text':
            try:meta.update(json.loads(block['text']))
            except (ValueError,TypeError):pass
    return {key:meta.get(key) for key in ('width','height','frame_age_ms','sequence')}|{'jpeg_bytes':len(data),'decoded_width':actual_dimensions[0],'decoded_height':actual_dimensions[1]}

def measure(row,command,samples,interval_ms,block,no_asm=False,host=None,post_detail_samples=20,metrics_enabled=None):
    # Check before starting old servers so an offline device is not timed as work.
    reachable(host)
    folder=checkout(row);env=environment();env['CAMERA_CONFIG_DIR']=str(folder)
    env['CAMERA_PREVIEW']=row['preview']
    if metrics_enabled is not None:env['CAMERA_METRICS']='1' if metrics_enabled else '0'
    if host:env['CAMERA_HOST']=host
    start=datetime.now(timezone.utc).isoformat()
    if row['runtime']=='rust-native-prototype':
        preview=Path('/tmp/rust-camera-preview.jpg')
        preview.touch(mode=0o600,exist_ok=True);preview.chmod(0o600)
        result=subprocess.run(command,cwd=folder,env=env,capture_output=True,timeout=45)
        import re
        text=result.stderr.decode(errors='replace')
        first=re.search(r'Native JPEG ready in ([0-9.]+)s',text)
        warm=[float(v) for v in re.findall(r'Cached fresh frame: ([0-9.]+)ms',text)]
        if result.returncode or not first or not warm:raise RuntimeError('Native prototype did not produce valid timing output')
        measurements=dict(boundary='native video.frame only; NO MCP server exists in this commit',cold_first_ms=float(first[1])*1000,samples=[dict(client_wall_ms=v) for v in warm],mcp_comparable=False,precision_ms=.001)
    else:
        client=Client(command,folder,env)
        try:
            _,initialize_ms=client.request('initialize',{'protocolVersion':'2024-11-05','capabilities':{},'clientInfo':{'name':'commit-latency-benchmark','version':'1'}})
            client.send({'jsonrpc':'2.0','method':'notifications/initialized'})
            listing,tools_list_ms=client.request('tools/list',{})
            names={t['name'] for t in listing['tools']}
            tool='camera_snapshot' if 'camera_snapshot' in names else 'camera_see'
            if tool not in names:raise RuntimeError('No supported stationary preview tool')
            result,first_snapshot_ms=client.request('tools/call',{'name':tool,'arguments':{'detail':False}})
            cold=(time.perf_counter()-client.started)*1000;first=image_info(result)
            rows=[]
            for index in range(samples):
                if interval_ms:time.sleep(interval_ms/1000)
                result,elapsed=client.request('tools/call',{'name':tool,'arguments':{'detail':False}})
                rows.append(dict(index=index,client_wall_ms=elapsed,**image_info(result)))
                if (index+1)%25==0:
                    print(f'{row["commit"][:7]}: {index+1}/{samples} valid previews',flush=True)
            device={};stages={};counters={};after_detail=[];detail_ms=None
            def json_text(value):
                for block in value.get('content',[]):
                    if block.get('type')=='text':
                        try:return json.loads(block['text'])
                        except ValueError:pass
                return {}
            if 'camera_status' in names:
                status,_=client.request('tools/call',{'name':'camera_status','arguments':{}})
                value=json_text(status)
                device={k:value.get(k) for k in ('manufacturer','model','firmware')}
                video=value.get('video',{})
                device['video']={k:video.get(k) for k in ('transport','encoding','buffered_frames','frames_received','reconnects')}
            if 'camera_metrics' in names:
                metric,_=client.request('tools/call',{'name':'camera_metrics','arguments':{}})
                value=json_text(metric)
                stages={k:v for k,v in value.get('durations',{}).items() if k in ('video.h264_decode','video.jpeg_encode','video.resize','video.yuv_to_rgb','video.detail_decode_encode')}
                counters={k:v for k,v in value.get('counters',{}).items() if k in ('video.published_frames','video.reconnects','jpeg.malformed_packets')}
            if row['runtime']=='rust-mcp' and post_detail_samples:
                detail,detail_ms=client.request('tools/call',{'name':tool,'arguments':{'detail':True}})
                image_info(detail)
                for index in range(post_detail_samples):
                    time.sleep(interval_ms/1000)
                    value,elapsed=client.request('tools/call',{'name':tool,'arguments':{'detail':False}})
                    after_detail.append(dict(index=index,client_wall_ms=elapsed,**image_info(value)))
            measurements=dict(boundary='stationary MCP preview: request serialization through complete JSON response parse; JPEG decode validation outside timer',cold_first_ms=cold,initialize_ms=initialize_ms,tools_list_ms=tools_list_ms,first_snapshot_ms=first_snapshot_ms,first_frame=first,samples=rows,mcp_comparable=True,interval_ms=interval_ms,device=device,producer_stages=stages,producer_counters=counters,detail_call_ms=detail_ms,post_detail_samples=after_detail)
        finally:client.close()
    values=sorted(r['client_wall_ms'] for r in measurements['samples'])
    def quantile(q):
        i=(len(values)-1)*q;lo=int(i);hi=min(lo+1,len(values)-1)
        return values[lo]+(values[hi]-values[lo])*(i-lo)
    return dict(commit=row['commit'],changes=row['changes'],measured_at=start,
        historical=False,block=block,preview=row['preview'],no_asm=no_asm,metrics_override=metrics_enabled,
        binary_sha256=hashlib.sha256(Path(command[0]).read_bytes()).hexdigest(),
        source_clean=not bool(subprocess.check_output(['git','status','--porcelain','--untracked-files=no'],cwd=folder,text=True).strip()),
        host=dict(system=platform.system(),machine=platform.machine(),python=platform.python_version(),load_average=os.getloadavg()),
        n=len(values),median_ms=statistics.median(values),p95_ms=quantile(.95),max_ms=max(values),**measurements)

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action',choices=['prepare','run'])
    parser.add_argument('--revision',action='append',help='Seven-character revision; repeat to select several')
    parser.add_argument('--samples',type=int,default=100)
    parser.add_argument('--interval-ms',type=int,default=200)
    parser.add_argument('--block',default='forward')
    parser.add_argument('--no-asm',action='store_true')
    parser.add_argument('--host',help='Override camera address without changing saved configuration')
    args=parser.parse_args()
    if args.samples<1 or args.interval_ms<0:parser.error('Invalid sample count/interval')
    rows=[r for r in MANIFEST['commits'] if not args.revision or r['commit'][:7] in args.revision]
    if not rows:parser.error('No matching revisions')
    WORK.mkdir(parents=True,exist_ok=True)
    for row in rows:
        short=row['commit'][:7]
        print(f'{short}: building',flush=True)
        try:
            command=build(row,args.no_asm)
            if args.action=='prepare':print(f'{short}: ready',flush=True);continue
            result=measure(row,command,args.samples,args.interval_ms,args.block,args.no_asm,args.host)
            out=WORK/'results';out.mkdir(exist_ok=True)
            name=short+'-'+args.block+('-no-asm' if args.no_asm else '')+'.json'
            (out/name).write_text(json.dumps(result,indent=2)+'\n')
            print(f'{short}: n={result["n"]}, median={result["median_ms"]:.3f} ms, p95={result["p95_ms"]:.3f} ms',flush=True)
        except Exception as error:
            # Do not print third-party errors that can contain credentialed URLs.
            print(f'{short}: failed ({type(error).__name__}); no timing recorded',flush=True)
            raise SystemExit(1)

if __name__=='__main__':main()
