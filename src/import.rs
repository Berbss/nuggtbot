use crate::model::{Action, Command, SubAction};
use serde_json::Value;
use std::collections::HashMap;

/// Import a Streamer.bot `data/` directory (actions.json + commands.json) into
/// nuggtbot Actions + Commands. Clean-room: we read the exported JSON data (not
/// Streamer.bot's code) and map the sub-action TYPE codes to our own sub-actions.
/// Unsupported sub-action types are skipped and counted.
pub fn import_streamerbot(dir: &str) -> anyhow::Result<(Vec<Action>, Vec<Command>, usize)> {
    let actions_json = read_json(&format!("{dir}/actions.json"))?;
    let commands_json = read_json(&format!("{dir}/commands.json"))?;

    // command id -> trigger texts (a command's `command` field may hold several lines)
    let mut cmd_triggers: HashMap<String, Vec<String>> = HashMap::new();
    if let Some(arr) = commands_json.get("commands").and_then(|v| v.as_array()) {
        for c in arr {
            let id = c.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let txt = c.get("command").and_then(|v| v.as_str()).unwrap_or("");
            let trigs: Vec<String> = txt
                .split(['\r', '\n'])
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
            if !id.is_empty() && !trigs.is_empty() {
                cmd_triggers.insert(id, trigs);
            }
        }
    }

    let mut actions = Vec::new();
    let mut commands = Vec::new();
    let mut skipped = 0usize;

    if let Some(arr) = actions_json.get("actions").and_then(|v| v.as_array()) {
        for a in arr {
            let name = a.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if name.is_empty() {
                continue;
            }
            let mut subs = Vec::new();
            if let Some(sas) = a.get("subActions").and_then(|v| v.as_array()) {
                for s in sas {
                    match map_subaction(s) {
                        Some(x) => subs.push(x),
                        None => skipped += 1,
                    }
                }
            }
            actions.push(Action {
                name: name.clone(),
                enabled: true,
                subactions: subs,
                ..Default::default()
            });

            // Bind commands: an action trigger of type 401 references a command by id.
            if let Some(trs) = a.get("triggers").and_then(|v| v.as_array()) {
                for tr in trs {
                    if tr.get("type").and_then(|v| v.as_i64()) == Some(401) {
                        if let Some(cid) = tr.get("commandId").and_then(|v| v.as_str()) {
                            if let Some(tgs) = cmd_triggers.get(cid) {
                                for t in tgs {
                                    commands.push(Command {
                                        trigger: t.clone(),
                                        response: String::new(),
                                        action: Some(name.clone()),
                                        enabled: true,
                                        global_cooldown: 0,
                                        user_cooldown: 0,
                                        grant_type: String::new(),
                                        groups: Vec::new(),
                                        users: Vec::new(),
                                        mode: String::new(),
                                        location: String::new(),
                                        case_sensitive: false,
                                        aliases: Vec::new(),
                                        platforms: Vec::new(),
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    Ok((actions, commands, skipped))
}

fn read_json(path: &str) -> anyhow::Result<Value> {
    let text = std::fs::read_to_string(path)?;
    let text = text.trim_start_matches('\u{feff}'); // strip UTF-8 BOM
    Ok(serde_json::from_str(text)?)
}

fn s(v: &Value, key: &str) -> String {
    v.get(key).and_then(|x| x.as_str()).unwrap_or("").to_string()
}

/// Map a Streamer.bot sub-action (by its numeric `type`) to a nuggtbot SubAction.
/// Type codes inferred from exported data field shapes.
fn map_subaction(sa: &Value) -> Option<SubAction> {
    match sa.get("type").and_then(|v| v.as_i64())? {
        // send chat message (10 Twitch, 4001 Kick, 35001 YouTube) — we send to Twitch
        10 | 4001 | 35001 => Some(SubAction::SendMessage { text: s(sa, "text") }),
        // set variable
        123 => {
            let name = s(sa, "variableName");
            if name.is_empty() {
                return None;
            }
            let value = match sa.get("value") {
                Some(Value::String(v)) => v.clone(),
                Some(Value::Null) | None => String::new(),
                Some(other) => other.to_string(),
            };
            Some(SubAction::SetVariable { name, value })
        }
        // VTube Studio hotkey
        12003 => {
            let h = sa.get("hotkeyName").and_then(|v| v.as_str())
                .or_else(|| sa.get("hotkeyId").and_then(|v| v.as_str()))
                .unwrap_or("")
                .to_string();
            Some(SubAction::VtsHotkey { hotkey: h })
        }
        // Throwing System trigger
        19001 => {
            let item = sa.get("triggerName").and_then(|v| v.as_str())
                .or_else(|| sa.get("triggerId").and_then(|v| v.as_str()))
                .unwrap_or("")
                .to_string();
            Some(SubAction::Throw { item })
        }
        // OBS source visibility (state: 0 off, 1 on, 2 toggle → approximate toggle as on)
        30 => Some(SubAction::ObsSource {
            scene: s(sa, "sceneName"),
            source: s(sa, "sourceName"),
            visible: sa.get("state").and_then(|v| v.as_i64()).unwrap_or(1) != 0,
        }),
        _ => None,
    }
}
