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
    let h264 = std::env::var("CAMERA_PREVIEW").is_ok_and(|s| s == "h264");
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
        .min_by_key(|p| p.width)
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
