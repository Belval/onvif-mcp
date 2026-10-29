//! Microphone RTSP capture for finite clips and explicitly started continuous hearing.
use crate::onvif::{Config, Onvif};
use anyhow::{Context, Result, anyhow, bail, ensure};
use futures_util::StreamExt;
use retina::{
    client::{Credentials, PlayOptions, Session, SessionOptions, SetupOptions},
    codec::{CodecItem, FrameFormat},
};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::time::timeout;
use url::Url;

#[derive(Clone)]
pub struct Input {
    url: Url,
    config: Config,
    video_transport: Arc<AtomicBool>,
}

pub fn validate_duration(seconds: f64) -> Result<()> {
    ensure!(
        seconds.is_finite() && (0.25..=15.).contains(&seconds),
        "Recording duration must be between 0.25 and 15 seconds"
    );
    Ok(())
}

#[derive(Clone, Copy)]
enum Codec {
    Alaw,
    Mulaw,
}
impl Codec {
    fn decode(self, byte: u8) -> i16 {
        match self {
            Self::Alaw => {
                let a = byte ^ 0x55;
                let segment = (a >> 4) & 7;
                let mut value = ((a & 15) as i32) * 16 + 8;
                if segment > 0 {
                    value += 256;
                }
                if segment > 1 {
                    value <<= segment - 1;
                }
                (if a & 128 != 0 { value } else { -value }) as i16
            }
            Self::Mulaw => {
                let a = !byte;
                let value = (((a & 15) as i32 * 8 + 132) << ((a >> 4) & 7)) - 132;
                (if a & 128 == 0 { value } else { -value }) as i16
            }
        }
    }
}

struct Capture {
    samples: Vec<i16>,
    target: usize,
    rate: u32,
    first_timestamp: Option<i64>,
    gap_samples: usize,
    lost_packets: u64,
}
impl Capture {
    fn new(rate: u32, seconds: f64) -> Self {
        let target = (rate as f64 * seconds).round() as usize;
        Self {
            samples: Vec::with_capacity(target),
            target,
            rate,
            first_timestamp: None,
            gap_samples: 0,
            lost_packets: 0,
        }
    }
    fn push(&mut self, timestamp: i64, samples: &[i16], loss: u16) -> Result<()> {
        self.lost_packets += u64::from(loss);
        let first = *self.first_timestamp.get_or_insert(timestamp);
        let Some(offset) = timestamp
            .checked_sub(first)
            .and_then(|v| usize::try_from(v).ok())
        else {
            return Ok(());
        };
        if offset > self.samples.len() {
            let gap = offset - self.samples.len();
            ensure!(
                gap <= self.rate as usize / 2,
                "Microphone stream lost more than half a second of audio"
            );
            let end = offset.min(self.target);
            self.gap_samples += end - self.samples.len();
            self.samples.resize(end, 0);
        }
        let skip = self.samples.len().saturating_sub(offset).min(samples.len());
        let count = (samples.len() - skip).min(self.target - self.samples.len());
        self.samples.extend_from_slice(&samples[skip..skip + count]);
        Ok(())
    }
    fn complete(&self) -> bool {
        self.samples.len() == self.target
    }
}

fn wav(samples: &[i16], rate: u32) -> Vec<u8> {
    let size = samples.len() as u32 * 2;
    let mut data = Vec::with_capacity(size as usize + 44);
    data.extend_from_slice(b"RIFF");
    data.extend_from_slice(&(size + 36).to_le_bytes());
    data.extend_from_slice(b"WAVEfmt ");
    data.extend_from_slice(&16u32.to_le_bytes());
    data.extend_from_slice(&1u16.to_le_bytes());
    data.extend_from_slice(&1u16.to_le_bytes());
    data.extend_from_slice(&rate.to_le_bytes());
    data.extend_from_slice(&(rate * 2).to_le_bytes());
    data.extend_from_slice(&2u16.to_le_bytes());
    data.extend_from_slice(&16u16.to_le_bytes());
    data.extend_from_slice(b"data");
    data.extend_from_slice(&size.to_le_bytes());
    for sample in samples {
        data.extend_from_slice(&sample.to_le_bytes());
    }
    data
}

impl Input {
    pub async fn discover(onvif: &Onvif) -> Result<Self> {
        let profile = onvif
            .profiles
            .iter()
            .filter(|p| p.encoding == "H264")
            .min_by_key(|p| p.width)
            .ok_or_else(|| anyhow!("No RTSP camera profile available for microphone discovery"))?;
        Ok(Self {
            url: onvif.stream_url(&profile.token).await?,
            config: onvif.config.clone(),
            video_transport: Arc::new(AtomicBool::new(false)),
        })
    }
    pub async fn open(&self) -> Result<AudioStream> {
        let video = self.video_transport.load(Ordering::Relaxed);
        match self.open_once(video).await {
            Err(error) if !video && error.to_string().contains("RTP-Info missing stream URL") => {
                self.video_transport.store(true, Ordering::Relaxed);
                crate::metrics::counter("audio.video_transport_fallback", 1);
                self.open_once(true).await
            }
            result => result,
        }
    }
    async fn open_once(&self, video_transport: bool) -> Result<AudioStream> {
        let creds = Credentials {
            username: self.config.username.clone(),
            password: self.config.password.clone(),
        };
        let mut session = crate::metrics::result("audio.rtsp_describe", timeout(Duration::from_secs(4),
                Session::describe(self.url.clone(), SessionOptions::default().creds(Some(creds))))).await
                .context("Microphone RTSP discovery timed out")?
                .map_err(|_| anyhow!("Microphone RTSP discovery failed; check the local camera account and available stream slots"))?;
        let (index, codec, rate, codec_name) = session.streams().iter().enumerate().find_map(|(index, stream)| {
                if stream.media() != "audio" || stream.channels().is_some_and(|n| n.get() != 1) { return None; }
                let codec = match stream.encoding_name() { "pcma" => Codec::Alaw, "pcmu" => Codec::Mulaw, _ => return None };
                let rate = stream.clock_rate_hz();
                (8000..=48000).contains(&rate).then_some((index, codec, rate, stream.encoding_name().to_owned()))
            }).ok_or_else(|| anyhow!("Camera does not advertise a supported mono G.711 microphone stream (PCMA or PCMU)"))?;
        if video_transport
            && let Some(video) = session.streams().iter().position(|s| s.media() == "video")
        {
            crate::metrics::result(
                "audio.video_transport_setup",
                timeout(
                    Duration::from_secs(3),
                    session.setup(
                        video,
                        SetupOptions::default().frame_format(FrameFormat::SIMPLE),
                    ),
                ),
            )
            .await
            .context("Microphone companion transport setup timed out")?
            .map_err(|_| anyhow!("Camera refused companion transport setup"))?;
        }
        crate::metrics::result(
            "audio.rtsp_setup",
            timeout(
                Duration::from_secs(3),
                session.setup(
                    index,
                    SetupOptions::default().frame_format(FrameFormat::SIMPLE),
                ),
            ),
        )
        .await
        .context("Microphone RTSP setup timed out")?
        .map_err(|_| anyhow!("Camera refused microphone RTSP setup"))?;
        let session = crate::metrics::result(
            "audio.rtsp_play",
            timeout(Duration::from_secs(3), session.play(PlayOptions::default())),
        )
        .await
        .context("Microphone RTSP start timed out")?
        .map_err(|error| {
            anyhow!(
                "Microphone RTSP start failed (status {:?}): {}",
                error.status_code(),
                error.to_string().lines().next().unwrap_or("unknown")
            )
        })?;
        let frames = session
            .demuxed()
            .map_err(|_| anyhow!("Microphone stream could not be decoded"))?;
        Ok(AudioStream {
            frames,
            index,
            codec,
            rate,
            codec_name,
            source: rand::random(),
            offset: 0,
            sequence: 0,
            last_samples: 0,
        })
    }
    pub async fn listen(&self, seconds: f64) -> Result<(Value, Vec<u8>)> {
        validate_duration(seconds)?;
        crate::metrics::result("audio.listen", async {
            let started = Instant::now();
            let mut stream = self.open().await?;
            let rate = stream.rate;
            let codec_name = stream.codec_name.clone();
            let mut capture = Capture::new(rate, seconds);
            let mut first_sample_delay_ms = None;
            let capture_started = Instant::now();
            crate::metrics::result("audio.capture", timeout(Duration::from_secs_f64(seconds+3.), async {
                while !capture.complete() {
                    let chunk = stream.next().await?;
                    first_sample_delay_ms.get_or_insert_with(|| started.elapsed().as_secs_f64()*1000.);
                    capture.push(chunk.timestamp,&chunk.samples,chunk.loss)?;
                }
                Ok::<_, anyhow::Error>(())
            })).await.context("Microphone recording did not complete in time")??;
            // Dropping the RTSP session closes it, including on cancellation/errors.
            drop(stream);
            if !capture.complete() { bail!("Microphone recording is incomplete"); }
            let capture_wall_ms = capture_started.elapsed().as_secs_f64()*1000.;
            crate::metrics::counter("audio.pcm_samples",capture.samples.len() as u64);
            crate::metrics::counter("audio.gap_samples",capture.gap_samples as u64);
            crate::metrics::counter("audio.lost_packets",capture.lost_packets);
            let data = crate::metrics::sync("audio.wav_encode", || wav(&capture.samples,rate));
            let metadata = json!({"kind":"microphone_recording","mime_type":"audio/wav","encoding":"PCM signed 16-bit little-endian","source_codec":codec_name,
                "sample_rate_hz":rate,"channels":1,"samples":capture.samples.len(),"duration_seconds":capture.samples.len() as f64/rate as f64,
                "timestamp_basis":"decoded sample count; gaps estimated from packet loss","requested_seconds":seconds,"gap_samples":capture.gap_samples,"lost_packets":capture.lost_packets,
                "first_sample_delay_ms":first_sample_delay_ms,"capture_wall_ms":capture_wall_ms,"elapsed_ms":started.elapsed().as_secs_f64()*1000.,
                "complete":true,"transcribed":false});
            Ok((metadata,data))
        }).await
    }
}

pub struct AudioChunk {
    pub samples: Vec<i16>,
    pub rate: u32,
    pub timestamp: i64,
    pub loss: u16,
    pub observed_at_ms: u64,
    pub source: u64,
    pub sequence: u64,
}
pub struct AudioStream {
    frames: retina::client::Demuxed,
    index: usize,
    codec: Codec,
    pub rate: u32,
    codec_name: String,
    source: u64,
    offset: u64,
    sequence: u64,
    last_samples: usize,
}
impl AudioStream {
    pub async fn next(&mut self) -> Result<AudioChunk> {
        loop {
            let frame = crate::metrics::result(
                "audio.packet_wait",
                timeout(Duration::from_secs(3), self.frames.next()),
            )
            .await
            .context("Microphone stopped delivering audio")?
            .ok_or_else(|| anyhow!("Microphone stream ended"))?
            .map_err(|_| anyhow!("Microphone RTP stream failed"))?;
            if let CodecItem::AudioFrame(frame) = frame {
                if frame.stream_id() != self.index {
                    continue;
                }
                crate::metrics::counter("audio.packets", 1);
                crate::metrics::counter("audio.encoded_bytes", frame.data().len() as u64);
                let samples: Vec<i16> = crate::metrics::sync("audio.g711_decode", || {
                    frame.data().iter().map(|b| self.codec.decode(*b)).collect()
                });
                // Some C210 firmware emits jittery audio RTP timestamps. G.711 has
                // one sample per payload byte, so use decoded sample counts and
                // sequence-loss accounting rather than manufacturing timestamp gaps.
                self.offset += u64::from(frame.loss()) * self.last_samples as u64;
                let timestamp = self.offset as i64;
                self.offset += samples.len() as u64;
                self.last_samples = samples.len();
                self.sequence += 1;
                return Ok(AudioChunk {
                    samples,
                    rate: self.rate,
                    timestamp,
                    loss: frame.loss(),
                    observed_at_ms: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64,
                    source: self.source,
                    sequence: self.sequence,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn g711_vectors_and_wav_header() {
        for (b, s) in [(0xd5, 8), (0x55, -8), (0xaa, 32256), (0x2a, -32256)] {
            assert_eq!(Codec::Alaw.decode(b), s);
        }
        for (b, s) in [(0xff, 0), (0x7f, 0), (0x80, 32124), (0x00, -32124)] {
            assert_eq!(Codec::Mulaw.decode(b), s);
        }
        let data = wav(&[8, -8], 8000);
        assert_eq!(data.len(), 48);
        assert_eq!(&data[..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(data[4..8].try_into().unwrap()), 40);
        assert_eq!(u32::from_le_bytes(data[24..28].try_into().unwrap()), 8000);
        assert_eq!(&data[44..], &[8, 0, 248, 255]);
    }
    #[test]
    fn timestamp_gaps_overlap_and_duration_bounds() {
        let mut c = Capture::new(8000, 0.001);
        c.push(100, &[8; 3], 0).unwrap();
        c.push(102, &[-8; 3], 1).unwrap();
        c.push(107, &[8; 4], 0).unwrap();
        assert!(c.complete());
        assert_eq!(c.samples, vec![8, 8, 8, -8, -8, 0, 0, 8]);
        assert_eq!(c.gap_samples, 2);
        assert_eq!(c.lost_packets, 1);
        let mut c = Capture::new(8000, 1.);
        c.push(0, &[8], 0).unwrap();
        assert!(c.push(9000, &[8], 0).is_err());
        for v in [f64::NAN, f64::INFINITY, -1., 0., 15.1] {
            assert!(validate_duration(v).is_err());
        }
        for v in [0.25, 3., 15.] {
            validate_duration(v).unwrap();
        }
    }
}
