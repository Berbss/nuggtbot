//! YouTube connector via the **official YouTube Data API v3** — the same approach
//! as Streamer.bot (confirmed by decompiling Streamer.bot.YouTube.dll, which uses
//! youtube.googleapis.com/youtube/v3 + Google OAuth with a loopback redirect and
//! the youtube.readonly / youtube.force-ssl scopes).
//!
//! * Auth: Google OAuth (see auth.rs) with the user's own Google Cloud app.
//! * Read: find the active liveChatId (liveBroadcasts.list mine=true, or an
//!   explicit video id), then poll liveChatMessages.list honoring
//!   pollingIntervalMillis + nextPageToken. Each message runs commands/triggers
//!   exactly like Twitch/Kick chat.
//! * Send: liveChatMessages.insert (yt_send), used by the Reply sub-action.

use crate::action::{self, ExecCtx};
use crate::engine;
use crate::integrations::Handles;
use crate::model::Event;
use crate::store::Store;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex};

const API: &str = "https://youtube.googleapis.com/youtube/v3";

pub fn spawn(
    store: Arc<Mutex<Store>>,
    bus: broadcast::Sender<Event>,
    handles: Handles,
    status: Arc<AtomicBool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let logged_in = { store.lock().await.settings.youtube.broadcaster.logged_in() };
            if !logged_in {
                tokio::time::sleep(std::time::Duration::from_secs(15)).await;
                continue;
            }
            if let Err(e) = run(&store, &bus, &handles, &status).await {
                tracing::error!("youtube: {e:#}");
            }
            status.store(false, Ordering::Relaxed);
            {
                let mut s = store.lock().await;
                s.yt_chat_id.clear();
            }
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        }
    })
}

async fn run(
    store: &Arc<Mutex<Store>>,
    bus: &broadcast::Sender<Event>,
    handles: &Handles,
    status: &AtomicBool,
) -> anyhow::Result<()> {
    let (token, video_id) = {
        let s = store.lock().await;
        (s.settings.youtube.broadcaster.access_token.clone(), s.settings.youtube.video_id.trim().to_string())
    };
    let c = reqwest::Client::new();

    let chat_id = match resolve_live_chat_id(&c, &token, &video_id).await? {
        Some(id) => id,
        None => {
            // Not live right now; wait and retry.
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            return Ok(());
        }
    };
    {
        let mut s = store.lock().await;
        s.yt_chat_id = chat_id.clone();
    }
    status.store(true, Ordering::Relaxed);
    let _ = bus.send(Event::System { text: "youtube: connected (live chat)".into() });

    // channel label for events = the broadcaster's channel id (for %channel% context).
    let channel = { store.lock().await.settings.youtube.broadcaster.user_id.clone() };

    // Prime: fetch once and ignore the backlog, keep its nextPageToken so we only
    // process messages that arrive after we connect.
    let mut page_token = String::new();
    let mut first = true;
    loop {
        // Reconnect if the broadcaster token changed or logged out.
        {
            let s = store.lock().await;
            if !s.settings.youtube.broadcaster.logged_in()
                || s.settings.youtube.broadcaster.access_token != token
            {
                return Ok(());
            }
        }

        let mut url = format!(
            "{API}/liveChatMessages?part=snippet,authorDetails&liveChatId={}&maxResults=200",
            urlenc(&chat_id)
        );
        if !page_token.is_empty() {
            url.push_str(&format!("&pageToken={}", urlenc(&page_token)));
        }
        let resp = c.get(&url).header("Authorization", format!("Bearer {token}")).send().await?;
        if !resp.status().is_success() {
            // 403/404 commonly means the live chat ended; drop and let spawn retry.
            anyhow::bail!("liveChatMessages.list: {} {}", resp.status(), resp.text().await.unwrap_or_default());
        }
        let v: serde_json::Value = resp.json().await?;
        page_token = v["nextPageToken"].as_str().unwrap_or("").to_string();
        let poll_ms = v["pollingIntervalMillis"].as_u64().unwrap_or(5000).max(1000);

        if !first {
            if let Some(items) = v["items"].as_array() {
                for it in items {
                    process_message(it, store, bus, handles, &channel).await;
                }
            }
        }
        first = false;

        if page_token.is_empty() {
            // Live chat closed.
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(poll_ms)).await;
    }
}

/// Find the active liveChatId: from an explicit video id, else the broadcaster's
/// current active broadcast. Returns None when the channel is not live.
async fn resolve_live_chat_id(
    c: &reqwest::Client,
    token: &str,
    video_id: &str,
) -> anyhow::Result<Option<String>> {
    if !video_id.is_empty() {
        let url = format!("{API}/videos?part=liveStreamingDetails&id={}", urlenc(video_id));
        let v: serde_json::Value = c
            .get(&url)
            .header("Authorization", format!("Bearer {token}"))
            .send()
            .await?
            .json()
            .await?;
        let id = v["items"][0]["liveStreamingDetails"]["activeLiveChatId"].as_str().unwrap_or("");
        return Ok(if id.is_empty() { None } else { Some(id.to_string()) });
    }
    let url = format!("{API}/liveBroadcasts?part=snippet&broadcastStatus=active&broadcastType=all&maxResults=1");
    let resp = c.get(&url).header("Authorization", format!("Bearer {token}")).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("liveBroadcasts.list: {} {}", resp.status(), resp.text().await.unwrap_or_default());
    }
    let v: serde_json::Value = resp.json().await?;
    let id = v["items"][0]["snippet"]["liveChatId"].as_str().unwrap_or("");
    Ok(if id.is_empty() { None } else { Some(id.to_string()) })
}

async fn process_message(
    it: &serde_json::Value,
    store: &Arc<Mutex<Store>>,
    bus: &broadcast::Sender<Event>,
    handles: &Handles,
    channel: &str,
) {
    let snip = &it["snippet"];
    let author = &it["authorDetails"];
    let kind = snip["type"].as_str().unwrap_or("");
    let user = author["displayName"].as_str().unwrap_or("").to_string();
    let text = snip["displayMessage"].as_str().unwrap_or("").to_string();

    // Non-text events (super chats, new/gifted members, milestones) fire an event
    // so actions can react, mirroring Streamer.bot's YouTube event types.
    match kind {
        "superChatEvent" | "superStickerEvent" => {
            crate::twitch::fire_event("youtube_superchat", &user, text.clone(), store, bus, handles, channel).await;
            return;
        }
        "newSponsorEvent" => {
            crate::twitch::fire_event("youtube_member", &user, text.clone(), store, bus, handles, channel).await;
            return;
        }
        "memberMilestoneChatEvent" => {
            crate::twitch::fire_event("youtube_member_milestone", &user, text.clone(), store, bus, handles, channel).await;
            return;
        }
        "membershipGiftingEvent" | "giftMembershipReceivedEvent" => {
            crate::twitch::fire_event("youtube_member_gift", &user, text.clone(), store, bus, handles, channel).await;
            return;
        }
        _ => {}
    }
    if kind != "textMessageEvent" || text.is_empty() {
        return;
    }

    let _ = bus.send(Event::Chat { platform: "youtube".into(), user: user.clone(), text: text.clone() });
    // Rich message for the chat overlay + dock (plain text; no YouTube emotes/badges).
    {
        let id = it["id"].as_str().unwrap_or("");
        let rich = crate::chat::youtube_msg(id, &user, &text);
        let parts = rich["parts"].as_array().cloned().unwrap_or_default();
        if !crate::chat::should_hide(&user, &parts, &[], "") {
            let _ = bus.send(Event::ChatMsg { msg: rich });
        }
    }

    let roles = roles_from_author(author);
    let (commands, variables, user_groups) = {
        let s = store.lock().await;
        (s.commands.clone(), s.variables.clone(), s.user_groups.clone())
    };
    let Some(cmd) = engine::match_command(&commands, &text, "youtube").filter(|c| c.enabled) else { return };
    let is_broadcaster = roles.iter().any(|r| r == "broadcaster");
    if !crate::cooldown::permitted(cmd, &user, &roles, &user_groups) {
        return;
    }
    if handles.cooldowns.check(&cmd.trigger, &user, cmd.global_cooldown, cmd.user_cooldown, is_broadcaster).await.is_some() {
        return;
    }
    let args: Vec<String> = text.split_whitespace().skip(1).map(|s| s.to_string()).collect();
    let mut extra = std::collections::HashMap::new();
    if let Some(id) = author["channelId"].as_str() {
        extra.insert("userId".into(), id.to_string());
    }
    if let Some(action_name) = cmd.action.clone().filter(|a| !a.is_empty()) {
        let ctx = ExecCtx {
            h: handles.clone(),
            store: store.clone(),
            user: user.clone(),
            channel: channel.to_string(),
            args,
            platform: "youtube".into(),
            extra,
        };
        tokio::spawn(async move { action::run_named(&action_name, &ctx).await });
    } else if !cmd.response.is_empty() {
        let mut vars = variables;
        vars.extend(extra);
        let reply = engine::render(&cmd.response, &user, &args, &vars);
        if let Err(e) = yt_send(store, &reply).await {
            tracing::error!("youtube reply: {e}");
        }
    }
}

/// YouTube author badges → permission roles (lowercase), mirroring other platforms.
fn roles_from_author(author: &serde_json::Value) -> Vec<String> {
    let mut roles = vec!["everyone".to_string()];
    if author["isChatOwner"].as_bool() == Some(true) {
        roles.push("broadcaster".into());
    }
    if author["isChatModerator"].as_bool() == Some(true) {
        roles.push("moderator".into());
    }
    if author["isChatSponsor"].as_bool() == Some(true) {
        roles.push("subscriber".into());
    }
    roles
}

/// Send a chat message to the active YouTube live chat. Uses the bot account if
/// logged in, else the broadcaster. Requires an active live chat (yt_chat_id set).
pub async fn yt_send(store: &Arc<Mutex<Store>>, text: &str) -> anyhow::Result<()> {
    let (token, chat_id) = {
        let s = store.lock().await;
        let yt = &s.settings.youtube;
        let token = if yt.bot.logged_in() {
            yt.bot.access_token.clone()
        } else if yt.broadcaster.logged_in() {
            yt.broadcaster.access_token.clone()
        } else {
            anyhow::bail!("YouTube not logged in");
        };
        (token, s.yt_chat_id.clone())
    };
    if chat_id.is_empty() {
        anyhow::bail!("no active YouTube live chat");
    }
    let body = serde_json::json!({
        "snippet": {
            "liveChatId": chat_id,
            "type": "textMessageEvent",
            "textMessageDetails": { "messageText": text }
        }
    });
    let resp = reqwest::Client::new()
        .post(format!("{API}/liveChatMessages?part=snippet"))
        .header("Authorization", format!("Bearer {token}"))
        .json(&body)
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("liveChatMessages.insert: {} {}", resp.status(), resp.text().await.unwrap_or_default());
    }
    Ok(())
}

fn urlenc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
