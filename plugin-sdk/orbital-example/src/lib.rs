//! Orbital Live — reference nuggtbot plugin.
//!
//! Fires an in-game catastrophe (via the Horizon/Minecraft mod's HTTP server)
//! when a viewer redeems a reward. The mod runs the server; this plugin is only
//! a client that does `GET http://<host>:<port>/fire?kind=..&by=..[&token=..]`.
//!
//! Build:  `cargo build --release`  → `target/release/liborbital.so`
//! Install: copy that file into the bot's `plugins/` folder, then click
//!          "Reiniciar bot" in the panel. Config is edited from the card.
use std::time::Duration;

use serde_json::{json, Value};

/// The 15 catastrophes the mod accepts (lowercase kind strings).
const KINDS: &[&str] = &[
    "plasma", "godly", "blackhole", "meteor", "mothership", "downfall", "planetkiller",
    "crystalbloom", "sinkhole", "payload", "nuke", "rod", "supercell", "hurricane", "tsunami",
];

/// The panel card + the sub-actions shown in the Actions editor.
fn manifest() -> Value {
    let kinds: Vec<&str> = KINDS.to_vec();
    json!({
        "name": "orbital",
        "card": {
            "title": "Orbital Live",
            "color": "#f0795e",
            "icon": r#"<circle cx="16.5" cy="7.5" r="3.3"/><path d="M14.1 9.9 4 20M9 6l-2.5 2.5M12.5 9.5 10 12"/>"#,
            "fields": [
                { "key": "host",  "label": "Host (Tailscale)", "placeholder": "100.x.y.z ou nome.ts.net", "kind": "text",   "options": [] },
                { "key": "port",  "label": "Port",             "placeholder": "8899",                      "kind": "number", "options": [] },
                { "key": "token", "label": "Token (opcional)", "placeholder": "só se definiu /orbital live token", "kind": "text", "options": [] }
            ],
            "has_switch": true,
            "has_status": true
        },
        "actions": [ {
            "name": "fire",
            "label": "orbital_fire",
            "doc": "Dispara uma catástrofe no jogo (Orbital Live) na posição do streamer. by = quem resgatou.",
            "fields": [ { "key": "catastrophe", "label": "catastrophe", "placeholder": "", "kind": "select", "options": kinds } ]
        } ]
    })
}

fn cfg_str(cfg: &Value, key: &str) -> String {
    cfg.get(key).and_then(|v| v.as_str()).unwrap_or("").trim().to_string()
}
fn cfg_port(cfg: &Value) -> u16 {
    cfg.get("port")
        .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())))
        .filter(|p| *p > 0)
        .unwrap_or(8899) as u16
}

/// ctx = `{ action, user, params:{catastrophe}, config:{enabled,host,port,token} }`.
fn run(ctx: Value) -> Value {
    let cfg = ctx.get("config").cloned().unwrap_or(Value::Null);
    if !cfg.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false) {
        return json!({ "ok": true }); // disabled → silently skip
    }
    let host = cfg_str(&cfg, "host");
    if host.is_empty() {
        return json!({ "ok": false, "error": "host not set" });
    }
    let kind = ctx
        .get("params")
        .and_then(|p| p.get("catastrophe"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if !KINDS.contains(&kind.as_str()) {
        return json!({ "ok": false, "error": format!("unknown kind {kind}") });
    }
    let user = ctx.get("user").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let token = cfg_str(&cfg, "token");
    let mut query: Vec<(&str, String)> = vec![("kind", kind.clone()), ("by", user)];
    if !token.is_empty() {
        query.push(("token", token));
    }
    let url = format!("http://{host}:{}/fire", cfg_port(&cfg));
    let client = match reqwest::blocking::Client::builder().timeout(Duration::from_secs(5)).build() {
        Ok(c) => c,
        Err(e) => return json!({ "ok": false, "error": e.to_string() }),
    };
    match client.get(&url).query(&query).send() {
        Ok(r) if !r.status().is_success() => json!({ "ok": false, "error": format!("fire {kind}: {}", r.status()) }),
        Ok(_) => json!({ "ok": true }),
        Err(e) => json!({ "ok": false, "error": format!("game offline? {e}") }),
    }
}

/// Reachability for the card badge: GET `/` within 2s (the mod replies 404).
fn status(cfg: Value) -> Value {
    let on = cfg.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);
    let host = cfg_str(&cfg, "host");
    let reachable = on
        && !host.is_empty()
        && reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .ok()
            .and_then(|c| c.get(format!("http://{host}:{}/", cfg_port(&cfg))).send().ok())
            .is_some();
    json!({ "enabled": on, "reachable": reachable })
}

plugin_sdk::export_plugin!(manifest = manifest(), run = run, status = status);
