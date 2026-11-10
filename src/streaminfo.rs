//! Stream info: set the title + category on Twitch and Kick at once, with
//! category search/autocomplete merged across both. Ported from the
//! chat-browser-stream-info Node dock. Uses the broadcaster tokens already in
//! Settings. Clean-room REST, same style as helix.rs.
use crate::store::Store;
use std::sync::Arc;
use tokio::sync::Mutex;

const UA: &str = "nuggtbot/0.1";
const TW_MAX: usize = 140;
const KICK_MAX: usize = 100;
const YT_MAX: usize = 100;

struct Tw {
    client_id: String,
    token: String,
    bid: String,
}

async fn tw(store: &Arc<Mutex<Store>>) -> anyhow::Result<Tw> {
    let s = store.lock().await;
    let t = &s.settings.twitch;
    if !t.broadcaster.logged_in() {
        anyhow::bail!("Twitch broadcaster not logged in");
    }
    Ok(Tw { client_id: t.client_id.clone(), token: t.broadcaster.access_token.clone(), bid: t.broadcaster.user_id.clone() })
}

async fn kick_token(store: &Arc<Mutex<Store>>) -> anyhow::Result<String> {
    let s = store.lock().await;
    if !s.settings.kick.broadcaster.logged_in() {
        anyhow::bail!("Kick broadcaster not logged in");
    }
    Ok(s.settings.kick.broadcaster.access_token.clone())
}

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

/// Which platforms have a usable token (drives the UI's ready flags + limits).
pub async fn status(store: &Arc<Mutex<Store>>) -> serde_json::Value {
    let (tw_ok, kick_ok, yt_ok) = {
        let s = store.lock().await;
        // video_id is optional now: if blank, update() auto-detects the active
        // broadcast. "ready" just means the account can be used; whether a live
        // is running is checked at apply time.
        let yt = s.settings.youtube.broadcaster.logged_in();
        (s.settings.twitch.broadcaster.logged_in(), s.settings.kick.broadcaster.logged_in(), yt)
    };
    serde_json::json!({
        "twitch": { "ready": tw_ok },
        "kick": { "ready": kick_ok },
        "youtube": { "ready": yt_ok },
        "limits": { "twitch": TW_MAX, "kick": KICK_MAX, "youtube": YT_MAX }
    })
}

/// Current title + category per logged-in platform.
pub async fn info(store: &Arc<Mutex<Store>>) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    if let Ok(t) = tw(store).await {
        out.insert("twitch".into(), match tw_info(&t).await {
            Ok(v) => v,
            Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
        });
    }
    if let Ok(tok) = kick_token(store).await {
        out.insert("kick".into(), match kick_info(&tok).await {
            Ok(v) => v,
            Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
        });
    }
    if let Ok((tok, cfg)) = yt(store).await {
        out.insert("youtube".into(), match yt_video_id(&tok, &cfg).await {
            Ok(vid) => match yt_info(&tok, &vid).await {
                Ok(v) => v,
                Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
            },
            Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
        });
    }
    serde_json::json!({ "info": out })
}

/// Token + the configured video_id (may be empty; resolved by yt_video_id).
async fn yt(store: &Arc<Mutex<Store>>) -> anyhow::Result<(String, String)> {
    let s = store.lock().await;
    if !s.settings.youtube.broadcaster.logged_in() {
        anyhow::bail!("YouTube broadcaster not logged in");
    }
    Ok((s.settings.youtube.broadcaster.access_token.clone(), s.settings.youtube.video_id.trim().to_string()))
}

/// Send a message to the active live chat as the broadcaster.
pub async fn youtube_send(store: &Arc<Mutex<Store>>, text: &str) -> anyhow::Result<()> {
    let (token, _cfg) = yt(store).await?;
    let chat_id = yt_live_chat_id(&token).await?;
    let resp = client()
        .post("https://www.googleapis.com/youtube/v3/liveChat/messages")
        .query(&[("part", "snippet")])
        .header("Authorization", format!("Bearer {token}"))
        .json(&serde_json::json!({ "snippet": {
            "liveChatId": chat_id,
            "type": "textMessageEvent",
            "textMessageDetails": { "messageText": text }
        }}))
        .send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("{}: {}", resp.status(), resp.text().await.unwrap_or_default());
    }
    Ok(())
}

async fn yt_live_chat_id(token: &str) -> anyhow::Result<String> {
    let resp = client()
        .get("https://www.googleapis.com/youtube/v3/liveBroadcasts")
        .query(&[("part", "snippet"), ("broadcastStatus", "active"), ("broadcastType", "all")])
        .header("Authorization", format!("Bearer {token}"))
        .send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("{}", resp.status());
    }
    let v: serde_json::Value = resp.json().await.unwrap_or_default();
    v.get("items").and_then(|i| i.get(0)).and_then(|it| it.get("snippet")).and_then(|s| s.get("liveChatId")).and_then(|x| x.as_str())
        .map(String::from)
        .ok_or_else(|| anyhow::anyhow!("no active YouTube live chat (go live)"))
}

/// Configured video_id wins; blank falls back to the channel's active broadcast.
async fn yt_video_id(token: &str, configured: &str) -> anyhow::Result<String> {
    if !configured.is_empty() {
        return Ok(configured.to_string());
    }
    let resp = client()
        .get("https://www.googleapis.com/youtube/v3/liveBroadcasts")
        .query(&[("part", "id"), ("broadcastStatus", "active"), ("broadcastType", "all")])
        .header("Authorization", format!("Bearer {token}"))
        .send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("{}", resp.status());
    }
    let v: serde_json::Value = resp.json().await.unwrap_or_default();
    v.get("items").and_then(|i| i.get(0)).and_then(|it| it.get("id")).and_then(|x| x.as_str())
        .map(String::from)
        .ok_or_else(|| anyhow::anyhow!("no active YouTube broadcast (go live, or set video_id)"))
}

async fn yt_get_snippet(token: &str, vid: &str) -> anyhow::Result<serde_json::Value> {
    let resp = client()
        .get("https://www.googleapis.com/youtube/v3/videos")
        .query(&[("part", "snippet"), ("id", vid)])
        .header("Authorization", format!("Bearer {token}"))
        .send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("{}", resp.status());
    }
    let v: serde_json::Value = resp.json().await.unwrap_or_default();
    Ok(v.get("items").and_then(|i| i.get(0)).and_then(|it| it.get("snippet")).cloned().unwrap_or_default())
}

async fn yt_info(token: &str, vid: &str) -> anyhow::Result<serde_json::Value> {
    let sn = yt_get_snippet(token, vid).await?;
    Ok(serde_json::json!({
        "ok": true,
        "title": sn.get("title").and_then(|x| x.as_str()).unwrap_or(""),
        // YouTube categories are video categories, not games; the picker doesn't apply.
        "category": { "id": "", "name": "" }
    }))
}

async fn tw_info(t: &Tw) -> anyhow::Result<serde_json::Value> {
    let resp = client()
        .get("https://api.twitch.tv/helix/channels")
        .query(&[("broadcaster_id", t.bid.as_str())])
        .header("Authorization", format!("Bearer {}", t.token))
        .header("Client-Id", &t.client_id)
        .send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("{}", resp.status());
    }
    let v: serde_json::Value = resp.json().await.unwrap_or_default();
    let d = v.get("data").and_then(|d| d.get(0)).cloned().unwrap_or_default();
    Ok(serde_json::json!({
        "ok": true,
        "title": d.get("title").and_then(|x| x.as_str()).unwrap_or(""),
        "category": { "id": d.get("game_id").and_then(|x| x.as_str()).unwrap_or(""),
                      "name": d.get("game_name").and_then(|x| x.as_str()).unwrap_or("") }
    }))
}

async fn kick_info(token: &str) -> anyhow::Result<serde_json::Value> {
    let resp = client()
        .get("https://api.kick.com/public/v1/channels")
        .header("Authorization", format!("Bearer {token}"))
        .header("User-Agent", UA)
        .send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("{}", resp.status());
    }
    let v: serde_json::Value = resp.json().await.unwrap_or_default();
    let d = v.get("data").and_then(|d| d.get(0)).cloned().unwrap_or_default();
    Ok(serde_json::json!({
        "ok": true,
        "title": d.get("stream_title").and_then(|x| x.as_str()).unwrap_or(""),
        "category": { "id": d.get("category").and_then(|c| c.get("id")).map(|x| x.to_string().trim_matches('"').to_string()).unwrap_or_default(),
                      "name": d.get("category").and_then(|c| c.get("name")).and_then(|x| x.as_str()).unwrap_or("") }
    }))
}

// ---------- category search + merge ----------

fn safe_image(url: &str) -> String {
    let ok = url.strip_prefix("https://").map(|rest| {
        let host = rest.split('/').next().unwrap_or("");
        host == "static-cdn.jtvnw.net" || host == "files.kick.com"
            || host.ends_with(".kick.com") || host.ends_with(".jtvnw.net")
    }).unwrap_or(false);
    if ok { url.to_string() } else { String::new() }
}

/// Merge key: lowercase + collapse whitespace. (No NFD accent folding — close
/// enough for autocomplete; upgrade to unicode normalization if needed.)
fn norm(name: &str) -> String {
    name.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// Search categories on both platforms, merged (max 8). Shape:
/// `{results:[{name,twitchId?,kickId?,image}], errors:{twitch?,kick?}}`.
pub async fn search(store: &Arc<Mutex<Store>>, q: &str) -> serde_json::Value {
    let mut results: Vec<serde_json::Value> = Vec::new();
    let mut index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut errors = serde_json::Map::new();

    // Twitch first, then Kick (insertion order drives display name/image).
    if let Ok(t) = tw(store).await {
        match tw_search(&t, q).await {
            Ok(list) => merge_in(&mut results, &mut index, list, true),
            Err(e) => { errors.insert("twitch".into(), serde_json::json!(e.to_string())); }
        }
    }
    if let Ok(tok) = kick_token(store).await {
        match kick_search(&tok, q).await {
            Ok(list) => merge_in(&mut results, &mut index, list, false),
            Err(e) => { errors.insert("kick".into(), serde_json::json!(e.to_string())); }
        }
    }
    // Both-platform entries first, then single-platform, keeping insertion order.
    results.sort_by_key(|r| {
        let both = r.get("twitchId").is_some() && r.get("kickId").is_some();
        !both
    });
    results.truncate(8);
    serde_json::json!({ "results": results, "errors": errors })
}

/// One raw category result before merge.
struct Cat { name: String, id: String, image: String }

fn merge_in(
    results: &mut Vec<serde_json::Value>,
    index: &mut std::collections::HashMap<String, usize>,
    list: Vec<Cat>,
    twitch: bool,
) {
    for c in list {
        let key = norm(&c.name);
        if key.is_empty() {
            continue;
        }
        let field = if twitch { "twitchId" } else { "kickId" };
        if let Some(&i) = index.get(&key) {
            let r = &mut results[i];
            if r.get(field).is_none() {
                r[field] = serde_json::json!(c.id);
            }
            if r.get("image").and_then(|x| x.as_str()).unwrap_or("").is_empty() && !c.image.is_empty() {
                r["image"] = serde_json::json!(c.image);
            }
        } else {
            let mut r = serde_json::json!({ "name": c.name, "image": c.image });
            r[field] = serde_json::json!(c.id);
            index.insert(key, results.len());
            results.push(r);
        }
    }
}

async fn tw_search(t: &Tw, q: &str) -> anyhow::Result<Vec<Cat>> {
    let resp = client()
        .get("https://api.twitch.tv/helix/search/categories")
        .query(&[("query", q), ("first", "8")])
        .header("Authorization", format!("Bearer {}", t.token))
        .header("Client-Id", &t.client_id)
        .send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("{}", resp.status());
    }
    let v: serde_json::Value = resp.json().await.unwrap_or_default();
    Ok(v.get("data").and_then(|d| d.as_array()).map(|a| a.iter().map(|g| {
        let img = g.get("box_art_url").and_then(|x| x.as_str()).unwrap_or("")
            .replace("{width}x{height}", "52x72");
        Cat {
            name: g.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            id: g.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            image: safe_image(&img),
        }
    }).collect()).unwrap_or_default())
}

async fn kick_search(token: &str, q: &str) -> anyhow::Result<Vec<Cat>> {
    let resp = client()
        .get("https://api.kick.com/public/v1/categories")
        .query(&[("q", q)])
        .header("Authorization", format!("Bearer {token}"))
        .header("User-Agent", UA)
        .send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("{}", resp.status());
    }
    let v: serde_json::Value = resp.json().await.unwrap_or_default();
    Ok(v.get("data").and_then(|d| d.as_array()).map(|a| a.iter().map(|g| Cat {
        name: g.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        id: g.get("id").map(|x| x.to_string().trim_matches('"').to_string()).unwrap_or_default(),
        image: safe_image(g.get("thumbnail").and_then(|x| x.as_str()).unwrap_or("")),
    }).collect()).unwrap_or_default())
}

// ---------- update ----------

/// Update title and/or category on the chosen platforms. Returns
/// `{results:{twitch?,kick?}}`, each `{ok:true,warning?}` or `{ok:false,error}`.
pub async fn update(
    store: &Arc<Mutex<Store>>,
    title: Option<&str>,
    tw_id: Option<&str>,
    kick_id: Option<&str>,
    do_tw: bool,
    do_kick: bool,
    do_yt: bool,
) -> serde_json::Value {
    let mut results = serde_json::Map::new();
    if do_tw {
        results.insert("twitch".into(), match tw(store).await {
            Ok(t) => tw_update(&t, title, tw_id).await,
            Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
        });
    }
    if do_kick {
        results.insert("kick".into(), match kick_token(store).await {
            Ok(tok) => kick_update(&tok, title, kick_id).await,
            Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
        });
    }
    if do_yt {
        results.insert("youtube".into(), match yt(store).await {
            Ok((tok, cfg)) => match yt_video_id(&tok, &cfg).await {
                Ok(vid) => yt_update(&tok, &vid, title).await,
                Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
            },
            Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
        });
    }
    serde_json::json!({ "results": results })
}

/// Updates only the title. videos.update replaces the whole snippet, so we read
/// the current one and resend description/tags/categoryId to avoid wiping them.
async fn yt_update(token: &str, vid: &str, title: Option<&str>) -> serde_json::Value {
    let title = match title_fits(title, YT_MAX) { Ok(v) => v, Err(e) => return e };
    let Some(title) = title else { return serde_json::json!({ "ok": false, "error": "nothing to update" }) };
    let sn = match yt_get_snippet(token, vid).await {
        Ok(s) => s,
        Err(e) => return serde_json::json!({ "ok": false, "error": e.to_string() }),
    };
    let mut snippet = serde_json::Map::new();
    snippet.insert("title".into(), serde_json::json!(title));
    // categoryId is required by videos.update; keep the video's current one.
    snippet.insert("categoryId".into(), serde_json::json!(sn.get("categoryId").and_then(|x| x.as_str()).unwrap_or("22")));
    for k in ["description", "tags", "defaultLanguage", "defaultAudioLanguage"] {
        if let Some(v) = sn.get(k) { snippet.insert(k.into(), v.clone()); }
    }
    let body = serde_json::json!({ "id": vid, "snippet": serde_json::Value::Object(snippet) });
    let resp = client()
        .put("https://www.googleapis.com/youtube/v3/videos")
        .query(&[("part", "snippet")])
        .header("Authorization", format!("Bearer {token}"))
        .json(&body)
        .send().await;
    settle(resp, None).await
}

/// A title longer than `max` is rejected for that platform (no truncation).
fn title_fits(title: Option<&str>, max: usize) -> Result<Option<&str>, serde_json::Value> {
    match title {
        Some(t) if !t.is_empty() => {
            if t.chars().count() > max {
                Err(serde_json::json!({ "ok": false, "error": format!("title too long: {}/{}", t.chars().count(), max) }))
            } else {
                Ok(Some(t))
            }
        }
        _ => Ok(None),
    }
}

async fn tw_update(t: &Tw, title: Option<&str>, game_id: Option<&str>) -> serde_json::Value {
    let title = match title_fits(title, TW_MAX) { Ok(v) => v, Err(e) => return e };
    let mut body = serde_json::Map::new();
    if let Some(t) = title { body.insert("title".into(), serde_json::json!(t)); }
    let mut warning = None;
    match game_id {
        Some(id) if !id.is_empty() => { body.insert("game_id".into(), serde_json::json!(id)); }
        _ if title.is_some() => warning = Some("category not found on this platform; only title updated"),
        _ => return serde_json::json!({ "ok": false, "error": "category not found on this platform" }),
    }
    if body.is_empty() {
        return serde_json::json!({ "ok": false, "error": "nothing to update" });
    }
    let resp = client()
        .patch("https://api.twitch.tv/helix/channels")
        .query(&[("broadcaster_id", t.bid.as_str())])
        .header("Authorization", format!("Bearer {}", t.token))
        .header("Client-Id", &t.client_id)
        .json(&serde_json::Value::Object(body))
        .send().await;
    settle(resp, warning).await
}

async fn kick_update(token: &str, title: Option<&str>, category_id: Option<&str>) -> serde_json::Value {
    let title = match title_fits(title, KICK_MAX) { Ok(v) => v, Err(e) => return e };
    let mut body = serde_json::Map::new();
    if let Some(t) = title { body.insert("stream_title".into(), serde_json::json!(t)); }
    let mut warning = None;
    match category_id.and_then(|id| id.parse::<u64>().ok()) {
        Some(id) => { body.insert("category_id".into(), serde_json::json!(id)); }
        None if title.is_some() => warning = Some("category not found on this platform; only title updated"),
        None => return serde_json::json!({ "ok": false, "error": "category not found on this platform" }),
    }
    if body.is_empty() {
        return serde_json::json!({ "ok": false, "error": "nothing to update" });
    }
    let resp = client()
        .patch("https://api.kick.com/public/v1/channels")
        .header("Authorization", format!("Bearer {token}"))
        .header("User-Agent", UA)
        .json(&serde_json::Value::Object(body))
        .send().await;
    settle(resp, warning).await
}

async fn settle(resp: reqwest::Result<reqwest::Response>, warning: Option<&str>) -> serde_json::Value {
    match resp {
        Ok(r) if r.status().is_success() => match warning {
            Some(w) => serde_json::json!({ "ok": true, "warning": w }),
            None => serde_json::json!({ "ok": true }),
        },
        Ok(r) => {
            let st = r.status();
            serde_json::json!({ "ok": false, "error": format!("{st}: {}", r.text().await.unwrap_or_default()) })
        }
        Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
    }
}
