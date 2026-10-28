use crate::onvif::Config;
use anyhow::{Result, bail};
use futures_util::StreamExt;
use image::{DynamicImage, ImageBuffer, codecs::jpeg::JpegEncoder};
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
fn decode_picture(
    mut decoder: openh264::decoder::Decoder,
    packet: Vec<u8>,
    encode: bool,
) -> Result<(openh264::decoder::Decoder, Option<EncodedPicture>)> {
    crate::metrics::sync_result("video.detail_decode_encode", || {
        let picture = {
            let decoded =
                crate::metrics::sync_result("video.h264_decode", || decoder.decode(&packet))?;
            if let Some(yuv) = decoded.filter(|_| encode) {
                let (w, h) = yuv.dimensions();
                let mut rgb = vec![0; w * h * 3];
                crate::metrics::sync("video.yuv_to_rgb", || yuv.write_rgb8(&mut rgb));
                let image = DynamicImage::ImageRgb8(
                    ImageBuffer::from_raw(w as u32, h as u32, rgb)
                        .ok_or_else(|| anyhow::anyhow!("Invalid video size"))?,
                )
                .thumbnail(1280, 960);
                let (w, h) = (image.width(), image.height());
                let mut data = vec![];
                crate::metrics::sync_result("video.detail_jpeg_encode", || {
                    JpegEncoder::new_with_quality(&mut data, 85).encode_image(&image)
                })?;
                Some((data, w, h))
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
        let (tx, receiver) = watch::channel(None);
        let metrics = Arc::new(Metrics::default());
        let counters = metrics.clone();
        let source_id = rand::random();
        let task = tokio::spawn(async move {
            let mut delay = Duration::from_millis(250);
            loop {
                let _ = Self::receive(
                    url.clone(),
                    config.clone(),
                    jpeg,
                    dimensions,
                    &tx,
                    &counters,
                    source_id,
                )
                .await;
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
        jpeg: bool,
        dimensions: (u32, u32),
        tx: &watch::Sender<Option<Arc<Frame>>>,
        metrics: &Metrics,
        source_id: u64,
    ) -> Result<()> {
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
            let mut decoder = openh264::decoder::Decoder::new()?;
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
                    let encode = encoded_at.elapsed() >= Duration::from_millis(150);
                    let rtp_timestamp = Some(frame.timestamp().timestamp());
                    let packet = frame.into_data();
                    // CPU-heavy detail work must not block the RTSP/PTZ/MCP I/O runtime.
                    let (next, picture) =
                        crate::metrics::blocking(move || decode_picture(decoder, packet, encode))
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
            metrics.connected.store(true, Ordering::Relaxed);
            while let Some(packet) = crate::metrics::result(
                "video.jpeg_packet_wait",
                tokio::time::timeout(Duration::from_secs(3), session.next()),
            )
            .await?
            {
                crate::metrics::counter("video.received_packets", 1);
                if let PacketItem::Rtp(packet) = packet?
                    && let Some(data) = assembler.push(
                        packet.timestamp().timestamp(),
                        packet.sequence_number(),
                        packet.mark(),
                        packet.payload(),
                    )?
                {
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
