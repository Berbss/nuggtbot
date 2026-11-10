use crate::engine;
use crate::integrations::{Handles, ObsCmd, ThrowCmd, VtsCmd};
use crate::model::{Action, Outgoing, SubAction};
use crate::store::Store;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Everything a running Action needs.
#[derive(Clone)]
pub struct ExecCtx {
    pub h: Handles,
    pub store: Arc<Mutex<Store>>,
    pub user: String,
    pub channel: String,
    pub args: Vec<String>,
    /// Platform the trigger came from ("twitch", "kick", or "" = default twitch).
    pub platform: String,
    /// Event-specific variables (e.g. userId, userColor, months) available as
    /// `%name%` in text, on top of the global variables.
    pub extra: std::collections::HashMap<String, String>,
}

/// Look up an Action by name and enqueue it (respecting its queue + flags).
pub async fn run_named(name: &str, ctx: &ExecCtx) {
    let action = {
        ctx.store
            .lock()
            .await
            .actions
            .iter()
            .find(|a| a.name.eq_ignore_ascii_case(name))
            .cloned()
    };
    match action {
        Some(a) if !a.enabled => tracing::debug!("action disabled: {name}"),
        Some(a) => ctx.h.queues.enqueue(a, ctx.clone()).await,
        None => tracing::warn!("action not found: {name}"),
    }
}

/// Execute an Action's sub-actions. Called by the queue worker. Honors `random`
/// (run one weighted-less random sub-action) and `concurrent` (all at once).
pub async fn run_action(action: &Action, ctx: &ExecCtx) {
    let subs = &action.subactions;
    if subs.is_empty() {
        return;
    }
    if action.random {
        use rand::Rng;
        let i = rand::thread_rng().gen_range(0..subs.len());
        run_subaction(&subs[i], ctx).await;
        return;
    }
    if action.concurrent {
        // All at once; Delay is meaningless here, so skip it.
        let futs = subs
            .iter()
            .filter(|sa| !matches!(sa, SubAction::Delay { .. }))
            .map(|sa| run_subaction(sa, ctx));
        futures_util::future::join_all(futs).await;
        return;
    }
    run_subs(subs, ctx).await;
}

/// Control flow a sub-action can request from the executor.
enum Flow {
    Continue,
    Stop,
    Skip(usize),
    /// Break the enclosing loop.
    Break,
    /// Skip to the next iteration of the enclosing loop.
    LoopContinue,
}

/// Run a list of sub-actions in order. Returns Stop if the list stopped the action
/// (so a Stop inside a block propagates to the whole action).
#[async_recursion::async_recursion]
async fn run_subs(subs: &[SubAction], ctx: &ExecCtx) -> Flow {
    let mut i = 0;
    while i < subs.len() {
        match run_subaction(&subs[i], ctx).await {
            Flow::Continue => i += 1,
            Flow::Skip(n) => i += 1 + n,
            other => return other, // Stop / Break / LoopContinue propagate up
        }
    }
    Flow::Continue
}

/// Run one sub-action and say how the action should proceed.
#[async_recursion::async_recursion]
async fn run_subaction(sa: &SubAction, ctx: &ExecCtx) -> Flow {
    match sa {
        SubAction::SendMessage { text } => {
            let out = render_arg(ctx, text).await;
            let _ = ctx.h.outgoing.send(Outgoing { channel: ctx.channel.clone(), text: out });
        }
        SubAction::Reply { text } => {
            let out = render_arg(ctx, text).await;
            if ctx.platform.eq_ignore_ascii_case("kick") {
                if let Err(e) = crate::helix::kick_send(&ctx.store, &out).await {
                    tracing::error!("kick reply: {e}");
                }
            } else if ctx.platform.eq_ignore_ascii_case("youtube") {
                if let Err(e) = crate::youtube::yt_send(&ctx.store, &out).await {
                    tracing::error!("youtube reply: {e}");
                }
            } else {
                let _ = ctx.h.outgoing.send(Outgoing { channel: ctx.channel.clone(), text: out });
            }
        }
        SubAction::Delay { ms } => {
            tokio::time::sleep(std::time::Duration::from_millis(*ms)).await;
        }
        SubAction::SetVariable { name, value } => {
            let vars = { ctx.store.lock().await.variables.clone() };
            let val = engine::render(value, &ctx.user, &ctx.args, &vars);
            let mut s = ctx.store.lock().await;
            s.variables.insert(name.clone(), val);
            let _ = s.save();
        }
        SubAction::ObsScene { scene } => {
            let _ = ctx.h.obs.try_send(ObsCmd::Scene(scene.clone()));
        }
        SubAction::ObsSource { scene, source, visible } => {
            let _ = ctx.h.obs.try_send(ObsCmd::Source {
                scene: scene.clone(),
                source: source.clone(),
                visible: *visible,
            });
        }
        SubAction::ObsRecordToggle => {
            let _ = ctx.h.obs.try_send(ObsCmd::RecordToggle);
        }
        SubAction::ObsFilter { source, filter, visible } => {
            let _ = ctx.h.obs.try_send(ObsCmd::Filter { source: source.clone(), filter: filter.clone(), enabled: *visible });
        }
        SubAction::ObsReplayStart => { let _ = ctx.h.obs.try_send(ObsCmd::ReplayStart); }
        SubAction::ObsReplayStop => { let _ = ctx.h.obs.try_send(ObsCmd::ReplayStop); }
        SubAction::ObsReplaySave => { let _ = ctx.h.obs.try_send(ObsCmd::ReplaySave); }
        SubAction::ObsStreamStart => { let _ = ctx.h.obs.try_send(ObsCmd::StreamStart); }
        SubAction::ObsStreamStop => { let _ = ctx.h.obs.try_send(ObsCmd::StreamStop); }
        SubAction::ObsStreamToggle => { let _ = ctx.h.obs.try_send(ObsCmd::StreamToggle); }
        SubAction::VtsHotkey { hotkey } => {
            if let Err(e) = ctx.h.vts.try_send(VtsCmd::Hotkey(hotkey.clone())) {
                tracing::warn!("vts hotkey dispatch failed (not connected?): {e}");
            } else { tracing::info!("vts hotkey dispatched: {hotkey}"); }
        }
        SubAction::Throw { item } => {
            if let Err(e) = ctx.h.throw.try_send(ThrowCmd::Throw(item.clone())) {
                tracing::warn!("throw dispatch failed (not connected?): {e}");
            } else { tracing::info!("throw dispatched: {item}"); }
        }
        SubAction::DiscordWebhook { url, content, username } => {
            let key = render_arg(ctx, url).await;
            // Master switch: skip sending when Discord is turned off in the panel.
            if !ctx.store.lock().await.settings.discord_enabled {
                return Flow::Continue;
            }
            // `url` may be a configured webhook name or a literal URL.
            let u = {
                let s = ctx.store.lock().await;
                s.settings.discord_webhooks.iter()
                    .find(|w| w.name.eq_ignore_ascii_case(key.trim()))
                    .map(|w| w.url.clone())
                    .unwrap_or(key)
            };
            let c = render_arg(ctx, content).await;
            let name = render_arg(ctx, username).await;
            let mut body = serde_json::json!({ "content": c });
            if !name.is_empty() { body["username"] = serde_json::json!(name); }
            // Discord/Cloudflare rejects requests without a User-Agent (403).
            let r = reqwest::Client::new()
                .post(&u)
                .header("User-Agent", "nuggtbot/0.1")
                .json(&body)
                .send()
                .await;
            match r {
                Ok(resp) if !resp.status().is_success() =>
                    tracing::error!("discord webhook: {} {}", resp.status(), resp.text().await.unwrap_or_default()),
                Err(e) => tracing::error!("discord webhook: {e}"),
                _ => {}
            }
        }
        SubAction::Plugin { plugin, action, params } => {
            // Render %vars% in each param, then hand off to the plugin registry.
            let mut p = std::collections::BTreeMap::new();
            for (k, v) in params {
                p.insert(k.clone(), render_arg(ctx, v).await);
            }
            crate::plugin::dispatch(plugin, action, p, &ctx.user).await;
        }
        SubAction::TwitchSetTitle { title } => {
            let v = render_arg(ctx, title).await;
            log_err("set title", crate::helix::set_title(&ctx.store, &v).await);
        }
        SubAction::TwitchSetGame { game } => {
            let v = render_arg(ctx, game).await;
            log_err("set game", crate::helix::set_game(&ctx.store, &v).await);
        }
        SubAction::TwitchAnnounce { text, color } => {
            let v = render_arg(ctx, text).await;
            log_err("announce", crate::helix::announce(&ctx.store, &v, color).await);
        }
        SubAction::TwitchClip => {
            log_err("clip", crate::helix::create_clip(&ctx.store).await.map(|_| ()));
        }
        SubAction::TwitchMarker { description } => {
            let v = render_arg(ctx, description).await;
            log_err("marker", crate::helix::create_marker(&ctx.store, &v).await);
        }
        SubAction::TwitchShoutout { target } => {
            let v = render_arg(ctx, target).await;
            log_err("shoutout", crate::helix::shoutout(&ctx.store, &v).await);
        }
        SubAction::KickMessage { text } => {
            let v = render_arg(ctx, text).await;
            log_err("kick send", crate::helix::kick_send(&ctx.store, &v).await);
        }
        SubAction::KickBan { target, duration, reason } => {
            let t = render_arg(ctx, target).await;
            let r = render_arg(ctx, reason).await;
            log_err("kick ban", crate::helix::kick_ban(&ctx.store, &t, *duration, &r).await);
        }
        SubAction::Reward { target, op, cost } => {
            let t = render_arg(ctx, target).await;
            for (plat, id) in crate::rewards::resolve_target(&ctx.store, &t).await {
                log_err("reward", crate::rewards::reward_op(&ctx.store, &plat, &id, op, *cost).await);
            }
        }
        SubAction::KickClearChat => {
            log_err("kick clear chat", crate::helix::kick_clear_chat(&ctx.store).await);
        }
        SubAction::KickUnban { target } => {
            let t = render_arg(ctx, target).await;
            log_err("kick unban", crate::helix::kick_unban(&ctx.store, &t).await);
        }
        SubAction::TwitchBan { target, duration, reason } => {
            let tgt = render_arg(ctx, target).await;
            let rs = render_arg(ctx, reason).await;
            log_err("ban", crate::helix::ban(&ctx.store, &tgt, *duration, &rs).await);
        }
        SubAction::TwitchUnban { target } => {
            let tgt = render_arg(ctx, target).await;
            log_err("unban", crate::helix::unban(&ctx.store, &tgt).await);
        }
        SubAction::TwitchClearChat => {
            log_err("clear chat", crate::helix::clear_chat(&ctx.store).await);
        }
        SubAction::TwitchCommercial { length } => {
            log_err("commercial", crate::helix::commercial(&ctx.store, *length).await);
        }
        SubAction::TwitchChatMode { mode, enabled, duration } => {
            log_err("chat mode", crate::helix::chat_mode(&ctx.store, mode, *enabled, *duration).await);
        }
        SubAction::TwitchPoll { title, choices, duration } => {
            let t = render_arg(ctx, title).await;
            let ch: Vec<String> = choices.split('|').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            log_err("poll", crate::helix::create_poll(&ctx.store, &t, &ch, *duration).await);
        }
        SubAction::AddToVariable { name, amount } => {
            let mut s = ctx.store.lock().await;
            let cur = s.variables.get(name).and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
            s.variables.insert(name.clone(), (cur + amount).to_string());
            let _ = s.save();
        }
        SubAction::IfStop { name, op, value } => {
            let cur = { ctx.store.lock().await.variables.get(name).cloned().unwrap_or_default() };
            let rhs = render_arg(ctx, value).await;
            return if compare(&cur, op, &rhs) { Flow::Continue } else { Flow::Stop };
        }
        SubAction::Stop => return Flow::Stop,
        SubAction::IfSkip { name, op, value, skip } => {
            let cur = { ctx.store.lock().await.variables.get(name).cloned().unwrap_or_default() };
            let rhs = render_arg(ctx, value).await;
            return if compare(&cur, op, &rhs) { Flow::Continue } else { Flow::Skip(*skip) };
        }
        SubAction::FetchUrl { url, into } => {
            let u = render_arg(ctx, url).await;
            match reqwest::Client::new().get(&u).send().await {
                Ok(r) => {
                    let body = r.text().await.unwrap_or_default();
                    let mut s = ctx.store.lock().await;
                    s.variables.insert(into.clone(), body);
                    let _ = s.save();
                }
                Err(e) => tracing::error!("fetch url: {e}"),
            }
        }
        SubAction::GetRandomNumber { min, max, into } => {
            use rand::Rng;
            let (lo, hi) = if min <= max { (*min, *max) } else { (*max, *min) };
            let n = rand::thread_rng().gen_range(lo..=hi);
            let mut s = ctx.store.lock().await;
            s.variables.insert(into.clone(), n.to_string());
            let _ = s.save();
        }
        SubAction::PlaySound { file, volume } => {
            let image = crate::web::image_for(file);
            let image_secs = crate::web::image_secs(file);
            let _ = ctx.h.bus.send(crate::model::Event::PlaySound { file: file.clone(), volume: *volume, image, image_secs });
        }
        SubAction::FileWrite { path, text, append } => {
            let p = render_arg(ctx, path).await;
            let body = render_arg(ctx, text).await;
            let res = if *append {
                use std::io::Write;
                std::fs::OpenOptions::new().create(true).append(true).open(&p)
                    .and_then(|mut f| f.write_all(body.as_bytes()))
            } else {
                std::fs::write(&p, body.as_bytes())
            };
            if let Err(e) = res { tracing::error!("file write {p}: {e}"); }
        }
        SubAction::FileRead { path, into } => {
            let p = render_arg(ctx, path).await;
            let body = std::fs::read_to_string(&p).unwrap_or_default();
            let mut s = ctx.store.lock().await;
            s.variables.insert(into.clone(), body);
            let _ = s.save();
        }
        SubAction::TimerEnable { name } => { set_timer_enabled(ctx, name, Some(true)).await; }
        SubAction::TimerDisable { name } => { set_timer_enabled(ctx, name, Some(false)).await; }
        SubAction::TimerToggle { name } => { set_timer_enabled(ctx, name, None).await; }
        SubAction::SystemRun { command: _, into: _ } => {
            // Intentionally not implemented: running shell commands from the web API
            // is a remote-code-execution surface. Left as a no-op until the user opts in.
            tracing::warn!("system_run is disabled for safety");
        }
        SubAction::LogMessage { text } => {
            let msg = render_arg(ctx, text).await;
            let _ = ctx.h.bus.send(crate::model::Event::System { text: msg });
        }
        SubAction::RunAction { name } => {
            let n = render_arg(ctx, name).await;
            run_named(&n, ctx).await;
        }
        SubAction::FireEvent { event } => {
            let ev = render_arg(ctx, event).await;
            crate::twitch::fire_event(&ev, &ctx.user, String::new(), &ctx.store, &ctx.h.bus, &ctx.h, &ctx.channel).await;
        }
        SubAction::IfBlock { name, op, value, then, otherwise } => {
            let cur = { ctx.store.lock().await.variables.get(name).cloned().unwrap_or_default() };
            let rhs = render_arg(ctx, value).await;
            let branch = if compare(&cur, op, &rhs) { then } else { otherwise };
            return run_subs(branch, ctx).await;
        }
        SubAction::RepeatBlock { count, body } => {
            for _ in 0..*count {
                match run_subs(body, ctx).await {
                    Flow::Stop => return Flow::Stop,
                    Flow::Break => break,
                    _ => {} // Continue / LoopContinue → next iteration
                }
            }
        }
        SubAction::WhileBlock { name, op, value, body, max_iter } => {
            let mut iter = 0;
            while iter < *max_iter {
                let cur = { ctx.store.lock().await.variables.get(name).cloned().unwrap_or_default() };
                let rhs = render_arg(ctx, value).await;
                if !compare(&cur, op, &rhs) {
                    break;
                }
                match run_subs(body, ctx).await {
                    Flow::Stop => return Flow::Stop,
                    Flow::Break => break,
                    _ => {}
                }
                iter += 1;
            }
        }
        SubAction::SwitchBlock { name, cases, default } => {
            let cur = { ctx.store.lock().await.variables.get(name).cloned().unwrap_or_default() };
            for c in cases {
                let v = render_arg(ctx, &c.value).await;
                if cur.eq_ignore_ascii_case(&v) {
                    return run_subs(&c.body, ctx).await;
                }
            }
            return run_subs(default, ctx).await;
        }
        SubAction::LoopBreak => return Flow::Break,
        SubAction::LoopContinue => return Flow::LoopContinue,
        SubAction::TwitchFollowage { user, into } => {
            let u = user_arg(ctx, user).await;
            let out = match crate::helix::followage_minutes(&ctx.store, &u).await {
                Ok(Some(m)) => fmt_duration(m),
                Ok(None) => "não segue".into(),
                Err(e) => { tracing::error!("followage: {e}"); String::new() }
            };
            let mut s = ctx.store.lock().await;
            s.variables.insert(into.clone(), out);
            let _ = s.save();
        }
        SubAction::TwitchRaid { target } => {
            let t = render_arg(ctx, target).await;
            log_err("raid", crate::helix::raid(&ctx.store, &t).await);
        }
        SubAction::TwitchAdSnooze => {
            log_err("ad snooze", crate::helix::ad_snooze(&ctx.store).await);
        }
        SubAction::TwitchGetFollowCount { into } => {
            match crate::helix::get_follow_count(&ctx.store).await {
                Ok(n) => { let mut s = ctx.store.lock().await; s.variables.insert(into.clone(), n); let _ = s.save(); }
                Err(e) => tracing::error!("follow count: {e}"),
            }
        }
        SubAction::TwitchGetSubCount { into } => {
            match crate::helix::get_sub_count(&ctx.store).await {
                Ok(n) => { let mut s = ctx.store.lock().await; s.variables.insert(into.clone(), n); let _ = s.save(); }
                Err(e) => tracing::error!("sub count: {e}"),
            }
        }
        SubAction::TwitchWhisper { target, text } => {
            let tgt = render_arg(ctx, target).await;
            let msg = render_arg(ctx, text).await;
            log_err("whisper", crate::helix::whisper(&ctx.store, &tgt, &msg).await);
        }
        SubAction::TwitchMod { target } => {
            let t = render_arg(ctx, target).await;
            log_err("mod", crate::helix::set_moderator(&ctx.store, &t, true).await);
        }
        SubAction::TwitchUnmod { target } => {
            let t = render_arg(ctx, target).await;
            log_err("unmod", crate::helix::set_moderator(&ctx.store, &t, false).await);
        }
        SubAction::TwitchVip { target } => {
            let t = render_arg(ctx, target).await;
            log_err("vip", crate::helix::set_vip(&ctx.store, &t, true).await);
        }
        SubAction::TwitchUnvip { target } => {
            let t = render_arg(ctx, target).await;
            log_err("unvip", crate::helix::set_vip(&ctx.store, &t, false).await);
        }
        SubAction::TwitchWarn { target, reason } => {
            let t = render_arg(ctx, target).await;
            let r = render_arg(ctx, reason).await;
            log_err("warn", crate::helix::warn(&ctx.store, &t, &r).await);
        }
        SubAction::TwitchPrediction { title, outcomes, window } => {
            let t = render_arg(ctx, title).await;
            let outs: Vec<String> = outcomes.split('|').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            log_err("prediction", crate::helix::create_prediction(&ctx.store, &t, &outs, *window).await);
        }
        SubAction::SetUserVariable { name, user, value } => {
            let u = user_arg(ctx, user).await;
            let val = render_arg(ctx, value).await;
            let mut s = ctx.store.lock().await;
            s.user_variables.entry(name.clone()).or_default().insert(u, val);
            let _ = s.save();
        }
        SubAction::AddToUserVariable { name, user, amount } => {
            let u = user_arg(ctx, user).await;
            let mut s = ctx.store.lock().await;
            let map = s.user_variables.entry(name.clone()).or_default();
            let cur = map.get(&u).and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
            map.insert(u, (cur + amount).to_string());
            let _ = s.save();
        }
        SubAction::GetUserVariable { name, user, into } => {
            let u = user_arg(ctx, user).await;
            let val = {
                let s = ctx.store.lock().await;
                s.user_variables.get(name).and_then(|m| m.get(&u)).cloned().unwrap_or_default()
            };
            let mut s = ctx.store.lock().await;
            s.variables.insert(into.clone(), val);
            let _ = s.save();
        }
        SubAction::FormatDuration { from, into } => {
            let mut s = ctx.store.lock().await;
            let mins = s.variables.get(from).and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
            let out = fmt_duration(mins);
            s.variables.insert(into.clone(), out);
            let _ = s.save();
        }
        SubAction::UserGroupAdd { group, user } => {
            let g = render_arg(ctx, group).await;
            let u = render_arg(ctx, user).await.trim().trim_start_matches('@').to_lowercase();
            if !g.is_empty() && !u.is_empty() {
                let mut s = ctx.store.lock().await;
                let members = s.user_groups.entry(g).or_default();
                if !members.iter().any(|m| m == &u) {
                    members.push(u);
                }
                let _ = s.save();
            }
        }
        SubAction::UserGroupRemove { group, user } => {
            let g = render_arg(ctx, group).await;
            let u = render_arg(ctx, user).await.trim().trim_start_matches('@').to_lowercase();
            let mut s = ctx.store.lock().await;
            if let Some(members) = s.user_groups.get_mut(&g) {
                members.retain(|m| m != &u);
            }
            let _ = s.save();
        }
        SubAction::QuoteAdd { text } => {
            let body = render_arg(ctx, text).await;
            if body.trim().is_empty() {
                return Flow::Continue;
            }
            let id = {
                let mut s = ctx.store.lock().await;
                s.quote_seq += 1;
                let id = s.quote_seq;
                let date = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0)
                    .to_string();
                s.quotes.push(crate::model::Quote { id, text: body, user: ctx.user.clone(), game: String::new(), date });
                let _ = s.save();
                id
            };
            let _ = ctx.h.outgoing.send(Outgoing { channel: ctx.channel.clone(), text: format!("Quote #{id} added!") });
        }
        SubAction::QuoteShow { id } => {
            let idn = render_arg(ctx, id).await.trim().parse::<u64>().ok().filter(|n| *n > 0);
            let q = {
                let s = ctx.store.lock().await;
                match idn {
                    Some(n) => s.quotes.iter().find(|q| q.id == n).cloned(),
                    None => {
                        if s.quotes.is_empty() {
                            None
                        } else {
                            use rand::Rng;
                            let i = rand::thread_rng().gen_range(0..s.quotes.len());
                            Some(s.quotes[i].clone())
                        }
                    }
                }
            };
            let msg = match q {
                Some(q) => {
                    let who = if q.user.is_empty() { String::new() } else { format!(" — {}", q.user) };
                    format!("#{}: {}{}", q.id, q.text, who)
                }
                None => "No quotes yet.".into(),
            };
            let _ = ctx.h.outgoing.send(Outgoing { channel: ctx.channel.clone(), text: msg });
        }
    }
    Flow::Continue
}

/// Minutes → "1y 3m 16d 15h 23min" (months=30d, years=365d), zero units omitted.
fn fmt_duration(mut m: i64) -> String {
    if m <= 0 {
        return "0min".into();
    }
    let units = [("y", 525600i64), ("m", 43200), ("d", 1440), ("h", 60), ("min", 1)];
    let mut parts = Vec::new();
    for (name, size) in units {
        let q = m / size;
        if q > 0 {
            parts.push(format!("{q}{name}"));
            m -= q * size;
        }
    }
    parts.join(" ")
}

#[test]
fn test_fmt_duration() {
    assert_eq!(fmt_duration(0), "0min");
    assert_eq!(fmt_duration(23), "23min");
    assert_eq!(fmt_duration(923), "15h 23min");
    assert_eq!(fmt_duration(525600 + 3 * 43200 + 16 * 1440 + 15 * 60 + 23), "1y 3m 16d 15h 23min");
}

/// Compare two values for the IfStop guard. Numeric for gt/lt, string otherwise.
fn compare(lhs: &str, op: &str, rhs: &str) -> bool {
    match op {
        "eq" => lhs.eq_ignore_ascii_case(rhs),
        "ne" => !lhs.eq_ignore_ascii_case(rhs),
        "contains" => lhs.to_lowercase().contains(&rhs.to_lowercase()),
        "gt" => lhs.parse::<f64>().ok().zip(rhs.parse::<f64>().ok()).map_or(false, |(a, b)| a > b),
        "lt" => lhs.parse::<f64>().ok().zip(rhs.parse::<f64>().ok()).map_or(false, |(a, b)| a < b),
        "ge" => lhs.parse::<f64>().ok().zip(rhs.parse::<f64>().ok()).map_or(false, |(a, b)| a >= b),
        "le" => lhs.parse::<f64>().ok().zip(rhs.parse::<f64>().ok()).map_or(false, |(a, b)| a <= b),
        _ => true,
    }
}

/// Enable/disable/toggle a timer by name (None = toggle).
async fn set_timer_enabled(ctx: &ExecCtx, name: &str, on: Option<bool>) {
    let mut s = ctx.store.lock().await;
    if let Some(t) = s.timers.iter_mut().find(|t| t.name.eq_ignore_ascii_case(name)) {
        t.enabled = on.unwrap_or(!t.enabled);
    }
    let _ = s.save();
}

/// Resolve a user argument: blank → the triggering user; else rendered + normalized.
async fn user_arg(ctx: &ExecCtx, user: &str) -> String {
    if user.trim().is_empty() {
        return ctx.user.to_lowercase();
    }
    // A template like "%input0%" renders empty when no target was given; fall
    // back to the caller instead of looking up the empty-string key.
    let rendered = render_arg(ctx, user).await.trim().trim_start_matches('@').to_lowercase();
    if rendered.is_empty() { ctx.user.to_lowercase() } else { rendered }
}

/// Render a sub-action string arg with the current user/args/globals + the
/// event-specific `extra` variables (extra overrides globals).
async fn render_arg(ctx: &ExecCtx, s: &str) -> String {
    let mut vars = { ctx.store.lock().await.variables.clone() };
    for (k, v) in &ctx.extra {
        vars.insert(k.clone(), v.clone());
    }
    engine::render(s, &ctx.user, &ctx.args, &vars)
}

fn log_err(what: &str, r: anyhow::Result<()>) {
    if let Err(e) = r {
        tracing::error!("{what}: {e:#}");
    }
}
