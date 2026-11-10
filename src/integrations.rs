use crate::model::{Event, Outgoing};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};

/// Live connection flags, set true by each integration/platform task on connect
/// and false on disconnect. Read by the web status endpoint. Accounts are not
/// here (their "connected" is derived from whether a token is present).
#[derive(Clone, Default)]
pub struct ConnStatus {
    pub twitch: Arc<AtomicBool>,
    pub youtube: Arc<AtomicBool>,
    pub obs: Arc<AtomicBool>,
    pub vts: Arc<AtomicBool>,
    pub throw: Arc<AtomicBool>,
    /// Desired connection state (On/Off/Connect buttons). The task connects while
    /// true and disconnects when it goes false. Seeded from `enabled` at startup.
    pub obs_want: Arc<AtomicBool>,
    pub vts_want: Arc<AtomicBool>,
    pub throw_want: Arc<AtomicBool>,
}

/// Commands the Action executor sends to each integration task.
#[derive(Debug)]
pub enum ObsCmd {
    Scene(String),
    Source { scene: String, source: String, visible: bool },
    RecordToggle,
    Filter { source: String, filter: String, enabled: bool },
    ReplayStart,
    ReplayStop,
    ReplaySave,
    StreamStart,
    StreamStop,
    StreamToggle,
}

#[derive(Debug)]
pub enum VtsCmd {
    Hotkey(String),
}

#[derive(Debug)]
pub enum ThrowCmd {
    Throw(String),
}

/// Send-side handles shared with the engine/web so any trigger can drive outputs.
#[derive(Clone)]
pub struct Handles {
    pub outgoing: mpsc::UnboundedSender<Outgoing>,
    pub obs: mpsc::Sender<ObsCmd>,
    pub vts: mpsc::Sender<VtsCmd>,
    pub throw: mpsc::Sender<ThrowCmd>,
    pub queues: crate::queue::QueueManager,
    pub cooldowns: crate::cooldown::Cooldowns,
    /// Event bus, so sub-actions can push to the overlay (e.g. play sound).
    pub bus: broadcast::Sender<Event>,
    /// Live connection flags for the status widget.
    pub status: ConnStatus,
}
