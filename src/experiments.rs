//! Opt-in live hardware experiments; never run by the MCP server.
use crate::{
    onvif::{Config, Onvif},
    video::Video,
    vision,
};
use anyhow::{Result, ensure};
use serde_json::json;
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

pub async fn motor() -> Result<()> {
    let root = PathBuf::from(
        std::env::var("CAMERA_CONFIG_DIR").unwrap_or_else(|_| env!("CARGO_MANIFEST_DIR").into()),
    );
    let config = Config::load()?;
    let onvif = Onvif::connect(config.clone()).await?;
    let origin = onvif.position().await?;
    let preview_mode = std::env::var("CAMERA_PREVIEW").unwrap_or_default();
    let h264 = preview_mode.starts_with("h264");
    let p = onvif
        .profiles
        .iter()
        .filter(|p| {
            if h264 {
                p.encoding == "H264" && p.width >= 640
            } else {
                p.encoding == "JPEG" && p.width == 640
            }
        })
        .min_by_key(|p| {
            if preview_mode == "h264-main" {
                u32::MAX - p.width
            } else {
                p.width
            }
        })
        .ok_or_else(|| anyhow::anyhow!("JPEG profile unavailable"))?;
    let url = onvif.stream_url(&p.token).await?;
    let video = if h264 {
        Video::h264_preview(url, config, (p.width, p.height))
    } else {
        Video::start(url, config, true, (p.width, p.height))
    };
    video.frame(None).await?;
    // This firmware may use opposite signs for velocity and absolute pose axes.
    let probe_start = onvif.position().await?;
    onvif.velocity(0.3, 0., 1.).await?;
    tokio::time::sleep(Duration::from_millis(120)).await;
    onvif.stop().await?;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let probe_end = onvif.position().await?;
    let velocity_sign = (probe_end[0] - probe_start[0]).signum();
    ensure!(
        (probe_end[0] - probe_start[0]).abs() > 0.002,
        "Velocity sign probe did not move"
    );
    onvif.absolute(origin).await?;
    let restore_deadline = Instant::now() + Duration::from_secs(6);
    loop {
        let p = onvif.position().await?;
        if (p[0] - origin[0]).abs().max((p[1] - origin[1]).abs()) < 0.002 {
            break;
        }
        ensure!(
            Instant::now() < restore_deadline,
            "Could not restore after velocity probe"
        );
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    onvif.stop().await?;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut rows = vec![];
    let mut active_case = json!(null);
    let output = root.join(
        std::env::var("CAMERA_BENCH_OUTPUT")
            .unwrap_or_else(|_| "benchmarks/i-spy/motor.json".into()),
    );
    let selected = std::env::var("CAMERA_BENCH_MODES").ok();
    let repeats = std::env::var("CAMERA_BENCH_REPEATS")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(3);
    let result=async {
        // Moderate movements keep enough image overlap for independent optical validation.
        let anchor=origin;
        ensure!(anchor[0] <= 0.79,"Aim away from the positive pan limit before the motor experiment");
        for repeat in 0..repeats {
            let mut modes:Vec<_>=selected.as_deref().map(|s|s.split(',').collect()).unwrap_or_else(||vec!["absolute_default","absolute_max","relative_max","continuous_max"]);
            if repeat%2==1 { modes.reverse(); }
            for mode in modes {
                for amount in [0.05,0.20] {
                    for direction in [1.,-1.] {
                        let start_pose=onvif.position().await?;
                        let target=if direction>0. { [anchor[0]+amount,anchor[1]] } else { anchor };
                        active_case=json!({"repeat":repeat,"mode":mode,"amount":amount,"direction":direction,"start":start_pose,"target":target});
                        let before=video.frame(None).await?;
                        let started=Instant::now();
                        match mode {
                            "absolute_default"=>onvif.absolute(target).await?,
                            "absolute_max"=>onvif.absolute_speed(target,1.).await?,
                            "relative_default"=>onvif.relative_default([target[0]-start_pose[0],target[1]-start_pose[1]]).await?,
                            m if m.starts_with("absolute_speed_")=>onvif.absolute_speed(target,m.trim_start_matches("absolute_speed_").parse()?).await?,
                            "continuous_max"=>onvif.velocity(velocity_sign*(target[0]-start_pose[0]).signum(),0.,2.).await?,
                            _=>onvif.relative([target[0]-start_pose[0],target[1]-start_pose[1]],1.).await?,
                        }
                        let acknowledged=Instant::now();
                        let mut polls=vec![];
                        loop {
                            if started.elapsed()>=Duration::from_secs(6) {
                                active_case["polls"]=json!(polls);
                                anyhow::bail!("Motor experiment arrival timed out");
                            }
                            let sent=Instant::now();let pose=onvif.position().await?;let received=Instant::now();
                            polls.push(json!({"sent_ms":sent.duration_since(started).as_secs_f64()*1000.,"received_ms":received.duration_since(started).as_secs_f64()*1000.,"pose":pose}));
                            let reached = if mode=="continuous_max" { (pose[0]-target[0])*(target[0]-start_pose[0]).signum() >= -0.002 } else { (pose[0]-target[0]).abs().max((pose[1]-target[1]).abs())<0.002 };
                            if reached { if mode=="continuous_max" { onvif.stop().await?; } break; }
                            tokio::time::sleep(Duration::from_millis(15)).await;
                        }
                        let reported=Instant::now();
                        let mut frames=vec![];
                        for offset in [0,50,100,300,700] {
                            tokio::time::sleep_until(tokio::time::Instant::from_std(reported+Duration::from_millis(offset))).await;
                            let barrier=Instant::now();let frame=video.frame(Some(barrier)).await?;
                            frames.push((offset,reported.elapsed().as_secs_f64()*1000.,frame));
                        }
                        onvif.stop().await?;
                        let reference=frames.last().unwrap().2.clone();
                        let mut optics=vec![];
                        for (offset,actual,frame) in &frames {
                            let a=frame.clone();let b=reference.clone();
                            let shifts=tokio::task::spawn_blocking(move || {
                                [[0.3,0.3],[0.5,0.3],[0.7,0.3],[0.3,0.5],[0.5,0.5],[0.7,0.5],[0.3,0.7],[0.5,0.7],[0.7,0.7]].into_iter().filter_map(|p|vision::track(&a.jpeg,&b.jpeg,p).map(|q|(q[0]-p[0]).hypot(q[1]-p[1]))).collect::<Vec<_>>()
                            }).await?;
                            let path=format!("/tmp/spy-motor-{repeat}-{mode}-{amount}-{direction}-{offset}.jpg");
                            std::fs::write(&path,frame.jpeg.as_slice())?;
                            optics.push(json!({"offset_ms":offset,"actual_ms":actual,"matches":shifts.len(),"max_residual":shifts.iter().copied().reduce(f64::max),"image":path,"sequence":frame.sequence}));
                        }
                        let row=json!({"repeat":repeat,"mode":mode,"amount":amount,"direction":direction,"start":start_pose,"target":target,"command_ms":acknowledged.duration_since(started).as_secs_f64()*1000.,"first_reported_arrival_ms":reported.duration_since(started).as_secs_f64()*1000.,"polls":polls,"optics":optics,"before_sequence":before.sequence,"final_pose":onvif.position().await?});
                        eprintln!("{}",json!({"mode":mode,"amount":amount,"direction":direction,"arrival_ms":row["first_reported_arrival_ms"],"early_optics":row["optics"][0]}));
                        rows.push(row);
                        std::fs::write(&output,serde_json::to_vec_pretty(&json!({"description":"15ms polling; first in-range pose; absolute/relative Stop after 700ms optical reference; continuous Stop at crossing; camera restored on normal/error exit","samples":rows}))?)?;
                    }
                }
            }
        }
        Ok::<_,anyhow::Error>(())
    }.await;
    // Restore with bounded polling on both success and experiment failure.
    let restore = async {
        onvif.absolute(origin).await?;
        let deadline = Instant::now() + Duration::from_secs(6);
        while Instant::now() < deadline {
            let pose = onvif.position().await?;
            if (pose[0] - origin[0]).abs().max((pose[1] - origin[1]).abs()) < 0.002 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        onvif.stop().await
    }
    .await;
    std::fs::write(
        &output,
        serde_json::to_vec_pretty(
            &json!({"description":"15ms polling; first in-range pose; absolute/relative Stop after 700ms optical reference; continuous Stop at crossing","velocity_probe":{"start":probe_start,"end":probe_end,"absolute_axis_sign_per_positive_velocity":velocity_sign},"samples":rows,"metrics":crate::metrics::snapshot(None,false,0),"completed":result.is_ok(),"failure":result.as_ref().err().map(ToString::to_string),"active_case":active_case}),
        )?,
    )?;
    result?;
    restore?;
    Ok(())
}

fn sharpness(bytes: &[u8]) -> Result<f64> {
    let image = image::load_from_memory(bytes)?.to_luma8();
    let mut sum = 0.;
    let mut count = 0u64;
    for y in 1..image.height() - 1 {
        for x in 1..image.width() - 1 {
            let lap = image.get_pixel(x - 1, y)[0] as f64
                + image.get_pixel(x + 1, y)[0] as f64
                + image.get_pixel(x, y - 1)[0] as f64
                + image.get_pixel(x, y + 1)[0] as f64
                - 4. * image.get_pixel(x, y)[0] as f64;
            sum += lap * lap;
            count += 1;
        }
    }
    Ok(sum / count as f64)
}

/// Real controller calls with matched endpoints and delayed optical references.
pub async fn framing() -> Result<()> {
    let root = PathBuf::from(
        std::env::var("CAMERA_CONFIG_DIR").unwrap_or_else(|_| env!("CARGO_MANIFEST_DIR").into()),
    );
    let camera = crate::camera::Camera::connect(&root).await?;
    let origin = camera.onvif.position().await?;
    let warm = camera.preview.frame(None).await?;
    let repeats = std::env::var("CAMERA_BENCH_REPEATS")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(3);
    let label = std::env::var("CAMERA_BENCH_LABEL").unwrap_or_else(|_| "framing".into());
    ensure!(
        label
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "Invalid benchmark label"
    );
    let output = root.join(format!("benchmarks/i-spy/{label}.json"));
    let mut rows = vec![];
    let result=async {
        for repeat in 0..repeats {
            for axis in ["pan","tilt","diagonal"] {
                let mut other=origin;
                if axis!="tilt" {other[0]+=if origin[0]>0.7 {-0.1}else{0.1};}
                if axis!="pan" {other[1]+=if origin[1]>0.7 {-0.2}else{0.2};}
                for (direction,target) in [(1,other),(-1,origin)] {
                    camera.begin_motion().await;
                    let began=Instant::now();
                    let (_,early)=crate::metrics::background("bench.framing",warm.source_id,async {
                        camera.goto(target).await?;
                        camera.observe(false,json!({}),false).await
                    }).await?;
                    let wall_ms=began.elapsed().as_secs_f64()*1000.;
                    let stats=crate::metrics::snapshot(None,false,8);
                    let id=stats["recent_background"].as_array().unwrap().iter().find(|r|r["tool"]=="bench.framing").unwrap()["request_id"].as_str().unwrap();
                    let trace=crate::metrics::snapshot(Some(id),true,1);
                    let early_path=format!("/tmp/spy-{label}-{repeat}-{axis}-{direction}-early.jpg");
                    std::fs::write(&early_path,early.jpeg.as_slice())?;
                    tokio::time::sleep(Duration::from_millis(400)).await;
                    let late=camera.preview.frame(Some(Instant::now())).await?;
                    let late_path=format!("/tmp/spy-{label}-{repeat}-{axis}-{direction}-late.jpg");
                    std::fs::write(&late_path,late.jpeg.as_slice())?;
                    let shifts=tokio::task::spawn_blocking(move || -> Result<_> {
                        let shifts=[[0.3,0.3],[0.5,0.3],[0.7,0.3],[0.3,0.5],[0.5,0.5],[0.7,0.5],[0.3,0.7],[0.5,0.7],[0.7,0.7]].into_iter().filter_map(|p|vision::track(&early.jpeg,&late.jpeg,p).map(|q|(q[0]-p[0]).hypot(q[1]-p[1]))).collect::<Vec<_>>();
                        let ratio=sharpness(&early.jpeg)?/sharpness(&late.jpeg)?.max(1e-9);
                        Ok((shifts,ratio))
                    }).await??;
                    let max_residual=shifts.0.iter().copied().reduce(f64::max);
                    let row=json!({"repeat":repeat,"axis":axis,"direction":direction,"target":target,"wall_ms":wall_ms,"optical_matches":shifts.0.len(),"max_residual":max_residual,"sharpness_ratio":shifts.1,"geometry_pass":shifts.0.len()>=3 && max_residual.is_some_and(|d|d<=0.01),"early_image":early_path,"late_image":late_path,"trace":trace});
                    eprintln!("{}",json!({"axis":axis,"direction":direction,"wall_ms":wall_ms,"geometry_pass":row["geometry_pass"],"max_residual":max_residual,"sharpness_ratio":shifts.1}));
                    rows.push(row);
                    std::fs::write(&output,serde_json::to_vec(&json!({"label":label,"origin":origin,"samples":rows}))?)?;
                }
            }
        }
        Ok::<_,anyhow::Error>(())
    }.await;
    camera.begin_motion().await;
    let restored = camera.goto(origin).await;
    let stopped = camera.stop().await;
    let parameters = [
        "CAMERA_PREVIEW",
        "CAMERA_SETTLE_MS",
        "CAMERA_ROUGH_SETTLE_MS",
        "CAMERA_POLL_MS",
        "CAMERA_STABLE_SAMPLES",
    ]
    .into_iter()
    .map(|s| (s, std::env::var(s).ok()))
    .collect::<std::collections::BTreeMap<_, _>>();
    std::fs::write(
        &output,
        serde_json::to_vec(
            &json!({"label":label,"origin":origin,"parameters":parameters,"description":"Native production-controller goto+observe call; timing excludes delayed reference/tracking; separate MCP game checks validate complete tool behavior","samples":rows,"metrics":crate::metrics::snapshot(None,false,0),"completed":result.is_ok(),"failure":result.as_ref().err().map(ToString::to_string),"restored":restored.is_ok(),"stopped":stopped.is_ok()}),
        )?,
    )?;
    result?;
    restored?;
    stopped?;
    Ok(())
}
