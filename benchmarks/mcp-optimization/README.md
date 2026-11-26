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

## Iteration 2: SIMD payload encoding

The next profile exposed base64 encoding of image/audio responses. These now use
[`base64-simd`](https://docs.rs/base64-simd/0.8.0/base64_simd/) with the same standard
alphabet and padding. Tests compare exact strings and round trips with the old
encoder for all lengths 0–1023 and large payloads, including partial final groups.
JPEG pixels, dimensions, quality, metadata and tool schemas are unchanged.

`payload-results.json` is a same-binary scalar/SIMD control: six alternating
blocks per size, 100 measured encodes after three warm-ups. String allocation is
timed; complete byte equality is checked outside the timer on every result.
Pooled medians across three blocks per encoder:

| Raw payload | Scalar | SIMD | Reduction |
| --- | ---: | ---: | ---: |
| 32 KiB | 0.02128 ms | 0.00448 ms | 79.0% |
| 128 KiB | 0.08554 ms | 0.01943 ms | 77.3% |
| 512 KiB | 0.30401 ms | 0.06182 ms | 79.7% |

Run `target/release/onvif-sight --benchmark-payload` to repeat. These are hot CPU
microbenchmarks, not an equivalent percentage reduction in full MCP latency.

The final live comparison adds `--intermediate /path/to/iteration-1` to the live
runner. It uses nine blocks in balanced order: A/B/C, B/C/A, C/A/B. A is the
unmodified `42b8450` release; B is iteration 1 (`28cb1a0`); C adds SIMD base64.
Each variant receives 600 stationary previews across three blocks, with metrics
enabled throughout. All three comparison builds use the same default feature
set, native CPU flags and toolchain. GPU audio support is retained separately in
the installed CUDA release.

Results in `final-live-results.json` and `summary.json`:

| Release | Requests | Median | p95 | Observed maximum |
| --- | ---: | ---: | ---: | ---: |
| Baseline | 600 | 1.697 ms | 2.065 ms | 2.439 ms |
| Frame cache + transport | 600 | 1.634 ms | 1.938 ms | 286.342 ms |
| + SIMD base64 | 600 | 1.539 ms | 1.863 ms | 2.869 ms |

Final median and p95 improved by 9.3% and 9.8%. The intermediate outlier is
retained; observed maxima are finite-window observations rather than guarantees.
Frame ages remained similar (p95 about 82 ms in all variants), so the improvement
does not come from serving older frames. The camera profile stayed unchanged.

The final installed binary was rebuilt through `scripts/cuda-cargo.sh`, validated
against an available CUDA device without starting microphone capture, and passed
the full stationary MCP verification. Tests: 47 passed; three explicit local
model/fixture tests remain ignored. Clippy passed with warnings denied.

## Stacked timing by camera action

`action-traces.json` contains 40 complete requests on the installed CUDA release:
three per action, except the single cold snapshot. All default/precise centering
trials reported centered; rough framing reported aimed. The temporary saved-view
store was isolated from user views; the starting physical pose was restored and
verified. Setup, restoration and diagnostic requests are excluded from action
timings. These are small, declared test moves, not universal operation costs.

`action-breakdown.json` selects the median-duration server trace per action.
The stack partitions wall time at every span boundary and assigns each interval
once to the deepest active categorized span. Equal-depth overlaps use the listed
category order; unclassified time remains Other MCP. This avoids adding nested
or parallel spans. ONVIF includes its client processing; controller waits include
polling/settling/velocity sleeps, not pure motor time. Frame wait includes capture
readiness; background decoder work is not added on top of that wait. Tracking
includes its blocking-worker queue. Response includes encoding, preparation and
completed pipe writes. Client parsing and agent time are outside the server bars.

```sh
.venv/bin/python benchmarks/mcp-optimization/measure_actions.py --host CAMERA_IP
python3 benchmarks/mcp-optimization/render_actions.py
python3 benchmarks/mcp-optimization/summarize.py
```

The action script physically exercises bounded centering, scanning and movement,
then restores the initial pose in `finally`. It never clears the user's saved
views. The chart offers absolute milliseconds and normalized shares; both use
the same complete stack data. CPU comparison results and CUDA action timings have
separate populations and timing boundaries.
