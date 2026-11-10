use crate::integrations::ObsCmd;
use crate::model::Event;
use crate::store::Store;
use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, Mutex};
use tokio_tungstenite::tungstenite::Message;

/// obs-websocket v5 client. Connects to the configured OBS, authenticates
/// (sha256 challenge), and executes ObsCmd requests. Reconnects on error.
pub fn spawn(
    store: Arc<Mutex<Store>>,
    bus: broadcast::Sender<Event>,
    mut rx: mpsc::Receiver<ObsCmd>,
    status: Arc<std::sync::atomic::AtomicBool>,
    want: Arc<std::sync::atomic::AtomicBool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let cfg = { store.lock().await.settings.obs.clone() };
            if !want.load(std::sync::atomic::Ordering::Relaxed) || cfg.address.trim().is_empty() {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                continue;
            }
            if let Err(e) = run(&cfg, &bus, &mut rx, &status, &want).await {
                tracing::error!("obs: {e:#}");
            }
            status.store(false, std::sync::atomic::Ordering::Relaxed);
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    })
}

/// Build a one-shot op6 request with no response handling.
fn simple(id: &str, request_type: &str, data: serde_json::Value) -> serde_json::Value {
    serde_json::json!({ "op": 6, "d": { "requestType": request_type, "requestId": id, "requestData": data } })
}

fn obs_auth(password: &str, salt: &str, challenge: &str) -> String {
    let b64 = base64::engine::general_purpose::STANDARD;
    let secret = b64.encode(Sha256::digest(format!("{password}{salt}").as_bytes()));
    b64.encode(Sha256::digest(format!("{secret}{challenge}").as_bytes()))
}

async fn run(
    cfg: &crate::model::ObsConfig,
    bus: &broadcast::Sender<Event>,
    rx: &mut mpsc::Receiver<ObsCmd>,
    status: &std::sync::atomic::AtomicBool,
    want: &std::sync::atomic::AtomicBool,
) -> anyhow::Result<()> {
    let url = format!("ws://{}:{}", cfg.address.trim(), cfg.port);
    let (ws, _) = tokio_tungstenite::connect_async(&url).await?;
    let (mut write, mut read) = ws.split();

    // Hello (op 0) → Identify (op 1)
    let hello = read
        .next()
        .await
        .ok_or_else(|| anyhow::anyhow!("obs closed before hello"))??;
    let hello: serde_json::Value = serde_json::from_str(&hello.into_text()?)?;
    let mut identify = serde_json::json!({ "op": 1, "d": { "rpcVersion": 1 } });
    if let Some(auth) = hello["d"]["authentication"].as_object() {
        let salt = auth.get("salt").and_then(|v| v.as_str()).unwrap_or("");
        let challenge = auth.get("challenge").and_then(|v| v.as_str()).unwrap_or("");
        identify["d"]["authentication"] =
            serde_json::Value::String(obs_auth(&cfg.password, salt, challenge));
    }
    write.send(Message::Text(identify.to_string())).await?;

    // Wait for Identified (op 2)
    loop {
        let msg = read
            .next()
            .await
            .ok_or_else(|| anyhow::anyhow!("obs closed before identified"))??;
        let v: serde_json::Value = serde_json::from_str(&msg.into_text()?)?;
        if v["op"] == 2 {
            break;
        }
    }
    let _ = bus.send(Event::System { text: "obs: connected".into() });
    status.store(true, std::sync::atomic::Ordering::Relaxed);

    let mut req_id: u64 = 0;
    // requestId -> (sceneName, desired visible) for a pending GetSceneItemId lookup.
    let mut pending: HashMap<String, (String, bool)> = HashMap::new();
    let mut want_tick = tokio::time::interval(std::time::Duration::from_secs(2));
    want_tick.tick().await;
    loop {
        tokio::select! {
            _ = want_tick.tick() => {
                if !want.load(std::sync::atomic::Ordering::Relaxed) { break; }
            }
            incoming = read.next() => {
                let Some(Ok(msg)) = incoming else { break };
                let Ok(text) = msg.into_text() else { continue };
                let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
                // RequestResponse (op 7): if it answers a pending GetSceneItemId, apply visibility.
                if v["op"] == 7 {
                    let rid = v["d"]["requestId"].as_str().unwrap_or("").to_string();
                    if let Some((scene, visible)) = pending.remove(&rid) {
                        if let Some(item_id) = v["d"]["responseData"]["sceneItemId"].as_i64() {
                            req_id += 1;
                            let set = serde_json::json!({ "op": 6, "d": {
                                "requestType": "SetSceneItemEnabled", "requestId": req_id.to_string(),
                                "requestData": { "sceneName": scene, "sceneItemId": item_id, "sceneItemEnabled": visible } } });
                            write.send(Message::Text(set.to_string())).await?;
                        }
                    }
                }
            }
            cmd = rx.recv() => {
                let Some(cmd) = cmd else { break };
                req_id += 1;
                let id = req_id.to_string();
                let req = match cmd {
                    ObsCmd::Scene(scene) => serde_json::json!({
                        "op": 6, "d": { "requestType": "SetCurrentProgramScene", "requestId": id,
                            "requestData": { "sceneName": scene } }
                    }),
                    ObsCmd::RecordToggle => serde_json::json!({
                        "op": 6, "d": { "requestType": "ToggleRecord", "requestId": id, "requestData": {} }
                    }),
                    ObsCmd::Source { scene, source, visible } => {
                        // Step 1: resolve the scene item id; step 2 happens on the response.
                        pending.insert(id.clone(), (scene.clone(), visible));
                        serde_json::json!({ "op": 6, "d": {
                            "requestType": "GetSceneItemId", "requestId": id,
                            "requestData": { "sceneName": scene, "sourceName": source } } })
                    }
                    ObsCmd::Filter { source, filter, enabled } => simple(&id, "SetSourceFilterEnabled",
                        serde_json::json!({ "sourceName": source, "filterName": filter, "filterEnabled": enabled })),
                    ObsCmd::ReplayStart => simple(&id, "StartReplayBuffer", serde_json::json!({})),
                    ObsCmd::ReplayStop => simple(&id, "StopReplayBuffer", serde_json::json!({})),
                    ObsCmd::ReplaySave => simple(&id, "SaveReplayBuffer", serde_json::json!({})),
                    ObsCmd::StreamStart => simple(&id, "StartStream", serde_json::json!({})),
                    ObsCmd::StreamStop => simple(&id, "StopStream", serde_json::json!({})),
                    ObsCmd::StreamToggle => simple(&id, "ToggleStream", serde_json::json!({})),
                };
                write.send(Message::Text(req.to_string())).await?;
            }
        }
    }

    Ok(())
}
