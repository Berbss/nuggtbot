use crate::integrations::VtsCmd;
use crate::model::Event;
use crate::store::Store;
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, Mutex};
use tokio_tungstenite::tungstenite::Message;

const PLUGIN: &str = "nuggtbot";

/// VTube Studio API client (ws:8001). Requests a plugin token on first run
/// (VTS shows an approval popup), saves it, authenticates, then triggers hotkeys.
pub fn spawn(
    store: Arc<Mutex<Store>>,
    bus: broadcast::Sender<Event>,
    mut rx: mpsc::Receiver<VtsCmd>,
    status: Arc<std::sync::atomic::AtomicBool>,
    want: Arc<std::sync::atomic::AtomicBool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let cfg = { store.lock().await.settings.vts.clone() };
            if !want.load(std::sync::atomic::Ordering::Relaxed) || cfg.address.trim().is_empty() {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                continue;
            }
            if let Err(e) = run(&store, &bus, &mut rx, &status, &want).await {
                tracing::error!("vts: {e:#}");
            }
            status.store(false, std::sync::atomic::Ordering::Relaxed);
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    })
}

fn req(message_type: &str, data: serde_json::Value) -> String {
    serde_json::json!({
        "apiName": "VTubeStudioPublicAPI",
        "apiVersion": "1.0",
        "requestID": "nuggtbot",
        "messageType": message_type,
        "data": data,
    })
    .to_string()
}

async fn read_json<S>(read: &mut S) -> anyhow::Result<serde_json::Value>
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    loop {
        let msg = read
            .next()
            .await
            .ok_or_else(|| anyhow::anyhow!("vts closed"))??;
        if let Message::Text(t) = msg {
            return Ok(serde_json::from_str(&t)?);
        }
    }
}

async fn run(
    store: &Arc<Mutex<Store>>,
    bus: &broadcast::Sender<Event>,
    rx: &mut mpsc::Receiver<VtsCmd>,
    status: &std::sync::atomic::AtomicBool,
    want: &std::sync::atomic::AtomicBool,
) -> anyhow::Result<()> {
    let cfg = { store.lock().await.settings.vts.clone() };
    let url = format!("ws://{}:{}", cfg.address.trim(), cfg.port);
    let (ws, _) = tokio_tungstenite::connect_async(&url).await?;
    let (mut write, mut read) = ws.split();

    // Get a token if we don't have one (user approves in VTS).
    let mut token = cfg.token.clone();
    if token.trim().is_empty() {
        write
            .send(Message::Text(req(
                "AuthenticationTokenRequest",
                serde_json::json!({ "pluginName": PLUGIN, "pluginDeveloper": PLUGIN }),
            )))
            .await?;
        let _ = bus.send(Event::System {
            text: "vts: approve the plugin in VTube Studio".into(),
        });
        loop {
            let v = read_json(&mut read).await?;
            if v["messageType"] == "AuthenticationTokenResponse" {
                token = v["data"]["authenticationToken"].as_str().unwrap_or("").to_string();
                let mut s = store.lock().await;
                s.settings.vts.token = token.clone();
                let _ = s.save();
                break;
            }
        }
    }

    // Authenticate with the token.
    write
        .send(Message::Text(req(
            "AuthenticationRequest",
            serde_json::json!({ "pluginName": PLUGIN, "pluginDeveloper": PLUGIN, "authenticationToken": token }),
        )))
        .await?;
    loop {
        let v = read_json(&mut read).await?;
        if v["messageType"] == "AuthenticationResponse" {
            if v["data"]["authenticated"].as_bool() == Some(true) {
                let _ = bus.send(Event::System { text: "vts: connected".into() });
                status.store(true, std::sync::atomic::Ordering::Relaxed);
                break;
            } else {
                anyhow::bail!("vts auth failed");
            }
        }
    }

    let mut want_tick = tokio::time::interval(std::time::Duration::from_secs(2));
    want_tick.tick().await;
    loop {
        tokio::select! {
            _ = want_tick.tick() => {
                if !want.load(std::sync::atomic::Ordering::Relaxed) { break; }
            }
            incoming = read.next() => {
                match incoming { Some(Ok(_)) => {}, _ => break }
            }
            cmd = rx.recv() => {
                let Some(VtsCmd::Hotkey(h)) = cmd else { break };
                write.send(Message::Text(req(
                    "HotkeyTriggerRequest",
                    serde_json::json!({ "hotkeyID": h }),
                ))).await?;
            }
        }
    }

    Ok(())
}
