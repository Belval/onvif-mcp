"""Local ONVIF sight and bounded pan/tilt tools over MCP stdio."""
from datetime import timedelta
from io import BytesIO
import math
import os
from pathlib import Path
import threading
import time
from urllib.parse import urlsplit, urlunsplit, quote

import av
from dotenv import load_dotenv
from mcp.server.fastmcp import FastMCP, Image
from onvif import ONVIFCamera
import requests
from zeep.helpers import serialize_object
from zeep.transports import Transport

load_dotenv(Path(__file__).with_name('.env'))
mcp = FastMCP('Camera sight')
_lock = threading.RLock()
_camera = None


class Camera:
    def __init__(self):
        self.host = os.environ.get('CAMERA_HOST', '192.168.0.62')
        self.user = os.environ['CAMERA_USERNAME']
        self.password = os.environ['CAMERA_PASSWORD']
        session = requests.Session()
        session.trust_env = False
        self.device = ONVIFCamera(self.host, int(os.environ.get('CAMERA_PORT', '2020')),
                                  self.user, self.password, no_cache=True, adjust_time=True,
                                  transport=Transport(session=session, timeout=8, operation_timeout=8))
        self.media = self.device.create_media_service()
        self.ptz = self.device.create_ptz_service()
        self.profiles = self.media.GetProfiles()
        self.profile = next((p for p in self.profiles if p.PTZConfiguration), self.profiles[0])

    def status(self):
        info = self.device.devicemgmt.GetDeviceInformation()
        return {'manufacturer': info.Manufacturer, 'model': info.Model,
                'profiles': [{'token': p.token, 'name': p.Name, 'ptz': bool(p.PTZConfiguration)} for p in self.profiles],
                'ptz': serialize_object(self.ptz.GetStatus({'ProfileToken': self.profile.token}))}

    def stop(self):
        self.ptz.Stop({'ProfileToken': self.profile.token, 'PanTilt': True, 'Zoom': True})

    def move(self, pan, tilt, seconds):
        if not all(math.isfinite(v) for v in (pan, tilt, seconds)):
            raise ValueError('Movement values must be finite')
        if not (-1 <= pan <= 1 and -1 <= tilt <= 1 and 0 < seconds <= 2):
            raise ValueError('pan/tilt must be -1..1; seconds must be >0 and <=2')
        try:
            self.ptz.ContinuousMove({'ProfileToken': self.profile.token,
                'Velocity': {'PanTilt': {'x': pan, 'y': tilt}},
                'Timeout': timedelta(seconds=seconds)})
            time.sleep(seconds)
        finally:
            self.stop()
        return {'stopped': True}

    def see(self):
        uri = self.media.GetStreamUri({'StreamSetup': {'Stream': 'RTP-Unicast',
                'Transport': {'Protocol': 'RTSP'}}, 'ProfileToken': self.profile.token}).Uri
        parts = urlsplit(uri)
        if parts.scheme != 'rtsp' or parts.hostname != self.host:
            raise ValueError('Camera returned an unexpected streaming endpoint')
        auth = quote(self.user, safe='') + ':' + quote(self.password, safe='')
        uri = urlunsplit((parts.scheme, auth + '@' + parts.netloc.split('@')[-1], parts.path, parts.query, ''))
        with av.open(uri, options={'rtsp_transport': 'tcp'}, timeout=(8, 8)) as stream:
            frame = next(stream.decode(video=0))
            img = frame.to_image()
            img.thumbnail((1280, 960))
            output = BytesIO()
            img.save(output, format='JPEG', quality=85)
            return output.getvalue()


def camera():
    global _camera
    if _camera is None:
        _camera = Camera()
    return _camera


def safe_call(method, *args):
    # Never send third-party exceptions containing authenticated URLs to clients.
    with _lock:
        try:
            return getattr(camera(), method)(*args)
        except ValueError:
            raise ValueError('Invalid camera request or endpoint') from None
        except Exception:
            raise RuntimeError('Camera operation failed. Check local connectivity and camera credentials.') from None


@mcp.tool()
def camera_status() -> dict:
    """Read camera model, available profiles and current pan/tilt status."""
    return safe_call('status')


@mcp.tool()
def camera_see() -> Image:
    """See a fresh camera frame as an MCP image. No recording is saved."""
    return Image(data=safe_call('see'), format='jpeg')


@mcp.tool()
def camera_move(pan: float = 0, tilt: float = 0, seconds: float = 0.3) -> dict:
    """Look around: pan/tilt velocities -1..1; positive right/up. Max 2 seconds, then stop.

    Use small movements and camera_see afterwards to check the view.
    """
    return safe_call('move', pan, tilt, seconds)


@mcp.tool()
def camera_stop() -> dict:
    """Explicitly stop all camera movement."""
    safe_call('stop')
    return {'stopped': True}


def main():
    mcp.run(transport='stdio')


if __name__ == '__main__':
    main()
