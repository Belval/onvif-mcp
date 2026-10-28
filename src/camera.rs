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
use tokio::sync::Mutex;

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
}
#[derive(Deserialize)]
struct Calibration {
    model: Option<String>,
    image_shift_per_pose_unit: [[f64; 2]; 2],
}
pub type ObservationResult = (Value, Arc<Frame>);

pub struct Camera {
    pub onvif: Arc<Onvif>,
    pub preview: Video,
    pose_poller: tokio::task::JoinHandle<()>,
    detail_token: String,
    detail_dimensions: (u32, u32),
    state: Mutex<State>,
    pub motion: Mutex<()>,
    cancelled: AtomicBool,
    calibration: Matrix2<f64>,
    views: Mutex<HashMap<String, [f64; 2]>>,
    views_path: std::path::PathBuf,
}
struct StopGuard {
    onvif: Arc<Onvif>,
    armed: bool,
}
impl StopGuard {
    async fn finish(&mut self) -> Result<()> {
        let result = self.onvif.stop().await;
        self.armed = false;
        result
    }
}
impl Drop for StopGuard {
    fn drop(&mut self) {
        if self.armed {
            let onvif = self.onvif.clone();
            tokio::spawn(async move {
                let _ = onvif.stop().await;
            });
        }
    }
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
fn calibrated_delta(matrix: Matrix2<f64>, point: [f64; 2]) -> Result<Vector2<f64>> {
    Ok(-0.8
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

impl Camera {
    pub async fn connect(root: &Path) -> Result<Arc<Self>> {
        let onvif = Arc::new(Onvif::connect(Config::load()?).await?);
        let profile = onvif
            .profiles
            .iter()
            .find(|p| p.encoding == "JPEG" && p.width == 640)
            .ok_or_else(|| {
                anyhow::anyhow!("This tuned server requires the C210 native 640px JPEG profile")
            })?;
        let preview = Video::start(
            onvif.stream_url(&profile.token).await?,
            onvif.config.clone(),
            true,
            (profile.width, profile.height),
        );
        let detail = onvif
            .profiles
            .iter()
            .filter(|p| p.encoding == "H264" && p.width >= 1280)
            .min_by_key(|p| p.width)
            .ok_or_else(|| anyhow::anyhow!("H264 detail profile unavailable"))?;
        let (detail_token, detail_dimensions) =
            (detail.token.clone(), (detail.width, detail.height));
        let calibration: Calibration =
            serde_json::from_slice(&std::fs::read(root.join("calibration.json"))?)?;
        ensure!(
            calibration.model.as_ref().is_none_or(|m| m == &onvif.model),
            "Calibration belongs to a different camera model"
        );
        let data = calibration.image_shift_per_pose_unit;
        let calibration = Matrix2::new(data[0][0], data[0][1], data[1][0], data[1][1]);
        ensure!(
            calibration.iter().all(|v| v.is_finite()) && calibration.determinant().abs() > 0.01,
            "Invalid camera calibration"
        );
        let views_path = root.join("views.json");
        let views = if views_path.exists() {
            serde_json::from_slice(&std::fs::read(&views_path)?)?
        } else {
            HashMap::new()
        };
        onvif.position().await?;
        let weak = Arc::downgrade(&onvif);
        let pose_poller = tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(150)).await;
                let Some(camera) = weak.upgrade() else {
                    break;
                };
                let _ = camera.position().await;
            }
        });
        Ok(Arc::new(Self {
            onvif,
            preview,
            pose_poller,
            detail_token,
            detail_dimensions,
            state: Mutex::new(State {
                epoch: rand::random(),
                ..State::default()
            }),
            motion: Mutex::new(()),
            cancelled: AtomicBool::new(false),
            calibration,
            views: Mutex::new(views),
            views_path,
        }))
    }
    pub async fn status(&self) -> Result<Value> {
        let pose = self.onvif.position().await?;
        Ok(
            json!({"manufacturer":self.onvif.manufacturer,"model":self.onvif.model,"firmware":self.onvif.firmware,
            "profiles":self.onvif.profiles,"ptz":{"Position":{"PanTilt":{"x":pose[0],"y":pose[1]}}},
            "video":{"transport":"persistent RTSP/TCP","encoding":"native JPEG passthrough","buffered_frames":1,
                "connected":self.preview.metrics.connected.load(Ordering::Relaxed),"frames_received":self.preview.metrics.frames.load(Ordering::Relaxed),
                "reconnects":self.preview.metrics.reconnects.load(Ordering::Relaxed),"frame_age_ms":self.preview.age_ms()},
            "remembered_views":self.views.lock().await.keys().cloned().collect::<Vec<_>>()}),
        )
    }
    async fn frame(&self, detail: bool, after: Option<Instant>) -> Result<Arc<Frame>> {
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
    }
    pub async fn observe(
        &self,
        detail: bool,
        metadata: Value,
        revisit: bool,
    ) -> Result<ObservationResult> {
        let barrier = self.state.lock().await.barrier;
        let frame = self.frame(detail, barrier).await?;
        let (pose, pose_at) = match self.onvif.cached_position() {
            Some(cached) => cached,
            None => (self.onvif.position().await?, Instant::now()),
        };
        let mut metadata = metadata;
        metadata["pose_age_ms"] = json!(pose_at.elapsed().as_secs_f64() * 1000.);
        self.record(frame, pose, metadata, revisit).await
    }
    async fn record(
        &self,
        frame: Arc<Frame>,
        pose: [f64; 2],
        mut metadata: Value,
        revisit: bool,
    ) -> Result<ObservationResult> {
        let mut state = self.state.lock().await;
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
        Ok((metadata, frame))
    }
    pub async fn stop(&self) -> Result<()> {
        self.cancelled.store(true, Ordering::SeqCst);
        let result = self.onvif.stop().await;
        self.state.lock().await.barrier = Some(Instant::now() + Duration::from_millis(100));
        result
    }
    pub fn begin_motion(&self) {
        self.cancelled.store(false, Ordering::SeqCst);
    }
    fn check_cancelled(&self) -> Result<()> {
        ensure!(
            !self.cancelled.load(Ordering::SeqCst),
            "Camera movement cancelled"
        );
        Ok(())
    }
    pub async fn goto(&self, target: [f64; 2]) -> Result<[f64; 2]> {
        self.goto_with_settle(target, Duration::from_millis(300))
            .await
    }
    async fn goto_with_settle(&self, target: [f64; 2], settle: Duration) -> Result<[f64; 2]> {
        self.check_cancelled()?;
        let target = clip(target);
        let mut guard = StopGuard {
            onvif: self.onvif.clone(),
            armed: true,
        };
        let started = Instant::now();
        self.onvif.absolute(target).await?;
        let commanded = Instant::now();
        let mut polls = 0;
        let deadline = Instant::now() + Duration::from_secs(6);
        let mut stable = 0;
        let pose = loop {
            self.check_cancelled()?;
            ensure!(
                Instant::now() < deadline,
                "Camera did not reach target position"
            );
            tokio::time::sleep(Duration::from_millis(60)).await;
            let actual = self.onvif.position().await?;
            polls += 1;
            stable = if max_delta(actual, target) < 0.002 {
                stable + 1
            } else {
                0
            };
            if stable >= 2 {
                break actual;
            }
        };
        let arrived = Instant::now();
        // The device position can lead optical arrival; hold briefly, then require a new video frame.
        tokio::time::sleep(settle).await;
        self.check_cancelled()?;
        guard.finish().await?;
        let mut state = self.state.lock().await;
        state.barrier = Some(Instant::now());
        state.last_move = json!({"command_ms":commanded.duration_since(started).as_secs_f64()*1000.,"arrival_ms":arrived.duration_since(commanded).as_secs_f64()*1000.,"settle_stop_ms":arrived.elapsed().as_secs_f64()*1000.,"polls":polls});
        Ok(pose)
    }
    pub async fn movement(&self, pan: f64, tilt: f64, seconds: f64) -> Result<Value> {
        self.begin_motion();
        let mut guard = StopGuard {
            onvif: self.onvif.clone(),
            armed: true,
        };
        self.onvif.velocity(pan, tilt, seconds).await?;
        let deadline = Instant::now() + Duration::from_secs_f64(seconds);
        while Instant::now() < deadline {
            self.check_cancelled()?;
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        guard.finish().await?;
        self.state.lock().await.barrier = Some(Instant::now() + Duration::from_millis(100));
        Ok(json!({"stopped":true}))
    }
    async fn track(a: Arc<Frame>, b: Arc<Frame>, point: [f64; 2]) -> Result<Option<[f64; 2]>> {
        tokio::task::spawn_blocking(move || vision::track(&a.jpeg, &b.jpeg, point))
            .await
            .map_err(Into::into)
    }
    pub async fn focus(
        &self,
        x: f64,
        y: f64,
        id: &str,
        precise: bool,
        rough: bool,
    ) -> Result<ObservationResult> {
        ensure!(!(rough && precise), "Choose rough or precise, not both");
        ensure!(
            [x, y]
                .iter()
                .all(|v| v.is_finite() && (0.0..=1.0).contains(v)),
            "Point coordinates must be in 0..1"
        );
        let observation = {
            let state = self.state.lock().await;
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
        self.begin_motion();
        let mut pose = self.onvif.position().await?;
        let mut pose_changed = max_delta(pose, old.pose) > 0.003;
        if pose_changed && old.revisit {
            pose = self.goto(old.pose).await?;
            pose_changed = false;
        }
        let barrier = if pose_changed {
            Some(Instant::now())
        } else {
            self.state.lock().await.barrier
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
                .goto_with_settle(target, Duration::from_millis(100))
                .await?;
            frame = self.preview.frame(self.state.lock().await.barrier).await?;
            let timing = self.state.lock().await.last_move.clone();
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
            if max_delta(target, pose) < 0.002 {
                status = "mechanical_limit";
                break;
            }
            let actual = self.goto(target).await?;
            let after = self.preview.frame(self.state.lock().await.barrier).await?;
            let Some(tracked) = Self::track(frame.clone(), after.clone(), point).await? else {
                frame = after;
                pose = actual;
                status = "tracking_lost";
                break;
            };
            let movement = Vector2::new(actual[0] - pose[0], actual[1] - pose[1]);
            let shift = Vector2::new(tracked[0] - point[0], tracked[1] - point[1]);
            corrections
                .push(json!({"before":point,"after":tracked,"pose_delta":movement.as_slice(),"move_timing":self.state.lock().await.last_move.clone()}));
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
    }
    pub async fn scan(
        &self,
        direction: &str,
        views: usize,
        step: f64,
    ) -> Result<Vec<ObservationResult>> {
        ensure!(
            matches!(direction, "left" | "right")
                && (2..=6).contains(&views)
                && step.is_finite()
                && (0.05..=0.35).contains(&step),
            "Invalid scan: direction left/right, 2..6 views, step 0.05.. 0.35"
        );
        self.begin_motion();
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
            result.push(self.observe(false,json!({"scan_index":index,"direction":direction,"revisitable":true,"pose":target,"move_timing":if index>0{self.state.lock().await.last_move.clone()}else{Value::Null}}),true).await?);
        }
        Ok(result)
    }
    pub async fn remember(&self, name: &str) -> Result<Value> {
        ensure!(
            !name.is_empty()
                && name.len() <= 64
                && name
                    .chars()
                    .all(|c| c.is_alphanumeric() || " _-".contains(c)),
            "Use a short view name with letters, digits, spaces, hyphens or underscores"
        );
        let pose = self.onvif.position().await?;
        let mut views = self.views.lock().await;
        views.insert(name.to_owned(), pose);
        let temp = self.views_path.with_extension("tmp");
        std::fs::write(&temp, serde_json::to_vec_pretty(&*views)?)?;
        std::fs::rename(temp, &self.views_path)?;
        Ok(json!({"name":name,"pose":pose,"saved":true}))
    }
    pub async fn look(&self, name: &str) -> Result<ObservationResult> {
        let pose = *self
            .views
            .lock()
            .await
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("Unknown remembered view"))?;
        self.begin_motion();
        self.goto(pose).await?;
        self.observe(false, json!({"view":name}), false).await
    }
}

impl Camera {
    pub async fn recalibrate(&self, root: &Path) -> Result<Value> {
        let _lock = self.motion.lock().await;
        self.begin_motion();
        let origin = self.onvif.position().await?;
        let baseline = self.preview.frame(None).await?;
        let measured=async {
            let mut matrix=Matrix2::zeros();let mut counts=vec![];
            for (axis,amount) in [(0,0.04),(1,0.12)] {
                let mut target=origin;target[axis]+=if origin[axis]+amount<0.95 {amount}else{-amount};
                let actual=self.goto(target).await?;
                let after=self.preview.frame(self.state.lock().await.barrier).await?;
                let mut shifts=vec![];
                for x in [0.3,0.5,0.7] {for y in [0.3,0.5,0.7] {
                    if let Some(point)=Self::track(baseline.clone(),after.clone(),[x,y]).await? {
                        shifts.push([(point[0]-x)/(actual[axis]-origin[axis]),(point[1]-y)/(actual[axis]-origin[axis])]);
                    }
                }}
                ensure!(shifts.len()>=4,"Not enough stationary texture for calibration; aim at the desk");
                counts.push(shifts.len());
                for row in 0..2 {let mut values:Vec<_>=shifts.iter().map(|v|v[row]).collect();values.sort_by(f64::total_cmp);matrix[(row,axis)]=values[values.len()/2];}
                self.goto(origin).await?;
            }
            ensure!(matrix.iter().all(|v|v.is_finite()) && matrix.determinant().abs()>0.01,"Unreliable calibration; previous file retained");
            Ok::<_,anyhow::Error>(json!({"model":self.onvif.model,"firmware":self.onvif.firmware,
                "image_shift_per_pose_unit":[[matrix[(0,0)],matrix[(0,1)]],[matrix[(1,0)],matrix[(1,1)]]],
                "method":"Rust FAST/BRIEF local affine RANSAC finite differences on native JPEG stream",
                "samples_per_axis":counts,"coordinate_system":"image normalized from top-left; PTZ position as reported by ONVIF"}))
        }.await;
        let restored = self.goto(origin).await;
        restored?;
        let value = measured?;
        let path = root.join("calibration.json");
        let temp = root.join("calibration.tmp");
        std::fs::write(&temp, serde_json::to_vec_pretty(&value)?)?;
        std::fs::rename(temp, path)?;
        Ok(value)
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
