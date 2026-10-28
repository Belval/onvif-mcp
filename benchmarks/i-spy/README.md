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
