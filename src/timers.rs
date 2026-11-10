use crate::model::Outgoing;
use crate::store::Store;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{mpsc, Mutex};

/// Timed Actions: send each timer's text to the Twitch channel every interval_secs.
/// Reads timers live from the store so edits take effect without restart.
pub fn spawn(
    store: Arc<Mutex<Store>>,
    outgoing: mpsc::UnboundedSender<Outgoing>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut last: HashMap<String, Instant> = HashMap::new();
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            let (timers, channel) = {
                let s = store.lock().await;
                (s.timers.clone(), s.settings.twitch.channel.trim().to_lowercase())
            };
            let now = Instant::now();
            for t in timers {
                if t.interval_secs == 0 || !t.enabled {
                    continue;
                }
                match last.get(&t.name) {
                    // First time we see it: schedule for +interval (do not fire now).
                    None => {
                        last.insert(t.name.clone(), now);
                    }
                    Some(prev) if now.duration_since(*prev).as_secs() >= t.interval_secs => {
                        last.insert(t.name.clone(), now);
                        // Empty platforms = twitch (back-compat); account defaults to bot.
                        let platforms = if t.platforms.is_empty() { vec!["twitch".to_string()] } else { t.platforms.clone() };
                        let broadcaster = t.account == "broadcaster";
                        for p in platforms {
                            match (p.as_str(), broadcaster) {
                                // Twitch bot goes through the IRC client (via the outgoing channel).
                                ("twitch", false) => {
                                    let _ = outgoing.send(Outgoing { channel: channel.clone(), text: t.text.clone() });
                                }
                                ("twitch", true) => { spawn_send(&store, t.text.clone(), "twitch_bc"); }
                                ("kick", false) => { spawn_send(&store, t.text.clone(), "kick_bot"); }
                                ("kick", true) => { spawn_send(&store, t.text.clone(), "kick_bc"); }
                                ("youtube", _) => { spawn_send(&store, t.text.clone(), "youtube"); }
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    })
}

/// Fire-and-forget a timer send so a slow API call never stalls the 1s loop.
fn spawn_send(store: &Arc<Mutex<Store>>, text: String, kind: &'static str) {
    let store = store.clone();
    tokio::spawn(async move {
        let r = match kind {
            "twitch_bc" => crate::helix::twitch_send(&store, &text).await,
            "kick_bot" => crate::helix::kick_send(&store, &text).await,
            "kick_bc" => crate::helix::kick_send_broadcaster(&store, &text).await,
            "youtube" => crate::streaminfo::youtube_send(&store, &text).await,
            _ => Ok(()),
        };
        if let Err(e) = r {
            tracing::error!("timer send ({kind}): {e}");
        }
    });
}
