mod onvif;
mod video;
use anyhow::Result;
use std::{sync::Arc,time::Instant};
#[tokio::main]
async fn main() -> Result<()> {
    let root=std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let _=dotenvy::from_path(root.join(".env"));
    let start=Instant::now();
    let camera=Arc::new(onvif::Onvif::connect(onvif::Config::load()?).await?);
    let profile=camera.profiles.iter().find(|p| p.encoding=="JPEG").ok_or_else(||anyhow::anyhow!("No JPEG profile"))?;
    let video=video::Video::start(camera.stream_url(&profile.token).await?,camera.config.clone(),true,(profile.width,profile.height));
    let frame=video.frame(None).await?;
    eprintln!("Native JPEG ready in {:0.3}s: {}x{}, {} bytes",start.elapsed().as_secs_f64(),frame.width,frame.height,frame.jpeg.len());
    std::fs::write("/tmp/rust-camera-preview.jpg",frame.jpeg.as_slice())?;
    for _ in 0..5 {
        let start=Instant::now();let frame=video.frame(None).await?;
        eprintln!("Cached fresh frame: {:0.3}ms, age {:0.1}ms",start.elapsed().as_secs_f64()*1000.,frame.received.elapsed().as_secs_f64()*1000.);
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    Ok(())
}
