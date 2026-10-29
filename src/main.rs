mod audio;
mod audio_diagnostics;
mod camera;
mod codex_audio;
mod experiments;
mod hearing;
mod jpeg;
mod metrics;
mod onvif;
mod server;
mod transport_metrics;
mod verify;
mod video;
mod views;
mod vision;
mod wake;
use anyhow::Result;
use rmcp::ServiceExt;
use std::{path::PathBuf, time::Instant};

fn root() -> PathBuf {
    std::env::var_os("CAMERA_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")))
}
#[tokio::main(worker_threads = 2)]
async fn main() {
    let result = Box::pin(run()).await;
    metrics::flush();
    if let Err(error) = result {
        eprintln!("Camera server: {error}");
        std::process::exit(1);
    }
}
async fn run() -> Result<()> {
    let root = root();
    let _ = dotenvy::from_path(root.join(".env"));
    metrics::init();
    if std::env::args().any(|a| a == "--measure-framing") {
        return experiments::framing().await;
    }
    if std::env::args().any(|a| a == "--measure-motor") {
        return experiments::motor().await;
    }
    if std::env::args().any(|a| a == "--verify-live" || a == "--verify-motion") {
        return verify::run(std::env::args().any(|a| a == "--verify-motion")).await;
    }
    if std::env::args().any(|a| a == "--calibrate" || a == "--probe") {
        let started = Instant::now();
        let camera = camera::Camera::connect(&root).await?;
        if std::env::args().any(|a| a == "--calibrate") {
            eprintln!("{}", camera.recalibrate(&root).await?);
            return Ok(());
        }
        let (metadata, frame) = camera.observe(false, serde_json::json!({}), false).await?;
        eprintln!(
            "First picture: {:.3}s; {}",
            started.elapsed().as_secs_f64(),
            metadata
        );
        std::fs::write("/tmp/rust-camera-preview.jpg", frame.jpeg.as_slice())?;
        for _ in 0..10 {
            let start = Instant::now();
            let (metadata, _) = camera.observe(false, serde_json::json!({}), false).await?;
            eprintln!(
                "Observe: {:.3}ms, frame age {}ms",
                start.elapsed().as_secs_f64() * 1000.,
                metadata["frame_age_ms"]
            );
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        return Ok(());
    }
    let server = server::CameraServer::new(root);
    let camera_slot = server.camera.clone();
    let hearing = server.hearing.clone();
    let service = server
        .serve(transport_metrics::instrument(rmcp::transport::stdio_pipes()))
        .await?;
    let result = tokio::select! {
        result=service.waiting()=>result.map(|_|()),
        _=tokio::signal::ctrl_c()=>Ok(()),
    };
    let _ = hearing.stop().await;
    if let Some(camera) = camera_slot.get() {
        let _ = camera.stop().await;
    }
    result?;
    Ok(())
}
