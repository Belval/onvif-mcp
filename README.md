# Camera sight MCP

Local stdio MCP server, verified with a TP-Link Tapo C210. Provides fresh JPEG
images and bounded pan/tilt movement using ONVIF and RTSP.

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
This checkout includes `.codex/config.toml` with absolute paths for this machine;
change them if relocating the repository. Restart/reload the Codex session to
load the newly configured server. Codex must trust the project for project-local
configuration to apply. See [Codex MCP configuration](https://learn.chatgpt.com/docs/extend/mcp).

## Tools

- `camera_see`: fresh video frame as an MCP JPEG image, at most 1280×960.
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

## Verification

```sh
uv pip install pytest
.venv/bin/python -m pytest -q
.venv/bin/python scripts/verify_live.py
# Physically pan briefly, then capture another frame:
.venv/bin/python scripts/verify_live.py --move
```

The live script launches the real stdio MCP server, checks discovery, reads status,
and decodes image content. `--move` also verifies a short movement and Stop.
