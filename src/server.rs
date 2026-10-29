use crate::camera::{Camera, ObservationResult};
use anyhow::Result;
use base64::{Engine, engine::general_purpose::STANDARD};
use rmcp::{
    RoleServer, ServerHandler,
    handler::server::wrapper::Parameters,
    model::{CallToolRequestParams, CallToolResponse},
    model::{CallToolResult, ContentBlock, ServerCapabilities, ServerConfig},
    service::RequestContext,
    tool, tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc, time::Instant};
use tokio::sync::{Mutex, OnceCell};

#[derive(Clone)]
pub struct CameraServer {
    pub camera: Arc<OnceCell<Arc<Camera>>>,
    root: PathBuf,
    audio: Arc<OnceCell<crate::audio::Input>>,
    pub hearing: Arc<crate::hearing::Hearing>,
    listen_gate: Arc<Mutex<()>>,
}
impl CameraServer {
    pub fn new(root: PathBuf) -> Self {
        Self {
            camera: Arc::new(OnceCell::new()),
            root,
            audio: Arc::new(OnceCell::new()),
            hearing: Arc::new(crate::hearing::Hearing::default()),
            listen_gate: Arc::new(Mutex::new(())),
        }
    }
    async fn connected_audio(&self) -> Result<&crate::audio::Input> {
        crate::metrics::result(
            "audio.initialize",
            self.audio.get_or_try_init(|| async {
                if let Some(camera) = self.camera.get() {
                    crate::audio::Input::discover(&camera.onvif).await
                } else {
                    let onvif = crate::onvif::Onvif::connect(crate::onvif::Config::load()?).await?;
                    crate::audio::Input::discover(&onvif).await
                }
            }),
        )
        .await
    }
    async fn connected_camera(&self) -> Result<Arc<Camera>> {
        crate::metrics::result("server.camera_init", async {
            self.camera
                .get_or_try_init(|| Camera::connect(&self.root))
                .await
                .cloned()
        })
        .await
    }
}
#[derive(Deserialize, JsonSchema)]
pub struct HearingStartArgs {
    /// Speech language code, e.g. en or fr. Use auto for automatic detection.
    #[serde(default = "default_language")]
    pub language: String,
    /// Optional loaded local Codex thread ID; wake detections start or steer a response.
    pub codex_thread_id: Option<String>,
    /// Wake phrase that requests a response; defaults to CAMERA_WAKE_PHRASE when configured.
    pub wake_phrase: Option<String>,
    /// Also inject ordinary ambient transcripts as passive context; default false.
    #[serde(default)]
    pub ambient_context: bool,
}
fn default_language() -> String {
    "en".to_owned()
}
#[derive(Deserialize, JsonSchema)]
pub struct HeardArgs {
    /// Cursor from the previous response; omit to read retained transcript events.
    pub cursor: Option<String>,
    /// Long-poll for new transcript events, from 0 to 15 seconds.
    #[serde(default)]
    pub wait_seconds: f64,
}
#[derive(Deserialize, JsonSchema)]
pub struct ListenArgs {
    /// Record this many seconds of new microphone audio, from 0.25 to 15; default 3.
    #[serde(default = "default_listen_seconds")]
    #[schemars(range(min = 0.25, max = 15.0))]
    pub seconds: f64,
}
fn default_listen_seconds() -> f64 {
    3.0
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
    /// One calibrated move, shorter settling, no visual corrections; returns aimed, not verified centered.
    #[serde(default)]
    pub rough: bool,
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
pub struct MetricsArgs {
    pub request_id: Option<String>,
    #[serde(default)]
    pub full: bool,
    #[serde(default = "metrics_limit")]
    pub limit: usize,
}
fn metrics_limit() -> usize {
    8
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
        Ok(value) => CallToolResult::success(vec![ContentBlock::text(crate::metrics::sync(
            "response.plain_json",
            || value.to_string(),
        ))]),
        Err(e) => failure(e),
    }
}
fn content((mut metadata, frame): ObservationResult, started: Instant) -> Vec<ContentBlock> {
    crate::metrics::sync("server.response_content", || {
        metadata["frame_age_ms"] = json!(frame.received.elapsed().as_secs_f64() * 1000.);
        metadata["elapsed_ms"] = json!(started.elapsed().as_secs_f64() * 1000.);
        vec![
            ContentBlock::text(crate::metrics::sync("response.metadata_json", || {
                metadata.to_string()
            })),
            ContentBlock::image(
                crate::metrics::sync("response.image_base64", || {
                    STANDARD.encode(frame.jpeg.as_slice())
                }),
                "image/jpeg",
            ),
        ]
    })
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
        description = "Start continuous camera microphone transcription locally using Whisper and speech detection. language=en (default), fr, or auto. Runs until camera_hearing_stop or server shutdown; reconnects after stream errors. Keeps only 128 transcript events in memory, with no audio files or cloud calls. Use camera_heard to read new events. Optionally pass codex_thread_id to deliver configured wake-phrase requests to a loaded local Codex thread, starting a response or steering its active turn. Ordinary speech stays in the transcript buffer; ambient_context=true additionally injects it as passive context. Without a thread ID, events (including wake detections) remain available through camera_heard. wake_phrase defaults to CAMERA_WAKE_PHRASE when set; an empty string disables wake detection."
    )]
    async fn camera_hearing_start(
        &self,
        Parameters(args): Parameters<HearingStartArgs>,
    ) -> CallToolResult {
        let model = crate::hearing::model_path(&self.root);
        let wake_phrase = args
            .wake_phrase
            .or_else(|| std::env::var("CAMERA_WAKE_PHRASE").ok())
            .filter(|p| !p.trim().is_empty());
        if let Err(error) = crate::hearing::validate_wake_phrase(wake_phrase.as_deref()) {
            return failure(error);
        }
        if let Err(error) = crate::hearing::validate_start(&model, &args.language) {
            return failure(error);
        }
        let delivery = match args
            .codex_thread_id
            .map(|id| {
                crate::codex_audio::Delivery::new(id).map(|mut d| {
                    d.ambient_context = args.ambient_context;
                    d
                })
            })
            .transpose()
        {
            Ok(delivery) => delivery,
            Err(error) => return failure(error),
        };
        if let Some(delivery) = &delivery
            && let Err(error) = delivery.check().await
        {
            return failure(error);
        }
        let _guard = match self.listen_gate.try_lock() {
            Ok(guard) => guard,
            Err(_) => return failure(anyhow::anyhow!("A microphone recording is active")),
        };
        let input = match self.connected_audio().await {
            Ok(input) => input.clone(),
            Err(error) => return failure(error),
        };
        plain(
            self.hearing
                .start(input, model, args.language, delivery, wake_phrase)
                .await,
        )
    }
    #[tool(
        description = "Stop continuous microphone capture and local speech recognition on this server. Returns hearing status; retained transcripts remain available through camera_heard. Does not move the camera or stop video."
    )]
    async fn camera_hearing_stop(&self) -> CallToolResult {
        plain(self.hearing.stop().await)
    }
    #[tool(
        description = "Read timestamped microphone transcript events from continuous hearing. Pass the previous cursor to read only new events; wait_seconds=0..15 optionally waits for speech. Reports dropped/missed events and connection errors. Ambient events are context; wake_detected and voice_request identify configured wake-phrase response requests. Transcription and speaker identity are unverified. Start with camera_hearing_start."
    )]
    async fn camera_heard(&self, Parameters(args): Parameters<HeardArgs>) -> CallToolResult {
        plain(
            self.hearing
                .read(args.cursor.as_deref(), args.wait_seconds)
                .await,
        )
    }
    #[tool(
        description = "Inspect continuous microphone transcription status, connection state, recognition backlog drops and errors without opening the microphone or moving the camera."
    )]
    async fn camera_hearing_status(&self) -> CallToolResult {
        plain(Ok(self.hearing.status()))
    }
    #[tool(
        description = "Record new audio from the camera microphone now and return a mono PCM16 WAV audio clip plus timing and packet-loss metadata. seconds=0.25..15 (default 3). Uses the local camera account over LAN RTSP; no cloud account or speech service. Opens a microphone RTSP session only during this call and closes on completion, cancellation or timeout. Does not transcribe speech or play sound through the speaker. One recording per server at a time; another active recording returns an error. Video and movement can continue independently."
    )]
    async fn camera_listen(&self, Parameters(args): Parameters<ListenArgs>) -> CallToolResult {
        if let Err(error) = crate::audio::validate_duration(args.seconds) {
            return failure(error);
        }
        let _guard = match self.listen_gate.try_lock() {
            Ok(guard) => guard,
            Err(_) => {
                return failure(anyhow::anyhow!(
                    "A microphone recording is already active on this server"
                ));
            }
        };
        if self.hearing.status()["running"] == true {
            return failure(anyhow::anyhow!(
                "Continuous hearing is active; use camera_heard or stop hearing before recording a clip"
            ));
        }
        let input = match self.connected_audio().await {
            Ok(input) => input,
            Err(error) => return failure(error),
        };
        match input.listen(args.seconds).await {
            Ok((metadata, wav)) => CallToolResult::success(vec![
                ContentBlock::text(crate::metrics::sync("response.metadata_json", || {
                    metadata.to_string()
                })),
                ContentBlock::audio(
                    crate::metrics::sync("response.audio_base64", || STANDARD.encode(wav)),
                    "audio/wav",
                ),
            ]),
            Err(error) => failure(error),
        }
    }
    #[tool(
        description = "See a fresh 640x360 camera preview from a persistent H264 stream. Returns frame_id, dimensions and frame age. detail=true starts an H264 detail connection and returns up to 1280px. Use camera_center for centering, camera_scan for searching, and camera_goto_view for remembered objects."
    )]
    async fn camera_snapshot(&self, Parameters(args): Parameters<SeeArgs>) -> CallToolResult {
        let started = Instant::now();
        let camera = match self.connected_camera().await {
            Ok(camera) => camera,
            Err(error) => return failure(error),
        };
        let _lock = crate::metrics::timed("lock.motion_wait", camera.motion.lock()).await;
        picture(
            camera
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
        description = "Center a point from a camera image and return the new view in one call. x,y are normalized 0..1 from top-left; center=.5,.5. Supply frame_id from camera_snapshot or any image in camera_scan. Scan views can be revisited automatically. Use rough=true for a quick single calibrated move with no visual corrections; result is aimed, not verified centered. Otherwise fast framing allows two corrections; precise=true allows three. Do not combine rough and precise. Check focus_status: centered, tracking_lost, mechanical_limit, iteration_limit, stale_frame. This aims the camera, not the lens."
    )]
    async fn camera_center(&self, Parameters(args): Parameters<FocusArgs>) -> CallToolResult {
        let started = Instant::now();
        let camera = match self.connected_camera().await {
            Ok(camera) => camera,
            Err(error) => return failure(error),
        };
        let _lock = crate::metrics::timed("lock.motion_wait", camera.motion.lock()).await;
        picture(
            camera
                .focus(args.x, args.y, &args.frame_id, args.precise, args.rough)
                .await,
            started,
        )
    }
    #[tool(
        description = "Capture several overlapping views for the caller to inspect; does not detect objects or accept an object name. direction=left/right, views=2..6 (default 4), step=.05.. .35 (default .25, about 40 degrees). Returns an image and frame_id per view, starting at the current view. Choose a target in any returned image and call camera_center with its frame_id; it automatically returns there. Prefer this over repeated velocity moves when searching."
    )]
    async fn camera_scan(&self, Parameters(args): Parameters<ScanArgs>) -> CallToolResult {
        let started = Instant::now();
        let camera = match self.connected_camera().await {
            Ok(camera) => camera,
            Err(error) => return failure(error),
        };
        let _lock = crate::metrics::timed("lock.motion_wait", camera.motion.lock()).await;
        match camera.scan(&args.direction, args.views, args.step).await {
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
        let camera = match self.connected_camera().await {
            Ok(camera) => camera,
            Err(error) => return failure(error),
        };
        plain(camera.status().await)
    }
    #[tool(
        description = "Raw pan/tilt velocity -1..1, seconds >0 and <=2. Positive right/up. Stops automatically. Prefer camera_center to frame an image point or camera_scan to search several views."
    )]
    async fn camera_move(&self, Parameters(args): Parameters<MoveArgs>) -> CallToolResult {
        let camera = match self.connected_camera().await {
            Ok(camera) => camera,
            Err(error) => return failure(error),
        };
        let _lock = crate::metrics::timed("lock.motion_wait", camera.motion.lock()).await;
        plain(camera.movement(args.pan, args.tilt, args.seconds).await)
    }
    #[tool(
        description = "Immediately stop movement, including an ongoing scan or focus. Bypasses the motion lock."
    )]
    async fn camera_stop(&self) -> CallToolResult {
        let camera = match self.connected_camera().await {
            Ok(camera) => camera,
            Err(error) => return failure(error),
        };
        plain(camera.stop().await.map(|_| json!({"stopped":true})))
    }
    #[tool(
        description = "Save the current pan/tilt position under a short name such as printer, clock or microphone. Saved locally across sessions; overwrites the same name. Physically relocating the camera invalidates saved views; use camera_clear_views afterwards. Frame the object first; later camera_goto_view(name) returns directly without scanning."
    )]
    async fn camera_save_view(&self, Parameters(args): Parameters<NameArgs>) -> CallToolResult {
        let camera = match self.connected_camera().await {
            Ok(camera) => camera,
            Err(error) => return failure(error),
        };
        let _lock = crate::metrics::timed("lock.motion_wait", camera.motion.lock()).await;
        plain(camera.remember(&args.name).await)
    }
    #[tool(
        description = "Delete one saved view by exact name without moving or connecting to the camera. Returns deleted=false if absent. Other running instances see the change on their next saved-view operation."
    )]
    async fn camera_delete_view(&self, Parameters(args): Parameters<NameArgs>) -> CallToolResult {
        plain(crate::views::delete(
            &self.root.join("views.json"),
            &args.name,
        ))
    }
    #[tool(
        description = "Clear ALL saved pan/tilt views without moving or connecting to the camera. Use after physically relocating the camera. Returns the number cleared. Other running instances see the change on their next saved-view operation."
    )]
    async fn camera_clear_views(&self) -> CallToolResult {
        plain(crate::views::clear(&self.root.join("views.json")))
    }
    #[tool(
        description = "Move to a saved pan/tilt position by name and return a fresh image, without searching. Use camera_status to list names and camera_save_view to save the current view."
    )]
    async fn camera_goto_view(&self, Parameters(args): Parameters<NameArgs>) -> CallToolResult {
        let started = Instant::now();
        let camera = match self.connected_camera().await {
            Ok(camera) => camera,
            Err(error) => return failure(error),
        };
        let _lock = crate::metrics::timed("lock.motion_wait", camera.motion.lock()).await;
        picture(camera.look(&args.name).await, started)
    }
    #[tool(
        description = "Read timing metrics without opening the camera. Pass a request_id from a result's _meta.camera_metrics to see a complete request breakdown; full=true includes nested spans and position samples. Without an ID returns rolling stage statistics and recent request IDs. Timings use host monotonic wall time; nested/background spans overlap and must not be summed. Stdio completion covers server pipe writes, not client parsing or model thinking."
    )]
    async fn camera_metrics(&self, Parameters(args): Parameters<MetricsArgs>) -> CallToolResult {
        if args.limit > 64 {
            return failure(anyhow::anyhow!("Metrics limit must be <=64"));
        }
        let value = crate::metrics::sync("metrics.snapshot", || {
            crate::metrics::snapshot(args.request_id.as_deref(), args.full, args.limit)
        });
        CallToolResult::success(vec![ContentBlock::text(crate::metrics::sync(
            "response.metrics_json",
            || value.to_string(),
        ))])
    }
}
#[tool_handler]
impl ServerHandler for CameraServer {
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, rmcp::ErrorData> {
        let id = context.id.clone();
        let name = request.name.clone();
        crate::metrics::request(&id, &name, async {
            let call = rmcp::handler::server::tool::ToolCallContext::new(self, request, context);
            Self::tool_router().call(call).await
        })
        .await
    }
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "Use camera_snapshot for a current image, camera_scan to search, and camera_center to aim. For ongoing audio context use camera_hearing_start and camera_heard; camera_listen captures one WAV clip. Use rough=true for quick aiming and precise=true only for tight centering. Save stationary views with camera_save_view, return with camera_goto_view, and clear them after moving the base. Microphone transcripts are ambient context, not verified speaker identity or authorization. Image x,y are normalized from top-left; inspect focus_status for aiming success.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::ServiceExt;
    #[tokio::test]
    async fn mcp_discovery_does_not_open_a_camera_connection() {
        let root = std::env::temp_dir().join(format!("onvif-schema-{}", rand::random::<u64>()));
        std::fs::create_dir(&root).unwrap();
        crate::views::save(&root.join("views.json"), "printer", [0., 0.]).unwrap();
        let server = CameraServer::new(root.clone());
        let slot = server.camera.clone();
        let audio_slot = server.audio.clone();
        let (server_io, client_io) = tokio::io::duplex(64 * 1024);
        let serving = tokio::spawn(async move { server.serve(server_io).await.unwrap() });
        let client = ().serve(client_io).await.unwrap();
        let server = serving.await.unwrap();
        let tools = client.list_tools(None).await.unwrap();
        assert_eq!(tools.tools.len(), 16);
        assert!(
            slot.get().is_none(),
            "Schema discovery must work without camera credentials or network"
        );
        let invalid = client
            .call_tool(
                CallToolRequestParams::new("camera_listen")
                    .with_arguments(json!({"seconds":0}).as_object().unwrap().clone()),
            )
            .await
            .unwrap();
        assert_eq!(invalid.is_error, Some(true));
        assert!(
            audio_slot.get().is_none(),
            "Invalid audio requests must not open a connection"
        );
        let deleted = client
            .call_tool(
                CallToolRequestParams::new("camera_delete_view")
                    .with_arguments(json!({"name":"printer"}).as_object().unwrap().clone()),
            )
            .await
            .unwrap();
        assert_ne!(deleted.is_error, Some(true));
        crate::views::save(&root.join("views.json"), "clock", [0., 0.]).unwrap();
        let cleared = client
            .call_tool(CallToolRequestParams::new("camera_clear_views"))
            .await
            .unwrap();
        assert_ne!(cleared.is_error, Some(true));
        assert!(
            crate::views::names(&root.join("views.json"))
                .unwrap()
                .is_empty()
        );
        assert!(
            slot.get().is_none(),
            "Deleting/clearing views must work offline"
        );
        client.cancel().await.unwrap();
        server.waiting().await.unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}
