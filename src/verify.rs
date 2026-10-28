//! End-to-end verification using a separate Rust MCP client and server process.
use crate::onvif::{Config, Onvif};
use anyhow::{Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use rmcp::{
    RoleClient, ServiceExt,
    model::{CallToolRequestParams, ContentBlock},
    service::RunningService,
    transport::TokioChildProcess,
};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

type Client = RunningService<RoleClient, ()>;
async fn call(
    client: &Client,
    name: &'static str,
    args: Value,
) -> Result<(Vec<Value>, Vec<Vec<u8>>)> {
    let result = client
        .call_tool(
            CallToolRequestParams::new(name).with_arguments(args.as_object().unwrap().clone()),
        )
        .await?;
    ensure!(
        result.is_error != Some(true),
        "Tool {name} failed: {:?}",
        result.content
    );
    let mut metadata = vec![];
    let mut images = vec![];
    for block in result.content {
        match block {
            ContentBlock::Text(text) => metadata.push(serde_json::from_str(&text.text)?),
            ContentBlock::Image(image) => {
                ensure!(image.mime_type == "image/jpeg", "Unexpected image format");
                images.push(STANDARD.decode(image.data)?);
            }
            _ => {}
        }
    }
    Ok((metadata, images))
}
async fn goto(onvif: &Onvif, target: [f64; 2]) -> Result<()> {
    let result = async {
        onvif.absolute(target).await?;
        let deadline = Instant::now() + Duration::from_secs(6);
        let mut stable = 0;
        while stable < 2 {
            ensure!(
                Instant::now() < deadline,
                "Verification could not restore/reach camera position"
            );
            tokio::time::sleep(Duration::from_millis(75)).await;
            let pose = onvif.position().await?;
            stable = if (pose[0] - target[0]).abs().max((pose[1] - target[1]).abs()) < 0.002 {
                stable + 1
            } else {
                0
            };
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    let stopped = onvif.stop().await;
    result?;
    stopped?;
    Ok(())
}
pub async fn run(moving: bool) -> Result<()> {
    let overall = Instant::now();
    let client = ()
        .serve(TokioChildProcess::new(tokio::process::Command::new(
            std::env::current_exe()?,
        ))?)
        .await?;
    let tools = client.list_tools(None).await?;
    ensure!(tools.tools.len() == 8, "Expected all eight camera tools");
    let (meta, images) = call(&client, "camera_see", json!({})).await?;
    ensure!(
        meta[0]["width"].as_u64() == Some(640) && meta[0]["height"].as_u64() == Some(360),
        "Expected native JPEG preview"
    );
    ensure!(
        image::load_from_memory(&images[0])?.width() == 640,
        "Invalid JPEG"
    );
    std::fs::write("/tmp/rust-camera-preview.jpg", &images[0])?;
    eprintln!(
        "First MCP image including process startup: {:0.1}ms",
        overall.elapsed().as_secs_f64() * 1000.
    );
    let mut timings = vec![];
    for _ in 0..20 {
        let start = Instant::now();
        let (metadata, _) = call(&client, "camera_see", json!({})).await?;
        timings.push(start.elapsed().as_secs_f64() * 1000.);
        ensure!(
            metadata[0]["frame_age_ms"].as_f64().unwrap_or(1000.) < 400.,
            "Stale camera frame"
        );
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    timings.sort_by(f64::total_cmp);
    eprintln!(
        "Warm MCP camera_see: median {:0.2}ms; p95 {:0.2}ms",
        timings[10], timings[18]
    );
    let (metadata, images) = call(&client, "camera_see", json!({"detail":true})).await?;
    ensure!(
        metadata[0]["width"].as_u64().unwrap_or(0) > 640,
        "Full detail unavailable"
    );
    std::fs::write("/tmp/rust-camera-detail.jpg", &images[0])?;
    let old = metadata[0]["frame_id"].clone();
    call(&client, "camera_see", json!({})).await?;
    let (stale, _) = call(
        &client,
        "camera_focus",
        json!({"x":0.5,"y":0.5,"frame_id":old}),
    )
    .await?;
    ensure!(
        stale[0]["focus_status"] == "stale_frame",
        "Old observations must be rejected"
    );
    if moving {
        let onvif = Onvif::connect(Config::load()?).await?;
        let origin = onvif.position().await?;
        let test = async {
            goto(&onvif, [0.045, -0.5]).await?;
            tokio::time::sleep(Duration::from_millis(200)).await;
            let (seen, images) = call(&client, "camera_see", json!({})).await?;
            std::fs::write("/tmp/rust-focus-before.jpg", &images[0])?;
            let start = Instant::now();
            let (focused, images) = call(
                &client,
                "camera_focus",
                json!({"x":0.65,"y":0.65,"frame_id":seen[0]["frame_id"],"precise":true}),
            )
            .await?;
            std::fs::write("/tmp/rust-focus-after.jpg", &images[0])?;
            eprintln!(
                "Focus {:0.1}ms: {}",
                start.elapsed().as_secs_f64() * 1000.,
                focused[0]
            );
            ensure!(
                focused[0]["focus_status"] == "centered",
                "Focus did not center the selected point"
            );
            let start = Instant::now();
            let (rough, images) = call(
                &client,
                "camera_focus",
                json!({"x":0.6,"y":0.6,"frame_id":focused[0]["frame_id"],"rough":true}),
            )
            .await?;
            ensure!(
                rough[0]["focus_status"] == "aimed" && rough[0]["iterations"].as_u64() == Some(1),
                "Rough framing must make exactly one unverified move"
            );
            ensure!(
                rough[0]["target"].is_null(),
                "Rough framing must not claim a visually verified target"
            );
            std::fs::write("/tmp/rust-rough-after.jpg", &images[0])?;
            eprintln!(
                "Rough focus {:.1}ms: {}",
                start.elapsed().as_secs_f64() * 1000.,
                rough[0]
            );
            let start = Instant::now();
            let (views, images) = call(
                &client,
                "camera_scan",
                json!({"direction":"left","views":3,"step":0.15}),
            )
            .await?;
            ensure!(
                views.len() == 3 && images.len() == 3,
                "Scan must return all views"
            );
            for (i, image) in images.iter().enumerate() {
                std::fs::write(format!("/tmp/rust-scan-{i}.jpg"), image)?;
            }
            eprintln!(
                "Three-view MCP scan: {:0.1}ms",
                start.elapsed().as_secs_f64() * 1000.
            );
            let start = Instant::now();
            let (revisited, _) = call(
                &client,
                "camera_focus",
                json!({"x":0.5,"y":0.5,"frame_id":views[0]["frame_id"]}),
            )
            .await?;
            ensure!(
                revisited[0]["focus_status"] == "centered",
                "Must be able to focus the first scan view after visiting others"
            );
            eprintln!(
                "Revisit first scan view: {:0.1}ms",
                start.elapsed().as_secs_f64() * 1000.
            );
            let moving = client.call_tool(
                CallToolRequestParams::new("camera_move").with_arguments(
                    json!({"pan":0.1,"seconds":1.0})
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
            );
            let stopping = async {
                tokio::time::sleep(Duration::from_millis(100)).await;
                call(&client, "camera_stop", json!({})).await
            };
            let (movement, stopped) = tokio::join!(moving, stopping);
            stopped?;
            ensure!(
                movement?.is_error == Some(true),
                "Stop should cancel a move without waiting for its duration"
            );
            eprintln!("Concurrent emergency stop cancelled the active move");
            Ok::<_, anyhow::Error>(())
        }
        .await;
        // Restore even if any verification assertion fails.
        goto(&onvif, origin).await?;
        test?;
    }
    call(&client, "camera_stop", json!({})).await?;
    client.cancel().await?;
    eprintln!("Live MCP verification passed");
    Ok(())
}
