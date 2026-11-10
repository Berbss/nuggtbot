use crate::integrations::ThrowCmd;
use crate::model::Event;
use crate::store::Store;
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, Mutex};
use tokio_tungstenite::tungstenite::Message;

/// Throwing System (T.I.T.S.) client — ws:42069, TITS Public API.
/// Protocol confirmed from Streamer.bot.ThrowingSystem.dll (apiName/apiVersion/
/// requestID/messageType/itemId). Sends TITSThrowItemsRequest for each item.
pub fn spawn(
    store: Arc<Mutex<Store>>,
    bus: broadcast::Sender<Event>,
    mut rx: mpsc::Receiver<ThrowCmd>,
    status: Arc<std::sync::atomic::AtomicBool>,
    want: Arc<std::sync::atomic::AtomicBool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let cfg = { store.lock().await.settings.throwing.clone() };
            if !want.load(std::sync::atomic::Ordering::Relaxed) || cfg.address.trim().is_empty() {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                continue;
            }
            if let Err(e) = run(&cfg, &bus, &mut rx, &status, &want).await {
                tracing::error!("throwing: {e:#}");
            }
            status.store(false, std::sync::atomic::Ordering::Relaxed);
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    })
}

async fn run(
    cfg: &crate::model::ThrowingConfig,
    bus: &broadcast::Sender<Event>,
    rx: &mut mpsc::Receiver<ThrowCmd>,
    status: &std::sync::atomic::AtomicBool,
    want: &std::sync::atomic::AtomicBool,
) -> anyhow::Result<()> {
    let url = format!("ws://{}:{}/websocket", cfg.address.trim(), cfg.port);
    let (ws, _) = tokio_tungstenite::connect_async(&url).await?;
    let (mut write, mut read) = ws.split();
    tracing::info!("throwing: connected to {url}");
    let _ = bus.send(Event::System { text: "throwing: connected".into() });
    status.store(true, std::sync::atomic::Ordering::Relaxed);

    // Ask TITS for its triggers and items so we can resolve a throw by name.
    // Triggers are named throw effects (what the user set up); items are raw assets.
    let ask = |t: &str| serde_json::json!({
        "apiName": "TITSPublicApi", "apiVersion": "1.0", "requestID": "nuggtbot", "messageType": t, "data": {}
    }).to_string();
    write.send(Message::Text(ask("TITSTriggerListRequest"))).await?;
    write.send(Message::Text(ask("TITSItemListRequest"))).await?;
    // lowercase name -> id
    let mut triggers: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut items: std::collections::HashMap<String, String> = std::collections::HashMap::new();

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
                match v.get("messageType").and_then(|x| x.as_str()) {
                    Some("TITSTriggerListResponse") => {
                        if let Some(arr) = v["data"]["triggers"].as_array() {
                            for t in arr {
                                let (Some(id), Some(name)) = (t.get("ID").and_then(|x| x.as_str()), t.get("name").and_then(|x| x.as_str())) else { continue };
                                triggers.insert(name.trim().to_lowercase(), id.to_string());
                            }
                            tracing::info!("throwing: {} triggers loaded", triggers.len());
                        }
                    }
                    Some("TITSItemListResponse") => {
                        if let Some(arr) = v["data"]["items"].as_array() {
                            for i in arr {
                                let (Some(id), Some(name)) = (i.get("ID").and_then(|x| x.as_str()), i.get("name").and_then(|x| x.as_str())) else { continue };
                                items.insert(name.trim().to_lowercase(), id.to_string());
                            }
                            tracing::info!("throwing: {} items loaded", items.len());
                        }
                    }
                    _ => {}
                }
            }
            cmd = rx.recv() => {
                let Some(ThrowCmd::Throw(name)) = cmd else { break };
                let key = name.trim().to_lowercase();
                let req = if let Some(id) = triggers.get(&key) {
                    tracing::info!("throwing: activating trigger '{name}'");
                    serde_json::json!({ "apiName": "TITSPublicApi", "apiVersion": "1.0", "requestID": "nuggtbot",
                        "messageType": "TITSTriggerActivateRequest", "data": { "triggerID": id } })
                } else {
                    // Fall back to throwing an item: resolve its id, else send the raw text.
                    let item_id = items.get(&key).cloned().unwrap_or_else(|| name.clone());
                    tracing::info!("throwing: throwing item '{name}' (id {item_id})");
                    serde_json::json!({ "apiName": "TITSPublicApi", "apiVersion": "1.0", "requestID": "nuggtbot",
                        "messageType": "TITSThrowItemsRequest",
                        "data": { "items": [item_id], "delayTime": 0.05, "amountOfThrows": 1, "errorOnMissingID": false } })
                };
                write.send(Message::Text(req.to_string())).await?;
            }
        }
    }

    Ok(())
}
