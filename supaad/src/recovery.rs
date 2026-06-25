//! Crash recovery and state persistence.
//!
//! The state file is written atomically (write to `.tmp`, rename over target)
//! so a crash mid-write cannot corrupt it.
//!
//! On every startup we check whether the previous shutdown was clean.
//! If not (crash / power loss) we run an emergency thaw + cgroup reset.
//! (P1 fix: also resets cgroup weights to defaults so resource limits from
//!  the previous session don't persist across a dirty restart.)

use anyhow::{Context, Result};
use log::{info, warn};
use std::fs;
use std::io::Write;
use std::path::Path;

use supaa_core::protocol::{LOG_DIR, STATE_FILE};
use supaa_core::state::{PersistedState, now_unix};
use crate::{cgroup, freezer, hyprland};

// ── State file I/O ────────────────────────────────────────────────────────────

pub fn load_state() -> Option<PersistedState> {
    let content = fs::read_to_string(STATE_FILE).ok()?;
    serde_json::from_str(&content)
        .map_err(|e| warn!("state file parse error: {}", e))
        .ok()
}

pub fn save_state(state: &PersistedState) -> Result<()> {
    if let Some(parent) = Path::new(STATE_FILE).parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(state).context("serialise state")?;
    let tmp  = format!("{}.tmp", STATE_FILE);
    {
        let mut f = fs::File::create(&tmp).context("create state tmp")?;
        f.write_all(json.as_bytes()).context("write state tmp")?;
        f.sync_all().context("fsync state tmp")?;
    }
    fs::rename(&tmp, STATE_FILE).context("rename state file")?;
    Ok(())
}

#[allow(dead_code)]
pub fn delete_state() {
    let _ = fs::remove_file(STATE_FILE);
}

// ── Startup recovery ──────────────────────────────────────────────────────────

/// Run on daemon startup.  If the previous state was dirty, recover fully.
pub fn run_recovery() -> Result<()> {
    let state = match load_state() {
        Some(s) => s,
        None => {
            info!("no state file found — fresh start");
            return Ok(());
        }
    };

    if state.clean_shutdown {
        info!("previous shutdown was clean — no recovery needed");
        return Ok(());
    }

    warn!(
        "dirty state detected (timestamp={}) — running recovery",
        state.timestamp
    );
    write_log("Recovery started after dirty shutdown");

    // 1. Thaw any frozen PIDs from the last session.
    let frozen_vec: Vec<u32> = state.frozen_pids.iter().copied().collect();
    if !frozen_vec.is_empty() {
        info!("thawing {} PIDs from previous session", frozen_vec.len());
        freezer::thaw_pid_list(&frozen_vec);
        write_log(&format!("Thawed {} PIDs", frozen_vec.len()));
    }

    // 2. Unlock workspace switching.
    if state.workspace_locked {
        info!("unlocking workspace switching from previous session");
        hyprland::unlock_workspace_switching()
            .unwrap_or_else(|e| { warn!("recovery workspace unlock: {}", e); false });
        write_log("Workspace switching unlocked");
    }

    // 3. Reset cgroup weights to defaults and tear down our directories.
    //    (P1 fix: supaa2 recovery left cgroup weights from the previous
    //     session in place, so background processes remained throttled.)
    info!("resetting cgroup state from previous session");
    cgroup::teardown_cgroups()
        .unwrap_or_else(|e| warn!("recovery cgroup teardown: {}", e));
    write_log("Cgroup state reset");

    // 4. Re-init clean cgroup hierarchy so the daemon is ready to use.
    cgroup::init_cgroups()
        .unwrap_or_else(|e| warn!("recovery cgroup re-init: {}", e));

    // 5. Persist a clean state so we don't recover again.
    let mut clean = PersistedState::new();
    clean.mark_clean();
    save_state(&clean)?;

    write_log("Recovery complete");
    info!("recovery complete");
    Ok(())
}

// ── Shutdown helpers ──────────────────────────────────────────────────────────

pub fn mark_dirty(state: &mut PersistedState) {
    state.mark_dirty();
    let _ = save_state(state);
}

pub fn mark_clean(state: &mut PersistedState) {
    state.mark_clean();
    let _ = save_state(state);
}

// ── Log helpers ───────────────────────────────────────────────────────────────

pub fn write_log(msg: &str) {
    let path = format!("{}/supaa.log", LOG_DIR);
    let _ = fs::create_dir_all(LOG_DIR);
    let timestamp = now_unix();
    let line = format!("[{}] {}\n", timestamp, msg);
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = f.write_all(line.as_bytes());
    }
}
