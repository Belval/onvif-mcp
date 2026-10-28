"""Live MCP point-aiming check. Moves camera; restores original pose on exit."""
import asyncio
import base64
import json
from pathlib import Path
import sys
from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
from camera_mcp import Camera

async def run():
    params = StdioServerParameters(command=str(ROOT/'.venv/bin/python'), args=[str(ROOT/'camera_mcp.py')])
    async with stdio_client(params) as (read, write):
        async with ClientSession(read,write) as session:
            await session.initialize()
            async def call(name,args=None):
                result=await session.call_tool(name,args or {})
                assert not result.isError, result
                meta=json.loads(next(c.text for c in result.content if c.type=='text'))
                block=next(c for c in result.content if c.type=='image')
                return meta,base64.b64decode(block.data)
            meta,jpeg=await call('camera_see')
            Path('/tmp/focus-live-before.jpg').write_bytes(jpeg)
            point=(float(sys.argv[1]),float(sys.argv[2])) if len(sys.argv)>2 else (.6,.6)
            meta,jpeg=await call('camera_focus',{'x':point[0],'y':point[1],'frame_id':meta['frame_id']})
            Path('/tmp/focus-live-after.jpg').write_bytes(jpeg)
            print(json.dumps(meta))
            assert meta['focus_status']=='centered',meta
            assert sum((v-.5)**2 for v in meta['target'])**.5 <= .02
            # The same old frame must no longer be accepted after another observation.
            frame=meta['frame_id']
            await call('camera_see')
            stale,_=await call('camera_focus',{'x':.7,'y':.3,'frame_id':frame})
            assert stale['focus_status']=='stale_frame'
            print('MCP point aiming and stale-frame protection passed')

if __name__ == '__main__':
    camera=Camera()
    origin=camera.position()
    try: asyncio.run(run())
    finally: camera.goto(origin)
