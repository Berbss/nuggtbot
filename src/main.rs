// Desktop (tray) builds on Windows are GUI apps: no console window. Headless/
// server builds (no `tray` feature) keep the console subsystem.
#![cfg_attr(all(windows, feature = "tray"), windows_subsystem = "windows")]

mod action;
mod auth;
mod chat;
mod clips;
mod config;
mod cooldown;
mod engine;
mod eventsub;
mod helix;
mod rewards;
mod import;
mod integrations;
mod kick;
mod model;
mod obs;
mod plugin;
mod queue;
mod store;
mod streaminfo;
mod throwing;
mod timers;
mod twitch;
mod vts;
mod watchtime;
mod web;
mod youtube;
#[cfg(feature = "tray")]
mod tray;

use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, Mutex};

/// Per-user data dir for the desktop (tray) build: `%APPDATA%\nuggtbot` on Windows,
/// `$XDG_DATA_HOME/nuggtbot` (default `~/.local/share/nuggtbot`) elsewhere.
/// `NUGGTBOT_HOME` overrides it. Config, data, secrets, plugins and the log live
/// here, so the app works no matter where the .exe/AppImage was launched from.
#[cfg(feature = "tray")]
fn desktop_data_dir() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    if let Some(p) = std::env::var_os("NUGGTBOT_HOME") {
        return Some(PathBuf::from(p));
    }
    #[cfg(windows)]
    let base = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(not(windows))]
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")));
    base.map(|b| b.join("nuggtbot"))
}

/// Log to `nuggtbot.log` in the data dir when there is no console (tray mode);
/// otherwise log to stderr. shortcut: the log file is append-only and never rotated,
/// add rotation if it grows large.
fn init_logging(to_file: bool) {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "nuggtbot=info".into());
    let builder = tracing_subscriber::fmt().with_env_filter(filter);
    if to_file {
        if let Ok(f) = std::fs::OpenOptions::new().create(true).append(true).open("nuggtbot.log") {
            builder.with_ansi(false).with_writer(std::sync::Mutex::new(f)).init();
            return;
        }
    }
    builder.init();
}

fn main() -> anyhow::Result<()> {
    // --headless (or NUGGTBOT_HEADLESS) forces server-only mode (systemd/servers).
    let headless = std::env::args().any(|a| a == "--headless")
        || std::env::var_os("NUGGTBOT_HEADLESS").is_some();

    #[cfg(feature = "tray")]
    let tray_mode = !headless && tray::display_available();
    #[cfg(not(feature = "tray"))]
    let tray_mode = {
        let _ = headless; // only the tray build branches on it
        false
    };

    #[cfg(feature = "tray")]
    if tray_mode {
        // Everything relative (config.toml, data.json, plugins/, ...) now resolves here.
        if let Some(dir) = desktop_data_dir() {
            std::fs::create_dir_all(&dir)?;
            std::env::set_current_dir(&dir)?;
        }
    }
    init_logging(tray_mode);

    #[cfg(feature = "tray")]
    if tray_mode {
        // Run the async app on a background thread; the tray owns the main thread.
        let bind = config::Config::load("config.toml")
            .map(|c| c.bind)
            .unwrap_or_else(|_| "127.0.0.1:7777".into());
        let url = tray::panel_url(&bind);
        std::thread::Builder::new()
            .name("nuggtbot-server".into())
            .spawn(|| {
                if let Err(e) = run_server_blocking() {
                    tracing::error!("server exited: {e}");
                    std::process::exit(1);
                }
            })?;
        tray::run(url); // blocks on the main thread; exits the process on Quit
        return Ok(());
    }
    run_server_blocking()
}

fn run_server_blocking() -> anyhow::Result<()> {
    tokio::runtime::Runtime::new()?.block_on(run_server())
}

async fn run_server() -> anyhow::Result<()> {
    // Load dynamic plugins from `plugins/` once, before anything can dispatch one.
    plugin::load_all();

    let cfg = config::Config::load("config.toml")?;
    let store = Arc::new(Mutex::new(store::Store::load(&cfg.data_file)?));
    let (bus, _rx) = broadcast::channel::<model::Event>(1024);

    let (out_tx, out_rx) = mpsc::unbounded_channel::<model::Outgoing>();
    let (obs_tx, obs_rx) = mpsc::channel::<integrations::ObsCmd>(64);
    let (vts_tx, vts_rx) = mpsc::channel::<integrations::VtsCmd>(64);
    let (throw_tx, throw_rx) = mpsc::channel::<integrations::ThrowCmd>(64);

    let status = integrations::ConnStatus::default();
    {
        // Seed desired-connection state from the persisted "auto connect" flags.
        use std::sync::atomic::Ordering::Relaxed;
        let s = store.lock().await;
        status.obs_want.store(s.settings.obs.enabled, Relaxed);
        status.vts_want.store(s.settings.vts.enabled, Relaxed);
        status.throw_want.store(s.settings.throwing.enabled, Relaxed);
    }
    let handles = integrations::Handles {
        outgoing: out_tx.clone(),
        obs: obs_tx,
        vts: vts_tx,
        throw: throw_tx,
        queues: queue::QueueManager::new(),
        cooldowns: cooldown::Cooldowns::new(),
        bus: bus.clone(),
        status: status.clone(),
    };

    let _obs = obs::spawn(store.clone(), bus.clone(), obs_rx, status.obs.clone(), status.obs_want.clone());
    let _vts = vts::spawn(store.clone(), bus.clone(), vts_rx, status.vts.clone(), status.vts_want.clone());
    let _throwing = throwing::spawn(store.clone(), bus.clone(), throw_rx, status.throw.clone(), status.throw_want.clone());
    let _twitch = twitch::spawn(store.clone(), bus.clone(), out_rx, handles.clone());
    let _youtube = youtube::spawn(store.clone(), bus.clone(), handles.clone(), status.youtube.clone());
    let _kick = kick::spawn(store.clone(), bus.clone(), handles.clone(), cfg.webhook_bind.clone());
    let _timers = timers::spawn(store.clone(), out_tx.clone());
    let _refresher = auth::spawn_refresher(store.clone());
    {
        let store = store.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
                if let Err(e) = store.lock().await.backup("backups", 50) {
                    tracing::warn!("auto backup: {e}");
                }
            }
        });
    }
    {
        // Fire a "startup" event once, so actions can run on boot.
        let store = store.clone();
        let bus = bus.clone();
        let handles = handles.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            let channel = store.lock().await.settings.twitch.channel.trim().to_lowercase();
            twitch::fire_event("startup", "", String::new(), &store, &bus, &handles, &channel).await;
        });
    }
    let _eventsub = eventsub::spawn(store.clone(), bus.clone(), handles.clone());
    // Kick redemptions arrive via the webhook event channel.reward.redemption.updated
    // (reliable for rewards owned by any app); the poll API only sees owned rewards.
    let _clips = clips::spawn(store.clone(), bus.clone(), handles.clone());
    let _watchtime = watchtime::spawn(store.clone());
    let _chat_emotes = chat::spawn_refresh(store.clone());

    let state = web::AppState {
        store: store.clone(),
        bus: bus.clone(),
        cfg: cfg.clone(),
        handles,
        kick_pending: Arc::new(Mutex::new(None)),
        yt_pending: Arc::new(Mutex::new(None)),
    };
    web::serve(cfg, state).await?;
    Ok(())
}
