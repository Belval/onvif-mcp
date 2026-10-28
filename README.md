# Camera sight MCP — Rust

An ONVIF camera MCP tuned and measured on a TP-Link Tapo C210. The runtime is a
native Rust executable: ONVIF/WS-Security, persistent RTSP/TCP, frame assembly,
MCP stdio, visual tracking and motor control. Python and FFmpeg are not required.

## Build and configure

Requires Rust 1.89+ and a C/C++ compiler for the optional OpenH264 detail decoder.
The preview path uses the camera's native JPEG stream and needs no decoder.

```sh
cargo build --release --locked
cp .env.example .env
chmod 600 .env
```

Fill in `.env` with the **local Camera Account** configured in the Tapo app under
Advanced Settings → Camera Account. This may differ from the Tapo cloud login.
Credentials are excluded from Git. Environment variables override `.env`.
The default ONVIF endpoint is `192.168.0.62:2020`; RTSP endpoints are discovered.
The executable loads `.env`, `calibration.json` and local `views.json` from this
checkout, regardless of its working directory. Set `CAMERA_CONFIG_DIR` to relocate
those files. Rebuild when moving to a different CPU: `.cargo/config.toml` enables
native CPU instructions, and release builds use thin LTO.

Run `target/release/onvif-sight` for MCP stdio, or register it globally in Codex:

```sh
codex mcp add camera -- /mnt/main/Git/onvif-mcp/target/release/onvif-sight
```

The project configuration in `.codex/config.toml` uses the same executable.
After replacing a running server, reload its MCP connection. Changing the server
command/configuration and invoking Codex's `config/mcpServer/reload` queues a
refresh for loaded threads; no full app restart is needed.

## Fast operation

- `camera_see(detail=false)`: a native 640×360 JPEG from the persistent buffer,
  with `frame_id`, dimensions, sequence number, frame age and request time.
  `detail=true` lazily starts an H.264 connection and returns up to 1280×960.
- `camera_focus(x,y,frame_id,precise=false)`: center a selected image point and
  return the resulting image in one call. Coordinates are fractions from the
  top-left: `(0,0)` top-left, `(0.5,0.5)` center. Fast framing allows two internal
  corrections to within 4% normalized distance; `precise=true` allows three
  corrections to within 2%. This aims the camera, not the lens.
- `camera_scan(direction,views=4,step=0.25)`: return several overlapping views in
  **one call**, starting with the current view. Use `left` or `right`, 2–6 views,
  and a step of 0.05–0.35 ONVIF position units. Default step is about 40° on this
  C210. Pick a point in any returned image and pass its `frame_id` to
  `camera_focus`; it returns to that view automatically before centering.
- `camera_remember(name)`: save the currently framed object as a local named view.
- `camera_look(name)`: move directly to a remembered view and return an image.
  Prefer this for stationary objects such as the printer, clock or microphone.
- `camera_status`: model, profiles, position, named views and streaming metrics.
- `camera_move(pan,tilt,seconds)`: raw velocities in -1..1, positive right/up,
  duration greater than zero and at most two seconds. Always attempts Stop.
- `camera_stop`: cancel active movement immediately, bypassing the motion lock.

The buffer holds **only the newest frame**. New frames overwrite old ones;
requests share frame bytes using `Arc`. A bounded history of up to 12 returned
observations supports scan revisits. Disconnected or old streams do not return
stale images; the receiver reconnects automatically with bounded backoff.
Preview pixels are never decoded, resized or recompressed. The camera's JPEG/RTP
quirk requires restoring eight missing JPEG header bytes, inferred from all
remaining luminance quantizers; the encoded image data is preserved exactly.
Unknown layouts/tables and incomplete/out-of-order frames are rejected.

PTZ position polling runs in the background to avoid network round trips on each
look. Focus always reads a fresh position before moving. Normal frame IDs are
invalidated by newer observations or a changed pose; scan observations can be
revisited. IDs include a per-process random epoch, so they cannot accidentally
refer to an image from an earlier server process. All observations expire after
two minutes. Low texture, occlusion, large subject motion and mechanical limits
can prevent centering. Inspect `focus_status`: `centered`, `tracking_lost`,
`mechanical_limit`, `iteration_limit` or `stale_frame`.

Operations involving movement are serialized. Stop bypasses that lock, and
cancellation/disconnect attempts to stop via a drop guard. The camera's timed
velocity command also has a device timeout of at most two seconds. Absolute
moves wait for reported arrival, settle briefly, and require a frame received
*after* movement. The C210 reports no ONVIF optical zoom.

## Calibration and verification

`calibration.json` stores measured image displacement per ONVIF position unit for
this camera/firmware. The controller uses damped, bounded local visual feedback
to handle perspective, motor dead zones and reversal backlash.

```sh
cargo test --release --locked
cargo clippy --all-targets -- -D warnings
# Full MCP client/server verification, detail image and stationary timing:
target/release/onvif-sight --verify-live
# Physically test centering, batch scanning, scan revisits and concurrent Stop;
# restores the camera's original position even if verification fails:
target/release/onvif-sight --verify-motion
# Direct capture/buffer diagnostics:
target/release/onvif-sight --probe
# Aim at a stationary textured scene before recalibrating; restores pose:
target/release/onvif-sight --calibrate
```

Live verification is itself Rust and launches a separate MCP server process.
It writes inspection images only under `/tmp/rust-*.jpg`. Normal calls keep images
in memory. Named positions are stored in ignored `views.json`, not on the camera.
Do not enable third-party RTSP debug logging with real camera credentials.

The previous Python implementation remains available in Git history before the
Rust migration. Comparisons must distinguish network/stream startup, buffered
frame access, complete MCP calls and physical motor travel. Fast tool calls do
not eliminate the assistant's time to inspect images and choose the next action;
batched scans and remembered views remove many of those round trips.
