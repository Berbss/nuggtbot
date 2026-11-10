//! Clip watcher: polls Twitch Helix for new clips (covers both `!clip` via the
//! twitch_clip sub-action and clips made directly on Twitch) and fires a
//! `clipcreated` event with the clip link. Bind an action to `clipcreated` and
//! post `%clipUrl%` to Discord. Variables set before firing: clipUrl, clipCreator,
//! clipTitle, clipId.

use crate::integrations::Handles;
use crate::model::Event;
use crate::store::Store;
use crate::twitch::fire_event;
use chrono::{SecondsFormat, Utc};
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex};

pub fn spawn(
    store: Arc<Mutex<Store>>,
    bus: broadcast::Sender<Event>,
    handles: Handles,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut seen: HashSet<String> = HashSet::new();
        let mut seeded = false;
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(15)).await;
            let (logged_in, channel) = {
                let s = store.lock().await;
                (s.settings.twitch.broadcaster.logged_in(), s.settings.twitch.channel.trim().to_lowercase())
            };
            if !logged_in {
                continue;
            }
            // Look back 1 hour; dedup by clip id.
            let started = (Utc::now() - chrono::Duration::hours(1)).to_rfc3339_opts(SecondsFormat::Secs, true);
            let clips = match crate::helix::get_clips(&store, &started).await {
                Ok(c) => c,
                Err(e) => { tracing::debug!("clips poll: {e}"); continue; }
            };
            if !seeded {
                // First pass: remember existing clips without firing.
                for c in &clips {
                    seen.insert(c.id.clone());
                }
                seeded = true;
                continue;
            }
            for c in clips {
                if c.id.is_empty() || seen.contains(&c.id) {
                    continue;
                }
                seen.insert(c.id.clone());
                {
                    let mut s = store.lock().await;
                    s.variables.insert("clipUrl".into(), c.url.clone());
                    s.variables.insert("clipCreator".into(), c.creator.clone());
                    s.variables.insert("clipTitle".into(), c.title.clone());
                    s.variables.insert("clipId".into(), c.id.clone());
                    let _ = s.save();
                }
                fire_event("clipcreated", &c.creator, c.url.clone(), &store, &bus, &handles, &channel).await;
            }
        }
    })
}
