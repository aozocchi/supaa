use serde::{Deserialize, Serialize};
use crate::modes::Mode;
use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

/// The state written to disk on every transition so we can recover
/// after a crash.  Written atomically (write-to-tmp, rename).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedState {
    /// Schema version — bump when the format changes.
    pub version: u32,

    /// Active mode at the time of the last write.
    pub mode: Mode,

    /// PIDs the daemon froze.  Must be thawed on recovery.
    pub frozen_pids: HashSet<u32>,

    /// Whether workspace lock was active.
    pub workspace_locked: bool,

    /// Unix timestamp of the last write.
    pub timestamp: u64,

    /// True only after a clean shutdown sequence completes.
    /// If false on next startup ⟹ crash / dirty shutdown ⟹ run recovery.
    pub clean_shutdown: bool,

    /// PID of the focus application at shutdown (informational).
    pub focus_app_pid: Option<u32>,

    /// Name of the focus application at shutdown (informational).
    pub focus_app_name: Option<String>,
}

impl Default for PersistedState {
    fn default() -> Self {
        Self {
            version:        1,
            mode:           Mode::Normal,
            frozen_pids:    HashSet::new(),
            workspace_locked: false,
            timestamp:      now_unix(),
            clean_shutdown: true,
            focus_app_pid:  None,
            focus_app_name: None,
        }
    }
}

impl PersistedState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mark as dirty (in-progress) — call before any destructive operation.
    pub fn mark_dirty(&mut self) {
        self.clean_shutdown = false;
        self.timestamp = now_unix();
    }

    /// Mark as clean — call only after full restoration is complete.
    pub fn mark_clean(&mut self) {
        self.clean_shutdown = true;
        self.timestamp = now_unix();
    }

    pub fn is_dirty(&self) -> bool {
        !self.clean_shutdown
    }
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
