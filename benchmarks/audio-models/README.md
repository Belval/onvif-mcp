# Local Whisper GPU comparison — 2026-10-29

Host: i7-6700T, GTX 1080 Ti (11 GB), NVIDIA 535.309.01, CUDA 12.2.2,
compute capability 6.1. Native whisper.cpp via whisper-rs 0.16.0. No inference API.

Three paths were measured on identical PCM16/16 kHz mono fixtures. The CPU base
model uses greedy decoding; GPU models use beam size 5. Model loading is excluded
from inference timings, as the continuous listener keeps its model resident.
This compares the selected production configurations, not GPU speed in isolation.

| Fixture | CPU base q5 | CUDA Large v3 Turbo FP16 | CUDA full Large v3 FP16 |
| --- | ---: | ---: | ---: |
| JFK, 11 seconds | 2,381 ms | 792 ms | 1,487 ms |
| Synthetic wake request through 8 kHz G.711 | 2,223 ms | 544 ms | 818 ms |
| Saved C210 room recording, 15 seconds | 2,265 ms | 554 ms | 834 ms |
| Synthetic near-name phrases | 2,276 ms | 611 ms | 1,075 ms |

On the saved camera recording:

- Base: `How are you doing?`
- Turbo: `Hey Remy, how are you doing?`
- Full Large v3: `How are you doing?`

The user confirmed saying the wake request. Turbo recovered its prefix and was
4.09 times faster than the old CPU/base path on this sample. This corrects an
earlier inference that the copied diagnostic audio might have lacked the wake
word: the stronger model recovered it from the same file. The weaker keyword
spotter still rejected this recording; speech transcription and wake calibration
are separate outcomes. Automatic Codex delivery remains unarmed pending the
acoustic gate's calibration.

On synthetic near-name phrases, Turbo and full Large v3 correctly transcribed
`Hey, are you ready? Hey Randy, read me the weather.` The base model produced
`Are you ready? Are you ready? Read me a webber.` All three reproduced the JFK
speech. This small, partly synthetic corpus is not a general word-error-rate
benchmark and does not establish a false-alarm rate.

Both larger models also generated `Thank you.` when raw silence was sent directly
to them in the first benchmark, bypassing VAD. The updated pipeline filters silence
and G.711 comfort-noise samples before inference, and suppresses punctuation-only
output. Real noise can still cause hallucinations; model size is not a substitute
for speech detection or independent acoustic wake verification.

Observed peak VRAM: Turbo 2,190 MiB; full Large v3 4,292 MiB. GPU utilization
reached 100% for both, confirming actual GPU execution. The first Turbo model
load plus inference took 15.3 seconds; cached subsequent six-fixture runs took
6.7 seconds (Turbo) and 6.9 seconds (full), including loading. Startup and steady
state must be assessed separately.

`initial-results.json` contains the first unfiltered outputs and timings. Raw
private room audio stays in ignored `captures/`. Reproduce with compatible local
PCM fixtures in a directory:

```sh
CAMERA_STT_DEVICE=cuda CAMERA_STT_BENCH_DIR=/absolute/path/to/pcm-fixtures \
CAMERA_STT_MODEL=/absolute/path/to/ggml-large-v3-turbo.bin \
scripts/cuda-cargo.sh test --locked local_model_benchmark -- --ignored --nocapture
```

Camera capability probe (read-only): `GetAudioEncoderConfigurations` returned a
single `G711`, `8000` Hz encoder with use count 2. Its options advertised only
G.711 at 8 kHz. No higher-bandwidth audio encoder was offered through ONVIF.

Model SHA-256 values and NVIDIA package SHA-256 values are pinned in the install
scripts/manifest. The CUDA setup is local to the user account and does not modify
the existing NVIDIA driver.

Post-filter smoke run: both GPU models returned empty text for the silence and
room-noise fixtures; see `post-filter-checks.json`. This run overlapped native
compilation, so its timings are not used in the comparison table. A CUDA-compiled
process with `CUDA_VISIBLE_DEVICES=-1` and `CAMERA_STT_DEVICE=auto` successfully
selected base/CPU and transcribed the JFK fixture, validating the fallback path.

A separate replay cropped to the six-second phrase region also produced `Hey Remy, how are you doing?` with Turbo (609 ms in that check).

Production smoke check: the reloaded shared MCP selected `ggml-large-v3-turbo.bin`,
CUDA, beam size 5, and used 2,190 MiB of VRAM. A live inference reported 591 ms,
with no dropped audio or connection errors. A noise segment still produced
`Thank you.`: the guards are useful but do not solve general noise hallucination.
Automatic turn delivery remains disconnected. Stopping hearing released model
memory (down to a 138 MiB CUDA context); restarting reloaded it successfully.
