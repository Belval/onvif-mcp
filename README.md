# Camera sight MCP

Local stdio MCP server, calibrated and verified with a TP-Link Tapo C210. Provides
fresh JPEG images, one-call point aiming, and pan/tilt movement using ONVIF and RTSP.

## Setup

Use Python 3.10+ and uv:

```sh
uv venv
uv pip install -e .
cp .env.example .env
chmod 600 .env
```

Fill in `.env` with the camera's **local Camera Account** credentials from the
Tapo app (Advanced Settings → Camera Account). These can differ from your Tapo
cloud login. `.env` is ignored by Git. Existing environment variables override it.
The default camera endpoint is `192.168.0.62:2020`; video uses the RTSP endpoint
advertised by the camera. The client must have LAN access to both services.

Run `.venv/bin/python camera_mcp.py`, or register that command in your MCP client.
For use in every project on this machine:

```sh
codex mcp add camera -- /mnt/main/Git/onvif-mcp/.venv/bin/python /mnt/main/Git/onvif-mcp/camera_mcp.py
```

The global registration has been installed for this workspace's user. This
checkout also includes `.codex/config.toml` with absolute paths for this machine;
change them if relocating the repository. Restart/reload the Codex session to
load the newly configured server. Codex must trust the project for project-local
configuration to apply. See [Codex MCP configuration](https://learn.chatgpt.com/docs/extend/mcp).

## Tools

- `camera_see`: fresh 640-pixel-wide JPEG preview (640×360 on this C210),
  plus `frame_id`, width and height. Use `camera_see(detail=true)` for up to
  1280×960 detail. Focus results also return previews. Full-resolution frames stay
  internal for visual tracking, so normalized aiming coordinates are unchanged.
- `camera_focus(x, y, frame_id)`: center a point from that image in one call and
  return the resulting image. Coordinates are fractions of image width/height,
  from the top-left: `(0,0)` is top-left, `(0.5,0.5)` is center. For example, a
  target at pixel `(960,180)` in a `1280×720` image is `(0.75,0.25)`. Supply the
  `frame_id` from the most recent image. This aims the camera, rather than
  changing lens focus.
- `camera_status`: camera model, profiles and reported PTZ position.
- `camera_move`: pan and tilt velocities from -1 to 1, duration greater than zero
  and at most two seconds. Start with 0.15 speed and 0.3 seconds. Direction depends
  on camera mounting/orientation; inspect a frame after moving.
- `camera_stop`: explicit stop.

Moves always attempt Stop in a finally block. The C210 requires a device timeout
of at least one second; that timeout is rounded up while the explicit Stop occurs
after the requested shorter duration. A disconnected client may therefore leave
a move running until the device timeout (at most two seconds). Operations are
serialized. C210 has no ONVIF optical zoom, so no zoom tool is exposed.

Normal image calls return pixels in memory without saving recordings. The live
verification script intentionally writes before/after images into `/tmp`.
Third-party exception details are suppressed at the MCP boundary to avoid
exposing authenticated RTSP URLs. Do not enable SOAP or video debug logging with
real credentials.

## Calibrated aiming

Prefer `camera_focus` over timed movement. It uses absolute ONVIF positions,
waits for the camera to arrive and settle, then uses nearby ORB features and RANSAC to track
the selected point. It makes up to three internal corrections, targeting a
normalized distance of 0.02 from image center. Speed is deliberately omitted
from AbsoluteMove: small explicit speed values caused this C210 to stall or
creep during live checks. Both axes can move in a single absolute request.

`calibration.json` contains measured image displacement per ONVIF position unit
for this camera and firmware. It includes the observed axis signs and small
cross-axis effects. Damped, bounded local corrections adapt that mapping for perspective without
amplifying small motor dead zones or reversal backlash. Video capture consumes
half a second of frames before selecting the image to avoid the initial keyframe. It is
an empirical model, not a claim that motor speed or image displacement is linear
across the entire camera range.

The result includes `focus_status`: `centered`, `tracking_lost`,
`mechanical_limit`, `iteration_limit`, or `stale_frame`. Low texture, occlusion,
large subject motion, or a target near a mechanical limit can prevent centering.
The tool returns an image and reports that outcome; inspect it before retrying.
It does not continuously follow faces or people. Old frame IDs, frames over two
minutes old, and frames from a different camera pose are rejected without moving.
The selected point is checked against a fresh frame before movement.

To recalibrate, aim at a stationary, textured scene (the desk works well), then:

```sh
.venv/bin/python scripts/calibrate.py
```

Calibration briefly moves each axis, measures visual displacement, restores the
original position, and saves only after obtaining enough matches. Restart the
MCP process after changing calibration or updating server code.

## Verification

```sh
uv pip install pytest
.venv/bin/python -m pytest -q
.venv/bin/python scripts/verify_live.py
# Physically pan briefly, then capture another frame:
.venv/bin/python scripts/verify_live.py --move
# Test centering, returning an image, and stale-frame rejection; restore pose:
.venv/bin/python scripts/verify_focus.py 0.6 0.6
```

The live script launches the real stdio MCP server, checks discovery, reads status,
and decodes image content. `--move` also verifies a short movement and Stop.
