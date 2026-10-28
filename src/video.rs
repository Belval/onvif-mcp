use crate::onvif::Config;
use anyhow::{Result, bail};
use futures_util::StreamExt;
use image::{DynamicImage, ImageBuffer};
use openh264::formats::YUVSource;
use retina::{
    client::{Credentials, PlayOptions, Session, SessionOptions, SetupOptions},
    codec::{CodecItem, FrameFormat},
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::{Duration, Instant};
use tokio::{sync::watch, task::JoinHandle};
use url::Url;

type EncodedPicture = (Vec<u8>, u32, u32);
struct H264Worker {
    decoder: openh264::decoder::Decoder,
    jpeg: turbojpeg::Compressor,
}
impl H264Worker {
    fn new() -> Result<Self> {
        Ok(Self {
            decoder: openh264::decoder::Decoder::new()?,
            jpeg: turbojpeg::Compressor::new()?,
        })
    }
}
fn encode_picture(
    jpeg: &mut turbojpeg::Compressor,
    image: DynamicImage,
    max_width: u32,
) -> Result<EncodedPicture> {
    let image = crate::metrics::sync("video.resize", || {
        if image.width() > max_width || image.height() > max_width * 3 / 4 {
            image.thumbnail(max_width, max_width * 3 / 4)
        } else {
            image
        }
    })
    .into_rgb8();
    let (w, h) = image.dimensions();
    jpeg.set_quality(if max_width <= 640 { 75 } else { 85 })?;
    jpeg.set_subsamp(turbojpeg::Subsamp::Sub2x2)?;
    let data = crate::metrics::sync_result("video.jpeg_encode", || {
        jpeg.compress_to_vec(turbojpeg::Image {
            pixels: image.as_raw().as_slice(),
            width: w as usize,
            height: h as usize,
            pitch: w as usize * 3,
            format: turbojpeg::PixelFormat::RGB,
        })
    })?;
    Ok((data, w, h))
}
fn decode_picture(
    mut decoder: H264Worker,
    packet: Vec<u8>,
    encode: bool,
    max_width: u32,
) -> Result<(H264Worker, Option<EncodedPicture>)> {
    crate::metrics::sync_result("video.detail_decode_encode", || {
        let picture = {
            let decoded = crate::metrics::sync_result("video.h264_decode", || {
                decoder.decoder.decode(&packet)
            })?;
            if let Some(yuv) = decoded.filter(|_| encode) {
                let (w, h) = yuv.dimensions();
                let mut rgb = vec![0; w * h * 3];
                crate::metrics::sync("video.yuv_to_rgb", || yuv.write_rgb8(&mut rgb));
                crate::metrics::counter("video.decoded_width_total", w as u64);
                crate::metrics::counter("video.decoded_height_total", h as u64);
                crate::metrics::counter("video.decoded_pictures", 1);
                let image = DynamicImage::ImageRgb8(
                    ImageBuffer::from_raw(w as u32, h as u32, rgb)
                        .ok_or_else(|| anyhow::anyhow!("Invalid video size"))?,
                );
                Some(encode_picture(&mut decoder.jpeg, image, max_width)?)
            } else {
                None
            }
        };
        Ok((decoder, picture))
    })
}
pub struct Frame {
    pub jpeg: Arc<Vec<u8>>,
    pub received: Instant,
    pub sequence: u64,
    pub source_id: u64,
    pub rtp_timestamp: Option<i64>,
    pub clock_rate_hz: u32,
    pub width: u32,
    pub height: u32,
}
#[derive(Default)]
pub struct Metrics {
    pub frames: AtomicU64,
    pub reconnects: AtomicU64,
    pub connected: AtomicBool,
}
#[derive(Clone, Copy)]
struct Encoding {
    jpeg: bool,
    dimensions: (u32, u32),
    max_width: u32,
    encode_interval: Duration,
}
pub struct Video {
    receiver: watch::Receiver<Option<Arc<Frame>>>,
    task: JoinHandle<()>,
    pub metrics: Arc<Metrics>,
}
impl Drop for Video {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Video {
    pub fn start(url: Url, config: Config, jpeg: bool, dimensions: (u32, u32)) -> Self {
        Self::configured(
            url,
            config,
            jpeg,
            dimensions,
            1280,
            Duration::from_millis(150),
        )
    }
    pub fn h264_preview(url: Url, config: Config, dimensions: (u32, u32)) -> Self {
        Self::configured(url, config, false, dimensions, 640, Duration::ZERO)
    }
    fn configured(
        url: Url,
        config: Config,
        jpeg: bool,
        dimensions: (u32, u32),
        max_width: u32,
        encode_interval: Duration,
    ) -> Self {
        let encoding = Encoding {
            jpeg,
            dimensions,
            max_width,
            encode_interval,
        };
        let (tx, receiver) = watch::channel(None);
        let metrics = Arc::new(Metrics::default());
        let counters = metrics.clone();
        let source_id = rand::random();
        let task = tokio::spawn(async move {
            let mut debug_errors = 0;
            let mut delay = Duration::from_millis(250);
            loop {
                let frames_before = counters.frames.load(Ordering::Relaxed);
                let result = Self::receive(
                    url.clone(),
                    config.clone(),
                    &tx,
                    &counters,
                    source_id,
                    encoding,
                )
                .await;
                if let Err(error) = &result {
                    // Classify locally; never export a library error that may contain URLs.
                    let message = error.to_string();
                    if let Some(path) = std::env::var_os("CAMERA_RTSP_DEBUG")
                        && debug_errors < 8
                    {
                        use std::io::Write;
                        let mut safe = message.lines().next().unwrap_or("").to_owned();
                        for secret in [&config.username, &config.password] {
                            if !secret.is_empty() {
                                safe = safe.replace(secret, "[redacted]");
                            }
                        }
                        let mut options = std::fs::OpenOptions::new();
                        options.create(true).append(true);
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::OpenOptionsExt;
                            options.mode(0o600);
                        }
                        if let Ok(mut file) = options.open(path) {
                            let _ = writeln!(
                                file,
                                "{}",
                                serde_json::json!({"first_line":safe,"unexpected_response":message.contains("Unexpected RTSP response"),"bad_request":message.contains("Bad Request"),"source_id":source_id})
                            );
                        }
                        debug_errors += 1;
                    }
                    let message = message.lines().next().unwrap_or("").to_ascii_lowercase();
                    let label = if error.is::<tokio::time::error::Elapsed>() {
                        "video.failure.timeout"
                    } else if message.contains("ssrc") {
                        "video.failure.ssrc"
                    } else if message.contains("timestamp") {
                        "video.failure.timestamp"
                    } else if message.contains("sequence") {
                        "video.failure.sequence"
                    } else if message.contains("rtcp") {
                        "video.failure.rtcp"
                    } else if message.contains("framing") {
                        "video.failure.framing"
                    } else if message.contains("ended") {
                        "video.failure.eof"
                    } else if message.contains("packet") {
                        "video.failure.packet"
                    } else {
                        "video.failure.other"
                    };
                    crate::metrics::counter(label, 1);
                }
                if counters.frames.load(Ordering::Relaxed) > frames_before {
                    delay = Duration::from_millis(250);
                }
                counters.connected.store(false, Ordering::Relaxed);
                tx.send_replace(None); // A disconnected stream must never serve an old picture.
                counters.reconnects.fetch_add(1, Ordering::Relaxed);
                crate::metrics::counter("video.reconnects", 1);
                crate::metrics::sleep("video.reconnect_backoff", delay).await;
                delay = (delay * 2).min(Duration::from_secs(3));
                if tx.is_closed() {
                    break;
                }
            }
        });
        Self {
            receiver,
            task,
            metrics,
        }
    }
    async fn receive(
        url: Url,
        config: Config,
        tx: &watch::Sender<Option<Arc<Frame>>>,
        metrics: &Metrics,
        source_id: u64,
        encoding: Encoding,
    ) -> Result<()> {
        let Encoding {
            jpeg,
            dimensions,
            max_width,
            encode_interval,
        } = encoding;
        crate::metrics::result("video.connection_lifetime", async {
            let (session, clock_rate_hz) =
                crate::metrics::background("rtsp.connect", source_id, async {
                    let creds = Credentials {
                        username: config.username,
                        password: config.password,
                    };
                    let mut session = crate::metrics::result(
                        "rtsp.describe",
                        tokio::time::timeout(
                            Duration::from_secs(4),
                            Session::describe(url, SessionOptions::default().creds(Some(creds))),
                        ),
                    )
                    .await??;
                    let index = session
                        .streams()
                        .iter()
                        .position(|s| {
                            matches!(s.media(), "video" | "image")
                                && s.encoding_name() == if jpeg { "jpeg" } else { "h264" }
                        })
                        .ok_or_else(|| anyhow::anyhow!("Requested video encoding unavailable"))?;
                    let clock_rate_hz = session.streams()[index].clock_rate_hz();
                    crate::metrics::result(
                        "rtsp.setup",
                        tokio::time::timeout(
                            Duration::from_secs(3),
                            session.setup(
                                index,
                                SetupOptions::default().frame_format(FrameFormat::SIMPLE),
                            ),
                        ),
                    )
                    .await??;
                    let session = crate::metrics::result(
                        "rtsp.play",
                        tokio::time::timeout(
                            Duration::from_secs(3),
                            session.play(PlayOptions::default()),
                        ),
                    )
                    .await??;
                    Ok::<_, anyhow::Error>((session, clock_rate_hz))
                })
                .await?;
            if jpeg {
                return Self::receive_jpeg(
                    session,
                    dimensions,
                    tx,
                    metrics,
                    source_id,
                    clock_rate_hz,
                )
                .await;
            }
            let mut frames = session.demuxed()?;
            let mut decoder = H264Worker::new()?;
            let mut encoded_at = Instant::now() - Duration::from_secs(1);
            metrics.connected.store(true, Ordering::Relaxed);
            while let Some(item) = crate::metrics::result(
                "video.h264_packet_wait",
                tokio::time::timeout(Duration::from_secs(3), frames.next()),
            )
            .await?
            {
                if let CodecItem::VideoFrame(frame) = item? {
                    let received = Instant::now();
                    let encode = encoded_at.elapsed() >= encode_interval;
                    let rtp_timestamp = Some(frame.timestamp().timestamp());
                    let packet = frame.into_data();
                    // CPU-heavy detail work must not block the RTSP/PTZ/MCP I/O runtime.
                    let (next, picture) = crate::metrics::blocking(move || {
                        decode_picture(decoder, packet, encode, max_width)
                    })
                    .await??;
                    decoder = next;
                    let Some((data, width, height)) = picture else {
                        continue;
                    };
                    encoded_at = Instant::now();
                    let sequence = metrics.frames.fetch_add(1, Ordering::Relaxed) + 1;
                    crate::metrics::counter("video.published_frames", 1);
                    crate::metrics::counter("video.jpeg_bytes", data.len() as u64);
                    crate::metrics::sync("video.publish", || {
                        tx.send_replace(Some(Arc::new(Frame {
                            jpeg: Arc::new(data),
                            received,
                            sequence,
                            source_id,
                            rtp_timestamp,
                            clock_rate_hz,
                            width,
                            height,
                        })))
                    });
                    if tx.is_closed() {
                        break;
                    }
                }
            }
            bail!("Video stream ended")
        })
        .await
    }
    async fn receive_jpeg(
        mut session: Session<retina::client::Playing>,
        dimensions: (u32, u32),
        tx: &watch::Sender<Option<Arc<Frame>>>,
        metrics: &Metrics,
        source_id: u64,
        clock_rate_hz: u32,
    ) -> Result<()> {
        crate::metrics::result("video.jpeg_session", async {
            use retina::client::PacketItem;
            let mut assembler = crate::jpeg::NativeJpeg::default();
            let mut last_frame: Option<Instant> = None;
            let diagnostic_path = std::env::var_os("CAMERA_JPEG_DEBUG");
            let mut failures_logged = 0;
            metrics.connected.store(true, Ordering::Relaxed);
            while let Some(packet) = crate::metrics::result(
                "video.jpeg_packet_wait",
                tokio::time::timeout(Duration::from_secs(3), session.next()),
            )
            .await?
            {
                crate::metrics::counter("video.received_packets", 1);
                if let PacketItem::Rtp(packet) = packet? {
                    let data = match assembler.push(
                        packet.timestamp().timestamp(),
                        packet.sequence_number(),
                        packet.mark(),
                        packet.payload(),
                    ) {
                        Ok(Some(data)) => data,
                        Ok(None) => continue,
                        Err(error) => {
                            if let Some(path) = &diagnostic_path && failures_logged < 24 {
                                use std::io::Write;
                                let payload=packet.payload();
                                let header = if payload.len()>=8 && payload[1..4]==[0,0,0] { &payload[..payload.len().min(88)] } else { &[] };
                                let mut options=std::fs::OpenOptions::new();options.create(true).append(true);
                                #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600); }
                                if let Ok(mut file)=options.open(path) { let _=writeln!(file,"{}",serde_json::json!({"reason":error.to_string(),"rtp_timestamp":packet.timestamp().timestamp(),"sequence":packet.sequence_number(),"payload_length":payload.len(),"first_packet_header":header})); }
                                failures_logged+=1;
                            }
                            // A damaged frame is not a dead RTSP connection. Resume at the
                            // next frame boundary; never publish guessed or partial pixels.
                            crate::metrics::counter("jpeg.malformed_packets", 1);
                            continue;
                        }
                    };
                    let now = Instant::now();
                    if let Some(last) = last_frame.replace(now) {
                        crate::metrics::observe(
                            "video.jpeg_frame_interval",
                            now.duration_since(last),
                        );
                    }
                    let sequence = metrics.frames.fetch_add(1, Ordering::Relaxed) + 1;
                    crate::metrics::counter("video.published_frames", 1);
                    crate::metrics::counter("video.jpeg_bytes", data.len() as u64);
                    crate::metrics::sync("video.publish", || {
                        tx.send_replace(Some(Arc::new(Frame {
                            jpeg: Arc::new(data),
                            received: Instant::now(),
                            sequence,
                            source_id,
                            rtp_timestamp: Some(packet.timestamp().timestamp()),
                            clock_rate_hz,
                            width: dimensions.0,
                            height: dimensions.1,
                        })))
                    });
                    if tx.is_closed() {
                        break;
                    }
                }
            }
            bail!("Video stream ended")
        })
        .await
    }
    pub async fn frame(&self, after: Option<Instant>) -> Result<Arc<Frame>> {
        crate::metrics::result("video.fresh_frame_wait", async {
            let mut rx = self.receiver.clone();
            let deadline = tokio::time::Instant::now() + Duration::from_secs(6);
            loop {
                if let Some(frame) = rx.borrow_and_update().clone()
                    && frame.received.elapsed() <= Duration::from_millis(400)
                    && after.is_none_or(|t| frame.received >= t)
                {
                    crate::metrics::event("video.frame_selected", serde_json::json!({"sequence":frame.sequence,"source_id":frame.source_id,"age_ms":frame.received.elapsed().as_secs_f64()*1000.,"after_barrier":after.is_some()}));
                    return Ok(frame);
                }
                crate::metrics::result("video.watch_wait", tokio::time::timeout_at(deadline, rx.changed()))
                    .await
                    .map_err(|_| anyhow::anyhow!("No fresh camera frame; check connectivity"))?
                    .map_err(|_| anyhow::anyhow!("Video receiver stopped"))?;
            }
        })
        .await
    }
    pub fn age_ms(&self) -> Option<u64> {
        self.receiver
            .borrow()
            .as_ref()
            .map(|f| f.received.elapsed().as_millis() as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn simd_jpeg_preserves_geometry_and_color_channels() {
        let mut jpeg = turbojpeg::Compressor::new().unwrap();
        for (width, height, max_width, expected) in [
            (640, 360, 640, (640, 360)),
            (1280, 720, 640, (640, 360)),
            (2304, 1296, 1280, (1280, 720)),
        ] {
            let image = image::RgbImage::from_fn(width, height, |x, _| {
                if x < width / 2 {
                    image::Rgb([220, 20, 20])
                } else {
                    image::Rgb([20, 20, 220])
                }
            });
            let (bytes, w, h) =
                encode_picture(&mut jpeg, DynamicImage::ImageRgb8(image), max_width).unwrap();
            assert_eq!((w, h), expected);
            let decoded = image::load_from_memory(&bytes).unwrap().into_rgb8();
            assert_eq!(decoded.dimensions(), expected);
            let red = decoded.get_pixel(w / 4, h / 2);
            let blue = decoded.get_pixel(w * 3 / 4, h / 2);
            assert!(red[0] > 180 && red[2] < 60 && blue[2] > 180 && blue[0] < 60);
        }
    }
    #[tokio::test]
    async fn latest_only_and_after_barrier() {
        let (tx, rx) = watch::channel(None);
        let make = |sequence| {
            Arc::new(Frame {
                jpeg: Arc::new(vec![sequence as u8]),
                received: Instant::now(),
                sequence,
                source_id: 1,
                rtp_timestamp: None,
                clock_rate_hz: 90000,
                width: 640,
                height: 360,
            })
        };
        tx.send_replace(Some(make(1)));
        tx.send_replace(Some(make(2)));
        let video = Video {
            receiver: rx,
            task: tokio::spawn(std::future::pending()),
            metrics: Arc::default(),
        };
        assert_eq!(video.frame(None).await.unwrap().sequence, 2);
        let barrier = Instant::now();
        let task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            tx.send_replace(Some(make(3)));
            tokio::time::sleep(Duration::from_millis(100)).await;
        });
        assert_eq!(video.frame(Some(barrier)).await.unwrap().sequence, 3);
        task.abort();
    }
}

#[cfg(test)]
mod freshness_tests {
    use super::*;
    #[tokio::test]
    async fn stale_frames_are_not_returned_as_current() {
        let (tx, rx) = watch::channel(Some(Arc::new(Frame {
            jpeg: Arc::new(vec![]),
            received: Instant::now() - Duration::from_secs(2),
            sequence: 1,
            source_id: 1,
            rtp_timestamp: None,
            clock_rate_hz: 90000,
            width: 640,
            height: 360,
        })));
        let video = Video {
            receiver: rx,
            task: tokio::spawn(std::future::pending()),
            metrics: Arc::default(),
        };
        assert!(
            tokio::time::timeout(Duration::from_millis(25), video.frame(None))
                .await
                .is_err()
        );
        drop(tx);
    }
    #[tokio::test]
    async fn latest_buffer_releases_superseded_frames() {
        let (tx, rx) = watch::channel(None);
        let first = Arc::new(Frame {
            jpeg: Arc::new(vec![1; 100]),
            received: Instant::now(),
            sequence: 1,
            source_id: 1,
            rtp_timestamp: None,
            clock_rate_hz: 90000,
            width: 640,
            height: 360,
        });
        let weak = Arc::downgrade(&first);
        tx.send_replace(Some(first));
        for sequence in 2..100 {
            tx.send_replace(Some(Arc::new(Frame {
                jpeg: Arc::new(vec![1; 100]),
                received: Instant::now(),
                sequence,
                source_id: 1,
                rtp_timestamp: None,
                clock_rate_hz: 90000,
                width: 640,
                height: 360,
            })));
        }
        assert!(weak.upgrade().is_none());
        assert_eq!(rx.borrow().as_ref().unwrap().sequence, 99);
    }
}
