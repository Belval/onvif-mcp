# Python → Rust checkpoint rerun

These are **new measurements of old source**, recorded on 26 November 2026.
They are not reconstructed October timings. `manifest.json` selects ten exact
commits covering eleven engineering checkpoints; two changes share one commit.

## Protocol

Prepare detached, clean worktrees and release binaries before timing anything:

```sh
python3 benchmarks/commit-history/benchmark.py prepare
.venv/bin/python benchmarks/commit-history/run_suite.py --host CAMERA_IP
```

Credentials come from the ignored repository `.env`; the host override does not
edit it. Worktrees, binaries, build logs and device configuration backups stay in
ignored `captures/commit-timings-2026-11-26/`. The historical Python environment
must provide its camera dependencies; the suite also needs onvif-zeep and Pillow.
Rust uses locked dependencies, release settings and NASM available on PATH. The
harness currently uses the local Cargo installation under `/home/edouard`.

The suite temporarily sets the camera's minor H.264 stream to 640 × 360, then
restores and verifies its entire previous encoder configuration in `finally`.
It does not move the camera. Do not run concurrent camera clients or builds.
An interrupted process may require explicit restoration:

```sh
.venv/bin/python benchmarks/commit-history/stream_profile.py restore --host CAMERA_IP
```

Each MCP checkpoint starts a new process. Cold time covers process launch,
initialization, tool discovery and the first complete image response. The warm
workload is 100 sequential preview requests, separated by 200 ms. Each request
is timed from JSON serialization through complete response parsing. Base64 and
JPEG decode validation happen after the timer. No agent reasoning, movement or
inference is included. Median and linearly interpolated p95 use all 100 samples;
failed calls are failures rather than substituted timeout durations.

Afterward, one full-detail request and 20 further previews exercise detail-stream
cleanup. Producer-stage telemetry is captured where the historical version
supports it. This is a stationary preview benchmark, not an exhaustive test of
tracking, motion, concurrent clients or every resize geometry.

## Boundaries and limitations

- `4eeaf12` has **no MCP server**. Its five printed cache-read timings are
  native-only, quantized to 0.001 ms, and excluded from the MCP chart.
- The Python implementation opens the main H.264 stream for each request and
  downsizes the result. Rust JPEG checkpoints use the camera's 640 × 360 JPEG
  source (advertised 1 fps).
  From `abef2b0`, explicitly selecting H.264 exercises 640 × 360 at 15 fps.
  A warm cache read is not sensor-to-agent latency. Frame age measures software
  receipt age, not camera exposure time.
- Changing architecture and language together does not isolate a Rust language
  speedup. Switching preview sources and caching the encoder share a commit.
- `eca26f7` records build provenance; it has no runtime source change relative
  to `978d768`. Both fresh builds use NASM. A difference between those rows is
  run-to-run variation, not a new code optimization.
- Device firmware for this rerun is recorded with the results. It differs from
  the original October archive. Fresh reruns and historical results remain
  separate; sequential single-block measurements do not establish tiny effects.
- Some original tool outputs contain earlier uncommitted prototypes. Their
  timestamps alone cannot establish which exact committed source produced them.

Raw per-request timings and binary hashes accompany the published comparison.
No images, credentials or authenticated stream URLs belong in the results.

## Tracing overhead control

`tracing-control-2026-11-26.json` holds six additional 100-request blocks from
the same final binary, in on/off/off/on/on/off order with 50 ms between calls.
Reproduce with `.venv/bin/python benchmarks/commit-history/trace_control.py
--host CAMERA_IP`. This control does not reconfigure the camera: the minor
stream was at its restored 1280 × 720 / 15 fps setting, so these absolute
latencies should not be substituted into the main 640 × 360 rerun table.

The enabled block medians were 1.796, 1.805 and 1.814 ms; disabled medians were
1.668, 1.677 and 1.670 ms. The control establishes measurable tracing overhead.
It does not attribute the entire difference between earlier JPEG implementations
and the final H.264 implementation to tracing, nor establish a SIMD speedup.
