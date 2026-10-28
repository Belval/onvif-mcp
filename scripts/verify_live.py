"""Exercise the actual stdio MCP server; optional short physical movement."""
import asyncio
import base64
from pathlib import Path
import sys
from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client

async def main():
    root = Path(__file__).resolve().parents[1]
    params = StdioServerParameters(command=str(root / '.venv/bin/python'), args=[str(root / 'camera_mcp.py')])
    async with stdio_client(params) as (read, write):
        async with ClientSession(read, write) as session:
            await session.initialize()
            names = {t.name for t in (await session.list_tools()).tools}
            assert names == {'camera_status','camera_see','camera_move','camera_stop'}
            async def call(name, args=None):
                result = await session.call_tool(name, args or {})
                assert not result.isError, result
                return result
            print((await call('camera_status')).content)
            for label in (['before', 'after'] if '--move' in sys.argv else ['before']):
                if label == 'after':
                    print((await call('camera_move', {'pan': .15, 'tilt': 0, 'seconds': .3})).content)
                result = await call('camera_see')
                block = next(c for c in result.content if c.type == 'image')
                assert block.mimeType == 'image/jpeg'
                path = Path('/tmp') / ('camera-' + label + '.jpg')
                path.write_bytes(base64.b64decode(block.data))
                print('MCP image:', path)
            print((await call('camera_status')).content)
            await call('camera_stop')

asyncio.run(main())
