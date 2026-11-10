use crate::action::{self, ExecCtx};
use crate::config::Config;
use crate::integrations::Handles;
use crate::model::{Action, Command, Event, EventTrigger, Settings, Timer};
use crate::store::Store;
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, State,
    },
    response::{Html, IntoResponse},
    routing::{delete, get, post},
    Json, Router,
};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex};

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<Mutex<Store>>,
    pub bus: broadcast::Sender<Event>,
    pub cfg: Config,
    pub handles: Handles,
    /// In-flight Kick PKCE login (verifier + state) awaiting its callback.
    pub kick_pending: Arc<Mutex<Option<crate::auth::KickPending>>>,
    /// In-flight Google/YouTube login awaiting its callback.
    pub yt_pending: Arc<Mutex<Option<crate::auth::KickPending>>>,
}

pub async fn serve(cfg: Config, state: AppState) -> anyhow::Result<()> {
    let app = Router::new()
        .route("/", get(index))
        .route("/api/health", get(health))
        .route("/api/status", get(status))
        .route("/api/integrations", get(list_integrations))
        .route("/api/plugins", get(list_plugins))
        .route("/api/plugins/{name}/config", get(get_plugin_config).put(put_plugin_config))
        .route("/api/plugins/{name}/status", get(plugin_status))
        .route("/plugin/{name}/{*path}", get(plugin_page_handler))
        .route("/api/restart", post(restart))
        .route("/api/integrations/{name}/{op}", post(integration_op))
        .route("/api/settings", get(get_settings).put(put_settings))
        .route("/api/commands", get(list_commands).post(upsert_command))
        .route("/api/commands/{trigger}", delete(delete_command))
        .route("/api/timers", get(list_timers).post(upsert_timer))
        .route("/api/timers/{name}", delete(delete_timer))
        .route("/api/variables", get(list_variables).post(set_variable))
        .route("/api/variables/{name}", delete(delete_variable))
        .route("/api/quotes", get(list_quotes).post(add_quote))
        .route("/api/quotes/{id}", delete(delete_quote))
        .route("/api/uservariables", get(list_uservariables))
        .route("/api/backups", get(list_backups).post(create_backup))
        .route("/api/backups/{name}", get(download_backup).delete(delete_backup))
        .route("/api/backups/{name}/restore", post(restore_backup))
        .route("/api/usergroups", get(list_usergroups).post(usergroup_add))
        .route("/api/usergroups/{group}", delete(usergroup_delete))
        .route("/api/usergroups/{group}/{user}", delete(usergroup_remove_user))
        .route("/api/sounds", get(list_sounds))
        .route("/api/sounds/{name}", post(upload_sound).delete(delete_sound))
        .route("/api/sounds/{name}/play", post(play_sound))
        .route("/api/sounds/{name}/image", post(upload_image).delete(delete_image))
        .route("/api/sounds/{name}/image/duration", post(set_image_duration))
        .route("/api/images", get(list_images))
        .route("/api/image_durations", get(image_durations))
        .route("/sounds/{name}", get(serve_sound))
        .route("/images/{name}", get(serve_image))
        .route("/overlay", get(overlay))
        .route("/overlay/chat", get(overlay_chat))
        .route("/dock/chat", get(dock_chat))
        .route("/dock/events", get(dock_events))
        .route("/dock/streaminfo", get(dock_streaminfo))
        .route("/events", get(events_sse))
        .route("/badges/kick/{name}", get(serve_kick_badge))
        .route("/api/overlay/chat", get(get_overlay_cfg).put(put_overlay_cfg))
        .route("/api/streaminfo", get(get_streaminfo).post(update_streaminfo))
        .route("/api/streaminfo/status", get(streaminfo_status))
        .route("/api/categories", get(search_categories))
        .route("/api/chat/send", post(chat_send))
        .route("/api/chat/ban", post(chat_ban))
        .route("/api/chat/delete", post(chat_delete))
        .route("/api/actions", get(list_actions).post(upsert_action))
        .route("/api/actions/{name}", delete(delete_action))
        .route("/api/triggers", get(list_triggers).post(upsert_trigger))
        .route("/api/triggers/{event}", delete(delete_trigger))
        .route("/api/run/{name}", post(run_action_api))
        .route("/api/import", post(import_sb))
        .route("/api/export", post(export_code))
        .route("/api/import_code", post(import_code))
        .route("/api/rewards", get(list_rewards).post(create_reward))
        .route("/api/rewards/{platform}/{id}", delete(delete_reward).patch(update_reward))
        .route("/api/rewards/{platform}/{id}/{op}", post(reward_op_handler))
        .route("/api/reward-links", get(list_reward_links))
        .route("/api/queues", get(list_queues).post(declare_queue))
        .route("/api/queues/pause-all", post(queues_pause_all))
        .route("/api/queues/resume-all", post(queues_resume_all))
        .route("/api/queues/{name}/{op}", post(queue_op))
        .route("/api/auth/twitch/start", post(twitch_login_start))
        .route("/api/auth/twitch/logout", post(twitch_logout))
        .route("/api/auth/kick/start", post(kick_login_start))
        .route("/api/auth/kick/logout", post(kick_logout))
        .route("/auth/kick/callback", get(kick_callback))
        .route("/api/auth/youtube/start", post(youtube_login_start))
        .route("/api/auth/youtube/logout", post(youtube_logout))
        .route("/auth/youtube/callback", get(youtube_callback))
        .route("/ws", get(ws_handler))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&cfg.bind).await?;
    tracing::info!("web UI on http://{}", cfg.bind);
    axum::serve(listener, app).await?;
    Ok(())
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "ok": true, "app": "nuggtbot" }))
}

/// Connection status for the panel's top-right widget. Only items the user has
/// set up (a token present, or the integration enabled) are counted/listed.
/// Accounts report connected == token present; integrations use the live flag.
/// List every discovered plugin with its panel card, sub-actions and saved config.
async fn list_plugins() -> Json<serde_json::Value> {
    Json(serde_json::json!(crate::plugin::registry()
        .iter()
        .map(crate::plugin::view)
        .collect::<Vec<_>>()))
}
async fn get_plugin_config(Path(name): Path<String>) -> Json<serde_json::Value> {
    Json(crate::plugin::read_config(&name))
}
async fn put_plugin_config(
    Path(name): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    // Only a registered plugin name may be written (guards against path traversal).
    if !crate::plugin::registry().iter().any(|h| h.name == name) {
        return Json(serde_json::json!({ "ok": false, "error": "unknown plugin" }));
    }
    match crate::plugin::write_config(&name, &body) {
        Ok(()) => Json(serde_json::json!({ "ok": true })),
        Err(e) => Json(serde_json::json!({ "ok": false, "error": e.to_string() })),
    }
}
async fn plugin_status(Path(name): Path<String>) -> Json<serde_json::Value> {
    Json(serde_json::json!(crate::plugin::status(&name).await))
}

/// Serve a plugin's own overlay / browser-dock page (see plugin::serve_page).
async fn plugin_page_handler(Path((name, path)): Path<(String, String)>) -> impl IntoResponse {
    use axum::http::StatusCode;
    match crate::plugin::serve_page(&name, &path).await {
        Some((ct, body)) => (StatusCode::OK, [("content-type", ct)], body),
        None => (StatusCode::NOT_FOUND, [("content-type", "text/plain".to_string())], Vec::new()),
    }
}
/// Restart the service so freshly-dropped plugin libraries get loaded. The
/// restart kills our own cgroup, so it runs in a detached transient unit 1s
/// later — long enough for this response to flush. No-op off systemd.
async fn restart() -> Json<serde_json::Value> {
    match std::process::Command::new("systemd-run")
        .args(["--on-active=1s", "systemctl", "restart", "nuggtbot.service"])
        .spawn()
    {
        Ok(_) => Json(serde_json::json!({ "ok": true })),
        Err(e) => Json(serde_json::json!({ "ok": false, "error": e.to_string() })),
    }
}
async fn status(State(st): State<AppState>) -> Json<serde_json::Value> {
    use std::sync::atomic::Ordering::Relaxed;
    let s = st.store.lock().await;
    let (t, k, o, v, th) = (
        &s.settings.twitch, &s.settings.kick, &s.settings.obs, &s.settings.vts, &s.settings.throwing,
    );
    // (name, configured, connected)
    let rows: Vec<(&str, bool, bool)> = vec![
        ("Twitch (chat)", !t.channel.trim().is_empty(), st.handles.status.twitch.load(Relaxed)),
        ("Twitch (broadcaster)", t.broadcaster.logged_in(), t.broadcaster.logged_in()),
        ("Twitch (bot)", t.bot.logged_in(), t.bot.logged_in()),
        ("YouTube (live chat)", s.settings.youtube.broadcaster.logged_in(), st.handles.status.youtube.load(Relaxed)),
        ("YouTube (bot)", s.settings.youtube.bot.logged_in(), s.settings.youtube.bot.logged_in()),
        ("Kick (broadcaster)", k.broadcaster.logged_in(), k.broadcaster.logged_in()),
        ("Kick (bot)", k.bot.logged_in(), k.bot.logged_in()),
        ("OBS", o.enabled, st.handles.status.obs.load(Relaxed)),
        ("VTS", v.enabled, st.handles.status.vts.load(Relaxed)),
        ("Throwing", th.enabled, st.handles.status.throw.load(Relaxed)),
    ];
    let items: Vec<serde_json::Value> = rows.iter().filter(|r| r.1)
        .map(|r| serde_json::json!({ "name": r.0, "connected": r.2 })).collect();
    let up = rows.iter().filter(|r| r.1 && r.2).count();
    Json(serde_json::json!({ "up": up, "total": items.len(), "items": items }))
}

/// Per-card integration state for the Integrations tab: live connection, desired
/// state (On/Off), and the persisted auto-connect flag.
async fn list_integrations(State(st): State<AppState>) -> Json<serde_json::Value> {
    use std::sync::atomic::Ordering::Relaxed;
    let s = st.store.lock().await;
    let c = &st.handles.status;
    let rows = [
        ("obs", "OBS", c.obs.load(Relaxed), c.obs_want.load(Relaxed), s.settings.obs.enabled),
        ("vts", "VTube Studio", c.vts.load(Relaxed), c.vts_want.load(Relaxed), s.settings.vts.enabled),
        ("throwing", "Throwing System", c.throw.load(Relaxed), c.throw_want.load(Relaxed), s.settings.throwing.enabled),
    ];
    let list: Vec<serde_json::Value> = rows.iter().map(|r| serde_json::json!({
        "key": r.0, "name": r.1, "connected": r.2, "want": r.3, "auto": r.4,
    })).collect();
    Json(serde_json::json!(list))
}

/// On/Off/Connect and auto-connect toggle for one integration.
async fn integration_op(
    State(st): State<AppState>,
    Path((name, op)): Path<(String, String)>,
) -> Json<serde_json::Value> {
    use std::sync::atomic::Ordering::Relaxed;
    let c = &st.handles.status;
    let want = match name.as_str() {
        "obs" => &c.obs_want,
        "vts" => &c.vts_want,
        "throwing" => &c.throw_want,
        _ => return Json(serde_json::json!({ "ok": false, "error": "unknown integration" })),
    };
    match op.as_str() {
        "on" | "connect" => want.store(true, Relaxed),
        "off" => want.store(false, Relaxed),
        "auto_on" | "auto_off" => {
            let on = op == "auto_on";
            let mut s = st.store.lock().await;
            match name.as_str() {
                "obs" => s.settings.obs.enabled = on,
                "vts" => s.settings.vts.enabled = on,
                "throwing" => s.settings.throwing.enabled = on,
                _ => {}
            }
            let _ = s.save();
            if on {
                want.store(true, Relaxed);
            }
        }
        _ => return Json(serde_json::json!({ "ok": false, "error": "unknown op" })),
    }
    Json(serde_json::json!({ "ok": true }))
}

async fn get_settings(State(st): State<AppState>) -> Json<Settings> {
    Json(st.store.lock().await.settings.clone())
}

async fn put_settings(State(st): State<AppState>, Json(mut s): Json<Settings>) -> impl IntoResponse {
    let mut store = st.store.lock().await;
    // The Accounts form never edits OAuth tokens; keep whatever logins we already
    // hold so a plain Save does not wipe them.
    s.twitch.broadcaster = store.settings.twitch.broadcaster.clone();
    s.twitch.bot = store.settings.twitch.bot.clone();
    s.kick.broadcaster = store.settings.kick.broadcaster.clone();
    s.kick.bot = store.settings.kick.bot.clone();
    s.youtube.broadcaster = store.settings.youtube.broadcaster.clone();
    s.youtube.bot = store.settings.youtube.bot.clone();
    store.settings = s;
    if let Err(e) = store.save() {
        tracing::error!("save: {e}");
    }
    let _ = st.bus.send(Event::System { text: "settings updated".into() });
    Json(store.settings.clone())
}

async fn list_commands(State(st): State<AppState>) -> Json<Vec<Command>> {
    Json(st.store.lock().await.commands.clone())
}

async fn upsert_command(State(st): State<AppState>, Json(cmd): Json<Command>) -> impl IntoResponse {
    let mut store = st.store.lock().await;
    store.commands.retain(|c| !c.trigger.eq_ignore_ascii_case(&cmd.trigger));
    store.commands.push(cmd);
    let _ = store.save();
    Json(store.commands.clone())
}

async fn delete_command(State(st): State<AppState>, Path(trigger): Path<String>) -> impl IntoResponse {
    let mut store = st.store.lock().await;
    store.commands.retain(|c| !c.trigger.eq_ignore_ascii_case(&trigger));
    let _ = store.save();
    Json(store.commands.clone())
}

async fn list_timers(State(st): State<AppState>) -> Json<Vec<Timer>> {
    Json(st.store.lock().await.timers.clone())
}

async fn upsert_timer(State(st): State<AppState>, Json(timer): Json<Timer>) -> impl IntoResponse {
    let mut store = st.store.lock().await;
    store.timers.retain(|t| !t.name.eq_ignore_ascii_case(&timer.name));
    store.timers.push(timer);
    let _ = store.save();
    Json(store.timers.clone())
}

async fn delete_timer(State(st): State<AppState>, Path(name): Path<String>) -> impl IntoResponse {
    let mut store = st.store.lock().await;
    store.timers.retain(|t| !t.name.eq_ignore_ascii_case(&name));
    let _ = store.save();
    Json(store.timers.clone())
}

async fn list_variables(State(st): State<AppState>) -> Json<BTreeMap<String, String>> {
    Json(st.store.lock().await.variables.clone())
}

#[derive(Deserialize)]
struct VarInput {
    name: String,
    value: String,
}

async fn set_variable(State(st): State<AppState>, Json(v): Json<VarInput>) -> impl IntoResponse {
    let mut store = st.store.lock().await;
    store.variables.insert(v.name, v.value);
    let _ = store.save();
    Json(store.variables.clone())
}

async fn delete_variable(State(st): State<AppState>, Path(name): Path<String>) -> impl IntoResponse {
    let mut store = st.store.lock().await;
    store.variables.remove(&name);
    let _ = store.save();
    Json(store.variables.clone())
}

async fn list_quotes(State(st): State<AppState>) -> Json<Vec<crate::model::Quote>> {
    Json(st.store.lock().await.quotes.clone())
}

#[derive(Deserialize)]
struct NewQuote {
    text: String,
    #[serde(default)]
    user: String,
    #[serde(default)]
    game: String,
}

async fn add_quote(State(st): State<AppState>, Json(q): Json<NewQuote>) -> impl IntoResponse {
    let mut s = st.store.lock().await;
    s.quote_seq += 1;
    let id = s.quote_seq;
    let date = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
        .to_string();
    s.quotes.push(crate::model::Quote { id, text: q.text, user: q.user, game: q.game, date });
    let _ = s.save();
    Json(s.quotes.clone())
}

async fn delete_quote(State(st): State<AppState>, Path(id): Path<u64>) -> impl IntoResponse {
    let mut s = st.store.lock().await;
    s.quotes.retain(|q| q.id != id);
    let _ = s.save();
    Json(s.quotes.clone())
}

// ---------- Backups ----------

const BACKUP_DIR: &str = "backups";

async fn list_backups() -> impl IntoResponse {
    let mut names: Vec<String> = std::fs::read_dir(BACKUP_DIR)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| e.file_name().to_str().map(String::from))
                .filter(|n| n.starts_with("data-") && n.ends_with(".json"))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names.reverse(); // newest first
    Json(names)
}

async fn create_backup(State(st): State<AppState>) -> impl IntoResponse {
    match st.store.lock().await.backup(BACKUP_DIR, 50) {
        Ok(name) => Json(serde_json::json!({ "ok": true, "name": name })),
        Err(e) => Json(serde_json::json!({ "ok": false, "error": e.to_string() })),
    }
}

async fn download_backup(Path(name): Path<String>) -> impl IntoResponse {
    if safe_backup(&name).is_none() {
        return (axum::http::StatusCode::BAD_REQUEST, String::new());
    }
    match std::fs::read_to_string(format!("{BACKUP_DIR}/{name}")) {
        Ok(s) => (axum::http::StatusCode::OK, s),
        Err(_) => (axum::http::StatusCode::NOT_FOUND, String::new()),
    }
}

async fn delete_backup(Path(name): Path<String>) -> impl IntoResponse {
    if safe_backup(&name).is_some() {
        let _ = std::fs::remove_file(format!("{BACKUP_DIR}/{name}"));
    }
    Json(serde_json::json!({ "ok": true }))
}

async fn restore_backup(State(st): State<AppState>, Path(name): Path<String>) -> impl IntoResponse {
    if safe_backup(&name).is_none() {
        return Json(serde_json::json!({ "ok": false, "error": "bad name" }));
    }
    let text = match std::fs::read_to_string(format!("{BACKUP_DIR}/{name}")) {
        Ok(t) => t,
        Err(e) => return Json(serde_json::json!({ "ok": false, "error": e.to_string() })),
    };
    let parsed: Store = match serde_json::from_str(&text) {
        Ok(s) => s,
        Err(e) => return Json(serde_json::json!({ "ok": false, "error": e.to_string() })),
    };
    let mut store = st.store.lock().await;
    match store.replace_data(parsed) {
        Ok(()) => {
            let _ = st.bus.send(Event::System { text: format!("restored backup {name}") });
            Json(serde_json::json!({ "ok": true }))
        }
        Err(e) => Json(serde_json::json!({ "ok": false, "error": e.to_string() })),
    }
}

fn safe_backup(name: &str) -> Option<()> {
    if name.contains('/') || name.contains('\\') || name.contains("..") {
        return None;
    }
    if name.starts_with("data-") && name.ends_with(".json") {
        Some(())
    } else {
        None
    }
}

async fn list_uservariables(State(st): State<AppState>) -> Json<BTreeMap<String, BTreeMap<String, String>>> {
    Json(st.store.lock().await.user_variables.clone())
}

async fn list_usergroups(State(st): State<AppState>) -> Json<BTreeMap<String, Vec<String>>> {
    Json(st.store.lock().await.user_groups.clone())
}

#[derive(Deserialize)]
struct UserGroupIn {
    group: String,
    #[serde(default)]
    user: String,
}

async fn usergroup_add(State(st): State<AppState>, Json(i): Json<UserGroupIn>) -> impl IntoResponse {
    let g = i.group.trim().to_string();
    if !g.is_empty() {
        let mut s = st.store.lock().await;
        let members = s.user_groups.entry(g).or_default();
        let u = i.user.trim().trim_start_matches('@').to_lowercase();
        if !u.is_empty() && !members.iter().any(|m| m == &u) {
            members.push(u);
        }
        let _ = s.save();
    }
    Json(st.store.lock().await.user_groups.clone())
}

async fn usergroup_delete(State(st): State<AppState>, Path(group): Path<String>) -> impl IntoResponse {
    let mut s = st.store.lock().await;
    s.user_groups.remove(&group);
    let _ = s.save();
    Json(s.user_groups.clone())
}

async fn usergroup_remove_user(State(st): State<AppState>, Path((group, user)): Path<(String, String)>) -> impl IntoResponse {
    let mut s = st.store.lock().await;
    if let Some(m) = s.user_groups.get_mut(&group) {
        let u = user.to_lowercase();
        m.retain(|x| x != &u);
    }
    let _ = s.save();
    Json(s.user_groups.clone())
}

async fn list_actions(State(st): State<AppState>) -> Json<Vec<Action>> {
    Json(st.store.lock().await.actions.clone())
}

async fn upsert_action(State(st): State<AppState>, Json(action): Json<Action>) -> impl IntoResponse {
    let mut store = st.store.lock().await;
    store.actions.retain(|a| !a.name.eq_ignore_ascii_case(&action.name));
    store.actions.push(action);
    let _ = store.save();
    Json(store.actions.clone())
}

async fn delete_action(State(st): State<AppState>, Path(name): Path<String>) -> impl IntoResponse {
    let mut store = st.store.lock().await;
    store.actions.retain(|a| !a.name.eq_ignore_ascii_case(&name));
    let _ = store.save();
    Json(store.actions.clone())
}

async fn list_triggers(State(st): State<AppState>) -> Json<Vec<EventTrigger>> {
    Json(st.store.lock().await.triggers.clone())
}

async fn upsert_trigger(State(st): State<AppState>, Json(t): Json<EventTrigger>) -> impl IntoResponse {
    let mut store = st.store.lock().await;
    store
        .triggers
        .retain(|x| !(x.event.eq_ignore_ascii_case(&t.event) && x.action.eq_ignore_ascii_case(&t.action)));
    store.triggers.push(t);
    let _ = store.save();
    Json(store.triggers.clone())
}

async fn delete_trigger(State(st): State<AppState>, Path(event): Path<String>) -> impl IntoResponse {
    let mut store = st.store.lock().await;
    store.triggers.retain(|x| !x.event.eq_ignore_ascii_case(&event));
    let _ = store.save();
    Json(store.triggers.clone())
}

async fn run_action_api(State(st): State<AppState>, Path(name): Path<String>) -> impl IntoResponse {
    let channel = st.store.lock().await.settings.twitch.channel.trim().to_lowercase();
    let ctx = ExecCtx {
        h: st.handles.clone(),
        store: st.store.clone(),
        user: "web".into(),
        channel,
        args: Vec::new(),
        platform: String::new(),
        extra: std::collections::HashMap::new(),
    };
    tokio::spawn(async move { action::run_named(&name, &ctx).await });
    Json(serde_json::json!({ "ok": true }))
}

async fn import_sb(State(st): State<AppState>) -> impl IntoResponse {
    match crate::import::import_streamerbot("/root/.local/lib/streamerbot/data") {
        Ok((acts, cmds, skipped)) => {
            let mut store = st.store.lock().await;
            for a in acts {
                store.actions.retain(|x| !x.name.eq_ignore_ascii_case(&a.name));
                store.actions.push(a);
            }
            for c in cmds {
                store.commands.retain(|x| !x.trigger.eq_ignore_ascii_case(&c.trigger));
                store.commands.push(c);
            }
            let _ = store.save();
            Json(serde_json::json!({ "ok": true, "actions": store.actions.len(),
                "commands": store.commands.len(), "skipped_subactions": skipped }))
        }
        Err(e) => Json(serde_json::json!({ "ok": false, "error": e.to_string() })),
    }
}

// ---------- OAuth login (two roles: broadcaster + bot) ----------

#[derive(Deserialize)]
struct RoleQ {
    #[serde(default)]
    role: String,
}

async fn twitch_login_start(
    State(st): State<AppState>,
    axum::extract::Query(rq): axum::extract::Query<RoleQ>,
) -> impl IntoResponse {
    let role = crate::auth::Role::parse(&rq.role);
    let client_id = { st.store.lock().await.settings.twitch.client_id.trim().to_string() };
    match crate::auth::twitch_device_start(&client_id, role).await {
        Ok(dev) => {
            let (uri, code, exp) = (dev.verification_uri.clone(), dev.user_code.clone(), dev.expires_in);
            // Poll in the background; tokens land in the role's slot on approval.
            let store = st.store.clone();
            tokio::spawn(async move {
                if let Err(e) = crate::auth::twitch_device_poll(store, client_id, role, dev).await {
                    tracing::error!("twitch device poll: {e:#}");
                }
            });
            Json(serde_json::json!({ "ok": true, "verification_uri": uri, "user_code": code, "expires_in": exp }))
        }
        Err(e) => Json(serde_json::json!({ "ok": false, "error": e.to_string() })),
    }
}

async fn twitch_logout(
    State(st): State<AppState>,
    axum::extract::Query(rq): axum::extract::Query<RoleQ>,
) -> impl IntoResponse {
    let mut s = st.store.lock().await;
    let slot = match crate::auth::Role::parse(&rq.role) {
        crate::auth::Role::Broadcaster => &mut s.settings.twitch.broadcaster,
        crate::auth::Role::Bot => &mut s.settings.twitch.bot,
    };
    *slot = crate::model::OAuthToken::default();
    let _ = s.save();
    Json(serde_json::json!({ "ok": true }))
}

async fn kick_logout(
    State(st): State<AppState>,
    axum::extract::Query(rq): axum::extract::Query<RoleQ>,
) -> impl IntoResponse {
    let mut s = st.store.lock().await;
    let slot = match crate::auth::Role::parse(&rq.role) {
        crate::auth::Role::Broadcaster => &mut s.settings.kick.broadcaster,
        crate::auth::Role::Bot => &mut s.settings.kick.bot,
    };
    *slot = crate::model::OAuthToken::default();
    let _ = s.save();
    Json(serde_json::json!({ "ok": true }))
}

fn origin(headers: &axum::http::HeaderMap) -> String {
    let host = headers
        .get("x-forwarded-host")
        .or_else(|| headers.get(axum::http::header::HOST))
        .and_then(|h| h.to_str().ok())
        .unwrap_or("localhost:7777");
    // Behind `tailscale serve` the browser is on https; honor the forwarded proto.
    let proto = headers
        .get("x-forwarded-proto")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("http");
    format!("{proto}://{host}")
}

#[derive(Deserialize)]
struct KickStart {
    /// The browser's own `location.origin`, so the redirect URI matches exactly
    /// what the user registered with Kick (TLS host/port included).
    #[serde(default)]
    base: String,
    #[serde(default)]
    role: String,
}

async fn kick_login_start(
    State(st): State<AppState>,
    headers: axum::http::HeaderMap,
    axum::extract::Query(q): axum::extract::Query<KickStart>,
) -> impl IntoResponse {
    let client_id = { st.store.lock().await.settings.kick.client_id.trim().to_string() };
    if client_id.is_empty() {
        return Json(serde_json::json!({ "ok": false, "error": "set the Kick client_id first" }));
    }
    let role = crate::auth::Role::parse(&q.role);
    let base = if q.base.trim().is_empty() { origin(&headers) } else { q.base.trim().trim_end_matches('/').to_string() };
    let redirect_uri = format!("{}/auth/kick/callback", base);
    let (url, pending) = crate::auth::kick_authorize_url(&client_id, &redirect_uri, role);
    *st.kick_pending.lock().await = Some(pending);
    Json(serde_json::json!({ "ok": true, "authorize_url": url }))
}

#[derive(Deserialize)]
struct KickCb {
    #[serde(default)]
    code: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    error: String,
}

async fn kick_callback(
    State(st): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<KickCb>,
) -> impl IntoResponse {
    if !q.error.is_empty() {
        return Html(format!("<p>Kick login failed: {}</p><a href=\"/\">back</a>", q.error));
    }
    let pending = { st.kick_pending.lock().await.clone() };
    let Some(pending) = pending else {
        return Html("<p>No pending Kick login. Start again from Accounts.</p><a href=\"/\">back</a>".to_string());
    };
    if pending.state != q.state {
        return Html("<p>State mismatch (possible CSRF). Try again.</p><a href=\"/\">back</a>".to_string());
    }
    let (client_id, client_secret) = {
        let s = st.store.lock().await;
        (s.settings.kick.client_id.trim().to_string(), s.settings.kick.client_secret.trim().to_string())
    };
    match crate::auth::kick_exchange_code(&st.store, &client_id, &client_secret, &q.code, &pending).await {
        Ok(()) => {
            *st.kick_pending.lock().await = None;
            Html("<p>Kick logged in. You may close this tab.</p><script>location.href='/'</script>".to_string())
        }
        Err(e) => Html(format!("<p>Kick token exchange failed: {e}</p><a href=\"/\">back</a>")),
    }
}

async fn youtube_login_start(
    State(st): State<AppState>,
    headers: axum::http::HeaderMap,
    axum::extract::Query(q): axum::extract::Query<KickStart>,
) -> impl IntoResponse {
    let client_id = { st.store.lock().await.settings.youtube.client_id.trim().to_string() };
    if client_id.is_empty() {
        return Json(serde_json::json!({ "ok": false, "error": "set the YouTube (Google) client_id first" }));
    }
    let role = crate::auth::Role::parse(&q.role);
    let base = if q.base.trim().is_empty() { origin(&headers) } else { q.base.trim().trim_end_matches('/').to_string() };
    let redirect_uri = format!("{}/auth/youtube/callback", base);
    let (url, pending) = crate::auth::google_authorize_url(&client_id, &redirect_uri, role);
    *st.yt_pending.lock().await = Some(pending);
    Json(serde_json::json!({ "ok": true, "authorize_url": url }))
}

async fn youtube_callback(
    State(st): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<KickCb>,
) -> impl IntoResponse {
    if !q.error.is_empty() {
        return Html(format!("<p>YouTube login failed: {}</p><a href=\"/\">back</a>", q.error));
    }
    let pending = { st.yt_pending.lock().await.clone() };
    let Some(pending) = pending else {
        return Html("<p>No pending YouTube login. Start again from Accounts.</p><a href=\"/\">back</a>".to_string());
    };
    if pending.state != q.state {
        return Html("<p>State mismatch (possible CSRF). Try again.</p><a href=\"/\">back</a>".to_string());
    }
    let (client_id, client_secret) = {
        let s = st.store.lock().await;
        (s.settings.youtube.client_id.trim().to_string(), s.settings.youtube.client_secret.trim().to_string())
    };
    match crate::auth::google_exchange_code(&st.store, &client_id, &client_secret, &q.code, &pending).await {
        Ok(()) => {
            *st.yt_pending.lock().await = None;
            Html("<p>YouTube logged in. You may close this tab.</p><script>location.href='/'</script>".to_string())
        }
        Err(e) => Html(format!("<p>YouTube token exchange failed: {e}</p><a href=\"/\">back</a>")),
    }
}

async fn youtube_logout(
    State(st): State<AppState>,
    axum::extract::Query(rq): axum::extract::Query<RoleQ>,
) -> impl IntoResponse {
    let mut s = st.store.lock().await;
    let slot = match crate::auth::Role::parse(&rq.role) {
        crate::auth::Role::Broadcaster => &mut s.settings.youtube.broadcaster,
        crate::auth::Role::Bot => &mut s.settings.youtube.bot,
    };
    *slot = crate::model::OAuthToken::default();
    let _ = s.save();
    Json(serde_json::json!({ "ok": true }))
}

// ---------- Channel point rewards ----------

async fn list_rewards(State(st): State<AppState>) -> impl IntoResponse {
    let mut rewards = Vec::new();
    let mut errors = Vec::new();
    match crate::rewards::twitch_list(&st.store).await {
        Ok(mut r) => rewards.append(&mut r),
        Err(e) => errors.push(format!("twitch: {e}")),
    }
    match crate::rewards::kick_list(&st.store).await {
        Ok(mut r) => rewards.append(&mut r),
        Err(e) => errors.push(format!("kick: {e}")),
    }
    Json(serde_json::json!({ "rewards": rewards, "errors": errors }))
}

async fn create_reward(
    State(st): State<AppState>,
    Json(n): Json<crate::rewards::NewReward>,
) -> impl IntoResponse {
    let want_tw = n.platform == "twitch" || n.platform == "both";
    let want_kick = n.platform == "kick" || n.platform == "both";
    if !want_tw && !want_kick {
        return Json(serde_json::json!({ "ok": false, "error": format!("unknown platform {}", n.platform) }));
    }
    // Create on each wanted platform, collecting the new reward ids.
    let mut entries: Vec<crate::model::RewardRef> = Vec::new();
    let mut errs: Vec<String> = Vec::new();
    if want_tw {
        match crate::rewards::twitch_create(&st.store, &n).await {
            Ok(id) => entries.push(crate::model::RewardRef { platform: "twitch".into(), id }),
            Err(e) => errs.push(format!("twitch: {e}")),
        }
    }
    if want_kick {
        match crate::rewards::kick_create(&st.store, &n).await {
            Ok(id) => entries.push(crate::model::RewardRef { platform: "kick".into(), id }),
            Err(e) => errs.push(format!("kick: {e}")),
        }
    }
    // "Ambas" is all-or-nothing: if one platform failed, roll back the one that succeeded
    // so we never leave an orphan reward + a bogus single-entry link.
    if n.platform == "both" && entries.len() < 2 {
        for e in &entries {
            let _ = match e.platform.as_str() {
                "twitch" => crate::rewards::twitch_delete(&st.store, &e.id).await,
                "kick" => crate::rewards::kick_delete(&st.store, &e.id).await,
                _ => Ok(()),
            };
        }
        return Json(serde_json::json!({ "ok": false, "error": errs.join(" | ") }));
    }
    if entries.is_empty() {
        return Json(serde_json::json!({ "ok": false, "error": errs.join(" | ") }));
    }
    // Store a stable link keyed by reward ids, so renames on the platform keep the binding.
    let key = format!("reward_{:08x}", rand::random::<u32>());
    {
        let mut s = st.store.lock().await;
        s.reward_links.push(crate::model::RewardLink { key: key.clone(), title: n.title.clone(), entries });
        let _ = s.save();
    }
    Json(serde_json::json!({ "ok": true, "key": key, "warn": errs.join(" | ") }))
}

async fn update_reward(
    State(st): State<AppState>,
    Path((platform, id)): Path<(String, String)>,
    Json(n): Json<crate::rewards::NewReward>,
) -> impl IntoResponse {
    let res = match platform.as_str() {
        "twitch" => crate::rewards::twitch_update(&st.store, &id, &n).await,
        "kick" => crate::rewards::kick_update(&st.store, &id, &n).await,
        p => Err(anyhow::anyhow!("unknown platform {p}")),
    };
    match res {
        Ok(()) => Json(serde_json::json!({ "ok": true })),
        Err(e) => Json(serde_json::json!({ "ok": false, "error": e.to_string() })),
    }
}

/// Enable / disable / toggle one reward on its platform (op = enable|disable|toggle).
async fn reward_op_handler(
    State(st): State<AppState>,
    Path((platform, id, op)): Path<(String, String, String)>,
) -> impl IntoResponse {
    match crate::rewards::reward_op(&st.store, &platform, &id, &op, 0).await {
        Ok(()) => Json(serde_json::json!({ "ok": true })),
        Err(e) => Json(serde_json::json!({ "ok": false, "error": e.to_string() })),
    }
}

async fn delete_reward(
    State(st): State<AppState>,
    Path((platform, id)): Path<(String, String)>,
) -> impl IntoResponse {
    let res = match platform.as_str() {
        "twitch" => crate::rewards::twitch_delete(&st.store, &id).await,
        "kick" => crate::rewards::kick_delete(&st.store, &id).await,
        p => Err(anyhow::anyhow!("unknown platform {p}")),
    };
    if res.is_ok() {
        // Drop this id from any link; remove links left with no rewards.
        let mut s = st.store.lock().await;
        for l in &mut s.reward_links {
            l.entries.retain(|e| !(e.platform == platform && e.id == id));
        }
        s.reward_links.retain(|l| !l.entries.is_empty());
        let _ = s.save();
    }
    match res {
        Ok(()) => Json(serde_json::json!({ "ok": true })),
        Err(e) => Json(serde_json::json!({ "ok": false, "error": e.to_string() })),
    }
}

async fn list_reward_links(State(st): State<AppState>) -> impl IntoResponse {
    Json(st.store.lock().await.reward_links.clone())
}

// ---------- Action queues ----------

async fn list_queues(State(st): State<AppState>) -> impl IntoResponse {
    Json(st.handles.queues.list().await)
}

#[derive(Deserialize)]
struct QueueDecl {
    name: String,
    #[serde(default)]
    blocking: bool,
}

async fn declare_queue(State(st): State<AppState>, Json(d): Json<QueueDecl>) -> impl IntoResponse {
    st.handles.queues.declare(&d.name, d.blocking).await;
    Json(st.handles.queues.list().await)
}

async fn queues_pause_all(State(st): State<AppState>) -> impl IntoResponse {
    st.handles.queues.pause_all().await;
    Json(serde_json::json!({ "ok": true }))
}

async fn queues_resume_all(State(st): State<AppState>) -> impl IntoResponse {
    st.handles.queues.resume_all().await;
    Json(serde_json::json!({ "ok": true }))
}

async fn queue_op(State(st): State<AppState>, Path((name, op)): Path<(String, String)>) -> impl IntoResponse {
    match op.as_str() {
        "pause" => st.handles.queues.pause(&name).await,
        "resume" => st.handles.queues.resume(&name).await,
        "clear" => st.handles.queues.clear(&name).await,
        _ => return Json(serde_json::json!({ "ok": false, "error": "unknown op" })),
    }
    Json(serde_json::json!({ "ok": true }))
}

// ---------- Sounds (browser-source overlay) ----------

const SOUNDS_DIR: &str = "sounds";

fn safe_name(name: &str) -> Option<String> {
    // Reject path traversal; keep a plain filename.
    if name.contains('/') || name.contains('\\') || name.contains("..") || name.trim().is_empty() {
        return None;
    }
    Some(name.to_string())
}

async fn list_sounds() -> impl IntoResponse {
    let mut names = Vec::new();
    if let Ok(rd) = std::fs::read_dir(SOUNDS_DIR) {
        for e in rd.flatten() {
            if let Some(n) = e.file_name().to_str() {
                names.push(n.to_string());
            }
        }
    }
    names.sort();
    Json(names)
}

async fn upload_sound(Path(name): Path<String>, body: axum::body::Bytes) -> impl IntoResponse {
    let Some(name) = safe_name(&name) else {
        return Json(serde_json::json!({ "ok": false, "error": "bad name" }));
    };
    if let Err(e) = std::fs::create_dir_all(SOUNDS_DIR) {
        return Json(serde_json::json!({ "ok": false, "error": e.to_string() }));
    }
    match std::fs::write(format!("{SOUNDS_DIR}/{name}"), &body) {
        Ok(()) => Json(serde_json::json!({ "ok": true, "name": name })),
        Err(e) => Json(serde_json::json!({ "ok": false, "error": e.to_string() })),
    }
}

async fn delete_sound(Path(name): Path<String>) -> impl IntoResponse {
    if let Some(name) = safe_name(&name) {
        let _ = std::fs::remove_file(format!("{SOUNDS_DIR}/{name}"));
        remove_image(&name);
    }
    Json(serde_json::json!({ "ok": true }))
}

async fn play_sound(State(st): State<AppState>, Path(name): Path<String>) -> impl IntoResponse {
    if let Some(name) = safe_name(&name) {
        let image = image_for(&name);
        let image_secs = image_secs(&name);
        let _ = st.bus.send(Event::PlaySound { file: name, volume: 1.0, image, image_secs });
    }
    Json(serde_json::json!({ "ok": true }))
}

const IMAGES_DIR: &str = "images";

const DURATIONS_FILE: &str = "durations.json";

/// Image filename bound to a sound, if any: stored as "<sound>.<ext>" in IMAGES_DIR.
pub fn image_for(sound: &str) -> Option<String> {
    let prefix = format!("{sound}.");
    std::fs::read_dir(IMAGES_DIR).ok()?.flatten().find_map(|e| {
        let n = e.file_name().to_str()?.to_string();
        (n != DURATIONS_FILE && n.starts_with(&prefix)).then_some(n)
    })
}

/// Per-sound image animation duration in seconds, from IMAGES_DIR/durations.json.
pub fn image_secs(sound: &str) -> Option<f32> {
    read_durations().get(sound).and_then(|v| v.as_f64()).map(|f| f as f32)
}

fn read_durations() -> serde_json::Map<String, serde_json::Value> {
    std::fs::read(format!("{IMAGES_DIR}/{DURATIONS_FILE}"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn write_durations(m: &serde_json::Map<String, serde_json::Value>) {
    let _ = std::fs::create_dir_all(IMAGES_DIR);
    if let Ok(s) = serde_json::to_vec(m) {
        let _ = std::fs::write(format!("{IMAGES_DIR}/{DURATIONS_FILE}"), s);
    }
}

async fn list_images() -> impl IntoResponse {
    let mut names = Vec::new();
    if let Ok(rd) = std::fs::read_dir(IMAGES_DIR) {
        for e in rd.flatten() {
            if let Some(n) = e.file_name().to_str() {
                if n != DURATIONS_FILE {
                    names.push(n.to_string());
                }
            }
        }
    }
    names.sort();
    Json(names)
}

async fn image_durations() -> impl IntoResponse {
    Json(read_durations())
}

#[derive(serde::Deserialize)]
struct ImgQuery {
    ext: String,
    secs: Option<f32>,
}

async fn upload_image(
    Path(name): Path<String>,
    axum::extract::Query(q): axum::extract::Query<ImgQuery>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    let Some(name) = safe_name(&name) else {
        return Json(serde_json::json!({ "ok": false, "error": "bad name" }));
    };
    let ext: String = q.ext.chars().filter(|c| c.is_ascii_alphanumeric()).take(8).collect();
    if ext.is_empty() {
        return Json(serde_json::json!({ "ok": false, "error": "bad ext" }));
    }
    if let Err(e) = std::fs::create_dir_all(IMAGES_DIR) {
        return Json(serde_json::json!({ "ok": false, "error": e.to_string() }));
    }
    remove_image(&name);
    if let Some(s) = q.secs {
        set_duration(&name, s);
    }
    let file = format!("{name}.{ext}");
    match std::fs::write(format!("{IMAGES_DIR}/{file}"), &body) {
        Ok(()) => Json(serde_json::json!({ "ok": true, "name": file })),
        Err(e) => Json(serde_json::json!({ "ok": false, "error": e.to_string() })),
    }
}

#[derive(serde::Deserialize)]
struct SecsBody {
    secs: f32,
}

async fn set_image_duration(Path(name): Path<String>, Json(b): Json<SecsBody>) -> impl IntoResponse {
    if let Some(name) = safe_name(&name) {
        set_duration(&name, b.secs);
    }
    Json(serde_json::json!({ "ok": true }))
}

fn set_duration(sound: &str, secs: f32) {
    let mut m = read_durations();
    m.insert(sound.to_string(), serde_json::json!(secs.max(0.0)));
    write_durations(&m);
}

async fn delete_image(Path(name): Path<String>) -> impl IntoResponse {
    if let Some(name) = safe_name(&name) {
        remove_image(&name);
    }
    Json(serde_json::json!({ "ok": true }))
}

fn remove_image(sound: &str) {
    if let Some(existing) = image_for(sound) {
        let _ = std::fs::remove_file(format!("{IMAGES_DIR}/{existing}"));
    }
    let mut m = read_durations();
    if m.remove(sound).is_some() {
        write_durations(&m);
    }
}

async fn serve_image(Path(name): Path<String>) -> impl IntoResponse {
    let Some(name) = safe_name(&name) else {
        return (axum::http::StatusCode::BAD_REQUEST, [("content-type", "text/plain")], Vec::new());
    };
    match std::fs::read(format!("{IMAGES_DIR}/{name}")) {
        Ok(bytes) => {
            let ct = match name.rsplit('.').next().unwrap_or("").to_lowercase().as_str() {
                "png" => "image/png",
                "jpg" | "jpeg" => "image/jpeg",
                "gif" => "image/gif",
                "webp" => "image/webp",
                "svg" => "image/svg+xml",
                "avif" => "image/avif",
                _ => "application/octet-stream",
            };
            (axum::http::StatusCode::OK, [("content-type", ct)], bytes)
        }
        Err(_) => (axum::http::StatusCode::NOT_FOUND, [("content-type", "text/plain")], Vec::new()),
    }
}

async fn serve_sound(Path(name): Path<String>) -> impl IntoResponse {
    let Some(name) = safe_name(&name) else {
        return (axum::http::StatusCode::BAD_REQUEST, [("content-type", "text/plain")], Vec::new());
    };
    match std::fs::read(format!("{SOUNDS_DIR}/{name}")) {
        Ok(bytes) => {
            let ct = match name.rsplit('.').next().unwrap_or("").to_lowercase().as_str() {
                "mp3" => "audio/mpeg",
                "ogg" => "audio/ogg",
                "wav" => "audio/wav",
                "m4a" | "aac" => "audio/aac",
                "webm" => "audio/webm",
                _ => "application/octet-stream",
            };
            (axum::http::StatusCode::OK, [("content-type", ct)], bytes)
        }
        Err(_) => (axum::http::StatusCode::NOT_FOUND, [("content-type", "text/plain")], Vec::new()),
    }
}

async fn overlay() -> Html<&'static str> {
    Html(OVERLAY_HTML)
}

const OVERLAY_HTML: &str = include_str!("overlay.html");

// ---------- Chat overlay (user-pasted HTML/CSS/JS, fed by SSE /events) ----------

/// The user's pasted overlay code. Kept in a gitignored file so it never lands
/// in the repo; the panel's Integrations card edits it.
const OVERLAY_CFG: &str = "overlay.json";

fn read_overlay_cfg() -> serde_json::Value {
    std::fs::read_to_string(OVERLAY_CFG)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({ "html": "", "css": "", "js": "", "ignoreUsers": "", "ignorePrefixes": "", "ignoreRewards": [] }))
}

async fn get_overlay_cfg() -> Json<serde_json::Value> {
    Json(read_overlay_cfg())
}

async fn put_overlay_cfg(Json(body): Json<serde_json::Value>) -> impl IntoResponse {
    let pick = |k: &str| body.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    // ignoreRewards is a list of reward ids (strings); keep only strings.
    let rewards: Vec<String> = body.get("ignoreRewards").and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let cfg = serde_json::json!({
        "html": pick("html"), "css": pick("css"), "js": pick("js"),
        "ignoreUsers": pick("ignoreUsers"), "ignorePrefixes": pick("ignorePrefixes"),
        "ignoreRewards": rewards,
    });
    match std::fs::write(OVERLAY_CFG, serde_json::to_vec_pretty(&cfg).unwrap_or_default()) {
        Ok(()) => Json(serde_json::json!({ "ok": true })),
        Err(e) => Json(serde_json::json!({ "ok": false, "error": e.to_string() })),
    }
}

/// Host page for the chat overlay: injects the user's HTML/CSS/JS (Streamlabs
/// style). The injected JS runs same-origin with the panel; only the user sets
/// it, over Tailscale, so it adds no privilege beyond what the user already has.
async fn overlay_chat() -> Html<String> {
    let cfg = read_overlay_cfg();
    let s = |k: &str| cfg.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    // Streamlabs themes use widget-setting placeholders the Streamlabs runtime
    // would fill; substitute the common ones (CSS only — per-message {from}/
    // {color}/{message}/{messageId} stay for the overlay runtime's template).
    // A theme can override any by writing a literal value in its CSS.
    let css = s("css")
        .replace("{text_color}", "#ffffff")
        .replace("{background_color}", "rgba(0,0,0,0)")
        .replace("{font_size}", "40px")
        .replace("{message_hide_delay}", "600s");
    // Overlay-only ignore lists (users / command prefixes / reward ids). The
    // runtime reads them to skip matching messages; the dock is unaffected.
    // Escape "<" so a user-typed value can't close the <script>.
    let filter = serde_json::json!({
        "users": cfg.get("ignoreUsers").and_then(|v| v.as_str()).unwrap_or(""),
        "prefixes": cfg.get("ignorePrefixes").and_then(|v| v.as_str()).unwrap_or(""),
        "rewards": cfg.get("ignoreRewards").cloned().unwrap_or_else(|| serde_json::json!([])),
    });
    let filter = serde_json::to_string(&filter).unwrap_or_else(|_| "{}".into()).replace('<', "\\u003c");
    // Runtime runs AFTER the user's JS so their MutationObserver is already set
    // when we start appending messages from /events into #log.
    Html(format!(
        "<!doctype html><html><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <style>{}</style></head><body>{}<script>{}</script>\
         <script>window.__ovFilter={};</script><script>{}</script></body></html>",
        css, s("html"), s("js"), filter, OVERLAY_RUNTIME
    ))
}

/// Streamlabs-chatbox emulation injected into the overlay host (see the file).
const OVERLAY_RUNTIME: &str = include_str!("overlay_runtime.js");

/// Standalone moderation dock for OBS (browser dock): reads /events, moderates
/// via /api/chat/*. Not the public overlay — it is the streamer's own view.
async fn dock_chat() -> Html<&'static str> {
    Html(include_str!("dock_chat.html"))
}

/// Standalone events dock for OBS: subs, rewards, bits, raids, follows. Consumes
/// /ws and keeps only Notice events.
async fn dock_events() -> Html<&'static str> {
    Html(include_str!("dock_events.html"))
}

/// Standalone stream-info dock for OBS: edit title + category from inside OBS.
async fn dock_streaminfo() -> Html<&'static str> {
    Html(include_str!("dock_streaminfo.html"))
}

// ---------- Chat dock (send / ban / delete) ----------

#[derive(Deserialize)]
struct ChatSend {
    text: String,
    #[serde(default)]
    targets: Vec<String>,
}

async fn chat_send(State(st): State<AppState>, Json(req): Json<ChatSend>) -> Json<serde_json::Value> {
    let text: String = req.text.replace(['\r', '\n'], " ");
    let text = text.trim();
    if text.is_empty() || text.chars().count() > 500 {
        return Json(serde_json::json!({ "ok": false, "error": "text must be 1..500 chars" }));
    }
    let mut results = serde_json::Map::new();
    // The dock sends as the BROADCASTER (not the bot): Twitch via Helix, Kick
    // via the broadcaster token, YouTube via the live chat API.
    for tgt in &req.targets {
        let r = match tgt.as_str() {
            "twitch" => crate::helix::twitch_send(&st.store, text).await,
            "kick" => crate::helix::kick_send_broadcaster(&st.store, text).await,
            "youtube" => crate::streaminfo::youtube_send(&st.store, text).await,
            _ => continue,
        };
        results.insert(tgt.clone(), match r {
            Ok(()) => serde_json::json!({ "ok": true }),
            Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
        });
    }
    Json(serde_json::json!({ "results": results }))
}

#[derive(Deserialize)]
struct ChatBan {
    platform: String,
    #[serde(default)]
    login: String,
    /// Timeout minutes; 0 = permanent ban.
    #[serde(default)]
    duration: u64,
    #[serde(default)]
    reason: String,
}

async fn chat_ban(State(st): State<AppState>, Json(r): Json<ChatBan>) -> Json<serde_json::Value> {
    let res = match r.platform.as_str() {
        // Twitch bans take seconds; Kick bans take minutes.
        "twitch" => crate::helix::ban(&st.store, &r.login, r.duration * 60, &r.reason).await,
        "kick" => crate::helix::kick_ban(&st.store, &r.login, r.duration, &r.reason).await,
        _ => Err(anyhow::anyhow!("bad platform")),
    };
    Json(match res {
        Ok(()) => serde_json::json!({ "ok": true }),
        Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
    })
}

#[derive(Deserialize)]
struct ChatDel {
    platform: String,
    id: String,
}

async fn chat_delete(State(st): State<AppState>, Json(r): Json<ChatDel>) -> Json<serde_json::Value> {
    let res = match r.platform.as_str() {
        "twitch" => crate::helix::twitch_delete_message(&st.store, &r.id).await,
        "kick" => crate::helix::kick_delete_message(&st.store, &r.id).await,
        _ => Err(anyhow::anyhow!("bad platform")),
    };
    if res.is_ok() {
        // Push the removal to the overlay + dock.
        let _ = st.bus.send(Event::ChatDelete { id: format!("{}:{}", r.platform, r.id) });
    }
    Json(match res {
        Ok(()) => serde_json::json!({ "ok": true }),
        Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
    })
}

// ---------- Stream info (title + category on Twitch + Kick) ----------

async fn streaminfo_status(State(st): State<AppState>) -> Json<serde_json::Value> {
    Json(crate::streaminfo::status(&st.store).await)
}

async fn get_streaminfo(State(st): State<AppState>) -> Json<serde_json::Value> {
    Json(crate::streaminfo::info(&st.store).await)
}

#[derive(Deserialize)]
struct CatQuery {
    q: String,
}

async fn search_categories(
    State(st): State<AppState>,
    axum::extract::Query(cq): axum::extract::Query<CatQuery>,
) -> Json<serde_json::Value> {
    let q = cq.q.trim();
    let n = q.chars().count();
    if n < 2 || n > 100 {
        return Json(serde_json::json!({ "results": [], "errors": {} }));
    }
    Json(crate::streaminfo::search(&st.store, q).await)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CatSel {
    #[serde(default)]
    twitch_id: Option<String>,
    #[serde(default)]
    kick_id: Option<String>,
}

#[derive(Deserialize)]
struct Targets {
    #[serde(default)]
    twitch: bool,
    #[serde(default)]
    kick: bool,
    #[serde(default)]
    youtube: bool,
}

#[derive(Deserialize)]
struct StreamInfoUpdate {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    category: Option<CatSel>,
    targets: Targets,
}

async fn update_streaminfo(
    State(st): State<AppState>,
    Json(req): Json<StreamInfoUpdate>,
) -> Json<serde_json::Value> {
    if !req.targets.twitch && !req.targets.kick && !req.targets.youtube {
        return Json(serde_json::json!({ "error": "no target selected" }));
    }
    let (tw_id, kick_id) = req.category.as_ref()
        .map(|c| (c.twitch_id.as_deref(), c.kick_id.as_deref()))
        .unwrap_or((None, None));
    Json(crate::streaminfo::update(&st.store, req.title.as_deref(), tw_id, kick_id, req.targets.twitch, req.targets.kick, req.targets.youtube).await)
}

async fn serve_kick_badge(Path(name): Path<String>) -> impl IntoResponse {
    let bad = || (axum::http::StatusCode::NOT_FOUND, [("content-type", "text/plain")], Vec::new());
    let Some(name) = safe_name(&name) else { return bad() };
    if !name.ends_with(".svg") {
        return bad();
    }
    match std::fs::read(format!("badges/kick/{name}")) {
        Ok(bytes) => (axum::http::StatusCode::OK, [("content-type", "image/svg+xml")], bytes),
        Err(_) => bad(),
    }
}

/// SSE feed the chat overlay (and dock) consume: `message` / `delete` / `clear`.
async fn events_sse(
    State(st): State<AppState>,
) -> axum::response::sse::Sse<impl futures_util::Stream<Item = Result<axum::response::sse::Event, std::convert::Infallible>>>
{
    use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
    let rx = st.bus.subscribe();
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    let sse = match &ev {
                        Event::ChatMsg { msg } => SseEvent::default().event("message").json_data(msg).ok(),
                        Event::ChatDelete { id } => {
                            SseEvent::default().event("delete").json_data(serde_json::json!({ "id": id })).ok()
                        }
                        Event::ChatClear => Some(SseEvent::default().event("clear").data("{}")),
                        _ => None,
                    };
                    if let Some(sse) = sse {
                        return Some((Ok(sse), rx));
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

// ---------- Native export / import (nuggtbot bundle) ----------

use base64::Engine as _;

#[derive(Deserialize, Default)]
struct ExportReq {
    /// Action names to export; empty = everything.
    #[serde(default)]
    actions: Vec<String>,
}

async fn export_code(State(st): State<AppState>, Json(req): Json<ExportReq>) -> impl IntoResponse {
    let s = st.store.lock().await;
    let bundle = if req.actions.is_empty() {
        serde_json::json!({
            "blv": 1,
            "actions": s.actions,
            "commands": s.commands,
            "triggers": s.triggers,
            "timers": s.timers,
        })
    } else {
        let set: Vec<String> = req.actions.iter().map(|a| a.to_lowercase()).collect();
        let has = |n: &str| set.iter().any(|x| x == &n.to_lowercase());
        let acts: Vec<_> = s.actions.iter().filter(|a| has(&a.name)).cloned().collect();
        let cmds: Vec<_> = s.commands.iter().filter(|c| c.action.as_deref().map_or(false, has)).cloned().collect();
        let trigs: Vec<_> = s.triggers.iter().filter(|t| has(&t.action)).cloned().collect();
        serde_json::json!({ "blv": 1, "actions": acts, "commands": cmds, "triggers": trigs, "timers": [] })
    };
    let code = base64::engine::general_purpose::STANDARD.encode(bundle.to_string());
    Json(serde_json::json!({ "ok": true, "code": format!("BLV1:{code}") }))
}

#[derive(Deserialize)]
struct ImportCodeReq {
    code: String,
}

async fn import_code(State(st): State<AppState>, Json(req): Json<ImportCodeReq>) -> impl IntoResponse {
    let raw: String = req.code.trim().trim_start_matches("BLV1:").split_whitespace().collect();
    let bytes = match base64::engine::general_purpose::STANDARD.decode(raw.as_bytes()) {
        Ok(b) => b,
        Err(e) => return Json(serde_json::json!({ "ok": false, "error": format!("bad code: {e}") })),
    };
    let v: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(e) => return Json(serde_json::json!({ "ok": false, "error": format!("bad bundle: {e}") })),
    };
    let mut store = st.store.lock().await;
    let mut na = 0;
    let mut nc = 0;
    let mut nt = 0;
    let mut ntim = 0;
    if let Some(arr) = v.get("actions").and_then(|x| x.as_array()) {
        for a in arr {
            if let Ok(action) = serde_json::from_value::<Action>(a.clone()) {
                store.actions.retain(|x| !x.name.eq_ignore_ascii_case(&action.name));
                store.actions.push(action);
                na += 1;
            }
        }
    }
    if let Some(arr) = v.get("commands").and_then(|x| x.as_array()) {
        for c in arr {
            if let Ok(cmd) = serde_json::from_value::<Command>(c.clone()) {
                store.commands.retain(|x| !x.trigger.eq_ignore_ascii_case(&cmd.trigger));
                store.commands.push(cmd);
                nc += 1;
            }
        }
    }
    if let Some(arr) = v.get("triggers").and_then(|x| x.as_array()) {
        for t in arr {
            if let Ok(tr) = serde_json::from_value::<EventTrigger>(t.clone()) {
                store.triggers.retain(|x| !(x.event.eq_ignore_ascii_case(&tr.event) && x.action.eq_ignore_ascii_case(&tr.action)));
                store.triggers.push(tr);
                nt += 1;
            }
        }
    }
    if let Some(arr) = v.get("timers").and_then(|x| x.as_array()) {
        for t in arr {
            if let Ok(tm) = serde_json::from_value::<Timer>(t.clone()) {
                store.timers.retain(|x| !x.name.eq_ignore_ascii_case(&tm.name));
                store.timers.push(tm);
                ntim += 1;
            }
        }
    }
    let _ = store.save();
    Json(serde_json::json!({ "ok": true, "actions": na, "commands": nc, "triggers": nt, "timers": ntim }))
}

async fn ws_handler(ws: WebSocketUpgrade, State(st): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| ws_loop(socket, st))
}

async fn ws_loop(mut socket: WebSocket, st: AppState) {
    let mut rx = st.bus.subscribe();
    while let Ok(event) = rx.recv().await {
        let Ok(text) = serde_json::to_string(&event) else { continue };
        if socket.send(Message::Text(text.into())).await.is_err() {
            break;
        }
    }
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

const INDEX_HTML: &str = include_str!("index.html");
