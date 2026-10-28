use openh264::formats::YUVSource;
use crate::onvif::Config;
use anyhow::{Result, bail, ensure};
use futures_util::StreamExt;
use image::{DynamicImage, ImageBuffer, codecs::jpeg::JpegEncoder};
use retina::{client::{Credentials, PlayOptions, Session, SessionOptions, SetupOptions}, codec::{CodecItem, FrameFormat}};
use std::sync::{Arc, atomic::{AtomicBool, AtomicU64, Ordering}};
use std::time::{Duration, Instant};
use tokio::{sync::watch, task::JoinHandle};
use url::Url;

pub struct Frame {
    pub jpeg: Arc<Vec<u8>>,
    pub received: Instant,
    pub sequence: u64,
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
impl Drop for Video { fn drop(&mut self) { self.task.abort(); } }
impl Video {
    pub fn start(url: Url, config: Config, jpeg: bool, dimensions: (u32,u32)) -> Self {
        let (tx, receiver) = watch::channel(None);
        let metrics = Arc::new(Metrics::default());
        let counters = metrics.clone();
        let task = tokio::spawn(async move {
            let mut delay = Duration::from_millis(250);
            loop {
                let _ = Self::receive(url.clone(), config.clone(), jpeg, dimensions, &tx, &counters).await;
                counters.connected.store(false, Ordering::Relaxed);
                tx.send_replace(None); // A disconnected stream must never serve an old picture.
                counters.reconnects.fetch_add(1, Ordering::Relaxed);
                tokio::time::sleep(delay).await;
                delay = (delay*2).min(Duration::from_secs(3));
                if tx.is_closed() { break; }
            }
        });
        Self { receiver, task, metrics }
    }
    async fn receive(url: Url, config: Config, jpeg: bool, dimensions: (u32,u32), tx: &watch::Sender<Option<Arc<Frame>>>, metrics: &Metrics) -> Result<()> {
        let creds = Credentials { username: config.username, password: config.password };
        let mut session = tokio::time::timeout(Duration::from_secs(4), Session::describe(url, SessionOptions::default().creds(Some(creds)))).await??;
        let index = session.streams().iter().position(|s| matches!(s.media(), "video" | "image") && s.encoding_name() == if jpeg { "jpeg" } else { "h264" })
            .ok_or_else(|| anyhow::anyhow!("Requested video encoding unavailable"))?;
        tokio::time::timeout(Duration::from_secs(3), session.setup(index, SetupOptions::default().frame_format(FrameFormat::SIMPLE))).await??;
        let session = tokio::time::timeout(Duration::from_secs(3), session.play(PlayOptions::default())).await??;
        let mut frames = session.demuxed()?;
        let mut decoder = if jpeg { None } else { Some(openh264::decoder::Decoder::new()?) };
        let mut encoded_at = Instant::now() - Duration::from_secs(1);
        metrics.connected.store(true, Ordering::Relaxed);
        while let Some(item) = tokio::time::timeout(Duration::from_secs(3), frames.next()).await? {
            if let CodecItem::VideoFrame(frame) = item? {
                let received = Instant::now();
                let (data, width, height) = if let Some(decoder) = decoder.as_mut() {
                    if let Some(yuv) = decoder.decode(frame.data())? {
                        if encoded_at.elapsed() < Duration::from_millis(150) { continue; }
                        let (w,h) = yuv.dimensions();
                        let mut rgb = vec![0;w*h*3]; yuv.write_rgb8(&mut rgb);
                        let image = DynamicImage::ImageRgb8(ImageBuffer::from_raw(w as u32,h as u32,rgb).ok_or_else(|| anyhow::anyhow!("Invalid video size"))?).thumbnail(1280,960);
                        let (w,h) = (image.width(),image.height());
                        let mut data = Vec::new(); JpegEncoder::new_with_quality(&mut data,85).encode_image(&image)?;
                        encoded_at = Instant::now();
                        (data,w,h)
                    } else { continue; }
                } else {
                    let data = frame.into_data();
                    ensure!(data.len() <= 4*1024*1024 && data.starts_with(&[0xff,0xd8]) && data.ends_with(&[0xff,0xd9]), "Invalid JPEG frame");
                    (data,dimensions.0,dimensions.1)
                };
                let sequence = metrics.frames.fetch_add(1,Ordering::Relaxed)+1;
                tx.send_replace(Some(Arc::new(Frame { jpeg: Arc::new(data),received,sequence,width,height })));
                if tx.is_closed() { break; }
            }
        }
        bail!("Video stream ended")
    }
    pub async fn frame(&self, after: Option<Instant>) -> Result<Arc<Frame>> {
        let mut rx = self.receiver.clone();
        let deadline = tokio::time::Instant::now()+Duration::from_secs(6);
        loop {
            if let Some(frame) = rx.borrow_and_update().clone() {
                if frame.received.elapsed() <= Duration::from_millis(400) && after.is_none_or(|t| frame.received >= t) {
                    return Ok(frame);
                }
            }
            tokio::time::timeout_at(deadline,rx.changed()).await.map_err(|_| anyhow::anyhow!("No fresh camera frame; check connectivity"))?
                .map_err(|_| anyhow::anyhow!("Video receiver stopped"))?;
        }
    }
    pub fn age_ms(&self) -> Option<u64> { self.receiver.borrow().as_ref().map(|f| f.received.elapsed().as_millis() as u64) }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test] async fn latest_only_and_after_barrier() {
        let (tx,rx)=watch::channel(None);
        let make=|sequence| Arc::new(Frame { jpeg: Arc::new(vec![sequence as u8]),received:Instant::now(),sequence,width:640,height:360 });
        tx.send_replace(Some(make(1)));tx.send_replace(Some(make(2)));
        let video=Video { receiver:rx,task:tokio::spawn(std::future::pending()),metrics:Arc::default() };
        assert_eq!(video.frame(None).await.unwrap().sequence,2);
        let barrier=Instant::now();
        let task=tokio::spawn(async move { tokio::time::sleep(Duration::from_millis(20)).await;tx.send_replace(Some(make(3)));tokio::time::sleep(Duration::from_millis(100)).await; });
        assert_eq!(video.frame(Some(barrier)).await.unwrap().sequence,3);
        task.abort();
    }
}
