//! OAuth logins, two per platform like Streamer.bot: a **broadcaster** account
//! (reads events/subs/points, drives Helix) and a **bot** account (sends chat so
//! messages show the bot's name). One registered app (client_id) per platform;
//! each role is a separate login.
//!
//! * Twitch: **Device Code Flow** (`/oauth2/device`) — only a public `client_id`,
//!   no secret, no redirect. The user opens `verification_uri` and types the
//!   short `user_code`. Refresh needs only the `client_id`.
//! * Kick: **Authorization Code + PKCE**, but Kick is a *confidential* client so
//!   the token exchange also needs `client_secret`. Redirect returns to nuggtbot.
//!
//! Clean-room: endpoints and grant types are each platform's public OAuth spec,
//! nothing copied from Streamer.bot.

use crate::model::OAuthToken;
use crate::store::Store;
use base64::Engine;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

const TW_DEVICE: &str = "https://id.twitch.tv/oauth2/device";
const TW_TOKEN: &str = "https://id.twitch.tv/oauth2/token";
const TW_VALIDATE: &str = "https://id.twitch.tv/oauth2/validate";

/// Broadcaster scopes: chat + the Helix sub-actions we reimplement + channel points.
pub const TW_SCOPES_BROADCASTER: &str = "chat:read chat:edit user:write:chat channel:read:subscriptions channel:read:redemptions channel:manage:redemptions bits:read moderator:read:followers channel:manage:broadcast channel:edit:commercial clips:edit moderator:manage:shoutouts moderator:manage:announcements moderator:manage:banned_users moderator:manage:chat_messages moderator:manage:chat_settings channel:manage:polls channel:manage:predictions channel:read:hype_train channel:read:goals channel:manage:moderators channel:manage:vips moderator:manage:warnings channel:manage:ads channel:manage:raids user:manage:whispers moderator:read:chatters user:read:email";
/// Bot scopes: only what a chat-sending account needs.
pub const TW_SCOPES_BOT: &str = "chat:read chat:edit";

const KICK_AUTHORIZE: &str = "https://id.kick.com/oauth/authorize";
const KICK_TOKEN: &str = "https://id.kick.com/oauth/token";
pub const KICK_SCOPES_BROADCASTER: &str = "user:read channel:read channel:write chat:write events:subscribe channel:rewards:read channel:rewards:write moderation:ban moderation:chat_message:manage";
pub const KICK_SCOPES_BOT: &str = "user:read chat:write";

const GOOGLE_AUTHORIZE: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const GOOGLE_TOKEN: &str = "https://oauth2.googleapis.com/token";
// force-ssl allows reading live chat + sending + moderating; readonly for events.
pub const GOOGLE_SCOPES_BROADCASTER: &str = "https://www.googleapis.com/auth/youtube.readonly https://www.googleapis.com/auth/youtube.force-ssl https://www.googleapis.com/auth/userinfo.profile";
pub const GOOGLE_SCOPES_BOT: &str = "https://www.googleapis.com/auth/youtube.force-ssl https://www.googleapis.com/auth/userinfo.profile";

/// Which login a flow targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Broadcaster,
    Bot,
}

impl Role {
    pub fn parse(s: &str) -> Role {
        if s.eq_ignore_ascii_case("bot") { Role::Bot } else { Role::Broadcaster }
    }
    fn twitch_scopes(self) -> &'static str {
        match self {
            Role::Broadcaster => TW_SCOPES_BROADCASTER,
            Role::Bot => TW_SCOPES_BOT,
        }
    }
    fn kick_scopes(self) -> &'static str {
        match self {
            Role::Broadcaster => KICK_SCOPES_BROADCASTER,
            Role::Bot => KICK_SCOPES_BOT,
        }
    }
    fn google_scopes(self) -> &'static str {
        match self {
            Role::Broadcaster => GOOGLE_SCOPES_BROADCASTER,
            Role::Bot => GOOGLE_SCOPES_BOT,
        }
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

// ---------- shared token response ----------

#[derive(Deserialize)]
struct TokenResp {
    access_token: String,
    #[serde(default)]
    refresh_token: String,
    #[serde(default)]
    expires_in: u64,
    // Twitch returns `scope` as an array; Kick returns a space-separated string.
    #[serde(default, deserialize_with = "de_scope")]
    scope: Vec<String>,
}

fn de_scope<'de, D>(d: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;
    match serde_json::Value::deserialize(d)? {
        serde_json::Value::Array(a) => {
            Ok(a.into_iter().filter_map(|v| v.as_str().map(String::from)).collect())
        }
        serde_json::Value::String(s) => Ok(s.split_whitespace().map(String::from).collect()),
        _ => Ok(Vec::new()),
    }
}

fn apply_token(slot: &mut OAuthToken, t: TokenResp) {
    slot.access_token = t.access_token;
    if !t.refresh_token.is_empty() {
        slot.refresh_token = t.refresh_token;
    }
    slot.token_expires = now() + if t.expires_in > 0 { t.expires_in } else { 3600 };
    if !t.scope.is_empty() {
        slot.scopes = t.scope.join(" ");
    }
}

// ---------- Twitch Device Code Flow ----------

#[derive(Deserialize, Clone)]
pub struct DeviceStart {
    pub device_code: String,
    pub user_code: String,
    #[serde(default)]
    pub verification_uri: String,
    #[serde(default)]
    pub expires_in: u64,
    #[serde(default = "d_interval")]
    pub interval: u64,
}
fn d_interval() -> u64 { 5 }

#[derive(Deserialize)]
pub struct ValidateResp {
    #[serde(default)]
    pub login: String,
    #[serde(default)]
    pub user_id: String,
}

/// Ask Twitch for a device + user code for the given role.
pub async fn twitch_device_start(client_id: &str, role: Role) -> anyhow::Result<DeviceStart> {
    if client_id.trim().is_empty() {
        anyhow::bail!("set the Twitch client_id first");
    }
    let c = reqwest::Client::new();
    let resp = c
        .post(TW_DEVICE)
        .form(&[("client_id", client_id), ("scopes", role.twitch_scopes())])
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("device start failed: {} {}", resp.status(), resp.text().await.unwrap_or_default());
    }
    Ok(resp.json::<DeviceStart>().await?)
}

/// Poll until the user approves, then store tokens into the role's slot.
pub async fn twitch_device_poll(
    store: Arc<Mutex<Store>>,
    client_id: String,
    role: Role,
    dev: DeviceStart,
) -> anyhow::Result<()> {
    let c = reqwest::Client::new();
    let deadline = now() + dev.expires_in.max(60);
    let interval = dev.interval.max(1);
    loop {
        if now() >= deadline {
            anyhow::bail!("device code expired before approval");
        }
        tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
        let resp = c
            .post(TW_TOKEN)
            .form(&[
                ("client_id", client_id.as_str()),
                ("scopes", role.twitch_scopes()),
                ("device_code", dev.device_code.as_str()),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ])
            .send()
            .await?;
        if resp.status().is_success() {
            let t: TokenResp = resp.json().await?;
            store_twitch(&store, &client_id, role, t).await?;
            return Ok(());
        }
        let body = resp.text().await.unwrap_or_default();
        if body.contains("authorization_pending") || body.contains("slow_down") {
            continue;
        }
        anyhow::bail!("device poll failed: {body}");
    }
}

async fn store_twitch(store: &Arc<Mutex<Store>>, client_id: &str, role: Role, t: TokenResp) -> anyhow::Result<()> {
    let v = twitch_validate(&t.access_token).await.ok();
    let prof = twitch_profile(client_id, &t.access_token).await.ok();
    let mut s = store.lock().await;
    let tw = &mut s.settings.twitch;
    let slot = match role {
        Role::Broadcaster => &mut tw.broadcaster,
        Role::Bot => &mut tw.bot,
    };
    apply_token(slot, t);
    if let Some(v) = v {
        slot.user_id = v.user_id;
        slot.user_login = v.login.to_lowercase();
    }
    if let Some((name, avatar)) = prof {
        if !name.is_empty() { slot.display_name = name; }
        slot.avatar_url = avatar;
    }
    // Default the channel to the broadcaster login.
    if role == Role::Broadcaster && tw.channel.trim().is_empty() {
        tw.channel = tw.broadcaster.user_login.clone();
    }
    s.save()?;
    Ok(())
}

/// Fetch (display_name, profile_image_url) from Helix /users for the token owner.
async fn twitch_profile(client_id: &str, token: &str) -> anyhow::Result<(String, String)> {
    let c = reqwest::Client::new();
    let resp = c
        .get("https://api.twitch.tv/helix/users")
        .header("Authorization", format!("Bearer {token}"))
        .header("Client-Id", client_id)
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("helix users: {}", resp.status());
    }
    let v: serde_json::Value = resp.json().await?;
    let u = v.get("data").and_then(|d| d.get(0)).cloned().unwrap_or_default();
    let name = u.get("display_name").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let avatar = u.get("profile_image_url").and_then(|x| x.as_str()).unwrap_or("").to_string();
    Ok((name, avatar))
}

pub async fn twitch_validate(token: &str) -> anyhow::Result<ValidateResp> {
    let c = reqwest::Client::new();
    let resp = c
        .get(TW_VALIDATE)
        .header("Authorization", format!("OAuth {token}"))
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("validate failed: {}", resp.status());
    }
    Ok(resp.json::<ValidateResp>().await?)
}

/// Refresh one Twitch slot (public client: client_id only, no secret).
async fn twitch_refresh_slot(store: &Arc<Mutex<Store>>, role: Role) -> anyhow::Result<()> {
    let (client_id, client_secret, refresh) = {
        let s = store.lock().await;
        let slot = match role {
            Role::Broadcaster => &s.settings.twitch.broadcaster,
            Role::Bot => &s.settings.twitch.bot,
        };
        (s.settings.twitch.client_id.clone(), s.settings.twitch.client_secret.clone(), slot.refresh_token.clone())
    };
    if client_id.is_empty() || refresh.is_empty() {
        return Ok(());
    }
    let c = reqwest::Client::new();
    // Confidential Twitch apps require client_secret on refresh; include it if set.
    let mut form = vec![
        ("client_id", client_id.as_str()),
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh.as_str()),
    ];
    if !client_secret.trim().is_empty() {
        form.push(("client_secret", client_secret.as_str()));
    }
    let resp = c.post(TW_TOKEN).form(&form).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("refresh failed: {} {}", resp.status(), resp.text().await.unwrap_or_default());
    }
    let t: TokenResp = resp.json().await?;
    store_twitch(store, &client_id, role, t).await?;
    tracing::info!("twitch token refreshed ({:?})", role);
    Ok(())
}

/// Background task: refresh any Twitch slot ~5 min before it expires.
pub fn spawn_refresher(store: Arc<Mutex<Store>>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            for role in [Role::Broadcaster, Role::Bot] {
                let (exp, has_refresh) = {
                    let s = store.lock().await;
                    let slot = match role {
                        Role::Broadcaster => &s.settings.twitch.broadcaster,
                        Role::Bot => &s.settings.twitch.bot,
                    };
                    (slot.token_expires, !slot.refresh_token.is_empty())
                };
                if has_refresh && exp > 0 && now() + 300 >= exp {
                    if let Err(e) = twitch_refresh_slot(&store, role).await {
                        tracing::error!("twitch refresh {:?}: {e:#}", role);
                    }
                }
            }
            for role in [Role::Broadcaster, Role::Bot] {
                let (exp, has_refresh) = {
                    let s = store.lock().await;
                    let slot = match role {
                        Role::Broadcaster => &s.settings.kick.broadcaster,
                        Role::Bot => &s.settings.kick.bot,
                    };
                    (slot.token_expires, !slot.refresh_token.is_empty())
                };
                if has_refresh && exp > 0 && now() + 300 >= exp {
                    if let Err(e) = kick_refresh_slot(&store, role).await {
                        tracing::error!("kick refresh {:?}: {e:#}", role);
                    }
                }
            }
            for role in [Role::Broadcaster, Role::Bot] {
                let (exp, has_refresh) = {
                    let s = store.lock().await;
                    let slot = match role {
                        Role::Broadcaster => &s.settings.youtube.broadcaster,
                        Role::Bot => &s.settings.youtube.bot,
                    };
                    (slot.token_expires, !slot.refresh_token.is_empty())
                };
                if has_refresh && exp > 0 && now() + 300 >= exp {
                    if let Err(e) = google_refresh_slot(&store, role).await {
                        tracing::error!("youtube refresh {:?}: {e:#}", role);
                    }
                }
            }
        }
    })
}

/// Refresh a Kick slot (confidential client: needs client_id + client_secret).
async fn kick_refresh_slot(store: &Arc<Mutex<Store>>, role: Role) -> anyhow::Result<()> {
    let (client_id, client_secret, refresh) = {
        let s = store.lock().await;
        let slot = match role {
            Role::Broadcaster => &s.settings.kick.broadcaster,
            Role::Bot => &s.settings.kick.bot,
        };
        (s.settings.kick.client_id.clone(), s.settings.kick.client_secret.clone(), slot.refresh_token.clone())
    };
    if client_id.is_empty() || refresh.is_empty() {
        return Ok(());
    }
    let resp = reqwest::Client::new()
        .post(KICK_TOKEN)
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", client_id.as_str()),
            ("client_secret", client_secret.as_str()),
            ("refresh_token", refresh.as_str()),
        ])
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("{} {}", resp.status(), resp.text().await.unwrap_or_default());
    }
    let t: TokenResp = resp.json().await?;
    let mut s = store.lock().await;
    let slot = match role {
        Role::Broadcaster => &mut s.settings.kick.broadcaster,
        Role::Bot => &mut s.settings.kick.bot,
    };
    apply_token(slot, t);
    s.save()?;
    tracing::info!("kick token refreshed ({:?})", role);
    Ok(())
}

// ---------- Kick Authorization Code + PKCE ----------

/// A pending Kick login: the PKCE verifier + state we must match on callback.
#[derive(Clone)]
pub struct KickPending {
    pub verifier: String,
    pub state: String,
    pub redirect_uri: String,
    pub role: Role,
}

fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn random_token(len: usize) -> String {
    use rand::RngCore;
    let mut b = vec![0u8; len];
    rand::thread_rng().fill_bytes(&mut b);
    b64url(&b)
}

/// Build the Kick authorize URL + the pending PKCE state to stash until callback.
pub fn kick_authorize_url(client_id: &str, redirect_uri: &str, role: Role) -> (String, KickPending) {
    let verifier = random_token(48);
    let challenge = b64url(&Sha256::digest(verifier.as_bytes()));
    let state = random_token(16);
    let url = format!(
        "{KICK_AUTHORIZE}?response_type=code&client_id={}&redirect_uri={}&scope={}&code_challenge={}&code_challenge_method=S256&state={}",
        urlenc(client_id),
        urlenc(redirect_uri),
        urlenc(role.kick_scopes()),
        challenge,
        state,
    );
    (url, KickPending { verifier, state, redirect_uri: redirect_uri.to_string(), role })
}

/// Exchange the Kick `code` for tokens (PKCE verifier + client_secret) and store.
pub async fn kick_exchange_code(
    store: &Arc<Mutex<Store>>,
    client_id: &str,
    client_secret: &str,
    code: &str,
    pending: &KickPending,
) -> anyhow::Result<()> {
    let c = reqwest::Client::new();
    let resp = c
        .post(KICK_TOKEN)
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("code", code),
            ("redirect_uri", pending.redirect_uri.as_str()),
            ("code_verifier", pending.verifier.as_str()),
        ])
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("kick token failed: {} {}", resp.status(), resp.text().await.unwrap_or_default());
    }
    let t: TokenResp = resp.json().await?;
    let prof = kick_profile(&t.access_token).await.ok();
    let mut s = store.lock().await;
    let k = &mut s.settings.kick;
    let slot = match pending.role {
        Role::Broadcaster => &mut k.broadcaster,
        Role::Bot => &mut k.bot,
    };
    apply_token(slot, t);
    if let Some((id, name, avatar)) = prof {
        slot.user_id = id;
        slot.user_login = name.to_lowercase();
        slot.display_name = name;
        slot.avatar_url = avatar;
    }
    s.save()?;
    Ok(())
}

/// Fetch (user_id, name, profile_picture) for the Kick token owner.
async fn kick_profile(token: &str) -> anyhow::Result<(String, String, String)> {
    let c = reqwest::Client::new();
    let resp = c
        .get("https://api.kick.com/public/v1/users")
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("kick users: {}", resp.status());
    }
    let v: serde_json::Value = resp.json().await?;
    let u = v.get("data").and_then(|d| d.get(0)).cloned().unwrap_or_default();
    let id = match u.get("user_id") {
        Some(serde_json::Value::Number(n)) => n.to_string(),
        Some(serde_json::Value::String(s)) => s.clone(),
        _ => String::new(),
    };
    let name = u.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let avatar = u.get("profile_picture").and_then(|x| x.as_str()).unwrap_or("").to_string();
    Ok((id, name, avatar))
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

// ---------- Google / YouTube Authorization Code + PKCE ----------

/// Build the Google authorize URL + pending PKCE state. `access_type=offline` +
/// `prompt=consent` so Google returns a refresh_token.
pub fn google_authorize_url(client_id: &str, redirect_uri: &str, role: Role) -> (String, KickPending) {
    let verifier = random_token(48);
    let challenge = b64url(&Sha256::digest(verifier.as_bytes()));
    let state = random_token(16);
    let url = format!(
        "{GOOGLE_AUTHORIZE}?response_type=code&client_id={}&redirect_uri={}&scope={}&code_challenge={}&code_challenge_method=S256&state={}&access_type=offline&prompt=consent&include_granted_scopes=true",
        urlenc(client_id),
        urlenc(redirect_uri),
        urlenc(role.google_scopes()),
        challenge,
        state,
    );
    (url, KickPending { verifier, state, redirect_uri: redirect_uri.to_string(), role })
}

/// Exchange the Google `code` for tokens and store into the YouTube account slot.
pub async fn google_exchange_code(
    store: &Arc<Mutex<Store>>,
    client_id: &str,
    client_secret: &str,
    code: &str,
    pending: &KickPending,
) -> anyhow::Result<()> {
    let resp = reqwest::Client::new()
        .post(GOOGLE_TOKEN)
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("code", code),
            ("redirect_uri", pending.redirect_uri.as_str()),
            ("code_verifier", pending.verifier.as_str()),
        ])
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("google token failed: {} {}", resp.status(), resp.text().await.unwrap_or_default());
    }
    let t: TokenResp = resp.json().await?;
    let prof = youtube_channel(&t.access_token).await.ok();
    let mut s = store.lock().await;
    let slot = match pending.role {
        Role::Broadcaster => &mut s.settings.youtube.broadcaster,
        Role::Bot => &mut s.settings.youtube.bot,
    };
    apply_token(slot, t);
    if let Some((id, name, avatar)) = prof {
        slot.user_id = id;
        slot.user_login = name.clone();
        slot.display_name = name;
        slot.avatar_url = avatar;
    }
    s.save()?;
    Ok(())
}

/// Refresh a Google token slot (confidential web client: needs client_id+secret).
async fn google_refresh_slot(store: &Arc<Mutex<Store>>, role: Role) -> anyhow::Result<()> {
    let (client_id, client_secret, refresh) = {
        let s = store.lock().await;
        let slot = match role {
            Role::Broadcaster => &s.settings.youtube.broadcaster,
            Role::Bot => &s.settings.youtube.bot,
        };
        (s.settings.youtube.client_id.clone(), s.settings.youtube.client_secret.clone(), slot.refresh_token.clone())
    };
    if client_id.is_empty() || refresh.is_empty() {
        return Ok(());
    }
    let resp = reqwest::Client::new()
        .post(GOOGLE_TOKEN)
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", client_id.as_str()),
            ("client_secret", client_secret.as_str()),
            ("refresh_token", refresh.as_str()),
        ])
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("{} {}", resp.status(), resp.text().await.unwrap_or_default());
    }
    let t: TokenResp = resp.json().await?;
    let mut s = store.lock().await;
    let slot = match role {
        Role::Broadcaster => &mut s.settings.youtube.broadcaster,
        Role::Bot => &mut s.settings.youtube.bot,
    };
    apply_token(slot, t);
    s.save()?;
    tracing::info!("youtube token refreshed ({:?})", role);
    Ok(())
}

/// Fetch (channelId, title, thumbnail) for the Google token owner's YouTube channel.
async fn youtube_channel(token: &str) -> anyhow::Result<(String, String, String)> {
    let resp = reqwest::Client::new()
        .get("https://youtube.googleapis.com/youtube/v3/channels?part=snippet&mine=true")
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("youtube channels: {}", resp.status());
    }
    let v: serde_json::Value = resp.json().await?;
    let item = v["items"].get(0).cloned().unwrap_or_default();
    let id = item["id"].as_str().unwrap_or("").to_string();
    let name = item["snippet"]["title"].as_str().unwrap_or("").to_string();
    let avatar = item["snippet"]["thumbnails"]["default"]["url"].as_str().unwrap_or("").to_string();
    Ok((id, name, avatar))
}
