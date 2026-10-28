"""Local ONVIF sight and bounded pan/tilt tools over MCP stdio."""
from datetime import timedelta
from io import BytesIO
import math
import json
import uuid
import os
from pathlib import Path
import threading
import time
from urllib.parse import urlsplit, urlunsplit, quote

import av
from PIL import Image as PILImage
from dotenv import load_dotenv
from mcp.server.fastmcp import FastMCP, Image
from mcp.types import CallToolResult, TextContent
import numpy as np
from aiming import track_point, load_calibration, image_size
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
        self.observation = None
        self.calibration = load_calibration(Path(__file__).with_name('calibration.json'))

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
                'Timeout': timedelta(seconds=max(1, math.ceil(seconds)))})
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
            frames = stream.decode(video=0)
            frame = next(frames)
            first_pts = frame.time
            # Consume a little video rather than returning the stream's first keyframe.
            for _ in range(30):
                frame = next(frames)
                if first_pts is not None and frame.time is not None and frame.time-first_pts >= .5:
                    break
            img = frame.to_image()
            img.thumbnail((1280, 960))
            output = BytesIO()
            img.save(output, format='JPEG', quality=85)
            return output.getvalue()


    def position(self):
        point = self.ptz.GetStatus({'ProfileToken': self.profile.token}).Position.PanTilt
        return np.array([point.x, point.y], dtype=float)

    def goto(self, target):
        """Move to a reported position, wait for arrival, and always send Stop."""
        target = np.clip(np.asarray(target, dtype=float), -1, 1)
        try:
            # Omit Speed: low speed values stall this C210 and make timing unreliable.
            self.ptz.AbsoluteMove({'ProfileToken': self.profile.token,
                'Position': {'PanTilt': {'x': float(target[0]), 'y': float(target[1])}}})
            deadline = time.monotonic() + 6
            stable = 0
            while time.monotonic() < deadline:
                time.sleep(.15)
                actual = self.position()
                stable = stable + 1 if np.max(np.abs(actual - target)) < .002 else 0
                if stable >= 2:
                    time.sleep(.65)
                    return actual
            raise RuntimeError('Camera did not reach the requested position')
        finally:
            self.stop()

    def record_observation(self, jpeg, pose, **metadata):
        frame_id = uuid.uuid4().hex[:16]
        self.observation = {'id': frame_id, 'time': time.monotonic(),
                            'pose': pose, 'jpeg': jpeg}
        width, height = image_size(jpeg)
        return {'frame_id': frame_id, 'width': width, 'height': height,
                'coordinates': 'x,y normalized 0..1 from top-left; center=(0.5,0.5)',
                **metadata}, jpeg

    def observe(self, **metadata):
        # Associate pixels with a stationary pose; reject images taken during movement.
        self.stop()
        pose = self.position()
        jpeg = self.see()
        if np.max(np.abs(self.position() - pose)) > .003:
            raise ValueError('Camera moved during capture; request another image')
        return self.record_observation(jpeg, pose, **metadata)

    def focus(self, x, y, frame_id):
        if not all(math.isfinite(v) and 0 <= v <= 1 for v in (x, y)):
            raise ValueError('Point coordinates must be finite and in 0..1')
        old = self.observation
        if old is None or frame_id != old['id'] or time.monotonic() - old['time'] > 120:
            return self.observe(focus_status='stale_frame', detail='Choose a point in this new image and retry.')
        pose = self.position()
        if np.max(np.abs(pose-old['pose'])) > .003:
            return self.observe(focus_status='stale_frame', detail='Camera moved since that image; choose a new point.')
        point = np.array([x, y], dtype=float)
        before = self.see()
        tracked = track_point(old['jpeg'], before, point)
        if tracked is None:
            return self.record_observation(before, pose, focus_status='tracking_lost',
                                           iterations=0, target=None)
        point = tracked
        matrix = self.calibration.copy()
        status = 'iteration_limit'
        iterations = 0
        corrections = []
        for _ in range(3):
            error = point - .5
            if np.linalg.norm(error) <= .02:
                status = 'centered'
                break
            delta = -.8 * np.linalg.solve(matrix, error)
            delta = np.clip(delta, [-.12, -.30], [.12, .30])
            target = np.clip(pose + delta, -1, 1)
            if np.max(np.abs(target-pose)) < .002:
                status = 'mechanical_limit'
                break
            actual = self.goto(target)
            after = self.see()
            iterations += 1
            tracked = track_point(before, after, point)
            if tracked is None:
                status = 'tracking_lost'
                before = after
                break
            movement = actual-pose
            corrections.append({'before': point.tolist(), 'after': tracked.tolist(), 'pose_delta': movement.tolist()})
            shift = tracked-point
            if np.dot(movement, movement) > 1e-6:
                # Local secant correction compensates for tilt, perspective and lens distortion.
                candidate = matrix + np.outer(shift-matrix@movement, movement) / np.dot(movement, movement)
                ratios = np.diag(candidate) / np.diag(self.calibration)
                if (np.linalg.cond(candidate) < 20 and np.all(ratios > .5)
                        and np.all(ratios < 2)
                        and np.max(np.abs(candidate-np.diag(np.diag(candidate)))) < .5):
                    matrix = .5*matrix + .5*candidate
            point, pose, before = tracked, actual, after
        if status == 'iteration_limit' and np.linalg.norm(point-.5) <= .02:
            status = 'centered'
        metadata = {'focus_status': status, 'iterations': iterations,
                    'target': point.tolist() if status != 'tracking_lost' else None, 'corrections': corrections}
        # Return exactly the frame used for the final tracking result.
        return self.record_observation(before, self.position(), **metadata)


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
def camera_see(detail: bool = False) -> CallToolResult:
    """See a fresh 640px-wide preview with a frame_id for camera_focus.

    Set detail=true for a 1280px image when reading small details. Coordinates are normalized 0..1.

    Use camera_focus(x,y,frame_id) to center a point in this image in one call.
    """
    return observation_result(safe_call('observe'), detail=detail)


def observation_result(observation, detail=False):
    metadata, jpeg = observation
    metadata = dict(metadata)
    metadata['source_width'], metadata['source_height'] = image_size(jpeg)
    if not detail:
        with PILImage.open(BytesIO(jpeg)) as image:
            image.thumbnail((640, 480))
            output = BytesIO()
            image.save(output, format='JPEG', quality=70)
            jpeg = output.getvalue()
    metadata['width'], metadata['height'] = image_size(jpeg)
    metadata['detail'] = 'full' if detail else 'preview'
    return CallToolResult(content=[TextContent(type='text', text=json.dumps(metadata)),
                                  Image(data=jpeg, format='jpeg').to_image_content()])


@mcp.tool()
def camera_focus(x: float, y: float, frame_id: str) -> CallToolResult:
    """Center a point from the latest camera image and return the resulting image.

    x,y are 0..1 from top-left (pixel x/width, pixel y/height). Supply frame_id
    from camera_see. This tool performs calibrated absolute movements, waits for
    arrival, and visually corrects up to three times. Read focus_status: centered,
    tracking_lost, mechanical_limit, iteration_limit, or stale_frame. A stale
    request returns a fresh image without moving. This aims the camera; it does
    not control lens focus or follow a subject continuously.
    """
    return observation_result(safe_call('focus', x, y, frame_id))


@mcp.tool()
def camera_move(pan: float = 0, tilt: float = 0, seconds: float = 0.3) -> dict:
    """Look around: pan/tilt velocities -1..1; positive right/up. Max 2 seconds, then stop.

    Prefer camera_focus to center an image point. This raw velocity command is
    not an image displacement. Use camera_see afterwards to check the view.
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
