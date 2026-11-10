use crate::model::{Action, Command, EventTrigger, Quote, RewardLink, Settings, Timer};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// JSON-file persistence for everything the web UI edits.
/// shortcut: JSON store; move to SQLite once the Actions/Variables schema settles.
#[derive(Default, Serialize, Deserialize)]
pub struct Store {
    #[serde(default)]
    pub commands: Vec<Command>,
    #[serde(default)]
    pub actions: Vec<Action>,
    #[serde(default)]
    pub triggers: Vec<EventTrigger>,
    #[serde(default)]
    pub timers: Vec<Timer>,
    #[serde(default)]
    pub variables: BTreeMap<String, String>,
    #[serde(default)]
    pub quotes: Vec<Quote>,
    #[serde(default)]
    pub quote_seq: u64,
    /// Custom user groups: group name → member logins (lowercased).
    #[serde(default)]
    pub user_groups: BTreeMap<String, Vec<String>>,
    /// Per-user variables: variable name → (user login → value). For counters,
    /// leaderboards, etc.
    #[serde(default)]
    pub user_variables: BTreeMap<String, BTreeMap<String, String>>,
    /// Cross-platform reward links (same reward on Twitch+Kick → one trigger key).
    #[serde(default)]
    pub reward_links: Vec<RewardLink>,
    /// Account credentials + connection config live in a SEPARATE file
    /// (secrets.json beside data_file), never written into data.json, so the
    /// repo/data.json can be shared without leaking tokens. Still deserialized
    /// here so a pre-split data.json migrates its embedded settings on first load.
    #[serde(default, skip_serializing)]
    pub settings: Settings,
    /// Recent Kick chat message ids (newest last), for `kick_clear_chat` which has
    /// to delete messages one by one (Kick has no bulk clear). Not persisted.
    #[serde(skip)]
    pub kick_chat_ids: std::collections::VecDeque<String>,
    /// Active YouTube live-chat id (set by the youtube poll task while live), so
    /// yt_send knows where to post. Not persisted.
    #[serde(skip)]
    pub yt_chat_id: String,
    #[serde(skip)]
    file: String,
    #[serde(skip)]
    secrets_file: String,
}

/// Credentials file path: "secrets.json" beside the data file.
fn secrets_path(data_file: &str) -> String {
    std::path::Path::new(data_file)
        .with_file_name("secrets.json")
        .to_string_lossy()
        .into_owned()
}

impl Store {
    pub fn load(file: &str) -> anyhow::Result<Self> {
        let mut store = match std::fs::read_to_string(file) {
            Ok(text) => serde_json::from_str::<Store>(&text)?,
            Err(_) => Store::default(),
        };
        store.file = file.to_string();
        store.secrets_file = secrets_path(file);
        // secrets.json is the source of truth for credentials; if it exists it wins
        // over anything still embedded in a pre-split data.json.
        if let Ok(text) = std::fs::read_to_string(&store.secrets_file) {
            store.settings = serde_json::from_str(&text)?;
        }
        // Persist once so the split is applied: secrets.json written, data.json
        // rewritten without the settings block (migration for existing installs).
        store.save()?;
        Ok(store)
    }

    pub fn save(&self) -> anyhow::Result<()> {
        std::fs::write(&self.file, serde_json::to_string_pretty(self)?)?;
        std::fs::write(&self.secrets_file, serde_json::to_string_pretty(&self.settings)?)?;
        Ok(())
    }

    /// Write a timestamped snapshot into `dir`, keeping at most `keep` newest.
    pub fn backup(&self, dir: &str, keep: usize) -> anyhow::Result<String> {
        std::fs::create_dir_all(dir)?;
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let name = format!("data-{ts}.json");
        std::fs::write(format!("{dir}/{name}"), serde_json::to_string_pretty(self)?)?;
        // Prune oldest backups beyond `keep`.
        let mut files: Vec<String> = std::fs::read_dir(dir)
            .map(|rd| {
                rd.flatten()
                    .filter_map(|e| e.file_name().to_str().map(String::from))
                    .filter(|n| n.starts_with("data-") && n.ends_with(".json"))
                    .collect()
            })
            .unwrap_or_default();
        files.sort(); // lexical sort == chronological (fixed-width unix secs)
        while files.len() > keep {
            let old = files.remove(0);
            let _ = std::fs::remove_file(format!("{dir}/{old}"));
        }
        Ok(name)
    }

    /// Replace all data (not the file path) with another store's, then save.
    pub fn replace_data(&mut self, other: Store) -> anyhow::Result<()> {
        self.commands = other.commands;
        self.actions = other.actions;
        self.triggers = other.triggers;
        self.timers = other.timers;
        self.variables = other.variables;
        self.quotes = other.quotes;
        self.quote_seq = other.quote_seq;
        self.user_groups = other.user_groups;
        self.user_variables = other.user_variables;
        self.reward_links = other.reward_links;
        // Credentials (settings) are kept in secrets.json and intentionally NOT
        // replaced here: a restored/imported bundle no longer carries them, so
        // keeping the current settings avoids wiping the user's tokens.
        self.save()
    }
}
