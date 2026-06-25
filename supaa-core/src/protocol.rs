use serde::{Deserialize, Serialize};
use crate::modes::Mode;

/// Path of the Unix domain socket the daemon listens on.
pub const SOCKET_PATH: &str = "/run/supaad.sock";

/// State file persisted to disk so recovery works across crashes.
pub const STATE_FILE: &str = "/var/lib/supaa/state.json";

/// Log directory.
pub const LOG_DIR: &str = "/var/log/supaa";

// ─────────────────────────────────────────────────────────
//  Commands  (client → daemon)
// ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Command {
    /// Change operating mode.
    SetMode { mode: Mode },

    /// Tell the daemon which PID/process is the current focus app.
    SetFocusApp { pid: u32, name: String },

    /// Clear focus app (return to idle).
    ClearFocusApp,

    /// Request current daemon status.
    GetStatus,

    /// Request full process list with freeze state.
    GetProcessList,

    /// Request a clean shutdown of the daemon.
    Shutdown,

    /// Emergency: restore everything and optionally exit.
    EmergencyRestore { quit_after: bool },
}

// ─────────────────────────────────────────────────────────
//  Responses  (daemon → client)
// ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Ok,
    Error { message: String },
    Status(DaemonStatus),
    ProcessList(Vec<ProcessInfo>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub mode:           Mode,
    pub focus_app_pid:  Option<u32>,
    pub focus_app_name: Option<String>,
    pub frozen_count:   usize,
    pub workspace_locked: bool,
    pub state_file_clean: bool,
    pub uptime_secs:    u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessInfo {
    pub pid:       u32,
    pub name:      String,
    pub is_frozen: bool,
    pub is_focus:  bool,
    pub is_whitelisted: bool,
    pub cpu_weight: Option<u64>,
}
