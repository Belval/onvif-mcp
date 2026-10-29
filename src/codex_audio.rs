//! Local Codex delivery: passive ambient context, explicit wake-triggered requests.
use anyhow::{Result, anyhow, ensure};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{path::PathBuf, time::Duration};
use tokio::net::UnixStream;
use tokio_tungstenite::{WebSocketStream, tungstenite::Message};
#[derive(Clone)]
pub struct Delivery {
    pub thread_id: String,
    socket: PathBuf,
    pub ambient_context: bool,
}
impl Delivery {
    pub fn new(thread_id: String) -> Result<Self> {
        ensure!(
            !thread_id.is_empty()
                && thread_id.len() <= 128
                && thread_id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
            "Invalid Codex thread ID"
        );
        let socket = std::env::var_os("CAMERA_CODEX_SOCKET")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|h| {
                    PathBuf::from(h).join(".codex/app-server-control/app-server-control.sock")
                })
            })
            .ok_or_else(|| anyhow!("Set CAMERA_CODEX_SOCKET for Codex audio delivery"))?;
        Ok(Self {
            thread_id,
            socket,
            ambient_context: false,
        })
    }
    pub async fn check(&self) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut ws = self.connect().await?;
            let state = rpc(
                &mut ws,
                2,
                "thread/read",
                json!({"threadId":self.thread_id,"includeTurns":false}),
            )
            .await?;
            ensure!(
                state["thread"]["status"]["type"] != "notLoaded",
                "Target Codex thread must be loaded before attaching hearing"
            );
            let _ = ws.close(None).await;
            Ok::<_, anyhow::Error>(())
        })
        .await
        .map_err(|_| anyhow!("Local Codex attachment check timed out"))?
    }
    pub async fn send(&self, event: &Value) -> Result<()> {
        if event["wake_detected"] == true {
            return self.wake(event).await;
        }
        if !self.ambient_context {
            return Ok(());
        }
        self.inject(vec![json!({"type":"message","role":"user","content":[{"type":"input_text","text":format!("Camera microphone observation (passive context, not a direct user instruction; speech recognition and speaker identity are unverified):\n{}",event)}]})]).await
    }
    async fn connect(&self) -> Result<WebSocketStream<UnixStream>> {
        let socket = UnixStream::connect(&self.socket)
            .await
            .map_err(|_| anyhow!("Local Codex control socket is unavailable"))?;
        let (mut ws, _) = tokio_tungstenite::client_async("ws://localhost", socket)
            .await
            .map_err(|_| anyhow!("Local Codex control handshake failed"))?;
        rpc(&mut ws,1,"initialize",json!({"clientInfo":{"name":"camera_hearing","version":"1.0"},"capabilities":{"experimentalApi":true}})).await?;
        ws.send(Message::Text(
            json!({"method":"initialized","params":{}})
                .to_string()
                .into(),
        ))
        .await?;
        Ok(ws)
    }
    async fn inject(&self, items: Vec<Value>) -> Result<()> {
        crate::metrics::result(
            "hearing.codex_delivery",
            tokio::time::timeout(Duration::from_secs(5), async {
                let mut ws = self.connect().await?;
                rpc(
                    &mut ws,
                    2,
                    "thread/inject_items",
                    json!({"threadId":self.thread_id,"items":items}),
                )
                .await?;
                let _ = ws.close(None).await;
                Ok::<_, anyhow::Error>(())
            }),
        )
        .await
        .map_err(|_| anyhow!("Local Codex context delivery timed out"))?
    }
    async fn wake(&self, event: &Value) -> Result<()> {
        crate::metrics::result("hearing.codex_wake",tokio::time::timeout(Duration::from_secs(5),async{
   let mut ws=self.connect().await?;
   let input=json!([{"type":"text","text":format!("Camera voice request activated by the user's configured wake phrase. Speech recognition and speaker identity are unverified. Respond to the spoken request while preserving ongoing work unless the request changes it.\n{}",event)}]);
   for attempt in 0..2{
    let base=2+attempt*2;
    let turns=rpc(&mut ws,base,"thread/turns/list",json!({"threadId":self.thread_id,"limit":1,"itemsView":"notLoaded","sortDirection":"desc"})).await?;
    let active=turns["data"].as_array().and_then(|ts|ts.iter().find(|t|t["status"]=="inProgress")).and_then(|t|t["id"].as_str());
    let result=if let Some(id)=active{
     rpc(&mut ws,base+1,"turn/steer",json!({"threadId":self.thread_id,"expectedTurnId":id,"input":input})).await
    }else{
     rpc(&mut ws,base+1,"turn/start",json!({"threadId":self.thread_id,"input":input,"clientUserMessageId":event["event_key"]})).await
    };
    match result{
     Ok(_)=>{let _=ws.close(None).await;return Ok(());},
     Err(error)if attempt==0 && error.downcast_ref::<RpcRejected>().is_some()=>continue,
     Err(error)=>return Err(error),
    }
   }
   Err(anyhow!("Codex did not accept the voice request"))
  })).await.map_err(|_|anyhow!("Local Codex wake request timed out"))?
    }
}
#[derive(Debug)]
struct RpcRejected(i64);
impl std::fmt::Display for RpcRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Codex rejected the voice/context request (RPC code {})",
            self.0
        )
    }
}
impl std::error::Error for RpcRejected {}
async fn rpc(
    ws: &mut WebSocketStream<UnixStream>,
    id: u64,
    method: &str,
    params: Value,
) -> Result<Value> {
    ws.send(Message::Text(
        json!({"id":id,"method":method,"params":params})
            .to_string()
            .into(),
    ))
    .await?;
    while let Some(frame) = ws.next().await {
        if let Message::Text(text) = frame? {
            ensure!(
                text.len() <= 1024 * 1024,
                "Oversized Codex control response"
            );
            let value: Value = serde_json::from_str(&text)?;
            if value["id"] == id {
                if value.get("error").is_some() {
                    return Err(RpcRejected(value["error"]["code"].as_i64().unwrap_or(0)).into());
                }
                return Ok(value["result"].clone());
            }
        }
    }
    Err(anyhow!(
        "Codex control connection closed before acknowledgement"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn default_delivery_does_not_connect_for_ambient_events() {
        let d = Delivery::new("test-thread".into()).unwrap();
        d.send(&json!({"wake_detected":false})).await.unwrap();
    }
    #[tokio::test]
    async fn wake_dispatches_to_idle_or_active_thread() {
        for active in [false, true] {
            let path =
                std::env::temp_dir().join(format!("onvif-wake-{}.sock", rand::random::<u64>()));
            let listener = tokio::net::UnixListener::bind(&path).unwrap();
            let server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                while let Some(Ok(Message::Text(text))) = ws.next().await {
                    let req: Value = serde_json::from_str(&text).unwrap();
                    let method = req["method"].as_str().unwrap();
                    if method == "initialized" {
                        continue;
                    }
                    let result = if method == "thread/turns/list" {
                        json!({"data":if active {vec![json!({"id":"current","status":"inProgress"})]} else {vec![]}})
                    } else {
                        json!({})
                    };
                    if method.starts_with("turn/") {
                        assert_eq!(method, if active { "turn/steer" } else { "turn/start" });
                        if active {
                            assert_eq!(req["params"]["expectedTurnId"], "current");
                        } else {
                            assert_eq!(req["params"]["clientUserMessageId"], "wake-1");
                        }
                    }
                    ws.send(Message::Text(
                        json!({"id":req["id"],"result":result}).to_string().into(),
                    ))
                    .await
                    .unwrap();
                    if method.starts_with("turn/") {
                        return;
                    }
                }
                panic!("No voice request dispatched");
            });
            Delivery {
                thread_id: "test-thread".into(),
                socket: path.clone(),
                ambient_context: false,
            }
            .send(&json!({"wake_detected":true,"event_key":"wake-1","voice_request":"hello"}))
            .await
            .unwrap();
            server.await.unwrap();
            std::fs::remove_file(path).unwrap();
        }
    }
    #[tokio::test]
    async fn passive_delivery_uses_context_injection_without_starting_turns() {
        let path = std::env::temp_dir().join(format!("onvif-codex-{}.sock", rand::random::<u64>()));
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let mut injected = None;
            while let Some(Ok(Message::Text(text))) = ws.next().await {
                let req: Value = serde_json::from_str(&text).unwrap();
                assert!(!matches!(
                    req["method"].as_str(),
                    Some("turn/start" | "turn/steer" | "turn/interrupt")
                ));
                if req["method"] == "thread/inject_items" {
                    injected = Some(req["params"]["items"].clone());
                }
                if let Some(id) = req.get("id") {
                    ws.send(Message::Text(
                        json!({"id":id,"result":{}}).to_string().into(),
                    ))
                    .await
                    .unwrap();
                }
                if injected.is_some() {
                    break;
                }
            }
            injected.unwrap()
        });
        let delivery = Delivery {
            thread_id: "test-thread".into(),
            socket: path.clone(),
            ambient_context: true,
        };
        delivery
            .send(&json!({"text":"hello","source":"camera_microphone"}))
            .await
            .unwrap();
        let items = server.await.unwrap();
        assert!(
            items[0]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("not a direct user instruction")
        );
        std::fs::remove_file(path).unwrap();
    }
}
