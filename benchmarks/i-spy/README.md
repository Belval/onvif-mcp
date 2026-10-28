# I spy with my little eye

A second camera is operated by an independent challenge-setting agent. It chooses
recognizable targets in the shared room and keeps its evidence until adjudication.
Only natural-language challenges reach the primary-camera operator: no primary
poses, frame coordinates, or remembered-view lookup. Credentials never enter this
folder. Camera images remain local under /tmp and are not committed.

Each run in runs.jsonl records the revision, prompt, phase, actual MCP calls,
client wall times, completed nested server traces, local evidence paths, verdict,
and total run wall time. Trace reads are diagnostic overhead outside timed tool
calls. Wall time includes assistant inspection and coordination; it is reported
separately from camera/tool time. Instrumentation failures are explicitly marked.
Initial harness setup is excluded; runs start at the recorded started_utc.

A pass requires the requested object to be recognizable, without material motion
smear, and its consistently defined visible center within normalized radius .08
of the image center. Occlusion and mechanical-limit failures must be recorded,
not silently removed. The independent agent adjudicates submitted final images.

Collect three baseline challenges, optimize an evidenced bottleneck, then replay
matched motion trials plus new independently selected challenges. Matched trials
use the same starting/target poses and randomized or alternating variant order;
they measure code changes without conflating them with learning the room layout.
A learned map or named-view shortcut is not counted as a hardware speedup.

Keep device, network and client/model delays separate. Report p50 and p95, success,
call count, frame waits, reconnects and motion phases. Do not sum overlapping spans
or call polling sleeps pure motor time. Check visual arrival separately from the
camera's position reports. Retain raw evidence for rejected approaches as well as
accepted ones. Preserve functional Stop, bounded execution and truthful results.
Camera wear is accepted by the user; longevity is not a speed constraint.

Stop only at an evidenced practical plateau: stream stalls addressed, deliberate
waits tested, command modes/speeds compared, repeated matched trials and fresh game
challenges verified, and remaining candidate changes unable to yield a useful
improvement (roughly 50 ms or 5% on relevant latency) without worse target success
or stale/blurred images. This is a measured plateau for this firmware/LAN/workload,
not proof that every possible camera firmware or protocol modification is optimal.

## Reproduced findings and experimental controls

- Baseline JPEG stream:200 warm snapshots, p50 .792ms, p95 .986ms, maximum2969.6ms;
  two whole-session reconnects. The low median hid the expensive tail.
- Frame-local recovery3847e1b:200 samples, p50 .330ms, p95 .460ms, maximum283.1ms;
  one reconnect in that window. Longer runs still rejected many JPEG headers and
  showed periodic session failures; this change alone did not solve the pipeline.
- SPY-R01's first fast rough frame failed both blur and .08 centering; a later
  refresh passed. Delayed refresh is not a settling-duration measurement.
- JPEG encoder configuration advertises1fps; host publication~80fps is not proof
  of exposure rate. H264 minor advertises15fps and640x360 support. Hardware change
  and backup provenance are in hardware-changes.jsonl; only resolution was changed.
- A15s framing failure also reproduced on the idle second C210. Direct RTSP probes
  confirm SET_PARAMETER400/previous CSeq and GET_PARAMETER200/matching CSeq, but
  OPTIONS also advertises GET_PARAMETER. The exact library failure path still
  requires diagnostic evidence before a keepalive-library patch is justified.

Experimental runtime switch: `CAMERA_PREVIEW=h264` selects persistent H264, encodes
all decoded frames at640px, and keeps detail capture separate. H264 is the candidate default; deployment follows repeated game/optical checks. The original
minor profile was1280x720; after setting640x360 the detail profile becomes the
2304px main stream and is downsampled to1280px, preserving the detail output size.

`target/release/onvif-sight --measure-motor` is an explicit live experiment. It
probes velocity-axis sign, compares default/max absolute, max relative and max
continuous movement with15ms polling, captures0/50/100/300/700ms images against a
late optical reference, writes motor.json incrementally, and restores origin on
normal/error exit. Continuous mode stops on crossing; other modes stop after the
reference capture. Actual capture offsets are recorded: stream stalls can push
samples later than their nominal offset, and such rows cannot prove early arrival.
`CAMERA_BENCH_MODES` is a comma-separated subset; `CAMERA_BENCH_REPEATS` defaults3;
`CAMERA_BENCH_OUTPUT` chooses a separate output file. Optical feature failures stay
explicit. Preliminary explicit absolute speed1 stalled; the failed run is retained.

Opt-in `CAMERA_JPEG_DEBUG` writes at most24 rejected packet header records per
connection to a local0600 file. `CAMERA_RTSP_DEBUG` writes at most8 sanitized
first-line session errors, with account values redacted; neither is enabled in
normal operation. Diagnostic runs are separate from performance comparisons.

The H264 prototype's200 stationary samples had no reconnects, p50 .446ms,
p95 1.181ms, maximum1.680ms; actual decoded source resolution640x360/15fps was
independently checked. Twelve default-absolute H264 trials completed, with
first-reported-arrival medians256.7ms for .05 pan units and940.9ms for .20 units.
Early optical residual was much smaller than the JPEG trials. Continuous-max
medians were324.4/971.6ms and final pan error reached .0339 units: rejected as a
speed optimization. Explicit relative-max timed out before a valid sample;
absolute-max also stalled in the retained initial experiment. Failed variants
are preserved and excluded from successful-trial averages, not hidden.

Keepalive caution: the secondary RTSP method probe confirmed the SET_PARAMETER
quirk, but both methods are advertised and the H264 prototype survived the same
15s boundary. No dependency keepalive patch has been justified or applied.

SIMD candidate uses a persistent TurboJPEG compressor and avoids thumbnailing an
image already within the requested size. RGB conversion is retained to preserve
OpenH264's color interpretation. Geometry/color-channel tests cover preview and
detail sizes. The first live MCP check passed precise/rough focus, scan, revisit
and concurrent Stop. Further wait/poll tuning and fresh game challenges remain.

Game-wall bookkeeping uses the independent judge's UTC for actual start-to-verdict
and prompt-to-verdict durations when available. Recording-close timestamps are
retained separately: later bookkeeping can occur well after the verdict and must
not be presented as response latency. Prompt times are second-resolution agent
records, and SPY-001's precise issue time is unavailable. Missing tool timing stays
explicit. The fresh H264/SIMD headphones challenge passed first attempt with918.85ms
of camera calls; its delivery-to-verdict time was51s, mostly assistant coordination.

Zero-settle/15ms polling/one-confirmation returned matched moves350–400ms sooner,
but3/18 exceeded the .01 optical-stability criterion, and the independent judge
confirmed material smear in both submitted worst pairs. Rejected despite speed.
Secondary-only bounded continuous probes found that simultaneous nonzero pan/tilt
velocities prioritize pan: tilt never progressed. No concurrent-axis win was
shown; encoder signs are velocity pan=-absolute sign, tilt=+absolute sign.

Current wait sweep (18 matched pan/tilt/diagonal calls per setting) is retained in
framing-*.json. Baseline median927ms;15ms poll/one report/50ms settle median622ms;
15ms poll/two reports/no extra settle median596ms. Geometry passed18/18 for these
short-wait variants, and independent early/late image inspection found
fine-detail softening. The later blind semantic review below resolves acceptance. Polling without
sleep gave no useful median gain and a worse maximum (1085ms vs888ms).

Encoder escape-hatch checks were bounded and reversible: bitrate2048, GOV1,
quality5, and JPEG15fps all failed complete-config verification and were rolled
back. The minor retained640x360/15fps/512kbps/quality3/GOV25. Reported option ranges
are not sufficient evidence of an applied setting. Vendor local-account auth on
the second camera failed read-only discovery; no credential guessing followed.

OpenH264 was previously cached without assembly. After installing NASM, its
**release** dependency was cleaned explicitly (`cargo clean --release -p
openh264-sys2`) and rebuilt. Defined SSE2 symbols in libopenh264_common.a confirm
assembly is now present. Installing NASM without rebuilding a previously cached
scalar dependency is insufficient.

A fresh-H264 calibration at the printer measured pan coefficient2.0861 versus the
old1.2648 and reduced one precise verification run2.14s→1.64s. This calibration is
still under review: the old algorithm reuses its initial baseline after returning
from the pan probe, which can contaminate the tilt cross term with return offset.
Gain1.0 did not beat .8 in the current calibration and was not promoted. One test
restore timed out after concurrent Stop; code inspection found deferred cleanup
Stops could overtake later commands. New owned motion leases, immediate Stop with
a queued completion barrier, and cancellation-durable Stop tasks address this;
two deterministic concurrency tests plus the full29-test unit suite passed.
Subsequent live stress verification passed three times (see final-candidate update).

Final-candidate update: `blind-quality.json` contains label-key-withheld scoring
of the two softest pairs per wait variant. Both two-confirmation/zero-extra-wait
pairs passed the original semantic framing rule; both one-confirmation/zero-wait
pairs failed for material directional smear. Fine texture softness is recorded
separately and does not silently redefine the game rule. Some images were familiar
to the judge, so this is not an image-naive blind trial. The candidate defaults
are H264 minor preview, 15ms polling, two arrival reports and no extra wait.
Fresh game checks and deployment remain pending.

Calibration now captures fresh baselines per axis/direction after engaging the
gears, compares signed slopes, and restores the original pose before committing
the matrix. Matched calibration plus gain1 reduced precise verification to
1094–1171ms with one correction. Optional backlash feedforward gave no speed gain
and was removed. Three live Stop/cancellation/restore checks passed after the
durable completion barrier fix. An adaptive bounded arrival timeout now covers
whole-room named moves rather than failing legitimate travel at a fixed6s.

Live functional gate supersedes the zero-wait proposal: precise focus passed
at778ms and rough at518ms, but scan revisit failed.100ms settling passed twice;
50ms passed once, including scan revisit and concurrent Stop. The next candidate
therefore uses50ms extra settling for tracked focus/scan/look and zero for rough.
Raw console evidence is in live-candidate-checks.jsonl; remaining fresh games
and repeated final-build checks are still required.

Secondary speed-space check: `secondary-speed-uri.json` compares eight bounded
encoder-arrival trials with awaited Stops and verified restores. Omitted Speed
arrived in161–261ms for small pan/tilt steps; all four explicit1.0 commands
using the advertised GenericSpeedSpace URI timed out at3s. Adding the URI does
not repair firmware Speed behavior. Secondary configuration reports default pan
speed1.0 (advertised maximum) and tilt.89; there is no advertised faster pan
space. This supports using omitted Speed, not a universal hardware-limit claim.

## Final candidate and deployment

Revision9c83345 uses persistent minor H264, assembly-enabled OpenH264, cached
TurboJPEG and SIMD resize, matched signed calibration with gain1, omitted-Speed
absolute movement,15ms polling/two arrival reports,50ms extra settling for tracked
operations and zero for rough framing. It retains fresh-frame barriers, bounded
execution and cancellation-durable Stop ordering.

`final-comparison.json` summarizes a fresh same-origin pair of18-trial blocks:
baseline300ms/60ms/two reports median933.0ms,p95 1239.0ms versus final50ms/15ms/two
reports median613.8ms,p95 911.1ms. Both passed18/18 optical geometry checks, restored
and stopped. Blocks ran baseline then candidate rather than
interleaved; source, binary and origin were identical. Native timing excludes
late-reference/tracking overhead; real MCP checks cover the complete tool path.

Held-out targets all passed their first submitted final frame. H02 headphones
used the earlier H264/SIMD candidate(918.85ms tool sum); H03 clock and H01 mug
used the final release build with complete traces:17003.88ms/7278.50ms tool sums,
66s/48s prompt-to-verdict. These are independent search tasks, not matched timing
comparisons. Direction choices, visual reasoning and judge coordination remain
in game wall time; learned-room navigation is not called a hardware speedup.

The project/global MCP configuration was promoted to rust-native-v5-h264 and
reloaded. Actual registered tools return H264 status and full metrics. The printer
named view was optically re-centered and saved; the air-outlet view was visually
verified above the open door. Printer-to-outlet look completed in8607ms with
complete wire trace(`deployed-whole-room-trace.json`), validating the adaptive
whole-room timeout. `deployment.json` records the active stream and zero
reconnects in the observed deployment window.

Practical plateau evidence: one arrival report with zero settling caused material
smear; two reports with zero settling failed a real scan revisit;50ms restored
that behavior. Busy polling and backlash feedforward had no useful gain. Explicit
speeds, including the advertised speed-space URI, were much slower; continuous
movement overshot without beating default absolute and did not produce simultaneous
axes. Encoder quality/bitrate/GOP attempts were refused or clamped and rolled back;
the unchanged main-stream preview produced unusable blocking. Remaining host image
retrieval is about2ms warm; physical sequential motion and sensor/stream cadence
dominate meaningful camera latency. This is a practical measured plateau for the
current firmware, LAN and available protocols, not a proof against firmware changes.

Fresh final matched candidate quality review (`final-pair-verdicts.json`) accepted
both lowest-sharpness-ratio pairs under the original semantic framing rule.
Moderate early fine-detail softness remains measurable; it is neither hidden nor
misrepresented as pixel-identical settled quality.
