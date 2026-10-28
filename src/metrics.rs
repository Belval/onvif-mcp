//! Bounded monotonic request traces plus rolling stage statistics.
//! Labels are static; request arguments, XML, endpoints, credentials and pixels are never logged.
use rmcp::model::{CallToolResponse, RequestId};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    future::Future,
    path::PathBuf,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

const MAX_SPANS: usize = 8192;
const MAX_TRACES: usize = 64;
const MAX_RPC: usize = 128;
const WINDOW: usize = 256;
static SERIAL: AtomicU64 = AtomicU64::new(1);
static LOG_DROPPED: AtomicU64 = AtomicU64::new(0);
static LOG_ERRORS: AtomicU64 = AtomicU64::new(0);
static ENABLED: OnceLock<bool> = OnceLock::new();
enum LogItem {
    Trace(Arc<Trace>),
    Flush(mpsc::SyncSender<()>),
}
static LOGGER: OnceLock<mpsc::SyncSender<LogItem>> = OnceLock::new();
pub fn enabled() -> bool {
    *ENABLED.get_or_init(|| {
        std::env::var("CAMERA_METRICS").is_ok_and(|v| v != "off" && v != "0")
            || std::env::var("CAMERA_METRICS").is_err()
    })
}
fn ns(d: Duration) -> u64 {
    d.as_nanos().min(u64::MAX as u128) as u64
}
fn ms(n: u64) -> f64 {
    n as f64 / 1_000_000.
}
#[derive(Clone, Default)]
struct Stat {
    count: u64,
    total: u64,
    min: u64,
    max: u64,
    errors: u64,
    cancelled: u64,
    samples: VecDeque<u64>,
}
impl Stat {
    fn add(&mut self, value: u64, outcome: &str) {
        self.count += 1;
        self.total = self.total.saturating_add(value);
        self.min = if self.count == 1 {
            value
        } else {
            self.min.min(value)
        };
        self.max = self.max.max(value);
        self.errors += u64::from(outcome == "error" || outcome == "panic");
        self.cancelled += u64::from(outcome == "cancelled");
        if self.samples.len() == WINDOW {
            self.samples.pop_front();
        }
        self.samples.push_back(value);
    }
    fn json(&self) -> Value {
        let mut values: Vec<_> = self.samples.iter().copied().collect();
        values.sort_unstable();
        let percentile = |q: f64| {
            values
                .get(((values.len().saturating_sub(1)) as f64 * q).ceil() as usize)
                .copied()
                .map(ms)
        };
        json!({"count":self.count,"total_ms":ms(self.total),"min_ms":ms(self.min),"max_ms":ms(self.max),"mean_ms":if self.count>0{ms(self.total)/self.count as f64}else{0.},"p50_ms":percentile(0.5),"p95_ms":percentile(0.95),"p99_ms":percentile(0.99),"percentile_window":values.len(),"errors":self.errors,"cancelled":self.cancelled})
    }
}
#[derive(Clone, Serialize)]
pub struct Record {
    pub id: u32,
    pub parent: Option<u32>,
    pub stage: &'static str,
    pub start_ns: u64,
    pub duration_ns: u64,
    pub outcome: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub values: Option<Value>,
}
struct Data {
    ingress_ready: Instant,
    wire_complete: bool,
    next: u32,
    records: Vec<Record>,
    dropped: u64,
    handler_done: Option<Instant>,
    wire_done: Option<Instant>,
    outcome: &'static str,
    response_bytes: u64,
}
pub struct Trace {
    pub id: String,
    pub kind: &'static str,
    pub tool: &'static str,
    pub started: Instant,
    pub wire_observed: bool,
    data: Mutex<Data>,
}
#[derive(Clone)]
pub struct Context {
    trace: Arc<Trace>,
    parent: Option<u32>,
}
tokio::task_local! {static CURRENT:Context;}
#[derive(Default)]
struct Registry {
    stages: BTreeMap<&'static str, Stat>,
    counters: BTreeMap<&'static str, u64>,
    requests: VecDeque<Arc<Trace>>,
    background: VecDeque<Arc<Trace>>,
    rpc: HashMap<String, Arc<Trace>>,
}
fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Registry::default()))
}
fn tool_name(name: &str) -> &'static str {
    match name {
        "camera_see" => "camera_see",
        "camera_focus" => "camera_focus",
        "camera_scan" => "camera_scan",
        "camera_status" => "camera_status",
        "camera_stop" => "camera_stop",
        "camera_move" => "camera_move",
        "camera_look" => "camera_look",
        "camera_remember" => "camera_remember",
        "camera_metrics" => "camera_metrics",
        _ => "unknown_tool",
    }
}
fn make_trace(kind: &'static str, tool: &'static str, started: Instant, wire: bool) -> Arc<Trace> {
    static EPOCH: OnceLock<u64> = OnceLock::new();
    let trace = Arc::new(Trace {
        id: format!(
            "{:016x}-{:x}",
            EPOCH.get_or_init(rand::random),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ),
        kind,
        tool,
        started,
        wire_observed: wire,
        data: Mutex::new(Data {
            ingress_ready: started,
            wire_complete: false,
            next: 1,
            records: vec![],
            dropped: 0,
            handler_done: None,
            wire_done: None,
            outcome: "running",
            response_bytes: 0,
        }),
    });
    let mut r = registry().lock().unwrap();
    let queue = if kind == "request" {
        &mut r.requests
    } else {
        &mut r.background
    };
    if queue.len() == MAX_TRACES {
        queue.pop_front();
    }
    queue.push_back(trace.clone());
    trace
}
pub fn observe(stage: &'static str, duration: Duration) {
    if enabled() {
        record_stat(stage, ns(duration), "ok");
    }
}
fn record_stat(stage: &'static str, value: u64, outcome: &str) {
    registry()
        .lock()
        .unwrap()
        .stages
        .entry(stage)
        .or_default()
        .add(value, outcome);
}
pub fn counter(name: &'static str, value: u64) {
    if enabled() {
        let mut r = registry().lock().unwrap();
        let n = r.counters.entry(name).or_default();
        *n = n.saturating_add(value);
    }
}
pub fn capture() -> Option<Context> {
    if enabled() {
        CURRENT.try_with(Clone::clone).ok()
    } else {
        None
    }
}
pub fn with_context<T>(context: Option<Context>, work: impl FnOnce() -> T) -> T {
    if let Some(context) = context {
        CURRENT.sync_scope(context, work)
    } else {
        work()
    }
}
pub async fn with_context_async<T>(context: Option<Context>, work: impl Future<Output = T>) -> T {
    if let Some(context) = context {
        CURRENT.scope(context, work).await
    } else {
        work.await
    }
}

struct Span {
    name: &'static str,
    started: Instant,
    context: Option<Context>,
    id: u32,
    ended: bool,
}
impl Span {
    fn new(name: &'static str) -> Self {
        let context = capture();
        let id = context
            .as_ref()
            .map(|c| {
                let mut d = c.trace.data.lock().unwrap();
                let id = d.next;
                d.next += 1;
                id
            })
            .unwrap_or(0);
        Self {
            name,
            started: Instant::now(),
            context,
            id,
            ended: false,
        }
    }
    fn child(&self) -> Option<Context> {
        self.context.as_ref().map(|c| Context {
            trace: c.trace.clone(),
            parent: Some(self.id),
        })
    }
    fn finish(&mut self, outcome: &'static str) {
        if self.ended {
            return;
        }
        self.ended = true;
        let duration = ns(self.started.elapsed());
        record_stat(self.name, duration, outcome);
        if let Some(c) = &self.context {
            push_record(
                &c.trace,
                Record {
                    id: self.id,
                    parent: c.parent,
                    stage: self.name,
                    start_ns: ns(self.started.saturating_duration_since(c.trace.started)),
                    duration_ns: duration,
                    outcome,
                    values: None,
                },
            );
        }
    }
}
impl Drop for Span {
    fn drop(&mut self) {
        if enabled() {
            self.finish(if std::thread::panicking() {
                "panic"
            } else {
                "cancelled"
            });
        }
    }
}
fn push_record(trace: &Trace, record: Record) {
    let mut d = trace.data.lock().unwrap();
    if d.wire_done.is_some() {
        return;
    }
    if d.records.len() < MAX_SPANS {
        d.records.push(record);
    } else {
        d.dropped += 1;
    }
}
pub async fn timed<T>(name: &'static str, work: impl Future<Output = T>) -> T {
    if !enabled() {
        return work.await;
    }
    let mut span = Span::new(name);
    let result = with_context_async(span.child(), work).await;
    span.finish("ok");
    result
}
pub async fn result<T, E>(
    name: &'static str,
    work: impl Future<Output = Result<T, E>>,
) -> Result<T, E> {
    if !enabled() {
        return work.await;
    }
    let mut span = Span::new(name);
    let result = with_context_async(span.child(), work).await;
    span.finish(if result.is_err() { "error" } else { "ok" });
    result
}
pub fn sync<T>(name: &'static str, work: impl FnOnce() -> T) -> T {
    if !enabled() {
        return work();
    }
    let mut span = Span::new(name);
    let result = with_context(span.child(), work);
    span.finish("ok");
    result
}
pub fn sync_result<T, E>(name: &'static str, work: impl FnOnce() -> Result<T, E>) -> Result<T, E> {
    if !enabled() {
        return work();
    }
    let mut span = Span::new(name);
    let result = with_context(span.child(), work);
    span.finish(if result.is_err() { "error" } else { "ok" });
    result
}
pub fn event(name: &'static str, values: Value) {
    if let Some(c) = capture() {
        let id = {
            let mut d = c.trace.data.lock().unwrap();
            let id = d.next;
            d.next += 1;
            id
        };
        push_record(
            &c.trace,
            Record {
                id,
                parent: c.parent,
                stage: name,
                start_ns: ns(c.trace.started.elapsed()),
                duration_ns: 0,
                outcome: "event",
                values: Some(values),
            },
        );
    }
}
pub async fn sleep(name: &'static str, duration: Duration) {
    timed(name, async {
        if !duration.is_zero() {
            tokio::time::sleep(duration).await;
        }
    })
    .await
}
pub async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, tokio::task::JoinError> {
    result("runtime.blocking_wait", async {
        let context = capture();
        let submitted = Instant::now();
        tokio::task::spawn_blocking(move || {
            with_context(context, || {
                interval("runtime.blocking_queue", submitted, Instant::now());
                sync("runtime.blocking_execute", work)
            })
        })
        .await
    })
    .await
}
fn interval(name: &'static str, start: Instant, end: Instant) {
    if !enabled() {
        return;
    }
    let duration = ns(end.saturating_duration_since(start));
    record_stat(name, duration, "ok");
    if let Some(c) = capture() {
        let id = {
            let mut d = c.trace.data.lock().unwrap();
            let id = d.next;
            d.next += 1;
            id
        };
        push_record(
            &c.trace,
            Record {
                id,
                parent: c.parent,
                stage: name,
                start_ns: ns(start.saturating_duration_since(c.trace.started)),
                duration_ns: duration,
                outcome: "ok",
                values: None,
            },
        );
    }
}

fn rpc_key(id: &RequestId) -> String {
    serde_json::to_string(id).unwrap_or_default()
}
pub fn inbound(line: &[u8], started: Instant, received: Instant) {
    if !enabled() {
        return;
    }
    counter("transport.request_bytes", line.len() as u64);
    let Ok(message) = serde_json::from_slice::<Value>(line) else {
        counter("transport.unparsed_messages", 1);
        return;
    };
    if message["method"] != "tools/call" {
        return;
    }
    let Some(id) = message.get("id") else {
        return;
    };
    let key = id.to_string();
    if key.len() > 256 {
        return;
    }
    let tool = tool_name(message["params"]["name"].as_str().unwrap_or(""));
    let trace = make_trace("request", tool, started, true);
    let ready = Instant::now();
    trace.data.lock().unwrap().ingress_ready = ready;
    with_context(
        Some(Context {
            trace: trace.clone(),
            parent: None,
        }),
        || {
            interval("transport.request_read", started, received);
            interval("transport.inspect_request", received, ready);
        },
    );
    let mut r = registry().lock().unwrap();
    if r.rpc.len() < MAX_RPC {
        r.rpc.insert(key, trace);
    } else {
        *r.counters.entry("transport.rpc_overflow").or_default() += 1;
    }
}
#[cfg(test)]
pub fn trace_for_response(key: &str) -> Option<Arc<Trace>> {
    registry().lock().unwrap().rpc.get(key).cloned()
}
fn complete(trace: &Arc<Trace>, wire_done: Option<Instant>, bytes: u64) {
    {
        let mut d = trace.data.lock().unwrap();
        if d.wire_done.is_some() {
            return;
        }
        d.response_bytes = bytes;
        d.wire_complete = wire_done.is_some();
        d.wire_done = Some(wire_done.unwrap_or_else(Instant::now));
    }
    if let Some(logger) = LOGGER.get()
        && logger.try_send(LogItem::Trace(trace.clone())).is_err()
    {
        LOG_DROPPED.fetch_add(1, Ordering::Relaxed);
    }
}
pub fn outbound_finished(key: &str, start: Instant, end: Instant, bytes: u64) {
    if !enabled() {
        return;
    }
    let trace = registry().lock().unwrap().rpc.remove(key);
    if let Some(trace) = trace {
        let handler_done = {
            let mut d = trace.data.lock().unwrap();
            if d.handler_done.is_none() {
                d.outcome = "protocol_error";
            }
            d.handler_done
        };
        if let Some(done) = handler_done {
            with_context(
                Some(Context {
                    trace: trace.clone(),
                    parent: None,
                }),
                || {
                    interval("transport.response_prepare", done, start);
                    interval("transport.response_write", start, end);
                },
            );
        }
        observe(
            "transport.request_wire",
            end.saturating_duration_since(trace.started),
        );
        complete(&trace, Some(end), bytes);
    }
}
struct RequestGuard {
    trace: Arc<Trace>,
    key: String,
    armed: bool,
}
impl Drop for RequestGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        {
            let mut d = self.trace.data.lock().unwrap();
            d.handler_done = Some(Instant::now());
            d.outcome = if std::thread::panicking() {
                "panic"
            } else {
                "cancelled"
            };
        }
        {
            let mut r = registry().lock().unwrap();
            if r.rpc
                .get(&self.key)
                .is_some_and(|t| Arc::ptr_eq(t, &self.trace))
            {
                r.rpc.remove(&self.key);
            }
        }
        complete(&self.trace, None, 0);
    }
}
pub async fn request(
    id: &RequestId,
    tool: &str,
    work: impl Future<Output = Result<CallToolResponse, rmcp::ErrorData>>,
) -> Result<CallToolResponse, rmcp::ErrorData> {
    if !enabled() {
        return work.await;
    }
    let key = rpc_key(id);
    let existing = registry().lock().unwrap().rpc.get(&key).cloned();
    let trace =
        existing.unwrap_or_else(|| make_trace("request", tool_name(tool), Instant::now(), false));
    let mut guard = RequestGuard {
        trace: trace.clone(),
        key,
        armed: true,
    };
    let context = Context {
        trace: trace.clone(),
        parent: None,
    };
    let handler_start = Instant::now();
    let ingress_ready = trace.data.lock().unwrap().ingress_ready;
    with_context(Some(context.clone()), || {
        interval("server.dispatch", ingress_ready, handler_start)
    });
    let mut output =
        with_context_async(Some(context.clone()), result("server.handler", work)).await;
    let ok = output
        .as_ref()
        .is_ok_and(|r| !matches!(r,CallToolResponse::Complete(c) if c.is_error==Some(true)));
    {
        let mut d = trace.data.lock().unwrap();
        d.handler_done = Some(Instant::now());
        d.outcome = if ok { "ok" } else { "error" };
    }
    if let Ok(CallToolResponse::Complete(result)) = &mut output {
        with_context(Some(context), || {
            sync("metrics.response_metadata", || {
                result.meta.get_or_insert_with(Default::default).insert("camera_metrics".into(),json!({"request_id":trace.id,"handler_ms":handler_start.elapsed().as_secs_f64()*1000.,"wire_timing_available_after_response":trace.wire_observed,"details_tool":"camera_metrics"}));
            })
        });
    }
    guard.armed = false;
    if !trace.wire_observed {
        complete(&trace, None, 0);
    }
    output
}
pub async fn background<T, E>(
    name: &'static str,
    source_id: u64,
    work: impl Future<Output = Result<T, E>>,
) -> Result<T, E> {
    if !enabled() {
        return work.await;
    }
    let trace = make_trace("background", name, Instant::now(), false);
    let context = Context {
        trace: trace.clone(),
        parent: None,
    };
    let output = with_context_async(Some(context), async {
        if source_id != 0 {
            event("video.source", json!({"source_id":source_id}));
        }
        result(name, work).await
    })
    .await;
    {
        let mut d = trace.data.lock().unwrap();
        d.handler_done = Some(Instant::now());
        d.outcome = if output.is_ok() { "ok" } else { "error" };
    }
    complete(&trace, None, 0);
    output
}
fn union_length(mut ranges: Vec<(u64, u64)>) -> u64 {
    ranges.sort_unstable();
    let mut total = 0;
    let mut current: Option<(u64, u64)> = None;
    for (a, b) in ranges {
        if let Some((start, end)) = current {
            if a <= end {
                current = Some((start, end.max(b)));
            } else {
                total += end - start;
                current = Some((a, b));
            }
        } else {
            current = Some((a, b));
        }
    }
    if let Some((a, b)) = current {
        total += b - a;
    }
    total
}
fn trace_json(trace: &Trace, full: bool, end: Option<Instant>) -> Value {
    let d = trace.data.lock().unwrap();
    let end = end
        .or(d.wire_done)
        .or(d.handler_done)
        .unwrap_or_else(Instant::now);
    let elapsed = ns(end.saturating_duration_since(trace.started));
    let mut grouped: BTreeMap<&str, (u64, u64, u64)> = BTreeMap::new();
    let mut children_by_parent: HashMap<u32, Vec<&Record>> = HashMap::new();
    for record in &d.records {
        if let Some(parent) = record.parent {
            children_by_parent.entry(parent).or_default().push(record);
        }
    }
    for span in &d.records {
        if span.outcome == "event" {
            continue;
        }
        let a = span.start_ns.min(elapsed);
        let b = (span.start_ns + span.duration_ns).min(elapsed);
        let children: Vec<_> = children_by_parent
            .get(&span.id)
            .into_iter()
            .flatten()
            .filter(|child| child.duration_ns > 0)
            .map(|child| {
                (
                    child.start_ns.max(a).min(b),
                    (child.start_ns + child.duration_ns).min(b).max(a),
                )
            })
            .filter(|(x, y)| y >= x)
            .collect();
        let exclusive = (b - a).saturating_sub(union_length(children));
        let entry = grouped.entry(span.stage).or_default();
        entry.0 += 1;
        entry.1 += b - a;
        entry.2 += exclusive;
    }
    let intervals = d
        .records
        .iter()
        .filter(|r| r.duration_ns > 0)
        .map(|r| {
            (
                r.start_ns.min(elapsed),
                (r.start_ns + r.duration_ns).min(elapsed),
            )
        })
        .collect();
    let covered = union_length(intervals);
    let stages:Vec<_>=grouped.into_iter().map(|(stage,(count,inclusive,exclusive))|json!({"stage":stage,"count":count,"inclusive_ms":ms(inclusive),"exclusive_ms":ms(exclusive)})).collect();
    let mut value = json!({"request_id":trace.id,"kind":trace.kind,"tool":trace.tool,"clock":"host_monotonic","wire_observed":trace.wire_observed,"response_complete":d.wire_complete,"trace_complete":d.wire_done.is_some(),"outcome":d.outcome,"elapsed_ms":ms(elapsed),"handler_ms":d.handler_done.map(|t|ms(ns(t.saturating_duration_since(trace.started)))),"response_bytes":d.response_bytes,"uncovered_ms":ms(elapsed.saturating_sub(covered)),"dropped_spans":d.dropped,"sensor_capture_latency_measured":false,"stages":stages});
    if full {
        value["spans"] = serde_json::to_value(&d.records).unwrap_or(Value::Null);
    }
    value
}
pub fn snapshot(request_id: Option<&str>, full: bool, limit: usize) -> Value {
    let (stats, counters, requests, background) = {
        let r = registry().lock().unwrap();
        (
            r.stages.clone(),
            r.counters.clone(),
            r.requests.iter().cloned().collect::<Vec<_>>(),
            r.background.iter().cloned().collect::<Vec<_>>(),
        )
    };
    let wanted = request_id.and_then(|id| {
        requests
            .iter()
            .chain(background.iter())
            .find(|t| t.id == id)
    });
    if request_id.is_some() {
        return wanted
            .map(|t| trace_json(t, full, None))
            .unwrap_or_else(|| json!({"error":"Trace not retained in this server process"}));
    }
    json!({"enabled":enabled(),"clock":"host_monotonic","percentiles":"most recent 256 samples per stage; totals are since process start","log_dropped":LOG_DROPPED.load(Ordering::Relaxed),"log_errors":LOG_ERRORS.load(Ordering::Relaxed),"retained_requests":requests.len(),"max_spans_per_trace":MAX_SPANS,"durations":stats.into_iter().map(|(k,s)|(k.to_owned(),s.json())).collect::<BTreeMap<_,_>>(),"counters":counters,"recent_requests":requests.iter().rev().take(limit.min(MAX_TRACES)).map(|t|trace_json(t,false,None)).collect::<Vec<_>>(),"recent_background":background.iter().rev().take(limit.min(8)).map(|t|trace_json(t,false,None)).collect::<Vec<_>>()})
}
pub fn init() {
    if !enabled() {
        return;
    }
    let Ok(path) = std::env::var("CAMERA_METRICS_FILE") else {
        return;
    };
    let path = PathBuf::from(path.replace("{pid}", &std::process::id().to_string()));
    let (tx, rx) = mpsc::sync_channel::<LogItem>(32);
    if LOGGER.set(tx).is_err() {
        return;
    }
    std::thread::spawn(move || {
        use std::io::Write;
        let open = |path: &PathBuf| {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut options = std::fs::OpenOptions::new();
            options.create(true).append(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            options.open(path)
        };
        let Ok(mut file) = open(&path) else {
            LOG_ERRORS.fetch_add(1, Ordering::Relaxed);
            return;
        };
        while let Ok(item) = rx.recv() {
            let trace = match item {
                LogItem::Trace(trace) => trace,
                LogItem::Flush(ack) => {
                    if file.flush().is_err() {
                        LOG_ERRORS.fetch_add(1, Ordering::Relaxed);
                    }
                    let _ = ack.try_send(());
                    continue;
                }
            };
            let value = trace_json(&trace, true, None);
            if file
                .metadata()
                .map(|m| m.len() > 16 * 1024 * 1024)
                .unwrap_or(false)
            {
                let backup = path.with_extension("jsonl.1");
                let _ = std::fs::rename(&path, backup);
                match open(&path) {
                    Ok(next) => file = next,
                    Err(_) => {
                        LOG_ERRORS.fetch_add(1, Ordering::Relaxed);
                        break;
                    }
                }
            }
            let started = Instant::now();
            let mut bytes = match serde_json::to_vec(&value) {
                Ok(v) => v,
                Err(_) => {
                    LOG_ERRORS.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
            };
            bytes.push(b'\n');
            observe("metrics.log_encode", started.elapsed());
            let started = Instant::now();
            if file.write_all(&bytes).is_err() {
                LOG_ERRORS.fetch_add(1, Ordering::Relaxed);
            }
            observe("metrics.log_write", started.elapsed());
        }
    });
}

#[derive(Clone)]
pub struct ConnectorLayer;
#[derive(Clone)]
pub struct Connector<S> {
    inner: S,
}
impl<S> tower::Layer<S> for ConnectorLayer {
    type Service = Connector<S>;
    fn layer(&self, inner: S) -> Self::Service {
        Connector { inner }
    }
}
impl<S, Req> tower::Service<Req> for Connector<S>
where
    S: tower::Service<Req>,
    S::Future: Send + 'static,
    S::Response: Send + 'static,
    S::Error: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future =
        std::pin::Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;
    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }
    fn call(&mut self, req: Req) -> Self::Future {
        let context = capture();
        let future = self.inner.call(req);
        Box::pin(async move {
            with_context_async(context, result("onvif.connection_setup", future)).await
        })
    }
}

/// Bounded shutdown drain, outside the request path.
pub fn flush() {
    if let Some(logger) = LOGGER.get() {
        let (tx, rx) = mpsc::sync_channel(1);
        if logger.try_send(LogItem::Flush(tx)).is_ok() {
            let _ = rx.recv_timeout(Duration::from_millis(500));
        } else {
            LOG_DROPPED.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unions_overlapping_intervals_without_double_counting() {
        assert_eq!(union_length(vec![(0, 10), (5, 20), (25, 30)]), 25);
    }
    #[tokio::test]
    async fn nested_and_blocking_spans_keep_their_parent() {
        let trace = make_trace("request", "test", Instant::now(), false);
        with_context_async(
            Some(Context {
                trace: trace.clone(),
                parent: None,
            }),
            timed("test.parent", async {
                timed("test.child", tokio::time::sleep(Duration::from_millis(2))).await;
                blocking(|| sync("test.cpu", || 42)).await.unwrap();
            }),
        )
        .await;
        let d = trace.data.lock().unwrap();
        let parent = d.records.iter().find(|s| s.stage == "test.parent").unwrap();
        assert!(
            d.records
                .iter()
                .any(|s| s.stage == "test.child" && s.parent == Some(parent.id))
        );
        assert!(d.records.iter().any(|s| s.stage == "test.cpu"));
    }
    #[tokio::test]
    async fn cancellation_is_recorded() {
        let trace = make_trace("request", "cancel", Instant::now(), false);
        let copy = trace.clone();
        let task = tokio::spawn(with_context_async(
            Some(Context {
                trace: copy,
                parent: None,
            }),
            timed("test.cancel", std::future::pending::<()>()),
        ));
        tokio::time::sleep(Duration::from_millis(5)).await;
        task.abort();
        let _ = task.await;
        assert!(
            trace
                .data
                .lock()
                .unwrap()
                .records
                .iter()
                .any(|s| s.stage == "test.cancel" && s.outcome == "cancelled")
        );
    }
    #[tokio::test]
    async fn tool_errors_and_cancelled_requests_finalize_truthfully() {
        let failed = request(&RequestId::Number(912345), "camera_see", async {
            Err(rmcp::ErrorData::internal_error("failure", None))
        })
        .await;
        assert!(failed.is_err());
        let trace = registry()
            .lock()
            .unwrap()
            .requests
            .iter()
            .rev()
            .find(|t| t.tool == "camera_see" && !t.wire_observed)
            .unwrap()
            .clone();
        assert_eq!(trace_json(&trace, true, None)["outcome"], "error");
        let before = SERIAL.load(Ordering::Relaxed);
        let task = tokio::spawn(async {
            request(
                &RequestId::Number(912346),
                "cancel_test",
                std::future::pending(),
            )
            .await
        });
        while SERIAL.load(Ordering::Relaxed) == before {
            tokio::task::yield_now().await;
        }
        // Wait for the guard to be polled before cancelling it.
        tokio::time::sleep(Duration::from_millis(2)).await;
        task.abort();
        let _ = task.await;
        let trace = registry()
            .lock()
            .unwrap()
            .requests
            .iter()
            .rev()
            .find(|t| t.tool == "unknown_tool")
            .unwrap()
            .clone();
        let value = trace_json(&trace, true, None);
        assert_eq!(value["outcome"], "cancelled");
        assert_eq!(value["trace_complete"], true);
        assert_eq!(value["response_complete"], false);
    }
    #[test]
    fn exclusive_time_subtracts_overlapping_children_once() {
        let start = Instant::now();
        let trace = make_trace("request", "accounting", start, false);
        for (id, parent, stage, a, duration) in [
            (1, None, "parent", 0, 100),
            (2, Some(1), "a", 10, 60),
            (3, Some(1), "b", 40, 40),
        ] {
            push_record(
                &trace,
                Record {
                    id,
                    parent,
                    stage,
                    start_ns: a,
                    duration_ns: duration,
                    outcome: "ok",
                    values: None,
                },
            );
        }
        let v = trace_json(&trace, true, Some(start + Duration::from_nanos(100)));
        let parent = v["stages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["stage"] == "parent")
            .unwrap();
        assert!((parent["exclusive_ms"].as_f64().unwrap() - 0.000030).abs() < 1e-9);
        assert_eq!(v["uncovered_ms"], 0.0);
    }
    #[test]
    fn request_arguments_and_external_ids_are_not_exported() {
        let message=br#"{"id":"secret-rpc-id","method":"tools/call","params":{"name":"camera_see","arguments":{"password":"secret-credential"}}}"#;
        inbound(message, Instant::now(), Instant::now());
        let trace = trace_for_response("\"secret-rpc-id\"").unwrap();
        let value = trace_json(&trace, true, None).to_string();
        assert!(!value.contains("secret-rpc-id"));
        assert!(!value.contains("secret-credential"));
    }
}
