use serde::{Deserialize, Serialize};

/// An event on the bus: integrations publish, web subscribers + engine consume.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Chat {
        platform: String,
        user: String,
        text: String,
    },
    System {
        text: String,
    },
    /// A platform event (sub, resub, raid, subgift, cheer, …) that can trigger an Action.
    Notice {
        kind: String,
        user: String,
        text: String,
    },
    /// Tell the browser-source overlay to play an audio file.
    PlaySound {
        file: String,
        volume: f32,
        /// Optional image filename (served at /images/<name>) shown full-screen while it plays.
        #[serde(skip_serializing_if = "Option::is_none")]
        image: Option<String>,
        /// Seconds the image animation lasts; None = overlay default.
        #[serde(skip_serializing_if = "Option::is_none")]
        image_secs: Option<f32>,
    },
    /// A chat message enriched for the chat overlay + dock. `msg` is already the
    /// overlay shape: {id, platform, from, color, badges, parts}. Fed to the SSE
    /// /events feed and the /ws dock.
    ChatMsg {
        msg: serde_json::Value,
    },
    /// Remove one overlay/dock message by its prefixed id ("twitch:..", "kick:..").
    ChatDelete {
        id: String,
    },
    /// Clear the overlay/dock chat.
    ChatClear,
}

/// Bind a platform event kind (e.g. "sub", "raid", "cheer") to an Action.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EventTrigger {
    pub event: String,
    pub action: String,
}

/// Links reward IDs across platforms under one stable trigger key, so renaming
/// a reward on Twitch/Kick keeps it bound (matching is by id, not title).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RewardLink {
    /// Stable event name a trigger binds to; never changes after creation.
    pub key: String,
    /// Display label (cache; the UI prefers the live API title).
    #[serde(default)]
    pub title: String,
    pub entries: Vec<RewardRef>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RewardRef {
    pub platform: String,
    pub id: String,
}

/// A message the Twitch task should send (from timers, commands, web).
#[derive(Clone, Debug)]
pub struct Outgoing {
    pub channel: String,
    pub text: String,
}

/// Chat command: first word == `trigger`. If `action` names an Action, run it;
/// otherwise reply with `response`. `$user` / `$var.NAME` expand.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Command {
    pub trigger: String,
    #[serde(default)]
    pub response: String,
    #[serde(default)]
    pub action: Option<String>,
    #[serde(default = "d_true")]
    pub enabled: bool,
    /// Seconds before anyone can run it again (0 = off). Broadcaster exempt.
    #[serde(default)]
    pub global_cooldown: u64,
    /// Seconds before the same user can run it again (0 = off). Broadcaster exempt.
    #[serde(default)]
    pub user_cooldown: u64,
    /// "" = anyone; "allow" = only listed groups/users; "deny" = everyone except.
    #[serde(default)]
    pub grant_type: String,
    /// Permission groups: broadcaster, moderator, vip, subscriber, everyone.
    #[serde(default)]
    pub groups: Vec<String>,
    /// Specific user logins for the allow/deny list.
    #[serde(default)]
    pub users: Vec<String>,
    /// "basic" (default) or "regex".
    #[serde(default)]
    pub mode: String,
    /// basic match location: "start" (default), "exact", or "anywhere".
    #[serde(default)]
    pub location: String,
    #[serde(default)]
    pub case_sensitive: bool,
    /// Extra triggers that also fire this command.
    #[serde(default)]
    pub aliases: Vec<String>,
    /// Platforms this command accepts ("twitch", "kick"); empty = all.
    #[serde(default)]
    pub platforms: Vec<String>,
}

/// An Action = ordered list of sub-actions, run on a trigger.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Action {
    pub name: String,
    #[serde(default)]
    pub group: String,
    #[serde(default = "d_true")]
    pub enabled: bool,
    /// Queue name this action runs in ("" = Default, non-blocking).
    #[serde(default)]
    pub queue: String,
    /// Run all sub-actions at once (ignores Delay).
    #[serde(default)]
    pub concurrent: bool,
    /// Run one random sub-action instead of all.
    #[serde(default)]
    pub random: bool,
    /// Run even when the queue is paused.
    #[serde(default)]
    pub always_run: bool,
    #[serde(default)]
    pub subactions: Vec<SubAction>,
}

/// The breadth of what an Action can do. One variant per sub-action kind.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SubAction {
    SendMessage { text: String },
    /// Reply on the platform the command/event came from (Twitch or Kick).
    Reply { text: String },
    Delay { ms: u64 },
    SetVariable { name: String, value: String },
    ObsScene { scene: String },
    ObsSource { scene: String, source: String, visible: bool },
    ObsRecordToggle,
    ObsFilter { source: String, filter: String, visible: bool },
    ObsReplayStart,
    ObsReplayStop,
    ObsReplaySave,
    ObsStreamStart,
    ObsStreamStop,
    ObsStreamToggle,
    VtsHotkey { hotkey: String },
    Throw { item: String },
    /// Post a message to a Discord channel via its webhook URL.
    DiscordWebhook { url: String, content: String, #[serde(default)] username: String },
    /// Generic drop-in plugin sub-action: routed to the plugin named `plugin`,
    /// which runs its `action` with `params`. See src/plugin.rs + the plugins/ dir.
    Plugin { plugin: String, action: String, #[serde(default)] params: std::collections::BTreeMap<String, String> },
    // Twitch Helix sub-actions (broadcaster account).
    TwitchSetTitle { title: String },
    TwitchSetGame { game: String },
    TwitchAnnounce {
        text: String,
        #[serde(default)]
        color: String,
    },
    TwitchClip,
    TwitchMarker {
        #[serde(default)]
        description: String,
    },
    TwitchShoutout { target: String },
    /// Ban (duration 0) or timeout (duration seconds) a user.
    TwitchBan {
        target: String,
        #[serde(default)]
        duration: u64,
        #[serde(default)]
        reason: String,
    },
    TwitchUnban { target: String },
    TwitchClearChat,
    TwitchCommercial {
        #[serde(default = "d_ad_len")]
        length: u64,
    },
    /// mode = emote | followers | subscribers | slow. `enabled` toggles it;
    /// `duration` is the follower-age minutes or slow-mode seconds.
    TwitchChatMode {
        mode: String,
        #[serde(default)]
        enabled: bool,
        #[serde(default)]
        duration: u64,
    },
    /// Choices separated by `|`.
    TwitchPoll {
        title: String,
        choices: String,
        #[serde(default = "d_poll_dur")]
        duration: u64,
    },
    // Kick chat.
    KickMessage { text: String },
    /// Kick ban (duration 0) or timeout (duration minutes, 1–10080) by username.
    KickBan { target: String, #[serde(default)] duration: u64, #[serde(default)] reason: String },
    KickUnban { target: String },
    /// Clear Kick chat (deletes recent buffered messages; Kick has no bulk clear).
    KickClearChat,
    /// Enable/disable or change the cost of a channel-point reward.
    /// target = "twitch:<id>" | "kick:<id>" | "link:<key>" (link hits every platform).
    /// op = enable | disable | toggle | cost (cost uses the `cost` field).
    Reward { target: String, op: String, #[serde(default)] cost: i64 },
    // Core: variables + logic.
    /// Add `amount` (may be negative) to a numeric global variable.
    AddToVariable { name: String, amount: i64 },
    /// Guard: if the comparison is false, stop running the rest of the action.
    /// op = eq | ne | gt | lt | contains (case-insensitive for eq/ne/contains).
    IfStop { name: String, op: String, value: String },
    /// Stop the action here.
    Stop,
    /// If the comparison is false, skip the next `skip` sub-actions (a conditional
    /// block without nesting). op as in IfStop.
    IfSkip { name: String, op: String, value: String, #[serde(default)] skip: usize },
    /// HTTP GET `url`; store the response body in global variable `into`.
    FetchUrl { url: String, into: String },
    /// Store a random integer in [min, max] into global variable `into`.
    GetRandomNumber { min: i64, max: i64, into: String },
    /// Write `text` to a file (overwrite, or append).
    FileWrite { path: String, text: String, #[serde(default)] append: bool },
    /// Read a whole file into global variable `into`.
    FileRead { path: String, into: String },
    /// Enable / disable / toggle a timer by name.
    TimerEnable { name: String },
    TimerDisable { name: String },
    TimerToggle { name: String },
    /// Run a shell command (`sh -c`). Optional `into` captures stdout.
    SystemRun { command: String, #[serde(default)] into: String },
    /// Write a line to the live event feed (and log).
    LogMessage { text: String },
    /// Run another action by name (inline, via its queue).
    RunAction { name: String },
    /// Fire a custom event, running any EventTrigger bound to `event`.
    FireEvent { event: String },
    /// Play an audio file through the browser-source overlay.
    PlaySound {
        file: String,
        #[serde(default = "d_vol")]
        volume: f32,
    },
    /// Real if/else block: run `then` when the comparison holds, else `otherwise`.
    IfBlock {
        name: String,
        op: String,
        value: String,
        #[serde(default)]
        then: Vec<SubAction>,
        #[serde(default, rename = "else")]
        otherwise: Vec<SubAction>,
    },
    /// Run `body` `count` times.
    RepeatBlock {
        count: u32,
        #[serde(default)]
        body: Vec<SubAction>,
    },
    /// Run `body` while the comparison holds, capped at `max_iter` for safety.
    WhileBlock {
        name: String,
        op: String,
        value: String,
        #[serde(default)]
        body: Vec<SubAction>,
        #[serde(default = "d_maxiter")]
        max_iter: u32,
    },
    /// Compare variable `name` (eq) to each case value; run the first match's body,
    /// else `default`.
    SwitchBlock {
        name: String,
        #[serde(default)]
        cases: Vec<SwitchCase>,
        #[serde(default)]
        default: Vec<SubAction>,
    },
    /// Break out of the enclosing loop.
    LoopBreak,
    /// Skip to the next iteration of the enclosing loop.
    LoopContinue,
    /// Write how long `user` (blank = triggerer) has followed into `into`
    /// (formatted like fmt_duration), or "não segue" if not following.
    TwitchFollowage { #[serde(default)] user: String, into: String },
    TwitchRaid { target: String },
    TwitchAdSnooze,
    TwitchGetFollowCount { into: String },
    TwitchGetSubCount { into: String },
    TwitchWhisper { target: String, text: String },
    TwitchMod { target: String },
    TwitchUnmod { target: String },
    TwitchVip { target: String },
    TwitchUnvip { target: String },
    TwitchWarn {
        target: String,
        #[serde(default)]
        reason: String,
    },
    /// Outcomes separated by `|` (2–10).
    TwitchPrediction {
        title: String,
        outcomes: String,
        #[serde(default = "d_poll_dur")]
        window: u64,
    },
    /// Per-user variable (blank `user` = the triggering user).
    SetUserVariable { name: String, #[serde(default)] user: String, value: String },
    AddToUserVariable { name: String, #[serde(default)] user: String, amount: i64 },
    /// Copy a per-user variable into a normal variable `into` for use in text.
    GetUserVariable { name: String, #[serde(default)] user: String, into: String },
    /// Read a minutes count from variable `from` and write a human duration
    /// (e.g. "1y 3m 16d 15h 23min") into variable `into`.
    FormatDuration { from: String, into: String },
    /// Add/remove a user to/from a custom user group.
    UserGroupAdd { group: String, user: String },
    UserGroupRemove { group: String, user: String },
    /// Save a quote (user = the triggering user). Announces the new quote number.
    QuoteAdd { text: String },
    /// Send a quote to chat. `id` blank or "0" picks a random quote.
    QuoteShow {
        #[serde(default)]
        id: String,
    },
}

fn d_ad_len() -> u64 { 60 }
fn d_poll_dur() -> u64 { 60 }
fn d_vol() -> f32 { 1.0 }
fn d_maxiter() -> u32 { 10000 }

/// A saved quote (built-in Quotes system).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Quote {
    pub id: u64,
    pub text: String,
    #[serde(default)]
    pub user: String,
    #[serde(default)]
    pub game: String,
    #[serde(default)]
    pub date: String,
}

/// One case of a SwitchBlock.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SwitchCase {
    pub value: String,
    #[serde(default)]
    pub body: Vec<SubAction>,
}

/// Timed action: send `text` to the connected Twitch channel every `interval_secs`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Timer {
    pub name: String,
    pub interval_secs: u64,
    pub text: String,
    #[serde(default = "d_true")]
    pub enabled: bool,
    /// Chats to post to ("twitch","kick","youtube"). Empty = twitch (back-compat).
    #[serde(default)]
    pub platforms: Vec<String>,
    /// Sending account: "bot" (default) or "broadcaster". YouTube ignores this
    /// (only a broadcaster login exists there).
    #[serde(default)]
    pub account: String,
}

/// One account's OAuth login (broadcaster or bot). Auto-refreshed before expiry.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct OAuthToken {
    #[serde(default)]
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: String,
    /// Unix seconds when `access_token` expires (0 = unknown).
    #[serde(default)]
    pub token_expires: u64,
    /// Numeric platform user id (needed by many Helix calls).
    #[serde(default)]
    pub user_id: String,
    /// Login name resolved at login (lowercased).
    #[serde(default)]
    pub user_login: String,
    /// Display name (original casing) shown on the account card.
    #[serde(default)]
    pub display_name: String,
    /// Profile picture URL shown on the account card.
    #[serde(default)]
    pub avatar_url: String,
    /// Space-separated scopes granted on the token.
    #[serde(default)]
    pub scopes: String,
}

impl OAuthToken {
    pub fn logged_in(&self) -> bool {
        !self.access_token.trim().is_empty()
    }
}

/// Twitch account/app credentials, edited in the web Accounts tab. Two logins,
/// matching Streamer.bot: `broadcaster` (reads events/subs/points, Helix) and
/// `bot` (sends chat so messages appear from the bot name). One app client_id.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TwitchAccount {
    #[serde(default)]
    pub client_id: String,
    #[serde(default)]
    pub client_secret: String,
    /// Channel to join (defaults to the broadcaster login).
    #[serde(default)]
    pub channel: String,
    #[serde(default)]
    pub broadcaster: OAuthToken,
    #[serde(default)]
    pub bot: OAuthToken,
}

/// Kick account/app credentials (confidential client: needs client_secret).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct KickAccount {
    #[serde(default)]
    pub client_id: String,
    #[serde(default)]
    pub client_secret: String,
    #[serde(default)]
    pub channel: String,
    #[serde(default)]
    pub broadcaster: OAuthToken,
    #[serde(default)]
    pub bot: OAuthToken,
    /// Public web-client token for Kick's Centrifugo websocket gateway
    /// (NEXT_PUBLIC_WEBSOCKET_CLIENT_TOKEN from kick.com). Needed to read chat.
    #[serde(default)]
    pub ws_client_token: String,
}

/// OBS (obs-websocket v5) connection.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ObsConfig {
    #[serde(default = "d_true")]
    pub enabled: bool,
    #[serde(default = "d_localhost")]
    pub address: String,
    #[serde(default = "d_4455")]
    pub port: u16,
    #[serde(default)]
    pub password: String,
}
impl Default for ObsConfig {
    fn default() -> Self {
        Self { enabled: false, address: d_localhost(), port: 4455, password: String::new() }
    }
}

/// VTube Studio API connection.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VtsConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "d_localhost")]
    pub address: String,
    #[serde(default = "d_8001")]
    pub port: u16,
    /// Auth token VTS hands back on first approval (saved so later runs skip the popup).
    #[serde(default)]
    pub token: String,
}
impl Default for VtsConfig {
    fn default() -> Self {
        Self { enabled: false, address: d_localhost(), port: 8001, token: String::new() }
    }
}

/// Throwing System connection.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ThrowingConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "d_localhost")]
    pub address: String,
    #[serde(default = "d_42069")]
    pub port: u16,
}
impl Default for ThrowingConfig {
    fn default() -> Self {
        Self { enabled: false, address: d_localhost(), port: 42069 }
    }
}

fn d_true() -> bool { true }
fn d_localhost() -> String { "127.0.0.1".into() }
fn d_4455() -> u16 { 4455 }
fn d_8001() -> u16 { 8001 }
fn d_42069() -> u16 { 42069 }

/// A named Discord webhook (so sub-actions can target one by name).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DiscordWebhook {
    pub name: String,
    pub url: String,
}

/// YouTube account/app credentials (Google Cloud OAuth app: client_id + secret).
/// Mirrors Streamer.bot: `broadcaster` monitors chat/events, `bot` (optional)
/// sends messages from another account. Live chat is read via YouTube Data API v3.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct YouTubeAccount {
    #[serde(default)]
    pub client_id: String,
    #[serde(default)]
    pub client_secret: String,
    /// Optional explicit broadcast/video id. Blank = auto-detect the active live
    /// broadcast on the broadcaster's channel.
    #[serde(default)]
    pub video_id: String,
    #[serde(default)]
    pub broadcaster: OAuthToken,
    #[serde(default)]
    pub bot: OAuthToken,
}

/// All settings.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub twitch: TwitchAccount,
    #[serde(default)]
    pub kick: KickAccount,
    #[serde(default)]
    pub youtube: YouTubeAccount,
    #[serde(default)]
    pub obs: ObsConfig,
    #[serde(default)]
    pub vts: VtsConfig,
    #[serde(default)]
    pub throwing: ThrowingConfig,
    /// Named Discord webhooks (name → URL), referenced by discord_webhook.
    #[serde(default)]
    pub discord_webhooks: Vec<DiscordWebhook>,
    /// Master on/off: when false, the discord_webhook sub-action is skipped.
    #[serde(default = "d_true")]
    pub discord_enabled: bool,
    /// Third-party donation/event API credentials (LivePix, Streamlabs, StreamElements).
    #[serde(default)]
    pub apis: ApiKeys,
}

/// Credentials for third-party APIs. Stored in secrets.json; event polling TBD.
#[derive(Default, Clone, Debug, Serialize, Deserialize)]
pub struct ApiKeys {
    #[serde(default)]
    pub livepix_client_id: String,
    #[serde(default)]
    pub livepix_client_secret: String,
    #[serde(default)]
    pub streamlabs_client_id: String,
    #[serde(default)]
    pub streamlabs_client_secret: String,
    #[serde(default)]
    pub streamlabs_socket_token: String,
    #[serde(default)]
    pub streamelements_jwt: String,
    #[serde(default)]
    pub streamelements_account_id: String,
}
