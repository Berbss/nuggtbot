//! Rich chat messages for the chat overlay and the chat dock.
//!
//! Each platform is already ingested elsewhere (twitch.rs via IRC, kick.rs via
//! the official webhook, youtube.rs via the Data API). Here we turn one incoming
//! message into the overlay's JSON shape — `{id, platform, from, color, badges,
//! parts}` with native + third-party emotes resolved — and the connectors
//! broadcast it as `Event::ChatMsg`.
//!
//! Ported from the chat-overlay Node apps (chat-browser-source). Two shared
//! caches (third-party emotes, Twitch badge URLs) live in statics, refreshed by
//! a background task (`spawn_refresh`), so the hot path never fetches.
use std::collections::HashMap;
use std::sync::RwLock;
use std::time::Duration;

use serde_json::{json, Value};
use twitch_irc::message::PrivmsgMessage;

/// name (case-sensitive) → emote image URL, for BTTV/FFZ/7TV.
static THIRD_PARTY: RwLock<Option<HashMap<String, String>>> = RwLock::new(None);
/// "set_id/version" → Twitch badge image URL.
static TW_BADGES: RwLock<Option<HashMap<String, String>>> = RwLock::new(None);

/// Streamer-bot-style bot accounts whose chat the overlay hides.
const BOT_USERS: &[&str] = &[
    "streamlabs", "streamelements", "nightbot", "moobot", "fossabot", "wizebot",
    "sery_bot", "soundalerts", "commanderroot", "botrix", "kickbot", "streamlabsbot",
];
/// Kick badge `type`s we ship an SVG for (public/badges/kick/<type>.svg).
const KICK_BADGE_TYPES: &[&str] = &[
    "broadcaster", "moderator", "vip", "og", "subscriber", "founder",
    "sub_gifter", "staff", "sidekick", "verified", "bot",
];

fn tw_emote_url(id: &str) -> String {
    format!("https://static-cdn.jtvnw.net/emoticons/v2/{}/default/dark/2.0", urlencode(id))
}
fn kick_emote_url(id: &str) -> String {
    format!("https://files.kick.com/emotes/{id}/fullsize")
}

/// Deterministic fallback colour for a name with no platform colour.
/// Mirrors the Node hash: h = (h*31 + charCode) >>> 0, hue = h % 360.
fn color_hash(name: &str) -> String {
    let mut h: u32 = 0;
    for c in name.encode_utf16() {
        h = h.wrapping_mul(31).wrapping_add(c as u32);
    }
    format!("hsl({}, 70%, 60%)", h % 360)
}

fn push_text(parts: &mut Vec<Value>, s: &str) {
    if !s.is_empty() {
        parts.push(json!({ "type": "text", "text": s }));
    }
}

// ---------- Twitch ----------

/// Build the overlay message for a Twitch PRIVMSG (native emotes + badges + color).
pub fn twitch_msg(m: &PrivmsgMessage) -> Value {
    let from = if m.sender.name.is_empty() { m.sender.login.clone() } else { m.sender.name.clone() };
    let color = m.name_color.as_ref().map(|c| format!("#{:02X}{:02X}{:02X}", c.r, c.g, c.b));
    // (url, set name) pairs, so badgeTypes stays index-aligned with badges.
    let (badges, badge_types): (Vec<String>, Vec<String>) = {
        let map = TW_BADGES.read().unwrap();
        m.badges.iter().filter_map(|b| {
            map.as_ref()
                .and_then(|m| m.get(&format!("{}/{}", b.name, b.version)))
                .map(|url| (url.clone(), b.name.clone()))
        }).unzip()
    };
    let parts = apply_third_party(twitch_parts(&m.message_text, &m.emotes));
    json!({
        "id": format!("twitch:{}", m.message_id),
        "platform": "twitch",
        "from": from,
        "login": m.sender.login,
        "userId": m.sender.id,
        "color": color,
        "badges": badges,
        "badgeTypes": badge_types,
        "parts": parts,
        "text": m.message_text,
        // Raw IRC tags, so the overlay can hand Streamlabs-style themes the same `tags`.
        "tags": m.source.tags.0.iter().map(|(k, v)| (k.clone(), v.clone().unwrap_or_default())).collect::<HashMap<_, _>>(),
        "action": m.is_action,
        "reward": m.source.tags.0.get("custom-reward-id").and_then(|v| v.as_deref()).unwrap_or(""),
    })
}

/// Split a Twitch message into text/emote parts using the IRC emote ranges
/// (char-indexed, end exclusive). Text between ranges is kept.
fn twitch_parts(text: &str, emotes: &[twitch_irc::message::Emote]) -> Vec<Value> {
    let chars: Vec<char> = text.chars().collect();
    let mut sorted: Vec<&twitch_irc::message::Emote> = emotes.iter().collect();
    sorted.sort_by_key(|e| e.char_range.start);
    let mut parts = Vec::new();
    let mut pos = 0usize;
    for e in sorted {
        let (s, en) = (e.char_range.start, e.char_range.end);
        if s < pos || s >= chars.len() {
            continue;
        }
        if s > pos {
            push_text(&mut parts, &chars[pos..s].iter().collect::<String>());
        }
        parts.push(json!({ "type": "emote", "url": tw_emote_url(&e.id) }));
        pos = en.min(chars.len());
    }
    if pos < chars.len() {
        push_text(&mut parts, &chars[pos..].iter().collect::<String>());
    }
    parts
}

// ---------- Kick (official webhook payload) ----------

/// Build the overlay message for a Kick `chat.message.sent` webhook message.
/// `badges` are the raw `sender.identity.badges[].type` strings.
pub fn kick_msg(id: &str, from: &str, user_id: &str, content: &str, color: Option<&str>, badge_types: &[String]) -> Value {
    let color = color
        .filter(|c| is_hex6(c))
        .map(|c| c.to_string())
        .unwrap_or_else(|| color_hash(from));
    let shown: Vec<&String> = badge_types.iter()
        .filter(|t| KICK_BADGE_TYPES.contains(&t.as_str()))
        .collect();
    let badges: Vec<String> = shown.iter().map(|t| format!("/badges/kick/{t}.svg")).collect();
    let parts = apply_third_party(kick_parts(content));
    json!({
        "id": format!("kick:{id}"),
        "platform": "kick",
        "from": from,
        "login": from,
        "userId": user_id,
        "color": color,
        "badges": badges,
        "badgeTypes": shown,
        "parts": parts,
        "text": content,
    })
}

/// Parse Kick content tokens `[emote:<id>:<name>]` into text/emote parts.
fn kick_parts(content: &str) -> Vec<Value> {
    let mut parts = Vec::new();
    let mut rest = content;
    // Hand-rolled scan for "[emote:<digits>:...]" — avoids a regex alloc per message.
    while let Some(start) = rest.find("[emote:") {
        let after = &rest[start + 7..];
        if let Some(colon) = after.find(':') {
            let id = &after[..colon];
            if !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()) {
                if let Some(close) = after[colon..].find(']') {
                    push_text(&mut parts, &rest[..start]);
                    parts.push(json!({ "type": "emote", "url": kick_emote_url(id) }));
                    rest = &after[colon + close + 1..];
                    continue;
                }
            }
        }
        // Not a valid token: keep up to and including the "[emote:" literal, move on.
        push_text(&mut parts, &rest[..start + 7]);
        rest = &rest[start + 7..];
    }
    push_text(&mut parts, rest);
    parts
}

// ---------- YouTube (plain text, no emotes/badges) ----------

pub fn youtube_msg(id: &str, from: &str, text: &str) -> Value {
    json!({
        "id": format!("youtube:{id}"),
        "platform": "youtube",
        "from": from,
        "login": "",
        "userId": "",
        "color": color_hash(from),
        "badges": [],
        "parts": apply_third_party(vec![json!({ "type": "text", "text": text })]),
        "text": text,
    })
}

// ---------- Hide filter (ported from chat-browser-source src/filter.js) ----------

/// Hide bot accounts, `!commands`, and TTS reward messages ("[voice] text").
pub fn should_hide(from: &str, parts: &[Value], badges: &[String], reward_id: &str) -> bool {
    let text: String = parts.iter()
        .filter_map(|p| if p.get("type").and_then(|t| t.as_str()) == Some("text") {
            p.get("text").and_then(|t| t.as_str())
        } else { None })
        .collect();
    let text = text.trim();
    if text.starts_with('!') {
        return true;
    }
    let low = from.to_lowercase();
    if BOT_USERS.contains(&low.as_str()) {
        return true;
    }
    if badges.iter().any(|b| b == "/badges/kick/bot.svg") {
        return true;
    }
    // A reward message whose text starts with "[voice]" is TTS; hide it.
    if !reward_id.is_empty() {
        let t = text.as_bytes();
        if let Some(close) = text.find(']') {
            if t.first() == Some(&b'[') && close >= 1 && close <= 40 {
                return true;
            }
        }
    }
    false
}

// ---------- Third-party emote cache + apply ----------

/// Replace whole words that match a third-party emote name (case-sensitive) with
/// an emote part. Existing emote parts pass through, so native emotes win.
fn apply_third_party(parts: Vec<Value>) -> Vec<Value> {
    let guard = THIRD_PARTY.read().unwrap();
    let Some(map) = guard.as_ref() else { return parts };
    if map.is_empty() {
        return parts;
    }
    let mut out = Vec::new();
    for p in parts {
        if p.get("type").and_then(|t| t.as_str()) != Some("text") {
            out.push(p);
            continue;
        }
        let text = p.get("text").and_then(|t| t.as_str()).unwrap_or("");
        let mut buf = String::new();
        for tok in split_keep_ws(text) {
            if !tok.chars().all(|c| c.is_whitespace()) {
                if let Some(url) = map.get(tok) {
                    push_text(&mut out, &buf);
                    buf.clear();
                    out.push(json!({ "type": "emote", "url": url }));
                    continue;
                }
            }
            buf.push_str(tok);
        }
        push_text(&mut out, &buf);
    }
    out
}

/// Split into tokens, keeping whitespace runs as their own tokens.
fn split_keep_ws(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut ws = s.chars().next().map(|c| c.is_whitespace()).unwrap_or(false);
    for (i, c) in s.char_indices() {
        if c.is_whitespace() != ws {
            out.push(&s[start..i]);
            start = i;
            ws = c.is_whitespace();
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// Fetch third-party emotes + Twitch badges now, then refresh hourly/6-hourly.
/// `channel` is the Twitch login; `user_id` its numeric id (for channel emotes).
pub fn spawn_refresh(
    store: std::sync::Arc<tokio::sync::Mutex<crate::store::Store>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut badges_age = std::time::Instant::now();
        loop {
            let (login, uid) = {
                let s = store.lock().await;
                let tw = &s.settings.twitch;
                (tw.channel.trim().to_lowercase(), tw.broadcaster.user_id.clone())
            };
            if !login.is_empty() {
                match load_third_party(&uid).await {
                    Ok(map) => {
                        tracing::info!("chat: {} third-party emotes loaded", map.len());
                        *THIRD_PARTY.write().unwrap() = Some(map);
                    }
                    Err(e) => tracing::warn!("chat: third-party emotes: {e}"),
                }
                // Badges change rarely; refresh at most every 6h (and on first run).
                if TW_BADGES.read().unwrap().is_none() || badges_age.elapsed().as_secs() > 6 * 3600 {
                    match load_tw_badges(&login).await {
                        Ok(map) => {
                            tracing::info!("chat: {} twitch badges loaded", map.len());
                            *TW_BADGES.write().unwrap() = Some(map);
                            badges_age = std::time::Instant::now();
                        }
                        Err(e) => tracing::warn!("chat: twitch badges: {e}"),
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(3600)).await;
        }
    })
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .user_agent("nuggtbot/0.1")
        .build()
        .unwrap_or_default()
}

/// Host allowlist for third-party emote CDNs.
fn safe_emote_host(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else { return false };
    let host = rest.split('/').next().unwrap_or("");
    host == "cdn.7tv.app"
        || host == "cdn.betterttv.net"
        || host == "frankerfacez.com"
        || host.ends_with(".frankerfacez.com")
}

/// Load global + channel emotes from FFZ, BTTV, 7TV. Later inserts win, matching
/// the Node precedence: global FFZ→BTTV→7TV, then channel FFZ→BTTV→7TV.
async fn load_third_party(user_id: &str) -> anyhow::Result<HashMap<String, String>> {
    let c = client();
    let mut map = HashMap::new();
    ffz_into(&mut map, &c, "https://api.frankerfacez.com/v1/set/global").await;
    bttv_into(&mut map, &c, "https://api.betterttv.net/3/cached/emotes/global").await;
    seventv_into(&mut map, &c, "https://7tv.io/v3/emote-sets/global", true).await;
    if !user_id.is_empty() {
        let id = urlencode(user_id);
        ffz_into(&mut map, &c, &format!("https://api.frankerfacez.com/v1/room/id/{id}")).await;
        bttv_into(&mut map, &c, &format!("https://api.betterttv.net/3/cached/users/twitch/{id}")).await;
        seventv_into(&mut map, &c, &format!("https://7tv.io/v3/users/twitch/{id}"), false).await;
    }
    Ok(map)
}

async fn get_json(c: &reqwest::Client, url: &str) -> Option<Value> {
    let r = c.get(url).send().await.ok()?;
    if r.status() == reqwest::StatusCode::NOT_FOUND {
        return None;
    }
    r.json().await.ok()
}

fn insert_emote(map: &mut HashMap<String, String>, name: &str, url: String) {
    if !name.is_empty() && safe_emote_host(&url) {
        map.insert(name.to_string(), url);
    }
}

async fn ffz_into(map: &mut HashMap<String, String>, c: &reqwest::Client, url: &str) {
    let Some(v) = get_json(c, url).await else { return };
    let defaults: Vec<i64> = v.get("default_sets").and_then(|d| d.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_i64()).collect()).unwrap_or_default();
    let Some(sets) = v.get("sets").and_then(|s| s.as_object()) else { return };
    for (set_id, set) in sets {
        if !defaults.is_empty() && !set_id.parse::<i64>().map(|n| defaults.contains(&n)).unwrap_or(false) {
            continue;
        }
        for e in set.get("emoticons").and_then(|e| e.as_array()).into_iter().flatten() {
            let name = e.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let u = e.get("urls").and_then(|u| u.get("2").or_else(|| u.get("1"))).and_then(|x| x.as_str());
            if let Some(u) = u {
                let u = if let Some(s) = u.strip_prefix("//") { format!("https://{s}") } else { u.to_string() };
                insert_emote(map, name, u);
            }
        }
    }
}

async fn bttv_into(map: &mut HashMap<String, String>, c: &reqwest::Client, url: &str) {
    let Some(v) = get_json(c, url).await else { return };
    let list: Vec<Value> = if let Some(arr) = v.as_array() {
        arr.clone()
    } else {
        let mut l = v.get("channelEmotes").and_then(|a| a.as_array()).cloned().unwrap_or_default();
        l.extend(v.get("sharedEmotes").and_then(|a| a.as_array()).cloned().unwrap_or_default());
        l
    };
    for e in list {
        let name = e.get("code").and_then(|n| n.as_str()).unwrap_or("");
        if let Some(id) = e.get("id").and_then(|i| i.as_str()) {
            insert_emote(map, name, format!("https://cdn.betterttv.net/emote/{id}/2x.webp"));
        }
    }
}

async fn seventv_into(map: &mut HashMap<String, String>, c: &reqwest::Client, url: &str, global: bool) {
    let Some(v) = get_json(c, url).await else { return };
    let emotes = if global {
        v.get("emotes").and_then(|e| e.as_array()).cloned()
    } else {
        v.get("emote_set").and_then(|s| s.get("emotes")).and_then(|e| e.as_array()).cloned()
    };
    for e in emotes.into_iter().flatten() {
        let name = e.get("name").and_then(|n| n.as_str()).unwrap_or("");
        if let Some(h) = e.get("data").and_then(|d| d.get("host")).and_then(|h| h.get("url")).and_then(|u| u.as_str()) {
            insert_emote(map, name, format!("https:{h}/2x.webp"));
        }
    }
}

/// Twitch badge image URLs via the IVR API (no auth). Channel overrides global.
/// Third-party; if it fails the badge arrays are simply empty.
async fn load_tw_badges(login: &str) -> anyhow::Result<HashMap<String, String>> {
    let c = client();
    let mut map = HashMap::new();
    ivr_badges_into(&mut map, &c, "https://api.ivr.fi/v2/twitch/badges/global").await;
    ivr_badges_into(&mut map, &c, &format!("https://api.ivr.fi/v2/twitch/badges/channel?login={}", urlencode(login))).await;
    Ok(map)
}

async fn ivr_badges_into(map: &mut HashMap<String, String>, c: &reqwest::Client, url: &str) {
    let Some(v) = get_json(c, url).await else { return };
    for set in v.as_array().into_iter().flatten() {
        let set_id = set.get("set_id").and_then(|s| s.as_str()).unwrap_or("");
        for ver in set.get("versions").and_then(|x| x.as_array()).into_iter().flatten() {
            let id = ver.get("id").and_then(|x| x.as_str()).unwrap_or("");
            let img = ver.get("image_url_2x").and_then(|x| x.as_str()).unwrap_or("");
            if !set_id.is_empty() && !id.is_empty() && img.starts_with("https://static-cdn.jtvnw.net/") {
                map.insert(format!("{set_id}/{id}"), img.to_string());
            }
        }
    }
}

fn is_hex6(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 7 && b[0] == b'#' && b[1..].iter().all(|c| c.is_ascii_hexdigit())
}

/// Minimal percent-encoding for a path segment (emote/badge ids, logins).
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
