# MCP optimization, 26 November 2026

## Iteration 1: reuse frame features and vectorize trace scanning

Tracking previously decoded and extracted FAST/BRIEF features twice for every
point. A lazy `OnceLock` now retains prepared descriptors on the immutable frame.
Consecutive corrections reuse the preceding frame's features; nine-point optical
validation prepares each image once. The cache dies with the existing bounded
frame history, preserves the matching algorithm, and never keys on camera sequence
numbers. It is absent until tracking needs it. Traces retain cache hit/miss counters.

Transport instrumentation now uses `memchr` to find newline boundaries rather
than scanning each base64 byte with a scalar iterator. Byte counts, fragmented
IDs, message boundaries and complete wire traces are preserved.

Paired releases ran in A/B/B/A/A/B order, with tracing enabled throughout and no
compilation during measurement. Pooled medians from three blocks per variant:

| Host workload | Before | After | Reduction |
| --- | ---: | ---: | ---: |
| Three consecutive correction tracks | 116.80 ms | 84.19 ms | 27.9% |
| Nine points on one image pair | 349.83 ms | 74.18 ms | 78.8% |
| Trace adapter, 512 KiB response to sink | 0.3164 ms | 0.0096 ms | 97.0% |

Each tracking block includes 30 measured batches after three warm-ups. Frame
objects are recreated each batch so feature caches start cold; JPEG inputs use
fixed seed 20261126, 640 × 360, and a known 5 × 3 px translation. Every result
must remain within .01 normalized radius of the expected point. Unit tests also
require cached and uncached coordinates to match exactly. This is host work,
not physical motion, sensor cadence, or full tool latency.

`phase1-pipeline-results.json` retains the samples and binary hashes.
`phase1-live-results.json` separately records 1,200 complete camera MCP requests
(200 per block, 50 ms pause, same A/B order), validated JPEG dimensions, frame
ages, full producer-stage summaries and a detail request after each block. No
camera settings or position changed; no camera images are retained here.

## Reproduce

Build release binaries before running any measurements. The original baseline is
`42b8450`. For its offline workload only, apply `baseline-harness.patch` to that
checkout: it adds the diagnostic CLI and a thin uncached tracker wrapper, without
changing production behavior. The baseline live test uses the unpatched release.
Build with the repository's native CPU flags and the same Cargo toolchain.

```sh
target/release/onvif-sight --benchmark-pipeline
python3 benchmarks/mcp-optimization/run_pipeline.py \
  --baseline /path/to/baseline-with-harness --candidate /path/to/candidate \
  --output /path/to/results.json
python3 benchmarks/mcp-optimization/run_live.py \
  --baseline /path/to/baseline --candidate /path/to/candidate \
  --host CAMERA_IP --output /path/to/live.json
```

The live runner needs Pillow and uses credentials from the ignored `.env`.
It overrides the host without editing configuration. Do not run other benchmark
clients or compilation concurrently. Timers cover JSON request serialization
through complete response parsing; JPEG validation happens outside the timer.
Repeated block medians and raw samples are supplied rather than treating a single
observed maximum as a guarantee or conflating component gains with motor speed.

Validation: 46 active tests passed (three model-dependent tests remain ignored),
Clippy passed with warnings denied, and both releases passed the deterministic
tracking workload's geometry assertions.
