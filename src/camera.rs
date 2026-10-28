use crate::{
    onvif::{Config, Onvif},
    video::{Frame, Video},
    vision,
};
use anyhow::{Result, ensure};
use nalgebra::{Matrix2, Vector2};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, OwnedMutexGuard};

#[derive(Clone)]
struct Observation {
    id: String,
    created: Instant,
    pose: [f64; 2],
    frame: Arc<Frame>,
    revisit: bool,
}
#[derive(Default)]
struct State {
    observations: VecDeque<Observation>,
    serial: u64,
    epoch: u64,
    latest: String,
    barrier: Option<Instant>,
    last_move: Value,
    last_motion_pose: Option<[f64; 2]>,
    last_direction: [i8; 2],
}
#[derive(Deserialize)]
struct Calibration {
    model: Option<String>,
    image_shift_per_pose_unit: [[f64; 2]; 2],
    #[serde(default)]
    backlash_pose_units: [f64; 2],
}
pub type ObservationResult = (Value, Arc<Frame>);

pub struct Camera {
    pub onvif: Arc<Onvif>,
    pub preview: Video,
    preview_encoding: &'static str,
    pose_poller: tokio::task::JoinHandle<()>,
    detail_token: String,
    detail_dimensions: (u32, u32),
    state: Arc<Mutex<State>>,
    command_gate: Arc<Mutex<()>>,
    pub motion: Mutex<()>,
    cancelled: AtomicBool,
    calibration: Matrix2<f64>,
    backlash: [f64; 2],
    views: Mutex<HashMap<String, [f64; 2]>>,
    views_path: std::path::PathBuf,
}
async fn stop_barrier<T>(
    gate: Arc<Mutex<()>>,
    stop: impl std::future::Future<Output = T>,
) -> (OwnedMutexGuard<()>, T) {
    tokio::join!(biased;
        crate::metrics::timed("lock.stop_cleanup_wait",gate.lock_owned()),
        stop
    )
}
fn durable_stop<T: Send + 'static>(
    lease: Option<OwnedMutexGuard<()>>,
    work: impl std::future::Future<Output = T> + Send + 'static,
) -> tokio::task::JoinHandle<(T, Option<OwnedMutexGuard<()>>)> {
    let context = crate::metrics::capture();
    tokio::spawn(async move {
        let result = crate::metrics::with_context_async(context, work).await;
        (result, lease)
    })
}
struct StopGuard {
    onvif: Arc<Onvif>,
    state: Arc<Mutex<State>>,
    lease: Option<OwnedMutexGuard<()>>,
    armed: bool,
}
impl StopGuard {
    async fn finish(&mut self) -> Result<()> {
        // The critical Stop RPC must survive cancellation of its caller. The
        // owned lease also survives, so no later move can overtake this Stop.
        self.armed = false;
        let onvif = self.onvif.clone();
        let lease = self.lease.take();
        crate::metrics::result("motion.stop_wait", async {
            let (result, lease) = durable_stop(lease, async move { onvif.stop().await }).await?;
            self.lease = lease;
            result
        })
        .await
    }
}
impl Drop for StopGuard {
    fn drop(&mut self) {
        if self.armed {
            let onvif = self.onvif.clone();
            let state = self.state.clone();
            let lease = self.lease.take();
            tokio::spawn(async move {
                let _lease = lease;
                let _ = crate::metrics::background("motion.cancel_cleanup", 0, async {
                    let result = onvif.stop().await;
                    state.lock().await.barrier = Some(Instant::now() + Duration::from_millis(100));
                    result
                })
                .await;
            });
        }
    }
}
fn backlash_target(
    pose: [f64; 2],
    target: [f64; 2],
    directions: [i8; 2],
    backlash: [f64; 2],
) -> ([f64; 2], [f64; 2]) {
    let mut adjusted = target;
    for axis in 0..2 {
        let delta = target[axis] - pose[axis];
        if delta.abs() > 0.002 {
            let sign = if delta > 0. { 1 } else { -1 };
            let fraction = if directions[axis] == 0 {
                0.5
            } else if directions[axis] != sign {
                1.
            } else {
                0.
            };
            adjusted[axis] += delta.signum() * backlash[axis] * fraction;
        }
    }
    let adjusted = clip(adjusted);
    (adjusted, [adjusted[0] - target[0], adjusted[1] - target[1]])
}
fn max_delta(a: [f64; 2], b: [f64; 2]) -> f64 {
    (a[0] - b[0]).abs().max((a[1] - b[1]).abs())
}
fn norm(point: [f64; 2]) -> f64 {
    (point[0] - 0.5).hypot(point[1] - 0.5)
}
fn can_compensate_pose_cache(pose_delta: f64, original: [f64; 2], tracked: [f64; 2]) -> bool {
    pose_delta <= 0.05 && (original[0] - tracked[0]).hypot(original[1] - tracked[1]) <= 0.02
}
fn focus_gain() -> f64 {
    std::env::var("CAMERA_FOCUS_GAIN")
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|n| n.is_finite() && (0.5..=1.1).contains(n))
        .unwrap_or(0.8)
}
fn calibrated_delta(matrix: Matrix2<f64>, point: [f64; 2]) -> Result<Vector2<f64>> {
    Ok(-focus_gain()
        * matrix
            .try_inverse()
            .ok_or_else(|| anyhow::anyhow!("Invalid calibration"))?
        * Vector2::new(point[0] - 0.5, point[1] - 0.5))
}
fn aim_target(matrix: Matrix2<f64>, pose: [f64; 2], point: [f64; 2]) -> Result<[f64; 2]> {
    let delta = calibrated_delta(matrix, point)?;
    Ok(clip([
        pose[0] + delta[0].clamp(-0.12, 0.12),
        pose[1] + delta[1].clamp(-0.30, 0.30),
    ]))
}
fn rough_target(matrix: Matrix2<f64>, pose: [f64; 2], point: [f64; 2]) -> Result<[f64; 2]> {
    let delta = calibrated_delta(matrix, point)?;
    // One-shot aiming is not constrained to the small steps used by visual correction.
    Ok(clip([pose[0] + delta[0], pose[1] + delta[1]]))
}
fn clip(point: [f64; 2]) -> [f64; 2] {
    [point[0].clamp(-1., 1.), point[1].clamp(-1., 1.)]
}

fn parameter(name: &str, fallback: u64, min: u64, max: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|n| (min..=max).contains(n))
        .unwrap_or(fallback)
}
fn delay(name: &str, fallback: u64) -> Duration {
    Duration::from_millis(parameter(name, fallback, 0, 1000))
}

impl Camera {
    pub async fn connect(root: &Path) -> Result<Arc<Self>> {
        crate::metrics::result("camera.initialize", async {
            let onvif = Arc::new(Onvif::connect(Config::load()?).await?);
            let preview_mode = std::env::var("CAMERA_PREVIEW").unwrap_or_default();
            let h264_preview = preview_mode.starts_with("h264");
            let profile = onvif
                .profiles
                .iter()
                .filter(|p| {
                    if h264_preview {
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
                .ok_or_else(|| anyhow::anyhow!("Requested preview profile unavailable"))?;
            let preview_url = onvif.stream_url(&profile.token).await?;
            let preview = if h264_preview {
                Video::h264_preview(
                    preview_url,
                    onvif.config.clone(),
                    (profile.width, profile.height),
                )
            } else {
                Video::start(
                    preview_url,
                    onvif.config.clone(),
                    true,
                    (profile.width, profile.height),
                )
            };
            let preview_encoding = if h264_preview {
                "H264 decode to 640px JPEG"
            } else {
                "native JPEG passthrough"
            };
            let detail = onvif
                .profiles
                .iter()
                .filter(|p| p.encoding == "H264" && p.width >= 1280)
                .min_by_key(|p| p.width)
                .ok_or_else(|| anyhow::anyhow!("H264 detail profile unavailable"))?;
            let (detail_token, detail_dimensions) =
                (detail.token.clone(), (detail.width, detail.height));
            let calibration: Calibration =
                crate::metrics::sync_result("config.calibration_load", || -> Result<_> {
                    Ok(serde_json::from_slice(&std::fs::read(
                        root.join("calibration.json"),
                    )?)?)
                })?;
            ensure!(
                calibration.model.as_ref().is_none_or(|m| m == &onvif.model),
                "Calibration belongs to a different camera model"
            );
            let backlash = calibration.backlash_pose_units;
            ensure!(
                backlash
                    .iter()
                    .all(|b| b.is_finite() && (0.0..=0.03).contains(b)),
                "Invalid backlash calibration"
            );
            let data = calibration.image_shift_per_pose_unit;
            let calibration = Matrix2::new(data[0][0], data[0][1], data[1][0], data[1][1]);
            ensure!(
                calibration.iter().all(|v| v.is_finite()) && calibration.determinant().abs() > 0.01,
                "Invalid camera calibration"
            );
            let views_path = root.join("views.json");
            let views = crate::metrics::sync_result("config.views_load", || -> Result<_> {
                Ok(if views_path.exists() {
                    serde_json::from_slice(&std::fs::read(&views_path)?)?
                } else {
                    HashMap::new()
                })
            })?;
            onvif.position().await?;
            let weak = Arc::downgrade(&onvif);
            let pose_poller = tokio::spawn(async move {
                loop {
                    crate::metrics::sleep(
                        "background.pose_poll_interval",
                        Duration::from_millis(150),
                    )
                    .await;
                    let Some(camera) = weak.upgrade() else {
                        break;
                    };
                    let _ = camera.position().await;
                }
            });
            Ok(Arc::new(Self {
                onvif,
                preview,
                preview_encoding,
                pose_poller,
                detail_token,
                detail_dimensions,
                state: Arc::new(Mutex::new(State {
                    epoch: rand::random(),
                    ..State::default()
                })),
                command_gate: Arc::new(Mutex::new(())),
                motion: Mutex::new(()),
                cancelled: AtomicBool::new(false),
                calibration,
                backlash,
                views: Mutex::new(views),
                views_path,
            }))
        })
        .await
    }
    pub async fn status(&self) -> Result<Value> {
        crate::metrics::result("camera.status", async {
        let pose = self.onvif.position().await?;
        Ok(
            json!({"manufacturer":self.onvif.manufacturer,"model":self.onvif.model,"firmware":self.onvif.firmware,
            "profiles":self.onvif.profiles,"ptz":{"Position":{"PanTilt":{"x":pose[0],"y":pose[1]}}},
            "video":{"transport":"persistent RTSP/TCP","encoding":self.preview_encoding,"buffered_frames":1,
                "connected":self.preview.metrics.connected.load(Ordering::Relaxed),"frames_received":self.preview.metrics.frames.load(Ordering::Relaxed),
                "reconnects":self.preview.metrics.reconnects.load(Ordering::Relaxed),"frame_age_ms":self.preview.age_ms()},
            "remembered_views":crate::metrics::timed("lock.views_wait",self.views.lock()).await.keys().cloned().collect::<Vec<_>>()}),
        )

}).await
    }
    async fn frame(&self, detail: bool, after: Option<Instant>) -> Result<Arc<Frame>> {
        crate::metrics::result("camera.capture", async {
            if detail {
                // Keep the preview connection hot, but don't let an occasional detail
                // request consume decoder/encoder or camera resources indefinitely.
                let source = Video::start(
                    self.onvif.stream_url(&self.detail_token).await?,
                    self.onvif.config.clone(),
                    false,
                    self.detail_dimensions,
                );
                source.frame(after).await
            } else {
                self.preview.frame(after).await
            }
        })
        .await
    }
    pub async fn observe(
        &self,
        detail: bool,
        metadata: Value,
        revisit: bool,
    ) -> Result<ObservationResult> {
        crate::metrics::result("camera.observe", async {
            self.await_cleanup().await;
            let barrier = crate::metrics::timed("lock.state_wait", self.state.lock())
                .await
                .barrier;
            let frame = self.frame(detail, barrier).await?;
            let (pose, pose_at) = match self.onvif.cached_position() {
                Some(cached) => cached,
                None => (self.onvif.position().await?, Instant::now()),
            };
            let mut metadata = metadata;
            metadata["pose_age_ms"] = json!(pose_at.elapsed().as_secs_f64() * 1000.);
            self.record(frame, pose, metadata, revisit).await
        })
        .await
    }
    async fn record(
        &self,
        frame: Arc<Frame>,
        pose: [f64; 2],
        mut metadata: Value,
        revisit: bool,
    ) -> Result<ObservationResult> {
        crate::metrics::result("camera.record_observation", async {
        let mut state = crate::metrics::timed("lock.state_wait",self.state.lock()).await;
        state.serial += 1;
        let id = format!(
            "{:016x}-{:x}-{:x}",
            state.epoch, frame.sequence, state.serial
        );
        state.latest = id.clone();
        state.observations.push_back(Observation {
            id: id.clone(),
            created: Instant::now(),
            pose,
            frame: frame.clone(),
            revisit,
        });
        while state.observations.len() > 12 {
            state.observations.pop_front();
        }
        metadata["frame_id"] = json!(id);
        metadata["width"] = json!(frame.width);
        metadata["height"] = json!(frame.height);
        metadata["coordinates"] = json!("x,y normalized 0..1 from top-left; center=(0.5,0.5)");
        metadata["frame_age_ms"] = json!(frame.received.elapsed().as_secs_f64() * 1000.);
        metadata["sequence"] = json!(frame.sequence);
        metadata["source_id"]=json!(frame.source_id);
        metadata["rtp_timestamp"]=json!(frame.rtp_timestamp);
        metadata["rtp_clock_rate_hz"]=json!(frame.clock_rate_hz);
        crate::metrics::event("video.observation",json!({"sequence":frame.sequence,"source_id":frame.source_id,"frame_age_ms":frame.received.elapsed().as_secs_f64()*1000.,"pose":pose,"rtp_timestamp":frame.rtp_timestamp}));
        Ok((metadata, frame))

}).await
    }
    pub async fn stop(&self) -> Result<()> {
        crate::metrics::result("camera.stop", async {
            self.cancelled.store(true, Ordering::SeqCst);
            // Queue the barrier before issuing Stop, while issuing Stop without
            // waiting for the active motion lease. This preserves immediate Stop
            // and prevents later moves from passing outstanding stop requests.
            let (_lease, result) = stop_barrier(self.command_gate.clone(), self.onvif.stop()).await;
            let mut state = crate::metrics::timed("lock.state_wait", self.state.lock()).await;
            state.barrier = Some(Instant::now() + Duration::from_millis(100));
            state.last_motion_pose = None;
            state.last_direction = [0, 0];
            result
        })
        .await
    }
    async fn await_cleanup(&self) {
        let _lease =
            crate::metrics::timed("lock.motion_cleanup_wait", self.command_gate.lock()).await;
    }
    pub async fn begin_motion(&self) {
        self.await_cleanup().await;
        self.cancelled.store(false, Ordering::SeqCst);
    }
    async fn stop_guard(&self) -> Result<StopGuard> {
        let lease = crate::metrics::timed(
            "lock.motion_cleanup_wait",
            self.command_gate.clone().lock_owned(),
        )
        .await;
        self.check_cancelled()?;
        Ok(StopGuard {
            onvif: self.onvif.clone(),
            state: self.state.clone(),
            lease: Some(lease),
            armed: true,
        })
    }
    fn check_cancelled(&self) -> Result<()> {
        ensure!(
            !self.cancelled.load(Ordering::SeqCst),
            "Camera movement cancelled"
        );
        Ok(())
    }
    pub async fn goto(&self, target: [f64; 2]) -> Result<[f64; 2]> {
        crate::metrics::result("motion.goto", async {
            self.goto_with_settle(target, delay("CAMERA_SETTLE_MS", 300))
                .await
        })
        .await
    }
    async fn goto_with_settle(&self, target: [f64; 2], settle: Duration) -> Result<[f64; 2]> {
        crate::metrics::result("motion.goto_with_settle",async {
            self.check_cancelled()?;
            let poll_delay=delay("CAMERA_POLL_MS",60);
            let confirmations=parameter("CAMERA_STABLE_SAMPLES",2,1,3);
            let target=clip(target);
            let before_pose=self.onvif.cached_position().map(|(p,_)|p);
            let mut guard=self.stop_guard().await?;
            let started=Instant::now();
            crate::metrics::event("motion.target",json!({"target":target,"settle_ms":settle.as_secs_f64()*1000.,"poll_ms":poll_delay.as_secs_f64()*1000.,"confirmations":confirmations}));
            let movement=async {
                self.onvif.absolute(target).await?;
                let commanded=Instant::now();
                let deadline=Instant::now()+Duration::from_secs(6);
                let mut stable=0;let mut polls=0;
                let pose=loop {
                    self.check_cancelled()?;
                    ensure!(Instant::now()<deadline,"Camera did not reach target position");
                    crate::metrics::sleep("motion.poll_interval",poll_delay).await;
                    let actual=self.onvif.position().await?;polls+=1;
                    stable=if max_delta(actual,target)<0.002 {stable+1}else{0};
                    crate::metrics::event("motion.poll",json!({"actual":actual,"target":target,"max_error":max_delta(actual,target),"stable_readings":stable,"poll":polls}));
                    if stable>=confirmations {crate::metrics::event("motion.reported_arrival",json!({"actual":actual,"polls":polls}));break actual;}
                };
                let arrived=Instant::now();
                crate::metrics::sleep("motion.settling_wait",settle).await;
                self.check_cancelled()?;
                Ok::<_,anyhow::Error>((pose,commanded,arrived,polls))
            }.await;
            // Logical errors/explicit cancellation stop before returning. Drop
            // cleanup is reserved for an abruptly dropped future or a panic.
            let stopped=guard.finish().await;
            let mut state=crate::metrics::timed("lock.state_wait",self.state.lock()).await;
            state.barrier=Some(Instant::now()+if movement.is_err(){Duration::from_millis(100)}else{Duration::ZERO});
            if movement.is_err() {state.last_motion_pose=None;state.last_direction=[0,0];}
            let (pose,commanded,arrived,polls)=movement?;
            if let Some(before)=before_pose {
                for axis in 0..2 {if (pose[axis]-before[axis]).abs()>0.002 {state.last_direction[axis]=if pose[axis]>before[axis] {1}else{-1};}}
            } else {state.last_direction=[0,0];}
            state.last_motion_pose=Some(pose);
            stopped?;
            state.last_move=json!({"command_ms":commanded.duration_since(started).as_secs_f64()*1000.,"arrival_ms":arrived.duration_since(commanded).as_secs_f64()*1000.,"settle_stop_ms":arrived.elapsed().as_secs_f64()*1000.,"polls":polls});
            Ok(pose)
        }).await
    }
    pub async fn movement(&self, pan: f64, tilt: f64, seconds: f64) -> Result<Value> {
        crate::metrics::result("motion.velocity", async {
            self.begin_motion().await;
            let mut guard = self.stop_guard().await?;
            let movement = async {
                self.onvif.velocity(pan, tilt, seconds).await?;
                let deadline = Instant::now() + Duration::from_secs_f64(seconds);
                while Instant::now() < deadline {
                    self.check_cancelled()?;
                    crate::metrics::sleep("motion.velocity_interval", Duration::from_millis(20))
                        .await;
                }
                Ok::<_, anyhow::Error>(())
            }
            .await;
            let stopped = guard.finish().await;
            let mut state = crate::metrics::timed("lock.state_wait", self.state.lock()).await;
            state.barrier = Some(Instant::now() + Duration::from_millis(100));
            state.last_motion_pose = None;
            state.last_direction = [0, 0];
            movement?;
            stopped?;
            Ok(json!({"stopped":true}))
        })
        .await
    }
    async fn track(a: Arc<Frame>, b: Arc<Frame>, point: [f64; 2]) -> Result<Option<[f64; 2]>> {
        crate::metrics::result("vision.dispatch", async {
            crate::metrics::blocking(move || vision::track(&a.jpeg, &b.jpeg, point))
                .await
                .map_err(Into::into)
        })
        .await
    }
    async fn compensated_target(
        &self,
        pose: [f64; 2],
        target: [f64; 2],
        precise: bool,
    ) -> ([f64; 2], [f64; 2]) {
        if !precise || !std::env::var("CAMERA_BACKLASH").is_ok_and(|s| s == "1") {
            return (target, [0., 0.]);
        }
        let state = crate::metrics::timed("lock.state_wait", self.state.lock()).await;
        let directions = if state
            .last_motion_pose
            .is_some_and(|p| max_delta(p, pose) < 0.002)
        {
            state.last_direction
        } else {
            [0, 0]
        };
        let (adjusted, compensation) = backlash_target(pose, target, directions, self.backlash);
        crate::metrics::event(
            "focus.backlash",
            json!({"directions":directions,"calibrated_gap":self.backlash,"compensation":compensation}),
        );
        (adjusted, compensation)
    }
    pub async fn focus(
        &self,
        x: f64,
        y: f64,
        id: &str,
        precise: bool,
        rough: bool,
    ) -> Result<ObservationResult> {
        crate::metrics::result("camera.focus", async {
        ensure!(!(rough && precise), "Choose rough or precise, not both");
        crate::metrics::event("focus.calibration",json!({"gain":focus_gain(),"matrix":[[self.calibration[(0,0)],self.calibration[(0,1)]],[self.calibration[(1,0)],self.calibration[(1,1)]]]}));
        ensure!(
            [x, y]
                .iter()
                .all(|v| v.is_finite() && (0.0..=1.0).contains(v)),
            "Point coordinates must be in 0..1"
        );
        let observation = {
            let state = crate::metrics::timed("lock.state_wait",self.state.lock()).await;
            state
                .observations
                .iter()
                .find(|o| o.id == id && (o.revisit || state.latest == id))
                .cloned()
        };
        let Some(old) = observation.filter(|o| o.created.elapsed() < Duration::from_secs(120))
        else {
            return self.observe(false,json!({"focus_status":"stale_frame","detail":"Choose a point in this new image"}),false).await;
        };
        self.begin_motion().await;
        let mut pose = self.onvif.position().await?;
        let mut pose_changed = max_delta(pose, old.pose) > 0.003;
        if pose_changed && old.revisit {
            pose = self.goto(old.pose).await?;
            pose_changed = false;
        }
        let barrier = if pose_changed {
            Some(Instant::now())
        } else {
            crate::metrics::timed("lock.state_wait",self.state.lock()).await.barrier
        };
        let mut frame = self.preview.frame(barrier).await?;
        let mut point = [x, y];
        if (old.frame.sequence != frame.sequence && !rough) || pose_changed {
            let tracked = Self::track(old.frame.clone(), frame.clone(), point).await?;
            if pose_changed
                && !tracked
                    .is_some_and(|p| can_compensate_pose_cache(max_delta(pose, old.pose), point, p))
            {
                return self.record(frame,pose,json!({"focus_status":"stale_frame","detail":"Camera view changed; choose a point in this new image"}),false).await;
            }
            let Some(tracked) = tracked else {
                return self
                    .record(
                        frame,
                        pose,
                        json!({"focus_status":"tracking_lost","iterations":0,"target":null}),
                        false,
                    )
                    .await;
            };
            point = tracked;
        }
        if rough {
            if norm(point) <= 0.08 {
                return self.record(frame,pose,json!({"focus_status":"aimed","rough":true,"iterations":0,"target":null,"detail":"Point was already near center; no move or visual verification"}),false).await;
            }
            let target = rough_target(self.calibration, pose, point)?;
            if max_delta(target, pose) < 0.002 {
                return self.record(frame,pose,json!({"focus_status":"mechanical_limit","rough":true,"iterations":0,"target":null}),false).await;
            }
            let actual = self
                .goto_with_settle(target, delay("CAMERA_ROUGH_SETTLE_MS",100))
                .await?;
            frame = self.preview.frame(crate::metrics::timed("lock.state_wait",self.state.lock()).await.barrier).await?;
            let timing = crate::metrics::timed("lock.state_wait",self.state.lock()).await.last_move.clone();
            return self.record(frame,actual,json!({"focus_status":"aimed","rough":true,"iterations":1,"target":null,"move_timing":timing,"detail":"Single calibrated move; centering not visually verified"}),false).await;
        }
        let threshold = if precise { 0.02 } else { 0.04 };
        let limit = if precise { 3 } else { 2 };
        let mut matrix = self.calibration;
        let mut status = "iteration_limit";
        let mut corrections = vec![];
        for _ in 0..limit {
            if norm(point) <= threshold {
                status = "centered";
                break;
            }
            self.check_cancelled()?;
            let target = aim_target(matrix, pose, point)?;
            let (target,compensation)=self.compensated_target(pose,target,precise).await;
            if max_delta(target, pose) < 0.002 {
                status = "mechanical_limit";
                break;
            }
            let actual = self.goto(target).await?;
            let after = self.preview.frame(crate::metrics::timed("lock.state_wait",self.state.lock()).await.barrier).await?;
            let Some(tracked) = Self::track(frame.clone(), after.clone(), point).await? else {
                frame = after;
                pose = actual;
                status = "tracking_lost";
                break;
            };
            let movement = Vector2::new(actual[0] - pose[0]-compensation[0], actual[1] - pose[1]-compensation[1]);
            let shift = Vector2::new(tracked[0] - point[0], tracked[1] - point[1]);
            corrections
                .push(json!({"before":point,"after":tracked,"pose_delta":[actual[0]-pose[0],actual[1]-pose[1]],"backlash_compensation":compensation,"move_timing":crate::metrics::timed("lock.state_wait",self.state.lock()).await.last_move.clone()}));
            if movement.norm_squared() > 1e-6 {
                let candidate = matrix
                    + (shift - matrix * movement) * movement.transpose() / movement.norm_squared();
                let ratios = [
                    candidate[(0, 0)] / self.calibration[(0, 0)],
                    candidate[(1, 1)] / self.calibration[(1, 1)],
                ];
                if ratios.iter().all(|r| *r > 0.5 && *r < 2.)
                    && candidate[(0, 1)].abs() < 0.5
                    && candidate[(1, 0)].abs() < 0.5
                    && candidate.determinant().abs() > 0.01
                {
                    matrix = (matrix + candidate) * 0.5;
                }
            }
            point = tracked;
            pose = actual;
            frame = after;
        }
        if status == "iteration_limit" && norm(point) <= threshold {
            status = "centered";
        }
        self.record(frame,pose,json!({"focus_status":status,"iterations":corrections.len(),"target":if status=="tracking_lost"{None}else{Some(point)},"corrections":corrections,"precise":precise}),false).await

}).await
    }
    pub async fn scan(
        &self,
        direction: &str,
        views: usize,
        step: f64,
    ) -> Result<Vec<ObservationResult>> {
        crate::metrics::result("camera.scan", async {
        ensure!(
            matches!(direction, "left" | "right")
                && (2..=6).contains(&views)
                && step.is_finite()
                && (0.05..=0.35).contains(&step),
            "Invalid scan: direction left/right, 2..6 views, step 0.05.. 0.35"
        );
        self.begin_motion().await;
        let origin = self.onvif.position().await?;
        let sign = if direction == "left" { 1. } else { -1. };
        let mut result = vec![];
        for index in 0..views {
            self.check_cancelled()?;
            let target = clip([origin[0] + sign * step * index as f64, origin[1]]);
            if index > 0 {
                let previous = clip([origin[0] + sign * step * (index - 1) as f64, origin[1]]);
                if max_delta(previous, target) < 0.01 {
                    break;
                }
                self.goto(target).await?;
            }
            result.push(self.observe(false,json!({"scan_index":index,"direction":direction,"revisitable":true,"pose":target,"move_timing":if index>0{crate::metrics::timed("lock.state_wait",self.state.lock()).await.last_move.clone()}else{Value::Null}}),true).await?);
        }
        Ok(result)

}).await
    }
    pub async fn remember(&self, name: &str) -> Result<Value> {
        crate::metrics::result("camera.remember", async {
            self.await_cleanup().await;
            ensure!(
                !name.is_empty()
                    && name.len() <= 64
                    && name
                        .chars()
                        .all(|c| c.is_alphanumeric() || " _-".contains(c)),
                "Use a short view name with letters, digits, spaces, hyphens or underscores"
            );
            let pose = self.onvif.position().await?;
            let mut views = crate::metrics::timed("lock.views_wait", self.views.lock()).await;
            views.insert(name.to_owned(), pose);
            crate::metrics::sync_result("views.persist", || -> Result<()> {
                let temp = self.views_path.with_extension("tmp");
                std::fs::write(&temp, serde_json::to_vec_pretty(&*views)?)?;
                std::fs::rename(temp, &self.views_path)?;
                Ok(())
            })?;
            Ok(json!({"name":name,"pose":pose,"saved":true}))
        })
        .await
    }
    pub async fn look(&self, name: &str) -> Result<ObservationResult> {
        crate::metrics::result("camera.look", async {
            let pose = *crate::metrics::timed("lock.views_wait", self.views.lock())
                .await
                .get(name)
                .ok_or_else(|| anyhow::anyhow!("Unknown remembered view"))?;
            self.begin_motion().await;
            self.goto(pose).await?;
            self.observe(false, json!({"view":name}), false).await
        })
        .await
    }
}

impl Camera {
    async fn calibration_shifts(before: Arc<Frame>, after: Arc<Frame>) -> Result<Vec<[f64; 2]>> {
        let mut shifts = vec![];
        for x in [0.3, 0.5, 0.7] {
            for y in [0.3, 0.5, 0.7] {
                if let Some(point) = Self::track(before.clone(), after.clone(), [x, y]).await? {
                    shifts.push([point[0] - x, point[1] - y]);
                }
            }
        }
        Ok(shifts)
    }
    pub async fn recalibrate(&self, root: &Path) -> Result<Value> {
        crate::metrics::result("camera.calibrate",async {
            let _lock=crate::metrics::timed("lock.motion_wait",self.motion.lock()).await;
            self.begin_motion().await;
            let origin=self.onvif.position().await?;
            let anchor=[origin[0].clamp(-0.8,0.8),origin[1].clamp(-0.7,0.7)];
            let settle=Duration::from_millis(400);
            let measured=async {
                let mut matrix=Matrix2::zeros();
                let mut counts=vec![];let mut samples=vec![];let mut return_offsets=vec![];let mut image_gaps=[0.;2];
                for (axis,amount) in [(0,0.04),(1,0.08)] {
                    let mut derivatives=vec![];
                    let mut first_origin:Option<Arc<Frame>>=None;
                    for direction in [1.,-1.] {
                        // Engage the gears in the measurement direction before BOTH
                        // baseline and sample; recapture baseline for every excursion.
                        let mut approach=anchor;approach[axis]-=direction*amount;
                        self.goto_with_settle(approach,settle).await?;
                        let before_pose=self.goto_with_settle(anchor,settle).await?;
                        let baseline=self.preview.frame(Some(Instant::now())).await?;
                        if let Some(first)=&first_origin {
                            let offsets=Self::calibration_shifts(first.clone(),baseline.clone()).await?;
                            if offsets.len()>=4 {let mut values:Vec<_>=offsets.iter().map(|v|v[axis]).collect();values.sort_by(f64::total_cmp);image_gaps[axis]=values[values.len()/2].abs();}
                            return_offsets.push(json!({"axis":axis,"samples":offsets}));
                        } else {first_origin=Some(baseline.clone());}
                        let mut target=anchor;target[axis]+=direction*amount;
                        let actual=self.goto_with_settle(target,settle).await?;
                        let after=self.preview.frame(Some(Instant::now())).await?;
                        let delta=actual[axis]-before_pose[axis];
                        ensure!(delta.abs()>0.005,"Calibration excursion too small");
                        let shifts=Self::calibration_shifts(baseline.clone(),after.clone()).await?;
                        ensure!(shifts.len()>=4,"Not enough stationary texture for calibration; choose a textured view");
                        let mut derivative=[0.;2];
                        for row in 0..2 {
                            let mut values:Vec<_>=shifts.iter().map(|v|v[row]/delta).collect();
                            values.sort_by(f64::total_cmp);derivative[row]=values[values.len()/2];
                        }
                        let before_path=format!("/tmp/rust-calibration-{axis}-{direction}-before.jpg");
                        let after_path=format!("/tmp/rust-calibration-{axis}-{direction}-after.jpg");
                        std::fs::write(&before_path,baseline.jpeg.as_slice())?;
                        std::fs::write(&after_path,after.jpeg.as_slice())?;
                        samples.push(json!({"axis":axis,"direction":direction,"before_pose":before_pose,"after_pose":actual,"delta":delta,"matched_points":shifts.len(),"derivative":derivative,"before_image":before_path,"after_image":after_path}));
                        derivatives.push((derivative,shifts.len()));
                    }
                    counts.push(derivatives.iter().map(|(_,n)|n).sum::<usize>());
                    let a=derivatives[0].0[axis];let b=derivatives[1].0[axis];
                    ensure!(a*b>0. && (a-b).abs()/a.abs().max(b.abs())<0.35,"Directional calibration disagrees; previous file retained");
                    for row in 0..2 {matrix[(row,axis)]=(derivatives[0].0[row]+derivatives[1].0[row])/2.;}
                }
                ensure!(matrix.iter().all(|v|v.is_finite()) && matrix.determinant().abs()>0.01,"Unreliable calibration; previous file retained");
                Ok::<_,anyhow::Error>(json!({"model":self.onvif.model,"firmware":self.onvif.firmware,
                    "image_shift_per_pose_unit":[[matrix[(0,0)],matrix[(0,1)]],[matrix[(1,0)],matrix[(1,1)]]],
                    "method":format!("Symmetric Rust FAST/BRIEF affine finite differences; matched approach direction and fresh baselines on {}",self.preview_encoding),
                    "backlash_pose_units":[(image_gaps[0]/matrix[(0,0)].abs()).min(0.03),(image_gaps[1]/matrix[(1,1)].abs()).min(0.03)],
                    "samples_per_axis":counts,"directional_samples":samples,"origin_return_offsets":return_offsets,
                    "coordinate_system":"image normalized from top-left; PTZ position as reported by ONVIF"}))
            }.await;
            // An explicit Stop keeps cancellation authoritative; otherwise restore.
            let restored=self.goto_with_settle(origin,settle).await;
            restored?;let value=measured?;
            crate::metrics::sync_result("config.calibration_persist",||->Result<()> {
                let temp=root.join("calibration.tmp");
                std::fs::write(&temp,serde_json::to_vec_pretty(&value)?)?;
                std::fs::rename(temp,root.join("calibration.json"))?;Ok(())
            })?;
            Ok(value)
        }).await
    }
}

impl Drop for Camera {
    fn drop(&mut self) {
        self.pose_poller.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn stop_bypasses_active_move_but_later_moves_wait_for_ack() {
        use tokio::sync::oneshot;
        let gate = Arc::new(Mutex::new(()));
        let active = gate.clone().lock_owned().await;
        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let copy = gate.clone();
        let stopping = tokio::spawn(async move {
            stop_barrier(copy, async {
                started_tx.send(()).unwrap();
                release_rx.await.unwrap();
            })
            .await
        });
        tokio::time::timeout(Duration::from_secs(1), started_rx)
            .await
            .unwrap()
            .unwrap();
        let copy = gate.clone();
        let mut later = tokio::spawn(async move { copy.lock_owned().await });
        drop(active);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut later)
                .await
                .is_err()
        );
        release_tx.send(()).unwrap();
        let (stop_lease, ()) = stopping.await.unwrap();
        drop(stop_lease);
        let _ = tokio::time::timeout(Duration::from_secs(1), later)
            .await
            .unwrap()
            .unwrap();
    }
    #[tokio::test]
    async fn cancelled_stop_waiter_keeps_motion_lease_until_rpc_completes() {
        use tokio::sync::oneshot;
        let gate = Arc::new(Mutex::new(()));
        let lease = gate.clone().lock_owned().await;
        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let waiting = tokio::spawn(async move {
            durable_stop(Some(lease), async {
                started_tx.send(()).unwrap();
                release_rx.await.unwrap();
            })
            .await
        });
        started_rx.await.unwrap();
        waiting.abort();
        let _ = waiting.await;
        assert!(
            gate.try_lock().is_err(),
            "Cancelling the caller must not release a pending Stop lease"
        );
        release_tx.send(()).unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(1), gate.lock())
            .await
            .unwrap();
    }
    #[test]
    fn backlash_compensation_respects_direction_and_hard_limits() {
        let p = [0., 0.];
        let t = [0.1, 0.];
        let b = [0.008, 0.];
        assert_eq!(backlash_target(p, t, [1, 0], b).0, t);
        assert!((backlash_target(p, t, [-1, 0], b).0[0] - 0.108).abs() < 1e-9);
        assert!((backlash_target(p, t, [0, 0], b).0[0] - 0.104).abs() < 1e-9);
        assert_eq!(
            backlash_target([0.99, 0.], [1., 0.], [-1, 0], b).0,
            [1., 0.]
        );
        assert_eq!(backlash_target(p, p, [-1, 0], b).0, p);
    }
    #[test]
    fn point_geometry_and_limits() {
        assert_eq!(clip([2., -2.]), [1., -1.]);
        assert_eq!(norm([0.5, 0.5]), 0.);
        let j = Matrix2::new(1.3, 0., 0., 0.8);
        let d = -j.try_inverse().unwrap() * Vector2::new(0.2, -0.2);
        assert!(d[0] < 0. && d[1] > 0.);
    }
}

#[cfg(test)]
mod cache_lag_tests {
    use super::*;
    #[test]
    fn small_encoder_lag_requires_visually_unchanged_target() {
        assert!(can_compensate_pose_cache(0.01, [0.6, 0.6], [0.601, 0.602]));
        assert!(!can_compensate_pose_cache(0.01, [0.6, 0.6], [0.65, 0.6]));
    }
    #[test]
    fn large_external_pan_still_invalidates_observation() {
        assert!(!can_compensate_pose_cache(0.1, [0.5, 0.5], [0.5, 0.5]));
        assert!(!can_compensate_pose_cache(f64::NAN, [0.5, 0.5], [0.5, 0.5]));
    }
}

#[cfg(test)]
mod rough_tests {
    use super::*;
    #[test]
    fn calibrated_rough_target_has_correct_direction_and_limits() {
        let matrix = Matrix2::new(1.3, 0., 0., 0.8);
        let target = rough_target(matrix, [0., 0.], [0.7, 0.3]).unwrap();
        assert!(target[0] < 0. && target[1] > 0.);
        let target = rough_target(matrix, [0.99, -0.99], [0., 1.]).unwrap();
        assert_eq!(target, [1., -1.]);
    }
}

#[cfg(test)]
mod rough_range_tests {
    use super::*;
    #[test]
    fn far_image_point_is_not_limited_to_a_small_correction_step() {
        let matrix = Matrix2::new(1.3, 0., 0., 0.8);
        let regular = aim_target(matrix, [0., 0.], [0.95, 0.95]).unwrap();
        let rough = rough_target(matrix, [0., 0.], [0.95, 0.95]).unwrap();
        assert!(rough[0].abs() > regular[0].abs());
        assert!(rough[1].abs() > regular[1].abs());
    }
}
