use crate::action::{self, ExecCtx};
use crate::engine;
use crate::integrations::Handles;
use crate::model::{Event, Outgoing};
use crate::store::Store;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, Mutex};
use twitch_irc::login::StaticLoginCredentials;
use twitch_irc::message::ServerMessage;
use twitch_irc::{ClientConfig, SecureTCPTransport, TwitchIRCClient};

/// Twitch chat task: read the configured channel, publish Chat events, run commands,
/// and send Outgoing messages (from timers/commands). Reads credentials from the
/// web-editable Settings. Reconnects on error; re-reads settings each cycle.
pub fn spawn(
    store: Arc<Mutex<Store>>,
    bus: broadcast::Sender<Event>,
    mut outgoing: mpsc::UnboundedReceiver<Outgoing>,
    handles: Handles,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            if let Err(e) = run(&store, &bus, &mut outgoing, &handles).await {
                tracing::error!("twitch: {e:#}");
            }
            handles.status.twitch.store(false, std::sync::atomic::Ordering::Relaxed);
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    })
}

async fn run(
    store: &Arc<Mutex<Store>>,
    bus: &broadcast::Sender<Event>,
    outgoing: &mut mpsc::UnboundedReceiver<Outgoing>,
    handles: &Handles,
) -> anyhow::Result<()> {
    let tw = { store.lock().await.settings.twitch.clone() };
    if tw.channel.trim().is_empty() {
        // No channel configured yet; wait for the user to set it in Accounts.
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        return Ok(());
    }

    // Prefer the bot account to send (messages show the bot's name); fall back to
    // the broadcaster login; otherwise connect anonymously (read-only).
    let sender = if tw.bot.logged_in() {
        Some((tw.bot.user_login.clone(), tw.bot.access_token.clone()))
    } else if tw.broadcaster.logged_in() {
        Some((tw.broadcaster.user_login.clone(), tw.broadcaster.access_token.clone()))
    } else {
        None
    };
    let can_send = sender.is_some();
    let creds = match &sender {
        Some((login, token)) => {
            let token = token.trim_start_matches("oauth:").to_string();
            StaticLoginCredentials::new(login.clone(), Some(token))
        }
        None => StaticLoginCredentials::anonymous(),
    };

    let client_config = ClientConfig::new_simple(creds);
    let (mut incoming, client) =
        TwitchIRCClient::<SecureTCPTransport, StaticLoginCredentials>::new(client_config);

    let active_token = sender.as_ref().map(|(_, t)| t.clone()).unwrap_or_default();
    let mut relogin_check = tokio::time::interval(std::time::Duration::from_secs(15));
    relogin_check.tick().await; // consume immediate first tick

    let channel = tw.channel.trim().to_lowercase();
    client.join(channel.clone())?;
    let _ = bus.send(Event::System {
        text: format!(
            "twitch: joined #{channel}{}",
            if can_send { "" } else { " (read-only, no bot token)" }
        ),
    });
    handles.status.twitch.store(true, std::sync::atomic::Ordering::Relaxed);

    // Per-user recent message ids so a CLEARCHAT (ban/timeout) can delete that
    // user's messages from the overlay. Bounded to keep memory flat.
    let mut user_msgs: std::collections::HashMap<String, std::collections::VecDeque<String>> =
        std::collections::HashMap::new();

    loop {
        tokio::select! {
            msg = incoming.recv() => {
                let Some(msg) = msg else { break };
                match msg {
                    ServerMessage::Privmsg(m) => {
                        let _ = bus.send(Event::Chat {
                            platform: "twitch".into(),
                            user: m.sender.login.clone(),
                            text: m.message_text.clone(),
                        });
                        // Rich message for the chat overlay + dock.
                        {
                            let rich = crate::chat::twitch_msg(&m);
                            let from = rich["from"].as_str().unwrap_or("");
                            let parts = rich["parts"].as_array().cloned().unwrap_or_default();
                            let badges: Vec<String> = rich["badges"].as_array()
                                .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                                .unwrap_or_default();
                            if !crate::chat::should_hide(from, &parts, &badges, "") {
                                let _ = bus.send(Event::ChatMsg { msg: rich });
                            }
                            let ids = user_msgs.entry(m.sender.id.clone()).or_default();
                            ids.push_back(m.message_id.clone());
                            while ids.len() > 20 { ids.pop_front(); }
                            if user_msgs.len() > 1000 { user_msgs.clear(); }
                        }
                        let (commands, variables, user_groups) = {
                            let s = store.lock().await;
                            (s.commands.clone(), s.variables.clone(), s.user_groups.clone())
                        };
                        if let Some(cmd) = engine::match_command(&commands, &m.message_text, "twitch")
                            .filter(|c| c.enabled)
                        {
                            let roles = roles_from_badges(&m.badges);
                            let is_broadcaster = roles.iter().any(|r| r == "broadcaster");
                            let args: Vec<String> =
                                m.message_text.split_whitespace().skip(1).map(|s| s.to_string()).collect();
                            if !crate::cooldown::permitted(cmd, &m.sender.login, &roles, &user_groups) {
                                tracing::debug!("command {} denied for {}", cmd.trigger, m.sender.login);
                            } else if let Some(rem) = handles
                                .cooldowns
                                .check(&cmd.trigger, &m.sender.login, cmd.global_cooldown, cmd.user_cooldown, is_broadcaster)
                                .await
                            {
                                tracing::debug!("command {} on cooldown ({rem}s) for {}", cmd.trigger, m.sender.login);
                            } else if let Some(action_name) = cmd.action.clone().filter(|a| !a.is_empty()) {
                                tracing::info!("cmd '{}' by {} → action '{}'", cmd.trigger, m.sender.login, action_name);
                                let mut extra = std::collections::HashMap::new();
                                extra.insert("userId".to_string(), m.sender.id.clone());
                                if let Some(c) = &m.name_color {
                                    extra.insert("userColor".to_string(), format!("#{:02X}{:02X}{:02X}", c.r, c.g, c.b));
                                }
                                let ctx = ExecCtx {
                                    h: handles.clone(),
                                    store: store.clone(),
                                    user: m.sender.login.clone(),
                                    channel: channel.clone(),
                                    args,
                                    platform: "twitch".into(),
                                    extra,
                                };
                                tokio::spawn(async move { action::run_named(&action_name, &ctx).await });
                            } else {
                                let mut vars = variables.clone();
                                vars.insert("userId".into(), m.sender.id.clone());
                                if let Some(c) = &m.name_color { vars.insert("userColor".into(), format!("#{:02X}{:02X}{:02X}", c.r, c.g, c.b)); }
                                let reply = engine::render(&cmd.response, &m.sender.login, &args, &vars);
                                if can_send {
                                    if let Err(e) = client.say(channel.clone(), reply).await {
                                        tracing::error!("twitch say: {e}");
                                    }
                                } else {
                                    tracing::info!("(read-only) would reply: {reply}");
                                }
                            }
                        }
                        if let Some(bits) = m.bits {
                            fire_event("cheer", &m.sender.login,
                                format!("cheered {bits} bits"), store, bus, handles, &channel).await;
                        }
                    }
                    ServerMessage::UserNotice(n) => {
                        // event_id is the raw msg-id: sub, resub, subgift, raid, submysterygift, …
                        fire_event(&n.event_id, &n.sender.login, n.system_message.clone(),
                            store, bus, handles, &channel).await;
                    }
                    ServerMessage::ClearMsg(c) => {
                        // A single deleted message.
                        let _ = bus.send(Event::ChatDelete { id: format!("twitch:{}", c.message_id) });
                    }
                    ServerMessage::ClearChat(c) => {
                        use twitch_irc::message::ClearChatAction::*;
                        match c.action {
                            ChatCleared => { let _ = bus.send(Event::ChatClear); }
                            UserBanned { user_id, .. } | UserTimedOut { user_id, .. } => {
                                if let Some(ids) = user_msgs.remove(&user_id) {
                                    for id in ids {
                                        let _ = bus.send(Event::ChatDelete { id: format!("twitch:{id}") });
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            _ = relogin_check.tick() => {
                // Reconnect if the login or channel changed in the web UI.
                let (cur_token, cur_channel) = {
                    let s = store.lock().await;
                    let tw = &s.settings.twitch;
                    let t = if tw.bot.logged_in() { tw.bot.access_token.clone() }
                        else if tw.broadcaster.logged_in() { tw.broadcaster.access_token.clone() }
                        else { String::new() };
                    (t, tw.channel.trim().to_lowercase())
                };
                if cur_token != active_token || cur_channel != channel {
                    tracing::info!("twitch: login/channel changed, reconnecting");
                    return Ok(());
                }
            }
            out = outgoing.recv() => {
                let Some(out) = out else { break };
                if can_send {
                    let ch = if out.channel.is_empty() { channel.clone() } else { out.channel };
                    if let Err(e) = client.say(ch, out.text).await {
                        tracing::error!("twitch say: {e}");
                    }
                }
            }
        }
    }

    Ok(())
}

/// Map Twitch chat badges to permission roles (lowercase). "everyone" is implied.
fn roles_from_badges(badges: &[twitch_irc::message::Badge]) -> Vec<String> {
    let mut roles = vec!["everyone".to_string()];
    for b in badges {
        match b.name.as_str() {
            "broadcaster" => roles.push("broadcaster".into()),
            "moderator" => roles.push("moderator".into()),
            "vip" => roles.push("vip".into()),
            "subscriber" | "founder" => roles.push("subscriber".into()),
            _ => {}
        }
    }
    roles
}

/// Publish a platform event to the feed and run any Action bound to its kind.
pub async fn fire_event(
    kind: &str,
    user: &str,
    text: String,
    store: &Arc<Mutex<Store>>,
    bus: &broadcast::Sender<Event>,
    handles: &Handles,
    channel: &str,
) {
    let _ = bus.send(Event::Notice {
        kind: kind.to_string(),
        user: user.to_string(),
        text,
    });
    run_triggers(kind, user, store, handles, channel).await;
}

/// Run every EventTrigger bound to `kind` (no feed Notice). Used to fire extra
/// event names (e.g. a reward's platform-qualified key) without a second notice.
pub async fn run_triggers(
    kind: &str,
    user: &str,
    store: &Arc<Mutex<Store>>,
    handles: &Handles,
    channel: &str,
) {
    let triggers = { store.lock().await.triggers.clone() };
    for t in triggers {
        if t.event.eq_ignore_ascii_case(kind) {
            let ctx = ExecCtx {
                h: handles.clone(),
                store: store.clone(),
                user: user.to_string(),
                channel: channel.to_string(),
                args: Vec::new(),
                platform: String::new(),
                extra: std::collections::HashMap::new(),
            };
            let action_name = t.action.clone();
            tokio::spawn(async move { action::run_named(&action_name, &ctx).await });
        }
    }
}
