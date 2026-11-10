//! Twitch Helix action calls (broadcaster token) + Kick chat send. These back the
//! Twitch/Kick sub-actions (set title/game, clip, marker, shoutout, announce).
//! Clean-room: plain documented REST endpoints.

use crate::store::Store;
use std::sync::Arc;
use tokio::sync::Mutex;

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

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

async fn check(resp: reqwest::Response, what: &str) -> anyhow::Result<serde_json::Value> {
    if !resp.status().is_success() {
        anyhow::bail!("{what}: {} {}", resp.status(), resp.text().await.unwrap_or_default());
    }
    Ok(resp.json().await.unwrap_or(serde_json::Value::Null))
}

pub async fn set_title(store: &Arc<Mutex<Store>>, title: &str) -> anyhow::Result<()> {
    let t = tw(store).await?;
    let resp = client()
        .patch("https://api.twitch.tv/helix/channels")
        .query(&[("broadcaster_id", t.bid.as_str())])
        .header("Authorization", format!("Bearer {}", t.token))
        .header("Client-Id", &t.client_id)
        .json(&serde_json::json!({ "title": title }))
        .send()
        .await?;
    check(resp, "set title").await.map(|_| ())
}

/// Resolve a game/category by name, then set it on the channel.
pub async fn set_game(store: &Arc<Mutex<Store>>, game: &str) -> anyhow::Result<()> {
    let t = tw(store).await?;
    let gid = resolve_game_id(&t, game).await?;
    let resp = client()
        .patch("https://api.twitch.tv/helix/channels")
        .query(&[("broadcaster_id", t.bid.as_str())])
        .header("Authorization", format!("Bearer {}", t.token))
        .header("Client-Id", &t.client_id)
        .json(&serde_json::json!({ "game_id": gid }))
        .send()
        .await?;
    check(resp, "set game").await.map(|_| ())
}

async fn resolve_game_id(t: &Tw, name: &str) -> anyhow::Result<String> {
    let resp = client()
        .get("https://api.twitch.tv/helix/games")
        .query(&[("name", name)])
        .header("Authorization", format!("Bearer {}", t.token))
        .header("Client-Id", &t.client_id)
        .send()
        .await?;
    let v = check(resp, "lookup game").await?;
    v.get("data")
        .and_then(|d| d.get(0))
        .and_then(|g| g.get("id"))
        .and_then(|x| x.as_str())
        .map(String::from)
        .ok_or_else(|| anyhow::anyhow!("game not found: {name}"))
}

pub async fn create_clip(store: &Arc<Mutex<Store>>) -> anyhow::Result<String> {
    let t = tw(store).await?;
    let resp = client()
        .post("https://api.twitch.tv/helix/clips")
        .query(&[("broadcaster_id", t.bid.as_str())])
        .header("Authorization", format!("Bearer {}", t.token))
        .header("Client-Id", &t.client_id)
        .send()
        .await?;
    let v = check(resp, "create clip").await?;
    Ok(v.get("data")
        .and_then(|d| d.get(0))
        .and_then(|c| c.get("id"))
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string())
}

pub async fn create_marker(store: &Arc<Mutex<Store>>, desc: &str) -> anyhow::Result<()> {
    let t = tw(store).await?;
    let mut body = serde_json::json!({ "user_id": t.bid });
    if !desc.is_empty() {
        body["description"] = serde_json::json!(desc);
    }
    let resp = client()
        .post("https://api.twitch.tv/helix/streams/markers")
        .header("Authorization", format!("Bearer {}", t.token))
        .header("Client-Id", &t.client_id)
        .json(&body)
        .send()
        .await?;
    check(resp, "create marker").await.map(|_| ())
}

pub async fn announce(store: &Arc<Mutex<Store>>, message: &str, color: &str) -> anyhow::Result<()> {
    let t = tw(store).await?;
    let color = if color.is_empty() { "primary" } else { color };
    let resp = client()
        .post("https://api.twitch.tv/helix/chat/announcements")
        .query(&[("broadcaster_id", t.bid.as_str()), ("moderator_id", t.bid.as_str())])
        .header("Authorization", format!("Bearer {}", t.token))
        .header("Client-Id", &t.client_id)
        .json(&serde_json::json!({ "message": message, "color": color }))
        .send()
        .await?;
    check(resp, "announce").await.map(|_| ())
}

pub async fn shoutout(store: &Arc<Mutex<Store>>, target_login: &str) -> anyhow::Result<()> {
    let t = tw(store).await?;
    let to_id = resolve_user_id(&t, target_login).await?;
    let resp = client()
        .post("https://api.twitch.tv/helix/chat/shoutouts")
        .query(&[
            ("from_broadcaster_id", t.bid.as_str()),
            ("to_broadcaster_id", to_id.as_str()),
            ("moderator_id", t.bid.as_str()),
        ])
        .header("Authorization", format!("Bearer {}", t.token))
        .header("Client-Id", &t.client_id)
        .send()
        .await?;
    check(resp, "shoutout").await.map(|_| ())
}

async fn resolve_user_id(t: &Tw, login: &str) -> anyhow::Result<String> {
    let login = login.trim_start_matches('@').to_lowercase();
    let resp = client()
        .get("https://api.twitch.tv/helix/users")
        .query(&[("login", login.as_str())])
        .header("Authorization", format!("Bearer {}", t.token))
        .header("Client-Id", &t.client_id)
        .send()
        .await?;
    let v = check(resp, "lookup user").await?;
    v.get("data")
        .and_then(|d| d.get(0))
        .and_then(|u| u.get("id"))
        .and_then(|x| x.as_str())
        .map(String::from)
        .ok_or_else(|| anyhow::anyhow!("user not found: {login}"))
}

pub async fn ban(store: &Arc<Mutex<Store>>, target: &str, duration: u64, reason: &str) -> anyhow::Result<()> {
    let t = tw(store).await?;
    let uid = resolve_user_id(&t, target).await?;
    let mut data = serde_json::json!({ "user_id": uid });
    if duration > 0 {
        data["duration"] = serde_json::json!(duration);
    }
    if !reason.is_empty() {
        data["reason"] = serde_json::json!(reason);
    }
    let resp = client()
        .post("https://api.twitch.tv/helix/moderation/bans")
        .query(&[("broadcaster_id", t.bid.as_str()), ("moderator_id", t.bid.as_str())])
        .header("Authorization", format!("Bearer {}", t.token))
        .header("Client-Id", &t.client_id)
        .json(&serde_json::json!({ "data": data }))
        .send()
        .await?;
    check(resp, "ban").await.map(|_| ())
}

pub async fn unban(store: &Arc<Mutex<Store>>, target: &str) -> anyhow::Result<()> {
    let t = tw(store).await?;
    let uid = resolve_user_id(&t, target).await?;
    let resp = client()
        .delete("https://api.twitch.tv/helix/moderation/bans")
        .query(&[
            ("broadcaster_id", t.bid.as_str()),
            ("moderator_id", t.bid.as_str()),
            ("user_id", uid.as_str()),
        ])
        .header("Authorization", format!("Bearer {}", t.token))
        .header("Client-Id", &t.client_id)
        .send()
        .await?;
    check(resp, "unban").await.map(|_| ())
}

pub async fn clear_chat(store: &Arc<Mutex<Store>>) -> anyhow::Result<()> {
    let t = tw(store).await?;
    let resp = client()
        .delete("https://api.twitch.tv/helix/moderation/chat")
        .query(&[("broadcaster_id", t.bid.as_str()), ("moderator_id", t.bid.as_str())])
        .header("Authorization", format!("Bearer {}", t.token))
        .header("Client-Id", &t.client_id)
        .send()
        .await?;
    check(resp, "clear chat").await.map(|_| ())
}

pub async fn commercial(store: &Arc<Mutex<Store>>, length: u64) -> anyhow::Result<()> {
    let t = tw(store).await?;
    let resp = client()
        .post("https://api.twitch.tv/helix/channels/commercial")
        .header("Authorization", format!("Bearer {}", t.token))
        .header("Client-Id", &t.client_id)
        .json(&serde_json::json!({ "broadcaster_id": t.bid, "length": length }))
        .send()
        .await?;
    check(resp, "commercial").await.map(|_| ())
}

/// mode = emote | followers | subscribers | slow.
pub async fn chat_mode(store: &Arc<Mutex<Store>>, mode: &str, enabled: bool, duration: u64) -> anyhow::Result<()> {
    let t = tw(store).await?;
    let mut body = serde_json::json!({});
    match mode {
        "emote" => body["emote_mode"] = serde_json::json!(enabled),
        "followers" => {
            body["follower_mode"] = serde_json::json!(enabled);
            if enabled {
                body["follower_mode_duration"] = serde_json::json!(duration);
            }
        }
        "subscribers" => body["subscriber_mode"] = serde_json::json!(enabled),
        "slow" => {
            body["slow_mode"] = serde_json::json!(enabled);
            if enabled {
                body["slow_mode_wait_time"] = serde_json::json!(duration.max(3));
            }
        }
        other => anyhow::bail!("unknown chat mode: {other}"),
    }
    let resp = client()
        .patch("https://api.twitch.tv/helix/chat/settings")
        .query(&[("broadcaster_id", t.bid.as_str()), ("moderator_id", t.bid.as_str())])
        .header("Authorization", format!("Bearer {}", t.token))
        .header("Client-Id", &t.client_id)
        .json(&body)
        .send()
        .await?;
    check(resp, "chat mode").await.map(|_| ())
}

pub async fn create_poll(store: &Arc<Mutex<Store>>, title: &str, choices: &[String], duration: u64) -> anyhow::Result<()> {
    let t = tw(store).await?;
    let choices_json: Vec<serde_json::Value> =
        choices.iter().map(|c| serde_json::json!({ "title": c })).collect();
    let resp = client()
        .post("https://api.twitch.tv/helix/polls")
        .header("Authorization", format!("Bearer {}", t.token))
        .header("Client-Id", &t.client_id)
        .json(&serde_json::json!({
            "broadcaster_id": t.bid,
            "title": title,
            "choices": choices_json,
            "duration": duration,
        }))
        .send()
        .await?;
    check(resp, "create poll").await.map(|_| ())
}

/// Add/remove a moderator or VIP, by login. `add`=true adds, false removes.
pub async fn set_moderator(store: &Arc<Mutex<Store>>, target: &str, add: bool) -> anyhow::Result<()> {
    let t = tw(store).await?;
    let uid = resolve_user_id(&t, target).await?;
    let req = client();
    let b = if add {
        req.post("https://api.twitch.tv/helix/moderation/moderators")
    } else {
        req.delete("https://api.twitch.tv/helix/moderation/moderators")
    };
    let resp = b
        .query(&[("broadcaster_id", t.bid.as_str()), ("user_id", uid.as_str())])
        .header("Authorization", format!("Bearer {}", t.token))
        .header("Client-Id", &t.client_id)
        .send()
        .await?;
    check(resp, "set moderator").await.map(|_| ())
}

pub async fn set_vip(store: &Arc<Mutex<Store>>, target: &str, add: bool) -> anyhow::Result<()> {
    let t = tw(store).await?;
    let uid = resolve_user_id(&t, target).await?;
    let req = client();
    let b = if add {
        req.post("https://api.twitch.tv/helix/channels/vips")
    } else {
        req.delete("https://api.twitch.tv/helix/channels/vips")
    };
    let resp = b
        .query(&[("broadcaster_id", t.bid.as_str()), ("user_id", uid.as_str())])
        .header("Authorization", format!("Bearer {}", t.token))
        .header("Client-Id", &t.client_id)
        .send()
        .await?;
    check(resp, "set vip").await.map(|_| ())
}

pub async fn warn(store: &Arc<Mutex<Store>>, target: &str, reason: &str) -> anyhow::Result<()> {
    let t = tw(store).await?;
    let uid = resolve_user_id(&t, target).await?;
    let resp = client()
        .post("https://api.twitch.tv/helix/moderation/warnings")
        .query(&[("broadcaster_id", t.bid.as_str()), ("moderator_id", t.bid.as_str())])
        .header("Authorization", format!("Bearer {}", t.token))
        .header("Client-Id", &t.client_id)
        .json(&serde_json::json!({ "data": { "user_id": uid, "reason": reason } }))
        .send()
        .await?;
    check(resp, "warn").await.map(|_| ())
}

pub async fn create_prediction(store: &Arc<Mutex<Store>>, title: &str, outcomes: &[String], window: u64) -> anyhow::Result<()> {
    let t = tw(store).await?;
    let outs: Vec<serde_json::Value> = outcomes.iter().map(|o| serde_json::json!({ "title": o })).collect();
    let resp = client()
        .post("https://api.twitch.tv/helix/predictions")
        .header("Authorization", format!("Bearer {}", t.token))
        .header("Client-Id", &t.client_id)
        .json(&serde_json::json!({
            "broadcaster_id": t.bid,
            "title": title,
            "outcomes": outs,
            "prediction_window": window,
        }))
        .send()
        .await?;
    check(resp, "create prediction").await.map(|_| ())
}

/// Start a raid to another channel (by login).
pub async fn raid(store: &Arc<Mutex<Store>>, target: &str) -> anyhow::Result<()> {
    let t = tw(store).await?;
    let to = resolve_user_id(&t, target).await?;
    let resp = client()
        .post("https://api.twitch.tv/helix/raids")
        .query(&[("from_broadcaster_id", t.bid.as_str()), ("to_broadcaster_id", to.as_str())])
        .header("Authorization", format!("Bearer {}", t.token))
        .header("Client-Id", &t.client_id)
        .send()
        .await?;
    check(resp, "raid").await.map(|_| ())
}

pub async fn ad_snooze(store: &Arc<Mutex<Store>>) -> anyhow::Result<()> {
    let t = tw(store).await?;
    let resp = client()
        .post("https://api.twitch.tv/helix/channels/ads/schedule/snooze")
        .query(&[("broadcaster_id", t.bid.as_str())])
        .header("Authorization", format!("Bearer {}", t.token))
        .header("Client-Id", &t.client_id)
        .send()
        .await?;
    check(resp, "ad snooze").await.map(|_| ())
}

/// Total follower count (Helix `total`).
pub async fn get_follow_count(store: &Arc<Mutex<Store>>) -> anyhow::Result<String> {
    let t = tw(store).await?;
    let resp = client()
        .get("https://api.twitch.tv/helix/channels/followers")
        .query(&[("broadcaster_id", t.bid.as_str()), ("first", "1")])
        .header("Authorization", format!("Bearer {}", t.token))
        .header("Client-Id", &t.client_id)
        .send()
        .await?;
    let v = check(resp, "follow count").await?;
    Ok(v.get("total").and_then(|x| x.as_i64()).unwrap_or(0).to_string())
}

/// Total subscriber count.
pub async fn get_sub_count(store: &Arc<Mutex<Store>>) -> anyhow::Result<String> {
    let t = tw(store).await?;
    let resp = client()
        .get("https://api.twitch.tv/helix/subscriptions")
        .query(&[("broadcaster_id", t.bid.as_str()), ("first", "1")])
        .header("Authorization", format!("Bearer {}", t.token))
        .header("Client-Id", &t.client_id)
        .send()
        .await?;
    let v = check(resp, "sub count").await?;
    Ok(v.get("total").and_then(|x| x.as_i64()).unwrap_or(0).to_string())
}

pub async fn whisper(store: &Arc<Mutex<Store>>, target: &str, message: &str) -> anyhow::Result<()> {
    // Send from the bot account if present, else broadcaster.
    let (client_id, token, from_id) = {
        let s = store.lock().await;
        let tw = &s.settings.twitch;
        if tw.bot.logged_in() {
            (tw.client_id.clone(), tw.bot.access_token.clone(), tw.bot.user_id.clone())
        } else if tw.broadcaster.logged_in() {
            (tw.client_id.clone(), tw.broadcaster.access_token.clone(), tw.broadcaster.user_id.clone())
        } else {
            anyhow::bail!("Twitch not logged in");
        }
    };
    let t = Tw { client_id: client_id.clone(), token: token.clone(), bid: from_id.clone() };
    let to_id = resolve_user_id(&t, target).await?;
    let resp = client()
        .post("https://api.twitch.tv/helix/whispers")
        .query(&[("from_user_id", from_id.as_str()), ("to_user_id", to_id.as_str())])
        .header("Authorization", format!("Bearer {token}"))
        .header("Client-Id", &client_id)
        .json(&serde_json::json!({ "message": message }))
        .send()
        .await?;
    check(resp, "whisper").await.map(|_| ())
}

/// Minutes since `login` followed the broadcaster, or None if not following.
pub async fn followage_minutes(store: &Arc<Mutex<Store>>, login: &str) -> anyhow::Result<Option<i64>> {
    let t = tw(store).await?;
    let uid = resolve_user_id(&t, login).await?;
    let resp = client()
        .get("https://api.twitch.tv/helix/channels/followers")
        .query(&[("broadcaster_id", t.bid.as_str()), ("user_id", uid.as_str())])
        .header("Authorization", format!("Bearer {}", t.token))
        .header("Client-Id", &t.client_id)
        .send()
        .await?;
    let v = check(resp, "followage").await?;
    let Some(fa) = v.get("data").and_then(|d| d.get(0)).and_then(|f| f.get("followed_at")).and_then(|x| x.as_str()) else {
        return Ok(None);
    };
    let when = chrono::DateTime::parse_from_rfc3339(fa)?.with_timezone(&chrono::Utc);
    Ok(Some((chrono::Utc::now() - when).num_minutes().max(0)))
}

/// Current chatters (viewers in chat, incl. lurkers). Paginates.
pub async fn get_chatters(store: &Arc<Mutex<Store>>) -> anyhow::Result<Vec<String>> {
    let t = tw(store).await?;
    let mut out = Vec::new();
    let mut cursor = String::new();
    loop {
        let mut q = vec![
            ("broadcaster_id", t.bid.as_str()),
            ("moderator_id", t.bid.as_str()),
            ("first", "1000"),
        ];
        if !cursor.is_empty() {
            q.push(("after", cursor.as_str()));
        }
        let resp = client()
            .get("https://api.twitch.tv/helix/chat/chatters")
            .query(&q)
            .header("Authorization", format!("Bearer {}", t.token))
            .header("Client-Id", &t.client_id)
            .send()
            .await?;
        let v = check(resp, "chatters").await?;
        if let Some(arr) = v.get("data").and_then(|d| d.as_array()) {
            for u in arr {
                if let Some(login) = u.get("user_login").and_then(|x| x.as_str()) {
                    out.push(login.to_lowercase());
                }
            }
        }
        cursor = v.get("pagination").and_then(|p| p.get("cursor")).and_then(|x| x.as_str()).unwrap_or("").to_string();
        if cursor.is_empty() {
            break;
        }
    }
    Ok(out)
}

#[derive(Clone)]
pub struct ClipInfo {
    pub id: String,
    pub url: String,
    pub creator: String,
    pub title: String,
}

/// Recent clips of the broadcaster created since `started_at` (RFC3339).
pub async fn get_clips(store: &Arc<Mutex<Store>>, started_at: &str) -> anyhow::Result<Vec<ClipInfo>> {
    let t = tw(store).await?;
    let resp = client()
        .get("https://api.twitch.tv/helix/clips")
        .query(&[("broadcaster_id", t.bid.as_str()), ("started_at", started_at), ("first", "50")])
        .header("Authorization", format!("Bearer {}", t.token))
        .header("Client-Id", &t.client_id)
        .send()
        .await?;
    let v = check(resp, "get clips").await?;
    let mut out = Vec::new();
    if let Some(arr) = v.get("data").and_then(|d| d.as_array()) {
        for c in arr {
            out.push(ClipInfo {
                id: c.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                url: c.get("url").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                creator: c.get("creator_name").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                title: c.get("title").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            });
        }
    }
    Ok(out)
}

/// (token, broadcaster_user_id) for Kick moderation calls.
async fn kick_broadcaster(store: &Arc<Mutex<Store>>) -> anyhow::Result<(String, i64)> {
    let s = store.lock().await;
    let k = &s.settings.kick;
    if !k.broadcaster.logged_in() {
        anyhow::bail!("Kick broadcaster not logged in");
    }
    let bid = k.broadcaster.user_id.parse::<i64>().unwrap_or(0);
    Ok((k.broadcaster.access_token.clone(), bid))
}

/// Resolve a Kick username (channel slug) to its numeric user id.
async fn kick_user_id(token: &str, name: &str) -> anyhow::Result<i64> {
    let slug = name.trim().trim_start_matches('@').to_lowercase();
    let resp = client()
        .get("https://api.kick.com/public/v1/channels")
        .query(&[("slug", slug.as_str())])
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await?;
    let v = check(resp, "kick user lookup").await?;
    v.get("data").and_then(|d| d.get(0)).and_then(|c| c.get("broadcaster_user_id")).and_then(|x| x.as_i64())
        .ok_or_else(|| anyhow::anyhow!("kick user not found: {slug}"))
}

/// Ban (duration 0) or timeout (duration minutes) a Kick user by username.
pub async fn kick_ban(store: &Arc<Mutex<Store>>, target: &str, duration_min: u64, reason: &str) -> anyhow::Result<()> {
    let (token, bid) = kick_broadcaster(store).await?;
    let uid = kick_user_id(&token, target).await?;
    let mut body = serde_json::json!({ "broadcaster_user_id": bid, "user_id": uid });
    if duration_min > 0 {
        body["duration"] = serde_json::json!(duration_min.min(10080));
    }
    if !reason.is_empty() {
        body["reason"] = serde_json::json!(reason.chars().take(100).collect::<String>());
    }
    let resp = client()
        .post("https://api.kick.com/public/v1/moderation/bans")
        .header("Authorization", format!("Bearer {token}"))
        .json(&body)
        .send()
        .await?;
    check(resp, "kick ban").await.map(|_| ())
}

pub async fn kick_unban(store: &Arc<Mutex<Store>>, target: &str) -> anyhow::Result<()> {
    let (token, bid) = kick_broadcaster(store).await?;
    let uid = kick_user_id(&token, target).await?;
    let resp = client()
        .delete("https://api.kick.com/public/v1/moderation/bans")
        .header("Authorization", format!("Bearer {token}"))
        .json(&serde_json::json!({ "broadcaster_user_id": bid, "user_id": uid }))
        .send()
        .await?;
    check(resp, "kick unban").await.map(|_| ())
}

/// Clear Kick chat by deleting buffered recent messages one by one (Kick has no
/// bulk clear). Only affects messages seen since the bot started, up to the buffer
/// cap. Needs the moderation:chat_message:manage scope on the broadcaster token.
pub async fn kick_clear_chat(store: &Arc<Mutex<Store>>) -> anyhow::Result<()> {
    let (token, _bid) = kick_broadcaster(store).await?;
    let ids: Vec<String> = {
        let mut s = store.lock().await;
        s.kick_chat_ids.drain(..).collect()
    };
    if ids.is_empty() {
        return Ok(());
    }
    let c = client();
    let mut failed = 0;
    for id in &ids {
        let resp = c
            .delete(format!("https://api.kick.com/public/v1/chat/{id}"))
            .header("Authorization", format!("Bearer {token}"))
            .send()
            .await;
        match resp {
            Ok(r) if r.status().is_success() => {}
            Ok(r) => { failed += 1; if failed == 1 { tracing::warn!("kick clear: {} {}", r.status(), r.text().await.unwrap_or_default()); } }
            Err(e) => { failed += 1; tracing::warn!("kick clear: {e}"); }
        }
    }
    tracing::info!("kick clear: deleted {}/{} messages", ids.len() - failed, ids.len());
    Ok(())
}

/// Delete one Twitch chat message by id (moderator:manage:chat_messages).
pub async fn twitch_delete_message(store: &Arc<Mutex<Store>>, message_id: &str) -> anyhow::Result<()> {
    let t = tw(store).await?;
    let resp = client()
        .delete("https://api.twitch.tv/helix/moderation/chat")
        .query(&[
            ("broadcaster_id", t.bid.as_str()),
            ("moderator_id", t.bid.as_str()),
            ("message_id", message_id),
        ])
        .header("Authorization", format!("Bearer {}", t.token))
        .header("Client-Id", &t.client_id)
        .send()
        .await?;
    check(resp, "delete message").await.map(|_| ())
}

/// Delete one Kick chat message by id (moderation:chat_message:manage).
pub async fn kick_delete_message(store: &Arc<Mutex<Store>>, id: &str) -> anyhow::Result<()> {
    let (token, _bid) = kick_broadcaster(store).await?;
    let resp = client()
        .delete(format!("https://api.kick.com/public/v1/chat/{id}"))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await?;
    check(resp, "kick delete").await.map(|_| ())
}

/// Send a Kick chat message into the broadcaster's channel. Posts as the bot
/// account when logged in (type "user" + the broadcaster's id), else as the
/// broadcaster to its own channel (type "bot").
pub async fn kick_send(store: &Arc<Mutex<Store>>, message: &str) -> anyhow::Result<()> {
    let (token, body) = {
        let s = store.lock().await;
        let k = &s.settings.kick;
        let bid = k.broadcaster.user_id.parse::<i64>().ok();
        if k.bot.logged_in() {
            // Post as the bot into the broadcaster's channel.
            let b = match bid {
                Some(id) => serde_json::json!({ "type": "user", "content": message, "broadcaster_user_id": id }),
                None => serde_json::json!({ "type": "bot", "content": message }),
            };
            (k.bot.access_token.clone(), b)
        } else if k.broadcaster.logged_in() {
            (k.broadcaster.access_token.clone(), serde_json::json!({ "type": "bot", "content": message }))
        } else {
            anyhow::bail!("Kick not logged in");
        }
    };
    let resp = client()
        .post("https://api.kick.com/public/v1/chat")
        .header("Authorization", format!("Bearer {token}"))
        .json(&body)
        .send()
        .await?;
    check(resp, "kick send").await.map(|_| ())
}

/// Send to Twitch chat AS THE BROADCASTER (Helix; needs user:write:chat scope).
/// The bot's IRC send uses the bot account; this is the dock's explicit sender.
pub async fn twitch_send(store: &Arc<Mutex<Store>>, message: &str) -> anyhow::Result<()> {
    let t = tw(store).await?;
    let resp = client()
        .post("https://api.twitch.tv/helix/chat/messages")
        .header("Authorization", format!("Bearer {}", t.token))
        .header("Client-Id", &t.client_id)
        .json(&serde_json::json!({ "broadcaster_id": t.bid, "sender_id": t.bid, "message": message }))
        .send()
        .await?;
    let v = check(resp, "twitch send").await?;
    // 200 can still carry is_sent=false with a drop reason (e.g. msg duplicate).
    if let Some(d) = v.get("data").and_then(|d| d.get(0)) {
        if d.get("is_sent").and_then(|x| x.as_bool()) == Some(false) {
            anyhow::bail!("{}", d.get("drop_reason").and_then(|r| r.get("message")).and_then(|x| x.as_str()).unwrap_or("message dropped"));
        }
    }
    Ok(())
}

/// Send to Kick chat as the broadcaster account (not the bot).
pub async fn kick_send_broadcaster(store: &Arc<Mutex<Store>>, message: &str) -> anyhow::Result<()> {
    let (token, body) = {
        let s = store.lock().await;
        let k = &s.settings.kick;
        if !k.broadcaster.logged_in() {
            anyhow::bail!("Kick broadcaster not logged in");
        }
        // type "user" + broadcaster_user_id posts as the authenticated user (the
        // broadcaster) into their own channel. type "bot" alone 404s here.
        let body = match k.broadcaster.user_id.parse::<i64>().ok() {
            Some(id) => serde_json::json!({ "type": "user", "content": message, "broadcaster_user_id": id }),
            None => serde_json::json!({ "type": "bot", "content": message }),
        };
        (k.broadcaster.access_token.clone(), body)
    };
    let resp = client()
        .post("https://api.kick.com/public/v1/chat")
        .header("Authorization", format!("Bearer {token}"))
        .json(&body)
        .send()
        .await?;
    check(resp, "kick send").await.map(|_| ())
}
