//! Process freeze / thaw using SIGSTOP / SIGCONT.
//!
//! SIGSTOP is a reliable, portable, kernel-enforced mechanism.  The process
//! cannot catch or ignore it.  SIGCONT resumes it.
//!
//! We intentionally do NOT use the cgroup v1 `freezer` controller because
//! cgroup v2 removed it in favour of `cgroup.freeze` — but that file is only
//! available when the cgroup is managed by systemd or a privileged manager.
//! SIGSTOP/SIGCONT is simpler, more portable, and gives the same result.
//!
//! Safety guarantees:
//!   - The whitelist is checked before every freeze.
//!   - We track frozen PIDs so we can always thaw on recovery.
//!   - Kernel threads and whitelisted processes are never frozen.

use anyhow::{Result, bail};
use log::{debug, info, warn};
use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use crate::whitelist;

/// Shared, thread-safe set of currently frozen PIDs.
pub type FrozenSet = Arc<Mutex<HashSet<u32>>>;

pub fn new_frozen_set() -> FrozenSet {
    Arc::new(Mutex::new(HashSet::new()))
}

// ── Freeze ──────────────────────────────────────────────────────────────────

/// Freeze one process.
/// Returns Ok(true) if frozen, Ok(false) if whitelisted/already frozen,
/// Err if the signal failed (process likely already dead).
pub fn freeze_pid(
    pid: u32,
    frozen: &FrozenSet,
    our_pids: &HashSet<u32>,
) -> Result<bool> {
    if whitelist::is_whitelisted(pid, our_pids) {
        return Ok(false);
    }

    {
        let set = frozen.lock().unwrap();
        if set.contains(&pid) {
            return Ok(false); // already frozen
        }
    }

    debug!("freezing pid {}", pid);
    kill(Pid::from_raw(pid as i32), Signal::SIGSTOP)
        .map_err(|e| anyhow::anyhow!("SIGSTOP pid {}: {}", pid, e))?;

    frozen.lock().unwrap().insert(pid);
    Ok(true)
}

/// Thaw one process.
pub fn thaw_pid(pid: u32, frozen: &FrozenSet) -> Result<()> {
    debug!("thawing pid {}", pid);
    // Ignore ESRCH — process may have exited while frozen.
    match kill(Pid::from_raw(pid as i32), Signal::SIGCONT) {
        Ok(_) => {}
        Err(nix::errno::Errno::ESRCH) => {
            debug!("pid {} already gone, skipping SIGCONT", pid);
        }
        Err(e) => bail!("SIGCONT pid {}: {}", pid, e),
    }
    frozen.lock().unwrap().remove(&pid);
    Ok(())
}

// ── Bulk operations ─────────────────────────────────────────────────────────

/// Freeze all non-whitelisted background processes.
/// `focus_pid` and its entire process tree (children, grandchildren…) are
/// excluded — this covers launchers that spawn the real game process.
pub fn freeze_all_background(
    focus_pid: Option<u32>,
    frozen: &FrozenSet,
    our_pids: &HashSet<u32>,
) -> Vec<u32> {
    let pids = whitelist::all_pids();

    // Build the complete descendant set of the focus app once, then use it
    // as a fast membership test below.
    let focus_tree: HashSet<u32> = match focus_pid {
        Some(fpid) => whitelist::get_process_tree(fpid),
        None       => HashSet::new(),
    };

    let mut newly_frozen = Vec::new();

    for pid in pids {
        // Skip the focus app and all its descendants.
        if focus_tree.contains(&pid) {
            continue;
        }
        match freeze_pid(pid, frozen, our_pids) {
            Ok(true)  => newly_frozen.push(pid),
            Ok(false) => {}
            Err(e)    => warn!("could not freeze pid {}: {}", pid, e),
        }
    }

    info!("froze {} background processes", newly_frozen.len());
    newly_frozen
}

/// Thaw all processes in the frozen set.
pub fn thaw_all(frozen: &FrozenSet) {
    let pids: Vec<u32> = frozen.lock().unwrap().iter().copied().collect();
    info!("thawing {} processes", pids.len());
    for pid in pids {
        if let Err(e) = thaw_pid(pid, frozen) {
            warn!("thaw pid {}: {}", pid, e);
        }
    }
}

/// Thaw all PIDs in `pids` (used during crash recovery when we have a saved
/// set from the state file, not from an in-memory FrozenSet).
pub fn thaw_pid_list(pids: &[u32]) {
    let dummy = new_frozen_set();
    for &pid in pids {
        // Pre-populate so thaw_pid removes from set correctly.
        dummy.lock().unwrap().insert(pid);
        if let Err(e) = thaw_pid(pid, &dummy) {
            warn!("recovery thaw pid {}: {}", pid, e);
        }
    }
}
