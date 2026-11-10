//! Twitch EventSub over WebSocket: live channel-point redemptions (and room to
//! add follows/subs/cheers later). On a redemption we fire the reward's title as
//! an event so an EventTrigger bound to that title runs its Action.
//!
//! Flow (per Twitch EventSub WebSocket docs): connect, receive `session_welcome`
//! with a session id, then create the subscription via Helix with transport
//! `websocket` + that session id. Notifications then arrive on the socket.

use crate::integrations::Handles;
use crate::model::Event;
use crate::store::Store;
use crate::twitch::fire_event;
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex};
use tokio_tungstenite::tungstenite::Message;

const WS_URL: &str = "wss://eventsub.wss.twitch.tv/ws";
const SUBSCRIPTIONS: &str = "https://api.twitch.tv/helix/eventsub/subscriptions";

pub fn spawn(
    store: Arc<Mutex<Store>>,
    bus: broadcast::Sender<Event>,
    handles: Handles,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            if let Err(e) = run(&store, &bus, &handles).await {
                tracing::debug!("eventsub: {e:#}");
            }
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    })
}

async fn run(
    store: &Arc<Mutex<Store>>,
    bus: &broadcast::Sender<Event>,
    handles: &Handles,
) -> anyhow::Result<()> {
    // Need a logged-in broadcaster (the redemption subscription is on their id).
    let (client_id, token, bid, channel) = {
        let s = store.lock().await;
        let tw = &s.settings.twitch;
        (
            tw.client_id.clone(),
            tw.broadcaster.access_token.clone(),
            tw.broadcaster.user_id.clone(),
            tw.channel.trim().to_lowercase(),
        )
    };
    if token.is_empty() || bid.is_empty() || client_id.is_empty() {
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        return Ok(());
    }

    let (ws, _) = tokio_tungstenite::connect_async(WS_URL).await?;
    let (mut write, mut read) = ws.split();
    tracing::info!("eventsub: connected");

    loop {
        // Twitch sends session_keepalive (default every 10s). If the socket dies
        // without a Close frame, read.next() would hang forever and we'd stop
        // getting redemptions; a timeout well above keepalive forces a reconnect.
        let msg = match tokio::time::timeout(std::time::Duration::from_secs(30), read.next()).await {
            Err(_) => {
                tracing::warn!("eventsub: no keepalive for 30s, reconnecting");
                break;
            }
            Ok(None) => break,
            Ok(Some(m)) => m?,
        };
        let text = match msg {
            Message::Text(t) => t,
            Message::Ping(p) => {
                write.send(Message::Pong(p)).await.ok();
                continue;
            }
            Message::Close(_) => break,
            _ => continue,
        };
        let v: serde_json::Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let mtype = v
            .get("metadata")
            .and_then(|m| m.get("message_type"))
            .and_then(|x| x.as_str())
            .unwrap_or("");
        match mtype {
            "session_welcome" => {
                let session_id = v
                    .get("payload")
                    .and_then(|p| p.get("session"))
                    .and_then(|s| s.get("id"))
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                let n = subscribe_all(&client_id, &token, &bid, &session_id).await;
                let _ = bus.send(Event::System { text: format!("eventsub: {n} subscriptions active") });
            }
            "notification" => {
                handle_notification(&v, store, bus, handles, &channel).await;
            }
            "session_reconnect" => {
                // Simplest: drop and let the outer loop reconnect fresh.
                break;
            }
            _ => {}
        }
    }
    Ok(())
}

/// Subscribe to every event type we support. `follow` needs `moderator_user_id`;
/// `raid` keys on `to_broadcaster_user_id`. Returns how many succeeded.
async fn subscribe_all(client_id: &str, token: &str, bid: &str, session_id: &str) -> usize {
    let bcond = serde_json::json!({ "broadcaster_user_id": bid });
    let follow_cond = serde_json::json!({ "broadcaster_user_id": bid, "moderator_user_id": bid });
    let raid_cond = serde_json::json!({ "to_broadcaster_user_id": bid });
    let subs: &[(&str, &str, &serde_json::Value)] = &[
        ("channel.channel_points_custom_reward_redemption.add", "1", &bcond),
        ("channel.follow", "2", &follow_cond),
        ("channel.raid", "1", &raid_cond),
        // hype_train.begin/end omitted: Twitch rejects the type/version for this
        // app/account (400) while every other sub works. Re-add when it's accepted.
        ("channel.poll.end", "1", &bcond),
        ("channel.prediction.end", "1", &bcond),
        ("channel.update", "2", &bcond),
        ("channel.goal.begin", "1", &bcond),
        ("channel.goal.end", "1", &bcond),
    ];
    let c = reqwest::Client::new();
    let mut ok = 0;
    for (ty, ver, cond) in subs {
        let body = serde_json::json!({
            "type": ty, "version": ver, "condition": cond,
            "transport": { "method": "websocket", "session_id": session_id },
        });
        match c.post(SUBSCRIPTIONS)
            .header("Authorization", format!("Bearer {token}"))
            .header("Client-Id", client_id)
            .json(&body).send().await
        {
            Ok(r) if r.status().is_success() => ok += 1,
            Ok(r) => tracing::warn!("eventsub {ty}: {} {}", r.status(), r.text().await.unwrap_or_default()),
            Err(e) => tracing::warn!("eventsub {ty}: {e}"),
        }
    }
    ok
}

async fn handle_notification(
    v: &serde_json::Value,
    store: &Arc<Mutex<Store>>,
    bus: &broadcast::Sender<Event>,
    handles: &Handles,
    channel: &str,
) {
    let sub_type = v.get("metadata").and_then(|m| m.get("subscription_type")).and_then(|x| x.as_str()).unwrap_or("");
    let Some(e) = v.get("payload").and_then(|p| p.get("event")) else { return };
    let s = |k: &str| e.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    let user_or = |k: &str| {
        e.get(k).and_then(|x| x.as_str())
            .or_else(|| e.get("user_login").and_then(|x| x.as_str()))
            .or_else(|| e.get("user_name").and_then(|x| x.as_str()))
            .unwrap_or("").to_string()
    };
    // Extra event name to also fire (reward platform-qualified key); no second notice.
    let mut extra_kind: Option<String> = None;
    // Map each EventSub type to a (kind, user, text) our trigger system understands.
    let (kind, user, text): (String, String, String) = match sub_type {
        "channel.channel_points_custom_reward_redemption.add" => {
            let title = e.get("reward").and_then(|r| r.get("title")).and_then(|x| x.as_str()).unwrap_or("reward").to_string();
            let rid = e.get("reward").and_then(|r| r.get("id")).and_then(|x| x.as_str()).unwrap_or("").to_string();
            // Linked → stable key. Unlinked → bare title (back-compat) + also "twitch:<title>"
            // so a trigger can target just this platform.
            let kind = match crate::rewards::link_key(store, "twitch", &rid).await {
                Some(k) => k,
                None => { extra_kind = Some(format!("twitch:{title}")); title.clone() }
            };
            let input = s("user_input");
            let txt = if input.is_empty() { format!("redeemed: {title}") } else { format!("redeemed: {title} ({input})") };
            (kind, user_or("user_login"), txt)
        }
        "channel.follow" => ("follow".into(), user_or("user_login"), "followed".into()),
        "channel.raid" => {
            let from = e.get("from_broadcaster_user_login").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let n = e.get("viewers").and_then(|x| x.as_i64()).unwrap_or(0);
            ("raid".into(), from, format!("raided with {n}"))
        }
        "channel.hype_train.begin" => ("hypetrainbegin".into(), String::new(), "hype train started".into()),
        "channel.hype_train.end" => {
            let lvl = e.get("level").and_then(|x| x.as_i64()).unwrap_or(0);
            ("hypetrainend".into(), String::new(), format!("hype train ended at level {lvl}"))
        }
        "channel.poll.end" => ("pollend".into(), String::new(), format!("poll ended: {}", s("title"))),
        "channel.prediction.end" => ("predictionend".into(), String::new(), format!("prediction ended: {}", s("title"))),
        "channel.update" => ("channelupdate".into(), String::new(), format!("{} — {}", s("title"), s("category_name"))),
        "channel.goal.begin" => ("goalbegin".into(), String::new(), format!("goal started: {}", s("type"))),
        "channel.goal.end" => ("goalend".into(), String::new(), format!("goal ended: {}", s("type"))),
        _ => return,
    };
    fire_event(&kind, &user, text, store, bus, handles, channel).await;
    if let Some(k) = extra_kind {
        crate::twitch::run_triggers(&k, &user, store, handles, channel).await;
    }
}
