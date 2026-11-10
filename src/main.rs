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

use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, Mutex};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "nuggtbot=info".into()),
        )
        .init();

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
