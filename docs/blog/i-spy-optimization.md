# Peek, Seek, and recursive self-improvement

*Two cheap cameras, a game of I spy, and an agent rewriting the tools it uses to see.*

![Peek and Seek, two Tapo cameras wearing googly eyes](assets/peek-and-seek.png)

I asked a coding agent to find the printer in my office. It found it, slowly:
snapshot, move, wait, correct. Because it could edit its camera runtime, every
awkward interaction became a candidate improvement.

The cameras are **Peek and Seek**, two Tapos costing about $25 each. Peek sets
the challenge; Seek searches. Adding googly eyes made AI-controlled cameras
looking around the room feel considerably less off-putting to me.

## Turn the task into the test

We made I spy a **recursive self-improvement (RSI) loop**. An independent agent
behind Peek chose an object and withheld its coordinates. Seek's operator
searched; the challenger judged recognition, centering, and motion smear.
Timing traces identified bottlenecks, and the operator rewrote its own tools
before playing again.

Fresh targets checked usefulness. Repeated same-origin movements separated
runtime improvements from learning where the printer lived.

![The RSI loop: Peek challenges, Seek searches, traces inform tool changes, and matched trials validate them](assets/improvement-loop.png)

## Improve the eyes, then the control loop

Reopening RTSP for each snapshot cost about **2.5 seconds**. We built a Rust
runtime with persistent H.264 ingestion, accelerated codecs, and a latest-frame
buffer. Warm previews reached approximately **2 ms**. This compared capture
architectures, rather than isolating language speed.

One image-point centering call absorbed calibrated ONVIF movement and bounded
visual corrections. Batched scans removed reasoning round trips. Faster variants
still had to pass: maximum-speed commands stalled, continuous motion overshot,
and zero-wait settings blurred images or broke scan revisits.

The final comparison cut median **movement-plus-image latency from 933 to 614 ms
(34%)**, with 18/18 optical geometry passes in each block.

![Latency history across camera runtime commits and tuning iterations, excluding all agent time](assets/latency-history.png)

*Tool/controller time only; agent time excluded. Snapshot maximums cover 200
calls. Crosses mark rejected motion variants. Only the final replay/candidate
pair shares the final origin; its two 18-trial blocks ran baseline first.*

An improvement survived only if independent challenges and replayed measurements
supported it. That external feedback closed the RSI loop.

<details>
<summary>Inspect the video and control pipeline</summary>
<p><img src="assets/camera-pipeline.png" alt="Persistent RTSP ingestion, native codecs, a latest-frame buffer, and calibrated ONVIF control"></p>
</details>

[Full-size chart](assets/latency-history.svg) ·
[Measurements and rejected experiments](../../benchmarks/i-spy/README.md) ·
[Chart data](assets/latency-history.csv) ·
[Chart provenance](assets/latency-history.json) ·
[Source code](https://github.com/Belval/onvif-mcp)
