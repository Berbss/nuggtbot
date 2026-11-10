use serde::Deserialize;

/// Process-level config (not edited in the UI). Account credentials live in the
/// web-editable Settings (store), not here.
#[derive(Clone, Debug, Deserialize)]
pub struct Config {
    /// Defaults to localhost only; each user sets their own reachable address
    /// (Tailscale/LAN) in their gitignored config.toml.
    #[serde(default = "d_bind")]
    pub bind: String,
    #[serde(default = "d_data_file")]
    pub data_file: String,
    /// Address for the public Kick webhook listener (exposed via Tailscale Funnel).
    #[serde(default = "d_webhook_bind")]
    pub webhook_bind: String,
}

fn d_bind() -> String {
    "127.0.0.1:7777".into()
}

fn d_data_file() -> String {
    "data.json".into()
}

fn d_webhook_bind() -> String {
    "127.0.0.1:7778".into()
}

impl Config {
    pub fn load(path: &str) -> anyhow::Result<Self> {
        // No config.toml → localhost defaults, so a fresh clone binds only to
        // 127.0.0.1 until the user picks their own address.
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(anyhow::anyhow!("read {path}: {e}")),
        };
        Ok(toml::from_str(&text)?)
    }
}
