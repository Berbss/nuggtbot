//! Kick connector via the **official Kick API** (same approach as Streamer.bot,
//! which uses api.kick.com + the events API — confirmed by decompiling its Kick
//! DLL). Chat is received through Kick webhook events delivered to a public
//! endpoint we expose with Tailscale Funnel; sending uses helix::kick_send.
//!
//! * Subscribe: POST api.kick.com/public/v1/events/subscriptions (user token,
//!   scope events:subscribe) for chat.message.sent + subscription events. The
//!   callback URL is set per-app in the Kick developer dashboard.
//! * Receive: POST /kick/webhook — verify the RSA signature (Kick public key),
//!   dedup by message id, then process like Twitch chat.

use crate::action::{self, ExecCtx};
use crate::engine;
use crate::integrations::Handles;
use crate::model::Event;
use crate::store::Store;
use crate::twitch::fire_event;
use axum::{extract::State, http::HeaderMap, routing::post, Router};
use rsa::pkcs8::DecodePublicKey;
use sha2::{Digest, Sha256};
use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex};

const API: &str = "https://api.kick.com/public/v1";
const UA: &str = "nuggtbot/0.1";

#[derive(Clone)]
struct KickState {
    store: Arc<Mutex<Store>>,
    bus: broadcast::Sender<Event>,
    handles: Handles,
    pubkey: Arc<Mutex<Option<String>>>,
    seen: Arc<Mutex<(HashSet<String>, VecDeque<String>)>>,
}

pub fn spawn(
    store: Arc<Mutex<Store>>,
    bus: broadcast::Sender<Event>,
    handles: Handles,
    webhook_bind: String,
) -> tokio::task::JoinHandle<()> {
    let st = KickState {
        store: store.clone(),
        bus,
        handles,
        pubkey: Arc::new(Mutex::new(None)),
        seen: Arc::new(Mutex::new((HashSet::new(), VecDeque::new()))),
    };
    // Keep Kick's signing public key cached (fetch now + refresh every 6h), so the
    // request path never fetches it.
    {
        let pubkey = st.pubkey.clone();
        tokio::spawn(async move {
            loop {
                match fetch_public_key().await {
                    Ok(p) => *pubkey.lock().await = Some(p),
                    Err(e) => tracing::warn!("kick public key: {e}"),
                }
                tokio::time::sleep(std::time::Duration::from_secs(6 * 3600)).await;
            }
        });
    }
    // Ensure event subscriptions whenever the broadcaster is logged in.
    {
        let store = store.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                if let Err(e) = ensure_subscriptions(&store).await {
                    tracing::debug!("kick subscribe: {e:#}");
                }
            }
        });
    }
    tokio::spawn(async move {
        let app = Router::new()
            .route("/kick/webhook", post(webhook))
            .with_state(st);
        match tokio::net::TcpListener::bind(&webhook_bind).await {
            Ok(l) => {
                tracing::info!("kick webhook on http://{webhook_bind}/kick/webhook");
                let _ = axum::serve(l, app).await;
            }
            Err(e) => tracing::error!("kick webhook bind {webhook_bind}: {e}"),
        }
    })
}

async fn webhook(State(st): State<KickState>, headers: HeaderMap, body: axum::body::Bytes) -> axum::http::StatusCode {
    // Cheap gate: drop anything that doesn't look like a Kick webhook BEFORE the
    // RSA verify, so random internet POSTs cost almost nothing.
    if body.len() > 65536 {
        return axum::http::StatusCode::PAYLOAD_TOO_LARGE;
    }
    let h = |k: &str| headers.get(k).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let msg_id = h("Kick-Event-Message-Id");
    let ts = h("Kick-Event-Message-Timestamp");
    let sig_b64 = h("Kick-Event-Signature");
    let event_type = h("Kick-Event-Type");
    // All Kick headers must be present.
    if msg_id.is_empty() || ts.is_empty() || sig_b64.is_empty() || event_type.is_empty() {
        return axum::http::StatusCode::BAD_REQUEST;
    }
    // Timestamp must be fresh (±5 min) — rejects replays and junk cheaply.
    match chrono::DateTime::parse_from_rfc3339(&ts) {
        Ok(t) => {
            if (chrono::Utc::now() - t.with_timezone(&chrono::Utc)).num_seconds().abs() > 300 {
                return axum::http::StatusCode::BAD_REQUEST;
            }
        }
        Err(_) => return axum::http::StatusCode::BAD_REQUEST,
    }
    // Only now do the (more expensive) signature check, against the cached key.
    let signed = format!("{msg_id}.{ts}.{}", String::from_utf8_lossy(&body));
    if !verify(&st, &signed, &sig_b64).await {
        return axum::http::StatusCode::UNAUTHORIZED;
    }
    // Dedup by message id (idempotent delivery).
    {
        let mut seen = st.seen.lock().await;
        if !seen.0.insert(msg_id.clone()) {
            return axum::http::StatusCode::OK;
        }
        seen.1.push_back(msg_id);
        while seen.1.len() > 2000 {
            if let Some(old) = seen.1.pop_front() {
                seen.0.remove(&old);
            }
        }
    }
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return axum::http::StatusCode::OK;
    };
    route_event(&event_type, &v, &st).await;
    axum::http::StatusCode::OK
}

async fn route_event(event_type: &str, v: &serde_json::Value, st: &KickState) {
    tracing::debug!("kick webhook event: {event_type}");
    let channel = { st.store.lock().await.settings.kick.channel.trim().to_lowercase() };
    match event_type {
        "chat.message.sent" => handle_chat(v, st, &channel).await,
        "channel.subscription.new" | "channel.subscription.renewal" => {
            let user = v.get("subscriber").and_then(|s| s.get("username")).and_then(|x| x.as_str()).unwrap_or("").to_string();
            fire_event("kicksub", &user, "subscribed".into(), &st.store, &st.bus, &st.handles, &channel).await;
        }
        "channel.subscription.gifts" => {
            let gifter = v.get("gifter").and_then(|g| g.get("username")).and_then(|x| x.as_str()).unwrap_or("").to_string();
            let n = v.get("giftees").and_then(|g| g.as_array()).map(|a| a.len()).unwrap_or(0);
            fire_event("kickgift", &gifter, format!("gifted {n} subs"), &st.store, &st.bus, &st.handles, &channel).await;
        }
        "channel.reward.redemption.updated" => handle_reward(v, st, &channel).await,
        _ => {}
    }
}

/// Channel-point reward redeemed on Kick (delivered for every channel reward,
/// regardless of which app owns it — unlike the ownership-limited poll API).
async fn handle_reward(v: &serde_json::Value, st: &KickState, channel: &str) {
    tracing::info!("kick reward redemption payload: {v}");
    let reward = v.get("reward");
    let title = reward.and_then(|r| r.get("title")).and_then(|x| x.as_str())
        .or_else(|| v.get("reward_title").and_then(|x| x.as_str()))
        .unwrap_or("reward").to_string();
    let rid = reward.and_then(|r| r.get("id")).and_then(|x| x.as_str())
        .or_else(|| v.get("reward_id").and_then(|x| x.as_str()))
        .map(|s| s.to_string()).unwrap_or_default();
    let user = v.get("redeemer").and_then(|u| u.get("username")).and_then(|x| x.as_str())
        .or_else(|| v.get("user").and_then(|u| u.get("username")).and_then(|x| x.as_str()))
        .or_else(|| v.get("username").and_then(|x| x.as_str()))
        .unwrap_or("").to_string();
    // Linked → stable key. Unlinked → bare title + also "kick:<title>".
    match crate::rewards::link_key(&st.store, "kick", &rid).await {
        Some(k) => fire_event(&k, &user, format!("kick reward: {title}"), &st.store, &st.bus, &st.handles, channel).await,
        None => {
            fire_event(&title, &user, format!("kick reward: {title}"), &st.store, &st.bus, &st.handles, channel).await;
            crate::twitch::run_triggers(&format!("kick:{title}"), &user, &st.store, &st.handles, channel).await;
        }
    }
}

/// Verify Kick's RSA-PKCS1v15-SHA256 signature against the cached public key.
/// Never fetches here, so a flood of bad signatures can't amplify into requests
/// to Kick's key endpoint (a background task keeps the key fresh).
async fn verify(st: &KickState, signed: &str, sig_b64: &str) -> bool {
    use base64::Engine;
    let Ok(sig) = base64::engine::general_purpose::STANDARD.decode(sig_b64) else { return false };
    let pem = { st.pubkey.lock().await.clone() };
    let Some(pem) = pem else { return false };
    let Ok(pk) = rsa::RsaPublicKey::from_public_key_pem(pem.trim()) else { return false };
    let hashed = Sha256::digest(signed.as_bytes());
    pk.verify(rsa::Pkcs1v15Sign::new::<Sha256>(), &hashed, &sig).is_ok()
}

async fn fetch_public_key() -> anyhow::Result<String> {
    let resp = reqwest::Client::new()
        .get(format!("{API}/public-key"))
        .header("User-Agent", UA)
        .send()
        .await?;
    let v: serde_json::Value = resp.json().await?;
    v.get("data").and_then(|d| d.get("public_key")).and_then(|x| x.as_str())
        .or_else(|| v.get("public_key").and_then(|x| x.as_str()))
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("no public_key in response"))
}

/// Subscribe to the events we need if not already subscribed.
async fn ensure_subscriptions(store: &Arc<Mutex<Store>>) -> anyhow::Result<()> {
    let token = {
        let s = store.lock().await;
        if !s.settings.kick.broadcaster.logged_in() {
            return Ok(());
        }
        s.settings.kick.broadcaster.access_token.clone()
    };
    let c = reqwest::Client::new();
    let desired = [
        "chat.message.sent",
        "channel.subscription.new",
        "channel.subscription.renewal",
        "channel.subscription.gifts",
        "channel.reward.redemption.updated",
    ];
    // Which are already subscribed? Subscribe only the missing ones (no duplicates).
    let list = c.get(format!("{API}/events/subscriptions"))
        .header("Authorization", format!("Bearer {token}"))
        .header("User-Agent", UA)
        .send().await?;
    let mut existing: std::collections::HashSet<String> = std::collections::HashSet::new();
    if list.status().is_success() {
        let v: serde_json::Value = list.json().await.unwrap_or_default();
        if let Some(arr) = v.get("data").and_then(|d| d.as_array()) {
            for s in arr {
                if let Some(e) = s.get("event").and_then(|x| x.as_str()) { existing.insert(e.to_string()); }
            }
        }
    }
    let events: Vec<serde_json::Value> = desired.iter()
        .filter(|e| !existing.contains(**e))
        .map(|e| serde_json::json!({ "name": e, "version": 1 }))
        .collect();
    if events.is_empty() {
        return Ok(());
    }
    let body = serde_json::json!({ "method": "webhook", "events": events });
    let resp = c.post(format!("{API}/events/subscriptions"))
        .header("Authorization", format!("Bearer {token}"))
        .header("User-Agent", UA)
        .json(&body)
        .send().await?;
    if resp.status().is_success() {
        tracing::info!("kick: event subscriptions created ({} new)", body["events"].as_array().map(|a| a.len()).unwrap_or(0));
    } else {
        tracing::warn!("kick subscribe failed: {} {}", resp.status(), resp.text().await.unwrap_or_default());
    }
    Ok(())
}

async fn handle_chat(v: &serde_json::Value, st: &KickState, channel: &str) {
    let content = v.get("content").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let sender = v.get("sender");
    let user = sender.and_then(|s| s.get("username")).and_then(|x| x.as_str()).unwrap_or("").to_string();
    if content.is_empty() || user.is_empty() {
        return;
    }
    let _ = st.bus.send(Event::Chat { platform: "kick".into(), user: user.clone(), text: content.clone() });

    // Rich message for the chat overlay + dock (emotes, badges, colour).
    {
        let id = v.get("message_id").or_else(|| v.get("id")).and_then(|x| x.as_str()).unwrap_or("");
        let identity = sender.and_then(|s| s.get("identity"));
        let color = identity.and_then(|i| i.get("username_color")).and_then(|x| x.as_str());
        let badge_types: Vec<String> = identity
            .and_then(|i| i.get("badges")).and_then(|b| b.as_array())
            .map(|a| a.iter().filter_map(|b| b.get("type").and_then(|t| t.as_str()).map(String::from)).collect())
            .unwrap_or_default();
        let uid = sender.and_then(|s| s.get("user_id")).map(|x| x.to_string().trim_matches('"').to_string()).unwrap_or_default();
        let rich = crate::chat::kick_msg(id, &user, &uid, &content, color, &badge_types);
        let parts = rich["parts"].as_array().cloned().unwrap_or_default();
        let badges: Vec<String> = rich["badges"].as_array()
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default();
        if !crate::chat::should_hide(&user, &parts, &badges, "") {
            let _ = st.bus.send(Event::ChatMsg { msg: rich });
        }
    }

    // Buffer the message id so kick_clear_chat can delete recent messages (no bulk clear API).
    if let Some(mid) = v.get("message_id").or_else(|| v.get("id")).and_then(|x| x.as_str()) {
        tracing::info!("kick chat buffered id={mid}");
        let mut s = st.store.lock().await;
        s.kick_chat_ids.push_back(mid.to_string());
        while s.kick_chat_ids.len() > 300 { s.kick_chat_ids.pop_front(); }
    } else {
        tracing::warn!("kick chat: no message_id; keys={:?}", v.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()));
    }

    let roles = roles_from_identity(sender);
    let (commands, variables, user_groups) = {
        let s = st.store.lock().await;
        (s.commands.clone(), s.variables.clone(), s.user_groups.clone())
    };
    let Some(cmd) = engine::match_command(&commands, &content, "kick").filter(|c| c.enabled) else { return };
    let is_broadcaster = roles.iter().any(|r| r == "broadcaster");
    if !crate::cooldown::permitted(cmd, &user, &roles, &user_groups) {
        return;
    }
    if st.handles.cooldowns.check(&cmd.trigger, &user, cmd.global_cooldown, cmd.user_cooldown, is_broadcaster).await.is_some() {
        return;
    }
    let args: Vec<String> = content.split_whitespace().skip(1).map(|s| s.to_string()).collect();
    // Event-specific variables usable as %userId% / %userColor% in replies.
    let mut extra = std::collections::HashMap::new();
    if let Some(s) = sender {
        if let Some(id) = s.get("user_id") {
            extra.insert("userId".into(), id.to_string());
        }
        if let Some(c) = s.get("identity").and_then(|i| i.get("username_color")).and_then(|x| x.as_str()) {
            extra.insert("userColor".into(), c.to_string());
        }
    }
    if let Some(action_name) = cmd.action.clone().filter(|a| !a.is_empty()) {
        let ctx = ExecCtx { h: st.handles.clone(), store: st.store.clone(), user: user.clone(), channel: channel.to_string(), args, platform: "kick".into(), extra };
        tokio::spawn(async move { action::run_named(&action_name, &ctx).await });
    } else if !cmd.response.is_empty() {
        let mut vars = variables;
        vars.extend(extra);
        let reply = engine::render(&cmd.response, &user, &args, &vars);
        if let Err(e) = crate::helix::kick_send(&st.store, &reply).await {
            tracing::error!("kick reply: {e}");
        }
    }
}

/// Kick official badges carry a `type`; map to permission roles (lowercase).
fn roles_from_identity(sender: Option<&serde_json::Value>) -> Vec<String> {
    let mut roles = vec!["everyone".to_string()];
    let badges = sender
        .and_then(|s| s.get("identity"))
        .and_then(|i| i.get("badges"))
        .and_then(|b| b.as_array());
    if let Some(badges) = badges {
        for b in badges {
            match b.get("type").and_then(|x| x.as_str()).unwrap_or("") {
                "broadcaster" => roles.push("broadcaster".into()),
                "moderator" => roles.push("moderator".into()),
                "vip" => roles.push("vip".into()),
                "subscriber" | "founder" | "og" | "sub_gifter" => roles.push("subscriber".into()),
                _ => {}
            }
        }
    }
    roles
}
