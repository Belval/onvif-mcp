# Acoustic wake verification, 2026-10-29

The previous implementation prompted Whisper with the wake phrase and allowed a
matching generated transcript to start a Codex turn. The user confirmed an
activation while absent from the office. Raw audio for that event was not saved,
so an exact replay of the false trigger is unavailable.

The replacement removes the Whisper prompt and requires agreement between its
transcript and a separate sherpa-onnx Gigaspeech 3.3M acoustic keyword model.
Settings: int8 encoder/joiner, CPU, one thread, keyword score 1.0, threshold 0.35,
two trailing blanks. Fresh acoustic decoder state per utterance. No fallback to
text-only activation. Model installer pins the archive SHA-256.

Validation on this host:

| Input/check | Result |
| --- | --- |
| 15 seconds of actual C210 room audio | Acoustic model rejected wake; 0.42 s to inspect full clip |
| 11-second JFK speech fixture | Acoustic model rejected wake; 0.30 s to inspect full clip |
| Upstream `LIGHT UP` speech sample | Acoustic model detected keyword; 0.10 s |
| Synthetic `Hey Remi, what time is it?`, through 8 kHz A-law quantization/resampling | Acoustic + Whisper gate accepted; Whisper transcribed `Hey Remy! What time is it?` |
| Synthetic near phrases (`are you ready`, `Hey Randy`, `read me`) | Joint gate rejected |
| Missing acoustic model | MCP start returned error; hearing remained stopped |
| Missing acoustic model, explicit empty wake phrase | Ordinary transcription started and stopped successfully |
| Live continuous C210 test | No wake deliveries, audio gaps or dropped chunks/utterances; **one candidate transcript was acoustically rejected** |

The live candidate was `Hey, Remy, how are you doing?`. It coincided with the
user asking whether an attempt was heard, but its raw utterance was not retained.
This is a likely false rejection, not a confirmed successful live wake test.
Synthetic and short negative fixtures do not establish a real-world false-alarm
rate. Automatic Codex delivery remains disconnected while collecting calibration
audio. Whisper still hallucinates short ambient transcripts; these cannot alone
activate a request.

Opt-in fixture checks (PCM16 little-endian mono, 16 kHz):

```sh
CAMERA_WAKE_TEST_PCM=/path/to/negative.pcm \
CAMERA_WAKE_TEST_EXPECT=false \
cargo test --locked local_acoustic_fixture -- --ignored --nocapture

CAMERA_STT_FIXTURE_PCM=/path/to/positive.pcm \
CAMERA_STT_EXPECT=hey CAMERA_STT_WAKE_EXPECT=true \
CAMERA_WAKE_TEST_ALIASES='Hey Remy' \
cargo test --locked local_model_transcribes_known_fixture -- --ignored --nocapture
```

Raw room/user audio stays outside Git. The `Hey Remy` alias handles alternate
transcription of the configured name; it is subject to both checks.
