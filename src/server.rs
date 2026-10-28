use crate::camera::{Camera, ObservationResult};
use anyhow::Result;
use base64::{Engine, engine::general_purpose::STANDARD};
use rmcp::{
    ServerHandler,
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock, ServerCapabilities, ServerConfig},
    tool, tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{sync::Arc, time::Instant};

#[derive(Clone)]
pub struct CameraServer {
    pub camera: Arc<Camera>,
}
#[derive(Deserialize, JsonSchema)]
pub struct SeeArgs {
    #[serde(default)]
    pub detail: bool,
}
#[derive(Deserialize, JsonSchema)]
pub struct FocusArgs {
    pub x: f64,
    pub y: f64,
    pub frame_id: String,
    /// Default false: up to two corrections to 4% of image center. True: up to three, 2%.
    #[serde(default)]
    pub precise: bool,
}
#[derive(Deserialize, JsonSchema)]
pub struct MoveArgs {
    #[serde(default)]
    pub pan: f64,
    #[serde(default)]
    pub tilt: f64,
    #[serde(default = "default_seconds")]
    pub seconds: f64,
}
fn default_seconds() -> f64 {
    0.3
}
#[derive(Deserialize, JsonSchema)]
pub struct ScanArgs {
    /// Image-space direction: left or right.
    pub direction: String,
    #[serde(default = "default_views")]
    pub views: usize,
    /// Absolute ONVIF step; default .25, approximately 40 degrees on this camera.
    #[serde(default = "default_step")]
    pub step: f64,
}
fn default_views() -> usize {
    4
}
fn default_step() -> f64 {
    0.25
}
#[derive(Deserialize, JsonSchema)]
pub struct NameArgs {
    pub name: String,
}

fn failure(error: anyhow::Error) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(error.to_string())])
}
fn plain(result: Result<Value>) -> CallToolResult {
    match result {
        Ok(value) => CallToolResult::success(vec![ContentBlock::text(value.to_string())]),
        Err(e) => failure(e),
    }
}
fn content((mut metadata, frame): ObservationResult, started: Instant) -> Vec<ContentBlock> {
    metadata["frame_age_ms"] = json!(frame.received.elapsed().as_secs_f64() * 1000.);
    metadata["elapsed_ms"] = json!(started.elapsed().as_secs_f64() * 1000.);
    vec![
        ContentBlock::text(metadata.to_string()),
        ContentBlock::image(STANDARD.encode(frame.jpeg.as_slice()), "image/jpeg"),
    ]
}
fn picture(result: Result<ObservationResult>, started: Instant) -> CallToolResult {
    match result {
        Ok(observation) => CallToolResult::success(content(observation, started)),
        Err(e) => failure(e),
    }
}
#[tool_router]
impl CameraServer {
    #[tool(
        description = "See a fresh 640x360 camera preview from a persistent native JPEG stream. Returns frame_id, dimensions and frame age. detail=true starts an H264 detail connection and returns up to 1280px. Use camera_focus for centering, camera_scan for searching, and camera_look for remembered objects."
    )]
    async fn camera_see(&self, Parameters(args): Parameters<SeeArgs>) -> CallToolResult {
        let started = Instant::now();
        let _lock = self.camera.motion.lock().await;
        picture(
            self.camera
                .observe(
                    args.detail,
                    json!({"detail":if args.detail{"full"}else{"preview"}}),
                    false,
                )
                .await,
            started,
        )
    }
    #[tool(
        description = "Center a point from a camera image and return the new view in one call. x,y are normalized 0..1 from top-left; center=.5,.5. Supply frame_id from camera_see or any image in camera_scan. Scan views can be revisited automatically. Default is fast framing; precise=true allows an extra correction. Check focus_status: centered, tracking_lost, mechanical_limit, iteration_limit, stale_frame. This aims the camera, not the lens."
    )]
    async fn camera_focus(&self, Parameters(args): Parameters<FocusArgs>) -> CallToolResult {
        let started = Instant::now();
        let _lock = self.camera.motion.lock().await;
        picture(
            self.camera
                .focus(args.x, args.y, &args.frame_id, args.precise)
                .await,
            started,
        )
    }
    #[tool(
        description = "Search several overlapping camera views in ONE call. direction=left/right, views=2..6 (default 4), step=.05.. .35 (default .25, about 40 degrees). Returns an image and frame_id per view, starting at the current view. Choose a target in any returned image and call camera_focus with its frame_id; it automatically returns there. Prefer this over repeated velocity moves when searching."
    )]
    async fn camera_scan(&self, Parameters(args): Parameters<ScanArgs>) -> CallToolResult {
        let started = Instant::now();
        let _lock = self.camera.motion.lock().await;
        match self
            .camera
            .scan(&args.direction, args.views, args.step)
            .await
        {
            Ok(observations) => CallToolResult::success(
                observations
                    .into_iter()
                    .flat_map(|o| content(o, started))
                    .collect(),
            ),
            Err(e) => failure(e),
        }
    }
    #[tool(
        description = "Read camera model, profiles, PTZ position, remembered views, and persistent video connection metrics."
    )]
    async fn camera_status(&self) -> CallToolResult {
        plain(self.camera.status().await)
    }
    #[tool(
        description = "Raw pan/tilt velocity -1..1, seconds >0 and <=2. Positive right/up. Stops automatically. Prefer camera_focus to frame an image point or camera_scan to search several views."
    )]
    async fn camera_move(&self, Parameters(args): Parameters<MoveArgs>) -> CallToolResult {
        let _lock = self.camera.motion.lock().await;
        plain(
            self.camera
                .movement(args.pan, args.tilt, args.seconds)
                .await,
        )
    }
    #[tool(
        description = "Immediately stop movement, including an ongoing scan or focus. Bypasses the motion lock."
    )]
    async fn camera_stop(&self) -> CallToolResult {
        plain(self.camera.stop().await.map(|_| json!({"stopped":true})))
    }
    #[tool(
        description = "Remember the current camera position under a short name such as printer, clock or microphone. Saved locally across sessions. Frame the object first; later camera_look(name) returns directly without scanning."
    )]
    async fn camera_remember(&self, Parameters(args): Parameters<NameArgs>) -> CallToolResult {
        let _lock = self.camera.motion.lock().await;
        plain(self.camera.remember(&args.name).await)
    }
    #[tool(
        description = "Look directly at a previously remembered named view and return a fresh image, without searching. Use camera_status to list names and camera_remember to save the current view."
    )]
    async fn camera_look(&self, Parameters(args): Parameters<NameArgs>) -> CallToolResult {
        let started = Instant::now();
        let _lock = self.camera.motion.lock().await;
        picture(self.camera.look(&args.name).await, started)
    }
}
#[tool_handler]
impl ServerHandler for CameraServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "Use camera_see for a quick current view; camera_scan batches a search into one call; camera_focus frames an object from any returned image. Save stationary objects with camera_remember and use camera_look to avoid future searches. Coordinates are normalized from image top-left. Frames come from a persistent, latest-only native JPEG buffer; never infer success if focus_status reports otherwise.")
    }
}
