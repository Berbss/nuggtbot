//! Action queues (Streamer.bot core model). Every action runs in a queue:
//! * `Default` (and any non-blocking queue): actions start immediately and may
//!   overlap.
//! * A blocking queue: actions run one at a time, in order.
//!
//! Queues can be paused (new actions wait), resumed, and cleared. An action with
//! `always_run` starts even while its queue is paused.

use crate::action::{run_action, ExecCtx};
use crate::model::Action;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{Mutex, Notify};

pub const DEFAULT_QUEUE: &str = "Default";

struct Queue {
    blocking: bool,
    paused: AtomicBool,
    completed: AtomicU64,
    pending: Mutex<VecDeque<(Action, ExecCtx)>>,
    notify: Notify,
}

#[derive(Clone, serde::Serialize)]
pub struct QueueInfo {
    pub name: String,
    pub blocking: bool,
    pub paused: bool,
    pub pending: usize,
    pub completed: u64,
}

#[derive(Clone)]
pub struct QueueManager {
    queues: Arc<Mutex<HashMap<String, Arc<Queue>>>>,
}

impl QueueManager {
    pub fn new() -> Self {
        QueueManager { queues: Arc::new(Mutex::new(HashMap::new())) }
    }

    fn norm(name: &str) -> String {
        let n = name.trim();
        if n.is_empty() { DEFAULT_QUEUE.to_string() } else { n.to_string() }
    }

    async fn get_or_create(&self, name: &str, blocking: bool) -> Arc<Queue> {
        let name = Self::norm(name);
        let mut map = self.queues.lock().await;
        if let Some(q) = map.get(&name) {
            return q.clone();
        }
        let q = Arc::new(Queue {
            blocking,
            paused: AtomicBool::new(false),
            completed: AtomicU64::new(0),
            pending: Mutex::new(VecDeque::new()),
            notify: Notify::new(),
        });
        map.insert(name.clone(), q.clone());
        if blocking {
            spawn_worker(q.clone());
        }
        q
    }

    /// Enqueue (or immediately run) an action according to its queue + flags.
    pub async fn enqueue(&self, action: Action, ctx: ExecCtx) {
        let q = self.get_or_create(&action.queue, false).await;
        let paused = q.paused.load(Ordering::Relaxed);

        // Always-run bypasses pause and ordering.
        if action.always_run {
            tokio::spawn(async move { run_action(&action, &ctx).await });
            return;
        }

        if q.blocking {
            q.pending.lock().await.push_back((action, ctx));
            q.notify.notify_one();
        } else if paused {
            q.pending.lock().await.push_back((action, ctx));
        } else {
            let qc = q.clone();
            tokio::spawn(async move {
                run_action(&action, &ctx).await;
                qc.completed.fetch_add(1, Ordering::Relaxed);
            });
        }
    }

    pub async fn pause(&self, name: &str) {
        if let Some(q) = self.queues.lock().await.get(&Self::norm(name)) {
            q.paused.store(true, Ordering::Relaxed);
        }
    }

    pub async fn resume(&self, name: &str) {
        let name = Self::norm(name);
        let q = { self.queues.lock().await.get(&name).cloned() };
        if let Some(q) = q {
            q.paused.store(false, Ordering::Relaxed);
            if q.blocking {
                q.notify.notify_one();
            } else {
                // Drain the actions that piled up while paused.
                let mut pend = q.pending.lock().await;
                while let Some((a, c)) = pend.pop_front() {
                    let qc = q.clone();
                    tokio::spawn(async move {
                        run_action(&a, &c).await;
                        qc.completed.fetch_add(1, Ordering::Relaxed);
                    });
                }
            }
        }
    }

    pub async fn pause_all(&self) {
        for q in self.queues.lock().await.values() {
            q.paused.store(true, Ordering::Relaxed);
        }
    }

    pub async fn resume_all(&self) {
        let names: Vec<String> = { self.queues.lock().await.keys().cloned().collect() };
        for n in names {
            self.resume(&n).await;
        }
    }

    pub async fn clear(&self, name: &str) {
        if let Some(q) = self.queues.lock().await.get(&Self::norm(name)) {
            q.pending.lock().await.clear();
        }
    }

    pub async fn list(&self) -> Vec<QueueInfo> {
        let map = self.queues.lock().await;
        let mut out = Vec::new();
        for (name, q) in map.iter() {
            out.push(QueueInfo {
                name: name.clone(),
                blocking: q.blocking,
                paused: q.paused.load(Ordering::Relaxed),
                pending: q.pending.lock().await.len(),
                completed: q.completed.load(Ordering::Relaxed),
            });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// Declare a queue (so it shows up / has the right blocking mode) before use.
    pub async fn declare(&self, name: &str, blocking: bool) {
        self.get_or_create(name, blocking).await;
    }
}

fn spawn_worker(q: Arc<Queue>) {
    tokio::spawn(async move {
        loop {
            // Wait until there is work and we are not paused.
            let job = loop {
                if q.paused.load(Ordering::Relaxed) {
                    q.notify.notified().await;
                    continue;
                }
                if let Some(job) = q.pending.lock().await.pop_front() {
                    break job;
                }
                q.notify.notified().await;
            };
            let (action, ctx) = job;
            run_action(&action, &ctx).await;
            q.completed.fetch_add(1, Ordering::Relaxed);
        }
    });
}
