//! Command cooldowns + permissions (Streamer.bot semantics).
//! * Global cooldown: nobody may re-run until N seconds pass.
//! * User cooldown: the same user must wait N seconds; others unaffected.
//! * The broadcaster is always exempt from both cooldowns and permissions.
//! * Permissions: grant_type "allow" = only listed groups/users; "deny" =
//!   everyone except; "" = anyone.

use crate::model::Command;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;

#[derive(Clone, Default)]
pub struct Cooldowns {
    inner: Arc<Mutex<State>>,
}

#[derive(Default)]
struct State {
    global: HashMap<String, Instant>,
    user: HashMap<(String, String), Instant>,
}

impl Cooldowns {
    pub fn new() -> Self {
        Self::default()
    }

    /// If on cooldown, return the remaining seconds (and do nothing). Otherwise
    /// mark the command as just-run and return None. Broadcaster is exempt.
    pub async fn check(
        &self,
        trigger: &str,
        user: &str,
        global_secs: u64,
        user_secs: u64,
        is_broadcaster: bool,
    ) -> Option<u64> {
        if is_broadcaster || (global_secs == 0 && user_secs == 0) {
            return None;
        }
        let now = Instant::now();
        let mut s = self.inner.lock().await;
        if global_secs > 0 {
            if let Some(t) = s.global.get(trigger) {
                let e = now.duration_since(*t).as_secs();
                if e < global_secs {
                    return Some(global_secs - e);
                }
            }
        }
        let ukey = (trigger.to_string(), user.to_string());
        if user_secs > 0 {
            if let Some(t) = s.user.get(&ukey) {
                let e = now.duration_since(*t).as_secs();
                if e < user_secs {
                    return Some(user_secs - e);
                }
            }
        }
        if global_secs > 0 {
            s.global.insert(trigger.to_string(), now);
        }
        if user_secs > 0 {
            s.user.insert(ukey, now);
        }
        None
    }
}

/// Decide whether `user` (with these roles) may run the command.
/// `roles` are lowercase: broadcaster, moderator, vip, subscriber (+ everyone).
/// A command group that isn't a built-in role is matched against custom user
/// groups (`user_groups`: name → member logins).
pub fn permitted(
    cmd: &Command,
    user_login: &str,
    roles: &[String],
    user_groups: &BTreeMap<String, Vec<String>>,
) -> bool {
    if roles.iter().any(|r| r == "broadcaster") {
        return true; // broadcaster always allowed
    }
    let gt = cmd.grant_type.to_lowercase();
    if gt != "allow" && gt != "deny" {
        return true; // no restriction
    }
    let user_l = user_login.to_lowercase();
    let in_users = cmd.users.iter().any(|u| u.eq_ignore_ascii_case(&user_l));
    let in_groups = cmd.groups.iter().any(|g| {
        let gl = g.to_lowercase();
        if gl == "everyone" || roles.iter().any(|r| *r == gl) {
            return true;
        }
        // Custom user group membership.
        user_groups
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(g))
            .map_or(false, |(_, members)| members.iter().any(|m| m.eq_ignore_ascii_case(&user_l)))
    });
    let matched = in_users || in_groups;
    if gt == "allow" {
        matched
    } else {
        !matched
    }
}
