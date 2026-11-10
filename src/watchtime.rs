//! Watchtime tracker: every minute, add 1 to each current chatter's watchtime
//! (minutes) in the per-user variable `watchtime`. Read it with the
//! get_user_variable sub-action (name="watchtime").

use crate::store::Store;
use std::sync::Arc;
use tokio::sync::Mutex;

pub fn spawn(store: Arc<Mutex<Store>>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            let logged_in = { store.lock().await.settings.twitch.broadcaster.logged_in() };
            if !logged_in {
                continue;
            }
            let chatters = match crate::helix::get_chatters(&store).await {
                Ok(c) => c,
                Err(e) => { tracing::debug!("watchtime chatters: {e}"); continue; }
            };
            if chatters.is_empty() {
                continue;
            }
            let mut s = store.lock().await;
            let map = s.user_variables.entry("watchtime".into()).or_default();
            for u in chatters {
                let cur = map.get(&u).and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
                map.insert(u, (cur + 1).to_string());
            }
            let _ = s.save();
        }
    })
}
