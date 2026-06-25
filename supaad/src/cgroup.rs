//! cgroup v2 helpers.
//!
//! Layout under /sys/fs/cgroup/supaa/:
//!
//!   supaa/             ← supaa root slice (Delegate=yes in systemd unit gives
//!                        us ownership of this subtree)
//!     focus/           ← focused application's process tree
//!     background/      ← every other user process
//!
//! Key additions vs supaa2:
//!   * ensure_cgroups()      — idempotent re-init (safe to call on every mode
//!                             transition; does NOT tear down existing dirs)
//!   * move_tree_to_focus()  — moves an entire process tree (P0.3 fix)
//!   * populate_background() — moves all non-focus user processes into the bg
//!                             cgroup so that cpu/io weights actually take effect
//!                             (P0.4 fix)

use anyhow::{Context, Result, bail};
use log::{debug, info, warn};
use std::collections::HashSet;
use std::fs;
use std::path::Path;

use crate::whitelist;

const SUPAA_ROOT: &str  = "/sys/fs/cgroup/supaa";
const FOCUS_PATH: &str  = "/sys/fs/cgroup/supaa/focus";
const BG_PATH: &str     = "/sys/fs/cgroup/supaa/background";
const CGROUP_ROOT: &str = "/sys/fs/cgroup";

// ── Public API ────────────────────────────────────────────────────────────────

/// Verify that cgroup v2 is available and that we can write to the supaa
/// subtree.  Returns Ok if we can proceed.
pub fn check_cgroup_v2() -> Result<()> {
    if !Path::new(CGROUP_ROOT).exists() {
        bail!("cgroup root /sys/fs/cgroup does not exist");
    }
    let mounts = fs::read_to_string("/proc/mounts").context("read /proc/mounts")?;
    if !mounts.contains("cgroup2") {
        bail!(
            "cgroup v2 (unified hierarchy) is not mounted. \
             Make sure your kernel is ≥5.2 and systemd is using the \
             unified hierarchy (systemd.unified_cgroup_hierarchy=1)."
        );
    }
    Ok(())
}

/// Initialise the supaa cgroup hierarchy.  Safe to call on every startup.
pub fn init_cgroups() -> Result<()> {
    check_cgroup_v2()?;
    ensure_cgroups()
}

/// Idempotent re-init: create dirs and enable controllers without tearing down
/// anything that already exists.  Call before every apply_mode() so that a
/// previous teardown_cgroups() (e.g. after emergency restore) does not leave
/// apply_mode() writing to directories that no longer exist.  (P0.2 fix)
pub fn ensure_cgroups() -> Result<()> {
    ensure_dir(SUPAA_ROOT)?;

    // Enable controllers at the cgroup root (may already be done by systemd's
    // Delegate=yes — log a warning if we can't, but carry on).
    enable_controllers(CGROUP_ROOT).unwrap_or_else(|e| {
        warn!(
            "could not enable controllers at cgroup root (OK if \
             Delegate=yes is set in the unit): {}",
            e
        );
    });

    enable_controllers(SUPAA_ROOT)?;
    ensure_dir(FOCUS_PATH)?;
    ensure_dir(BG_PATH)?;

    info!("cgroup hierarchy ensured: {}", SUPAA_ROOT);
    Ok(())
}

/// Set CPU weight for the focus cgroup. Range 1–10000, default 100.
pub fn set_focus_cpu_weight(weight: u64) -> Result<()> {
    write_cgroup(FOCUS_PATH, "cpu.weight", &weight.to_string())
}

/// Set CPU weight for the background cgroup.
pub fn set_bg_cpu_weight(weight: u64) -> Result<()> {
    write_cgroup(BG_PATH, "cpu.weight", &weight.to_string())
}

/// Set I/O weight for the focus cgroup.
pub fn set_focus_io_weight(weight: u64) -> Result<()> {
    write_cgroup(FOCUS_PATH, "io.weight", &format!("default {}", weight))
}

/// Set I/O weight for the background cgroup.
pub fn set_bg_io_weight(weight: u64) -> Result<()> {
    write_cgroup(BG_PATH, "io.weight", &format!("default {}", weight))
}

/// Set memory.high for the background cgroup.  None → "max" (no limit).
pub fn set_bg_memory_high(bytes: Option<u64>) -> Result<()> {
    let value = match bytes {
        Some(b) => b.to_string(),
        None => "max".to_owned(),
    };
    write_cgroup(BG_PATH, "memory.high", &value)
}

/// Move a single PID into the focus cgroup.
pub fn move_to_focus(pid: u32) -> Result<()> {
    debug!("moving pid {} to focus cgroup", pid);
    write_cgroup(FOCUS_PATH, "cgroup.procs", &pid.to_string())
}

/// Move an entire process tree into the focus cgroup.  (P0.3 fix)
///
/// Silently skips PIDs that have already exited or that we can't move.
pub fn move_tree_to_focus(root_pid: u32) {
    let tree = whitelist::get_process_tree(root_pid);
    debug!("moving process tree ({} pids) to focus cgroup", tree.len());
    for pid in &tree {
        if let Err(e) = move_to_focus(*pid) {
            debug!("could not move pid {} to focus: {}", pid, e);
        }
    }
}

/// Move a single PID into the background cgroup.
pub fn move_to_background(pid: u32) -> Result<()> {
    debug!("moving pid {} to background cgroup", pid);
    write_cgroup(BG_PATH, "cgroup.procs", &pid.to_string())
}

/// Populate the background cgroup with all user processes that are neither in
/// the focus tree nor whitelisted.  This is what makes cpu/io weight settings
/// actually take effect — without it the background cgroup is empty and the
/// kernel ignores its weights.  (P0.4 fix)
///
/// Only touches processes under /user.slice/ (avoids system services).
/// Silently skips any PID that has exited or is un-moveable.
pub fn populate_background(focus_pid: Option<u32>, our_pids: &HashSet<u32>) {
    let focus_tree: HashSet<u32> = match focus_pid {
        Some(fpid) => whitelist::get_process_tree(fpid),
        None => HashSet::new(),
    };

    for pid in whitelist::all_pids() {
        if focus_tree.contains(&pid) {
            continue;
        }
        if whitelist::is_whitelisted(pid, our_pids) {
            continue;
        }
        // Only move user-space processes (those living under /user.slice/).
        // This leaves system services (which often share a cgroup with many
        // children) completely undisturbed.
        if !is_user_process(pid) {
            continue;
        }
        if let Err(e) = move_to_background(pid) {
            debug!("could not move pid {} to background: {}", pid, e);
        }
    }
}

/// Move a PID back to the root cgroup.  Silently ignores errors.
pub fn release_pid(pid: u32) {
    let _ = write_cgroup(CGROUP_ROOT, "cgroup.procs", &pid.to_string());
}

/// Reset weights to sane defaults and remove cgroup directories.
/// Call this only on shutdown or emergency restore — not on mode transitions.
/// For mode transitions use policy::restore_on_transition() instead.
pub fn teardown_cgroups() -> Result<()> {
    info!("tearing down cgroup policies");

    let _ = write_cgroup(FOCUS_PATH, "cpu.weight", "100");
    let _ = write_cgroup(BG_PATH,    "cpu.weight", "100");
    let _ = write_cgroup(FOCUS_PATH, "io.weight",  "default 100");
    let _ = write_cgroup(BG_PATH,    "io.weight",  "default 100");
    let _ = write_cgroup(BG_PATH,    "memory.high","max");

    drain_cgroup(FOCUS_PATH);
    drain_cgroup(BG_PATH);

    let _ = fs::remove_dir(FOCUS_PATH);
    let _ = fs::remove_dir(BG_PATH);
    let _ = fs::remove_dir(SUPAA_ROOT);

    Ok(())
}

/// Read the total installed memory in bytes from /proc/meminfo.
pub fn total_memory_bytes() -> u64 {
    let Ok(content) = fs::read_to_string("/proc/meminfo") else {
        return 0;
    };
    for line in content.lines() {
        if line.starts_with("MemTotal:") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if let Some(kb) = parts.get(1).and_then(|s| s.parse::<u64>().ok()) {
                return kb * 1024;
            }
        }
    }
    0
}

// ── Private helpers ───────────────────────────────────────────────────────────

fn ensure_dir(path: &str) -> Result<()> {
    fs::create_dir_all(path).with_context(|| format!("create cgroup dir {}", path))
}

fn write_cgroup(dir: &str, filename: &str, value: &str) -> Result<()> {
    let path = format!("{}/{}", dir, filename);
    debug!("cgroup write: {} = {}", path, value);
    fs::write(&path, value).with_context(|| format!("write {} = {}", path, value))
}

fn enable_controllers(dir: &str) -> Result<()> {
    write_cgroup(dir, "cgroup.subtree_control", "+cpu +io +memory")
}

fn drain_cgroup(cgroup_dir: &str) {
    let procs_path = format!("{}/cgroup.procs", cgroup_dir);
    if let Ok(content) = fs::read_to_string(&procs_path) {
        for line in content.lines() {
            if let Ok(pid) = line.trim().parse::<u32>() {
                let _ = fs::write(
                    format!("{}/cgroup.procs", CGROUP_ROOT),
                    pid.to_string(),
                );
            }
        }
    }
}

/// Returns true if the process lives under /user.slice/ (a user desktop
/// process) — as opposed to /system.slice/ or other system cgroups.
fn is_user_process(pid: u32) -> bool {
    let path = format!("/proc/{}/cgroup", pid);
    if let Ok(content) = fs::read_to_string(path) {
        for line in content.lines() {
            let parts: Vec<&str> = line.splitn(3, ':').collect();
            if parts.len() == 3 && parts[0] == "0" {
                return parts[2].starts_with("/user.slice")
                    || parts[2].starts_with("/supaa"); // already in our cgroups
            }
        }
    }
    false
}
