# Camera sight MCP — Rust

An ONVIF camera MCP tuned and measured on a TP-Link Tapo C210. The runtime is a
native Rust executable: ONVIF/WS-Security, persistent RTSP/TCP, frame assembly,
MCP stdio, visual tracking and motor control. Python and FFmpeg are not required.

## Build and configure

Requires Rust 1.89+, a C/C++ compiler, CMake and NASM for bundled OpenH264/TurboJPEG codecs.
The default preview decodes a persistent H.264 stream and produces 640px JPEG images.
On this C210 the minor stream is configured at 640×360/15fps; see the measured
experiments in `benchmarks/i-spy/`. `CAMERA_PREVIEW=jpeg` selects the legacy stream.

```sh
cargo build --release --locked
cp .env.example .env
chmod 600 .env
```

Fill in `.env` with the **local Camera Account** configured in the Tapo app under
Advanced Settings → Camera Account. This may differ from the Tapo cloud login.
Credentials are excluded from Git. If `cargo` is not yet on PATH on this machine,
source `~/.cargo/env` before building. Environment variables override `.env`.
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

- `camera_listen(seconds=3)`: capture 0.25–15 seconds from the camera microphone
  and return a mono PCM16 WAV audio clip with timing and packet-loss metadata.
  This uses your existing local account; it does not require Tapo cloud access,
  transcribe speech, or play sound through the speaker. Video and movement can
  continue while recording.
- `camera_snapshot(detail=false)`: a 640px JPEG from the persistent H.264 buffer,
  with `frame_id`, dimensions, sequence number, frame age and request time.
  `detail=true` opens a short-lived main-stream H.264 connection and returns
  up to 1280×960. It closes after the capture so occasional detail requests do
  not keep decoding video or compete with PTZ and preview traffic.
- `camera_center(x,y,frame_id,precise=false,rough=false)`: center a selected image point and
  return the resulting image in one call. Coordinates are fractions from the
  top-left: `(0,0)` top-left, `(0.5,0.5)` center. Use `rough=true` for one calibrated
  move, two arrival confirmations, and no visual corrections; status is `aimed`, not
  verified `centered`. Points already within 8% of center are left alone.
  Rough and precise cannot be combined. Ordinary fast framing allows two internal
  corrections to within 4% normalized distance; `precise=true` allows three
  corrections to within 2%. This aims the camera, not the lens.
- `camera_scan(direction,views=4,step=0.25)`: return several overlapping views in
  **one call**, starting with the current view. Use `left` or `right`, 2–6 views,
  and a step of 0.05–0.35 ONVIF position units. Default step is about 40° on this
  C210. Pick a point in any returned image and pass its `frame_id` to
  `camera_center`; it returns to that view automatically before centering.
- `camera_save_view(name)`: save the currently framed object as a local named view.
- `camera_goto_view(name)`: move directly to a remembered view and return an image.
  Prefer this for stationary objects such as the printer, clock or microphone.
- `camera_delete_view(name)`: delete one saved pan/tilt position; missing names return `deleted=false`.
- `camera_clear_views()`: clear all saved views after physically relocating the camera.
  Both commands work offline and do not move the camera. Saved-view reads and
  writes share a file lock across processes; other running instances observe
  changes on their next saved-view operation, with no restart needed.
- `camera_status`: model, profiles, position, named views and streaming metrics.
- `camera_move(pan,tilt,seconds)`: raw velocities in -1..1, positive right/up,
  duration greater than zero and at most two seconds. Always attempts Stop.
- `camera_stop`: cancel active movement immediately, bypassing the motion lock.

MCP startup and tool discovery do not open the camera. A connection is created
on the first camera call, then shared by that server for subsequent calls. This
avoids unused streams in other projects and allows schema discovery while offline.

The buffer holds **only the newest frame**. New frames overwrite old ones;
requests share frame bytes using `Arc`. A bounded history of up to 12 returned
observations supports scan revisits. Disconnected or old streams do not return
stale images; the receiver reconnects automatically with bounded backoff.
H.264 decoding uses OpenH264 with assembly acceleration; JPEG encoding uses a
persistent TurboJPEG compressor. Native-size previews skip resizing; larger
frames use SIMD resizing. The optional legacy JPEG/RTP path repairs known header
quirks and drops malformed frames without restarting the whole session.

PTZ position polling runs in the background to avoid network round trips on each
look. Focus always reads a fresh position before moving. Normal frame IDs are
invalidated by newer observations or a materially changed view; small position
cache lag is accepted only when visual matching confirms the selected point
stayed in place. Scan observations can be revisited. IDs include a per-process random epoch, so they cannot accidentally
refer to an image from an earlier server process. All observations expire after
two minutes. Low texture, occlusion, large subject motion and mechanical limits
can prevent centering. Inspect `focus_status`: `centered`, `tracking_lost`,
`mechanical_limit`, `iteration_limit` or `stale_frame`. Rough mode reports `aimed`
and leaves `target` null because the final point has not been visually verified.

Operations involving movement are serialized. Stop bypasses that lock, and
cancellation/disconnect attempts to stop via a drop guard. The camera's timed
velocity command also has a device timeout of at most two seconds. Absolute
moves require two arrival reports with 15ms polling, a 50ms settling delay
(zero for rough framing), and then a frame received
*after* movement. The C210 reports no ONVIF optical zoom.

## Calibration and verification

`calibration.json` stores measured image displacement per ONVIF position unit for
this camera/firmware. The controller uses bounded local visual feedback
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

## Measured performance

Live measurements on this LAN, with the release binary and native CPU flags:

| Operation | Measurement |
| --- | --- |
| First valid MCP preview, including process startup | 0.12–0.16 s |
| Warm MCP preview, 20-call sample | about 1.5–1.6 ms median, 1.8–2.2 ms p95 |
| Precise centering, two visual corrections | 2.2–2.8 s in the reference run; varies with frame/motion waits |
| Rough framing, one move (smaller test offset) | 0.68 s |
| Preview-only three-view scan, 0.10 step | 1.97 s |
| Wider three-view scan, 0.15 step, full verification run | 7–11 s |

These are request timings, not sensor exposure latency or assistant thinking time.
Motor angle, network load and camera scheduling affect movement substantially.
The previous Python implementation reopened the stream for each snapshot and
measured about 2.5 seconds per stationary capture. The architectural change,
not simply compiling the same loop in Rust, removes most of that cost.

## Timing metrics

Metrics are enabled by default. Every MCP tool result carries a generated trace
ID in `_meta.camera_metrics.request_id`. Call
`camera_metrics(request_id="…", full=true)` **after that response** to inspect its
completed trace. Without an ID, the tool returns stage totals, errors,
cancellations, a rolling 256-sample p50/p95/p99, counters and recent traces.
It works without opening the camera.

Coverage includes motion/state/view lock waits; lazy initialization and local
configuration; ONVIF connection setup, WS-Security, HTTP headers/body and XML;
absolute/velocity commands, each position sample, polling intervals and settling;
RTSP DESCRIBE/SETUP/PLAY, reconnect backoff, packet waits, JPEG repair/assembly and
publication; fresh-frame/barrier waits; blocking-worker queue/execution; JPEG
tracking decode, resize/grayscale, FAST/BRIEF, matching, RANSAC and refit; detail
H264 decode, RGB conversion and JPEG encoding; base64, metadata JSON, SDK response
preparation and server pipe writes. Frame metadata includes a source ID, RTP
clock/timestamp and host receipt age for correlation.

Durations use the **host monotonic wall clock**, including scheduling delays.
`inclusive_ms` includes children; `exclusive_ms` removes their interval union.
Do not sum inclusive stages, concurrent spans or background packet statistics.
`uncovered_ms` reports trace time outside measured spans; parent exclusive time
includes uninstrumented glue and measurement overhead. ONVIF HTTP duration
includes network and device processing; it cannot separate them without packet
or device-side evidence. Position reports are not optical arrival feedback.
RTP timestamps are camera media time, not verified sensor exposure timestamps.
Pipe completion excludes client decoding, transport after the pipe, image
inspection and model thinking. RTSP ingestion is background work, not charged
as processing time to a request waiting for a frame.

The process retains 64 request traces and 64 connection traces, with at most
8192 spans per trace. Overflow and logging failures are exposed. Labels are
static; no credentials, request arguments, SOAP, URLs or image bytes are logged.
Pose samples are included. To persist full completed traces off the I/O thread:

```sh
CAMERA_METRICS_FILE='/path/to/metrics/camera-{pid}.jsonl' target/release/onvif-sight
```

The bounded logger queue holds 32 traces and drops rather than blocking requests.
Files are created with mode 0600 and rotate at 16 MiB to one `.jsonl.1` backup.
`{pid}` keeps simultaneous servers separate; old process files require cleanup.
The checkout's `metrics/` directory is ignored by Git. Metrics do not stream to
stderr/stdout outside the MCP protocol. Set `CAMERA_METRICS=off` to bypass timing
and stdio inspection for overhead comparisons. Live verification checks completed
wire traces and motion phase coverage; rough-focus inspection traces are written
to `/tmp/rust-rough-trace.json`.

A 2026-10-28 live sample with metrics and JSONL logging enabled measured warm
previews at 0.63 ms median / 0.80 ms p95, versus 0.64 / 0.86 ms with metrics off
(20 calls each; overhead below run-to-run variation, not proven zero). A 1909.7 ms
rough-focus trace isolated 1030.3 ms in fresh-video watch waits, 426.0 ms in poll
intervals, 341.6 ms in HTTP headers/connect, 101.2 ms settling and 9.2 ms HTTP body
reads. Only 0.002 ms lay outside measured spans. These are one run's exclusive
stages, not a general hardware latency guarantee.

Measured C210 tuning and rejected alternatives are documented in
[`benchmarks/i-spy/README.md`](benchmarks/i-spy/README.md). The final same-origin
18-trial comparison reduced move-plus-image median933ms→614ms with18/18 optical
geometry passes in each block. Three independent held-out targets passed their
first submitted final image. These measurements separate tool time from assistant
search/judging time and do not claim to measure sensor exposure latency.

Tool names were clarified: `camera_see`→`camera_snapshot`, `camera_focus`→
`camera_center`, `camera_remember`→`camera_save_view`, and `camera_look`→
`camera_goto_view`. Old names are no longer advertised. `camera_scan` returns
images for the caller to inspect; it does not perform object recognition.
Saved views contain pan/tilt positions only; saving an existing name replaces it.

## Microphone capture

`camera_listen` discovers the microphone track through RTSP. Mono G.711 A-law (PCMA) and mu-law (PCMU) streams are decoded in Rust
and returned as a WAV `AudioContent` block alongside JSON metadata. No Python,
FFmpeg, cloud API, speech model or additional credentials are needed.

Each recording opens a microphone connection and closes it on completion,
cancellation, stream failure or timeout. The server rejects overlapping recording
requests on the same instance and does not record between calls. It does not save
audio files automatically. Each active call uses one camera RTSP stream slot.

Packet-loss gaps up to half a second are filled with silence and reported in
`gap_samples`/`lost_packets`; larger estimated gaps, stalls and premature stream
ends return errors rather than silently claiming a complete recording.
`duration_seconds` is the returned sample duration; `first_sample_delay_ms`,
`capture_wall_ms` and the request trace separate stream startup from capture and
response encoding. These are host timings, not measured sensor latency.


The C210 firmware tested requires SETUP of both video and audio for audio packets
to arrive. The server automatically retries with the minor video track and discards
video without decoding it. Packet timestamps on this firmware are inconsistent;
sample positions use decoded sample counts and sequence-loss estimates instead.

## Continuous local speech and wake phrases

Install the multilingual base model once (about 60 MB):

```sh
mkdir -p models
curl -fL https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base-q5_1.bin -o models/ggml-base-q5_1.bin
```

Set `CAMERA_STT_MODEL` to use another compatible local Whisper model.
`camera_hearing_start(language="en", wake_phrase="Hey Remi")` starts persistent
microphone capture, WebRTC speech detection and local whisper.cpp inference on CPU.
`language="fr"` or `"auto"` also work. No transcription API or Tapo cloud account
is involved. Model loading and transcription have separate metrics.

Read incremental events with `camera_heard(cursor=previous_cursor, wait_seconds=15)`.
The cursor resets after a server restart; missed events and dropped audio are
reported. Only 128 events are retained in memory; raw audio is not saved by default.
`camera_hearing_status` reports connectivity, inference and delivery errors.
`camera_hearing_stop` releases the stream; server shutdown also stops hearing.
One-shot recording is unavailable while continuous hearing is active.

Install the independent acoustic wake model once:

```sh
scripts/install-wake-model.sh
```

The installer verifies the archive SHA-256. The default 3.3M-parameter English
Zipformer keyword model runs locally on one CPU thread. The build links the
pinned sherpa-onnx native runtime; its Cargo build script downloads the native
archive at build time. Inference requires no network connection.
Set `CAMERA_WAKE_MODEL_DIR` to relocate the extracted model directory.

Wake phrases can be set per start or through `CAMERA_WAKE_PHRASE` in `.env`.
An empty phrase disables wake detection, including aliases. Matching uses whole
words without case or punctuation sensitivity. `wake_aliases=["Hey Remy"]` can
accept alternative spellings of a name. The environment default is
`CAMERA_WAKE_ALIASES`, with aliases separated by semicolons; pass `[]` to override.
Aliases require the same acoustic verification as the primary phrase.

Wake requests require **both** a transcript match and an independent acoustic
keyword match on that utterance's PCM samples. Whisper receives no wake-phrase
prompt. Each acoustic check starts with fresh state; lost audio resets utterance
assembly. If the wake model cannot load, hearing startup fails rather than
falling back to transcript-only activation. Ordinary transcription with
`wake_phrase=""` does not require the acoustic model.

The acoustic threshold defaults to 0.35 with the upstream keyword score of 1.0; optionally set
`CAMERA_WAKE_THRESHOLD` (0.1–1, higher is stricter). A keyword threshold is a model
setting, not a calibrated probability that a user spoke. The chosen setting has
been checked against local room noise and synthetic speech; your voice and room
still need testing. Playback or another speaker can trigger a genuine phrase;
this is not speaker verification. Status reports `acoustic_threshold` and
`acoustic_rejections`; transcript events include `acoustic_wake_detected`.

To wake a local Codex agent, also pass its loaded `codex_thread_id`. A detected
wake phrase starts a turn when idle or steers the current turn when active.
Ordinary transcripts stay in the buffer by default. `ambient_context=true` opts
into adding every transcript to chat as passive context. Delivered transcripts
become part of that Codex thread's context; speaker identity is unverified.
The adapter uses the local Codex control socket, optionally overridden through
`CAMERA_CODEX_SOCKET`. Other MCP clients can poll events and implement their own
wake handling without this adapter.


For local calibration, `camera_hearing_start(diagnostics=true)` retains the last
eight exact resampled utterances in an ignored, private `captures/hearing-*`
folder. Each `.pcm` file is 16 kHz mono signed PCM16 little-endian; its `.json`
sidecar records sample count and timestamps. This is the audio actually passed
to Whisper and the acoustic verifier, without opening another RTSP session.
Status reports `diagnostic_dir`/`diagnostic_error`; events report
`diagnostic_audio_path`. Slots are overwritten after eight utterances, so check
sidecar timestamps before associating a file with an older event. Files remain
on disk after stopping; remove the session folder when diagnosis is finished.
Diagnostics are off by default.
