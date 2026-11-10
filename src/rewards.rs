//! Channel point rewards for Twitch (Helix) and Kick (public API): list, create,
//! delete. Live redemption → Action is handled elsewhere (Twitch via EventSub in
//! `eventsub.rs`; Kick via a polling task here).
//!
//! Clean-room: calls follow each platform's documented REST API.

use crate::integrations::Handles;
use crate::store::Store;
use crate::twitch::fire_event;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex};

/// A reward as the web UI sees it (both platforms normalized to one shape).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Reward {
    pub platform: String,
    pub id: String,
    pub title: String,
    pub cost: i64,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub color: String,
}

/// Parameters the UI sends to create a reward.
#[derive(Debug, Deserialize)]
pub struct NewReward {
    pub platform: String,
    pub title: String,
    pub cost: i64,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub user_input_required: bool,
}

// ---------- Twitch (Helix channel points) ----------

fn tw_client() -> reqwest::Client {
    reqwest::Client::new()
}

async fn tw_creds(store: &Arc<Mutex<Store>>) -> anyhow::Result<(String, String, String)> {
    let s = store.lock().await;
    let tw = &s.settings.twitch;
    if !tw.broadcaster.logged_in() {
        anyhow::bail!("log in the Twitch broadcaster account first");
    }
    Ok((tw.client_id.clone(), tw.broadcaster.access_token.clone(), tw.broadcaster.user_id.clone()))
}

pub async fn twitch_list(store: &Arc<Mutex<Store>>) -> anyhow::Result<Vec<Reward>> {
    let (client_id, token, bid) = tw_creds(store).await?;
    let resp = tw_client()
        .get("https://api.twitch.tv/helix/channel_points/custom_rewards")
        .query(&[("broadcaster_id", bid.as_str())])
        .header("Authorization", format!("Bearer {token}"))
        .header("Client-Id", client_id)
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("twitch list rewards: {} {}", resp.status(), resp.text().await.unwrap_or_default());
    }
    let v: serde_json::Value = resp.json().await?;
    let mut out = Vec::new();
    if let Some(arr) = v.get("data").and_then(|d| d.as_array()) {
        for r in arr {
            out.push(Reward {
                platform: "twitch".into(),
                id: r.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                title: r.get("title").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                cost: r.get("cost").and_then(|x| x.as_i64()).unwrap_or(0),
                description: r.get("prompt").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                enabled: r.get("is_enabled").and_then(|x| x.as_bool()).unwrap_or(false),
                color: r.get("background_color").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            });
        }
    }
    Ok(out)
}

/// Find the stable trigger key a redemption maps to, by (platform, reward id).
/// Returns None for rewards not created through our panel (fall back to title).
pub async fn link_key(store: &Arc<Mutex<Store>>, platform: &str, id: &str) -> Option<String> {
    if id.is_empty() { return None; }
    let s = store.lock().await;
    s.reward_links.iter()
        .find(|l| l.entries.iter().any(|e| e.platform == platform && e.id == id))
        .map(|l| l.key.clone())
}

pub async fn twitch_create(store: &Arc<Mutex<Store>>, n: &NewReward) -> anyhow::Result<String> {
    let (client_id, token, bid) = tw_creds(store).await?;
    let mut body = serde_json::json!({
        "title": n.title,
        "cost": n.cost,
        "is_user_input_required": n.user_input_required,
    });
    if !n.description.is_empty() {
        body["prompt"] = serde_json::json!(n.description);
    }
    if !n.color.is_empty() {
        body["background_color"] = serde_json::json!(n.color);
    }
    let resp = tw_client()
        .post("https://api.twitch.tv/helix/channel_points/custom_rewards")
        .query(&[("broadcaster_id", bid.as_str())])
        .header("Authorization", format!("Bearer {token}"))
        .header("Client-Id", client_id)
        .json(&body)
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("twitch create reward: {} {}", resp.status(), resp.text().await.unwrap_or_default());
    }
    let v: serde_json::Value = resp.json().await?;
    Ok(v["data"][0]["id"].as_str().unwrap_or("").to_string())
}

/// Expand a sub-action reward target into (platform, id) pairs.
/// "link:<key>" → every linked reward; "twitch:<id>"/"kick:<id>" → one pair.
pub async fn resolve_target(store: &Arc<Mutex<Store>>, target: &str) -> Vec<(String, String)> {
    if let Some(key) = target.strip_prefix("link:") {
        let s = store.lock().await;
        if let Some(l) = s.reward_links.iter().find(|l| l.key == key) {
            return l.entries.iter().map(|e| (e.platform.clone(), e.id.clone())).collect();
        }
        return Vec::new();
    }
    match target.split_once(':') {
        Some((p @ ("twitch" | "kick"), id)) => vec![(p.to_string(), id.to_string())],
        _ => Vec::new(),
    }
}

/// enable | disable | toggle | cost on one reward of one platform.
pub async fn reward_op(store: &Arc<Mutex<Store>>, platform: &str, id: &str, op: &str, cost: i64) -> anyhow::Result<()> {
    let body = match op {
        "enable" => serde_json::json!({ "is_enabled": true }),
        "disable" => serde_json::json!({ "is_enabled": false }),
        "cost" => serde_json::json!({ "cost": cost }),
        "toggle" => {
            let list = if platform == "twitch" { twitch_list(store).await? } else { kick_list(store).await? };
            let cur = list.iter().find(|r| r.id == id).map(|r| r.enabled).unwrap_or(false);
            serde_json::json!({ "is_enabled": !cur })
        }
        _ => anyhow::bail!("unknown reward op {op}"),
    };
    match platform {
        "twitch" => twitch_patch(store, id, &body).await,
        "kick" => kick_patch(store, id, &body).await,
        p => anyhow::bail!("unknown platform {p}"),
    }
}

async fn twitch_patch(store: &Arc<Mutex<Store>>, id: &str, body: &serde_json::Value) -> anyhow::Result<()> {
    let (client_id, token, bid) = tw_creds(store).await?;
    let resp = tw_client()
        .patch("https://api.twitch.tv/helix/channel_points/custom_rewards")
        .query(&[("broadcaster_id", bid.as_str()), ("id", id)])
        .header("Authorization", format!("Bearer {token}"))
        .header("Client-Id", client_id)
        .json(body)
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("twitch reward patch: {} {}", resp.status(), resp.text().await.unwrap_or_default());
    }
    Ok(())
}

async fn kick_patch(store: &Arc<Mutex<Store>>, id: &str, body: &serde_json::Value) -> anyhow::Result<()> {
    let token = kick_token(store).await?;
    let resp = tw_client()
        .patch(format!("{KICK_REWARDS}/{id}"))
        .header("Authorization", format!("Bearer {token}"))
        .json(body)
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("kick reward patch: {} {}", resp.status(), resp.text().await.unwrap_or_default());
    }
    Ok(())
}

pub async fn twitch_update(store: &Arc<Mutex<Store>>, id: &str, n: &NewReward) -> anyhow::Result<()> {
    let (client_id, token, bid) = tw_creds(store).await?;
    let mut body = serde_json::json!({
        "title": n.title,
        "cost": n.cost,
        "is_user_input_required": n.user_input_required,
    });
    if !n.description.is_empty() {
        body["prompt"] = serde_json::json!(n.description);
    }
    if !n.color.is_empty() {
        body["background_color"] = serde_json::json!(n.color);
    }
    let resp = tw_client()
        .patch("https://api.twitch.tv/helix/channel_points/custom_rewards")
        .query(&[("broadcaster_id", bid.as_str()), ("id", id)])
        .header("Authorization", format!("Bearer {token}"))
        .header("Client-Id", client_id)
        .json(&body)
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("twitch update reward: {} {}", resp.status(), resp.text().await.unwrap_or_default());
    }
    Ok(())
}

pub async fn twitch_delete(store: &Arc<Mutex<Store>>, id: &str) -> anyhow::Result<()> {
    let (client_id, token, bid) = tw_creds(store).await?;
    let resp = tw_client()
        .delete("https://api.twitch.tv/helix/channel_points/custom_rewards")
        .query(&[("broadcaster_id", bid.as_str()), ("id", id)])
        .header("Authorization", format!("Bearer {token}"))
        .header("Client-Id", client_id)
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("twitch delete reward: {} {}", resp.status(), resp.text().await.unwrap_or_default());
    }
    Ok(())
}

// ---------- Kick (public API channel rewards) ----------

const KICK_REWARDS: &str = "https://api.kick.com/public/v1/channels/rewards";
const KICK_REDEMPTIONS: &str = "https://api.kick.com/public/v1/channels/rewards/redemptions";

async fn kick_token(store: &Arc<Mutex<Store>>) -> anyhow::Result<String> {
    let s = store.lock().await;
    if !s.settings.kick.broadcaster.logged_in() {
        anyhow::bail!("log in the Kick broadcaster account first");
    }
    Ok(s.settings.kick.broadcaster.access_token.clone())
}

pub async fn kick_list(store: &Arc<Mutex<Store>>) -> anyhow::Result<Vec<Reward>> {
    let token = kick_token(store).await?;
    let resp = tw_client()
        .get(KICK_REWARDS)
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("kick list rewards: {} {}", resp.status(), resp.text().await.unwrap_or_default());
    }
    let v: serde_json::Value = resp.json().await?;
    let mut out = Vec::new();
    if let Some(arr) = v.get("data").and_then(|d| d.as_array()) {
        for r in arr {
            out.push(Reward {
                platform: "kick".into(),
                id: r.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                title: r.get("title").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                cost: r.get("cost").and_then(|x| x.as_i64()).unwrap_or(0),
                description: r.get("description").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                enabled: r.get("is_enabled").and_then(|x| x.as_bool()).unwrap_or(false),
                color: r.get("background_color").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            });
        }
    }
    Ok(out)
}

pub async fn kick_create(store: &Arc<Mutex<Store>>, n: &NewReward) -> anyhow::Result<String> {
    let token = kick_token(store).await?;
    let mut body = serde_json::json!({ "title": n.title, "cost": n.cost });
    if !n.description.is_empty() {
        body["description"] = serde_json::json!(n.description);
    }
    if !n.color.is_empty() {
        body["background_color"] = serde_json::json!(n.color);
    }
    if n.user_input_required {
        body["is_user_input_required"] = serde_json::json!(true);
    }
    let resp = tw_client()
        .post(KICK_REWARDS)
        .header("Authorization", format!("Bearer {token}"))
        .json(&body)
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("kick create reward: {} {}", resp.status(), resp.text().await.unwrap_or_default());
    }
    let v: serde_json::Value = resp.json().await?;
    let id = v["data"]["id"].as_str()
        .or_else(|| v["data"][0]["id"].as_str())
        .or_else(|| v["id"].as_str())
        .unwrap_or("").to_string();
    if id.is_empty() {
        tracing::warn!("kick create reward: no id in response: {v}");
    }
    Ok(id)
}

pub async fn kick_update(store: &Arc<Mutex<Store>>, id: &str, n: &NewReward) -> anyhow::Result<()> {
    let token = kick_token(store).await?;
    let mut body = serde_json::json!({ "title": n.title, "cost": n.cost });
    if !n.description.is_empty() {
        body["description"] = serde_json::json!(n.description);
    }
    if !n.color.is_empty() {
        body["background_color"] = serde_json::json!(n.color);
    }
    body["is_user_input_required"] = serde_json::json!(n.user_input_required);
    let resp = tw_client()
        .patch(format!("{KICK_REWARDS}/{id}"))
        .header("Authorization", format!("Bearer {token}"))
        .json(&body)
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("kick update reward: {} {}", resp.status(), resp.text().await.unwrap_or_default());
    }
    Ok(())
}

pub async fn kick_delete(store: &Arc<Mutex<Store>>, id: &str) -> anyhow::Result<()> {
    let token = kick_token(store).await?;
    let resp = tw_client()
        .delete(format!("{KICK_REWARDS}/{id}"))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("kick delete reward: {} {}", resp.status(), resp.text().await.unwrap_or_default());
    }
    Ok(())
}

/// Poll Kick pending redemptions (Kick's live events are webhooks, which need a
/// public URL we don't have behind Tailscale). Each new redemption fires its
/// reward title as an event, then is accepted so it isn't seen again.
#[allow(dead_code)] // kept as a fallback; redemptions now come via the Kick webhook
pub fn spawn_kick_poller(
    store: Arc<Mutex<Store>>,
    bus: broadcast::Sender<crate::model::Event>,
    handles: Handles,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut seen: HashSet<String> = HashSet::new();
        // Only fire redemptions redeemed after the bot started; older ones are backlog.
        let started = chrono::Utc::now().timestamp();
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(8)).await;
            let (token, channel) = {
                let s = store.lock().await;
                if !s.settings.kick.broadcaster.logged_in() {
                    continue;
                }
                (s.settings.kick.broadcaster.access_token.clone(), s.settings.kick.channel.trim().to_lowercase())
            };
            let resp = match tw_client()
                .get(KICK_REDEMPTIONS)
                .query(&[("status", "pending")])
                .header("Authorization", format!("Bearer {token}"))
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => { tracing::debug!("kick redemptions: {e}"); continue; }
            };
            if !resp.status().is_success() {
                continue;
            }
            let Ok(v) = resp.json::<serde_json::Value>().await else { continue };
            // Shape: data[] = reward groups, each { reward:{id,title}, redemptions:[{id,status,redeemer:{user_id}}] }.
            let Some(groups) = v.get("data").and_then(|d| d.as_array()).cloned() else { continue };
            let mut accept_ids = Vec::new();
            for g in groups {
                let reward = g.get("reward");
                let rid = reward.and_then(|r| r.get("id")).and_then(|x| x.as_str()).unwrap_or("").to_string();
                let title = reward.and_then(|r| r.get("title")).and_then(|x| x.as_str()).unwrap_or("reward").to_string();
                let Some(reds) = g.get("redemptions").and_then(|d| d.as_array()) else { continue };
                for rd in reds {
                    if rd.get("status").and_then(|x| x.as_str()) != Some("pending") { continue; }
                    let id = rd.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
                    if id.is_empty() { continue; }
                    // Fulfill every pending redemption (clears the Kick queue, incl. old backlog).
                    accept_ids.push(id.clone());
                    if seen.contains(&id) { continue; }
                    seen.insert(id.clone());
                    let ts = rd.get("redeemed_at").and_then(|x| x.as_str())
                        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                        .map(|d| d.timestamp()).unwrap_or(0);
                    if ts < started { tracing::debug!("kick redemption backlog skip: {title} ts={ts} < {started}"); continue; } // backlog
                    tracing::info!("kick redemption fire: '{title}' rid={rid} id={id}");
                    let user = rd.get("redeemer").and_then(|u| u.get("user_id")).and_then(|x| x.as_i64()).map(|n| n.to_string())
                        .or_else(|| rd.get("redeemer").and_then(|u| u.get("username")).and_then(|x| x.as_str()).map(String::from))
                        .unwrap_or_default();
                    // Linked → stable key. Unlinked → bare title + also "kick:<title>".
                    match link_key(&store, "kick", &rid).await {
                        Some(k) => fire_event(&k, &user, format!("kick reward: {title}"), &store, &bus, &handles, &channel).await,
                        None => {
                            fire_event(&title, &user, format!("kick reward: {title}"), &store, &bus, &handles, &channel).await;
                            crate::twitch::run_triggers(&format!("kick:{title}"), &user, &store, &handles, &channel).await;
                        }
                    }
                }
            }
            if !accept_ids.is_empty() {
                let _ = tw_client()
                    .post(format!("{KICK_REDEMPTIONS}/accept"))
                    .header("Authorization", format!("Bearer {token}"))
                    .json(&serde_json::json!({ "ids": accept_ids }))
                    .send()
                    .await;
            }
        }
    })
}
