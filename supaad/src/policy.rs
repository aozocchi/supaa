//! Policy engine — translates a Mode into concrete cgroup + OS calls.
//!
//! Key fixes vs supaa2:
//!
//!   P0.2  restore_on_transition() thaws + unlocks WITHOUT tearing down cgroup
//!         directories.  This means apply_mode() after a mode switch always
//!         finds the directories it needs.  teardown_cgroups() is only called
//!         on daemon shutdown or emergency restore.
//!
//!   P0.3  apply_mode() now moves the focus *process tree* into the focus cgroup,
//!         not just the single PID that was registered.
//!
//!   P0.4  apply_mode() calls cgroup::populate_background() so the background
//!         cgroup actually contains processes and the cpu/io weights take effect.

use anyhow::Result;
use log::{info, warn};
use std::collections::HashSet;

use supaa_core::modes::Mode;
use crate::{cgroup, freezer, hyprland};
use crate::freezer::FrozenSet;

/// Apply a full mode transition.
///
/// **Normal is a complete no-op** — returns immediately without touching
/// cgroups, the freezer, or Hyprland.  The caller's `restore_on_transition()`
/// already reset everything before we get here.
///
/// Precondition (Supaa / Supaa++): cgroup directories must exist.
/// Call `cgroup::ensure_cgroups()` before this on any code path that may
/// have previously called `teardown_cgroups()`.
pub fn apply_mode(
    mode: Mode,
    focus_pid: Option<u32>,
    frozen: &FrozenSet,
    our_pids: &HashSet<u32>,
) -> Result<()> {
    // ── GUARD: Normal = Supaa completely inactive ──────────────────────────
    // Zero cgroup manipulation, zero freezer operations, zero Hyprland calls.
    // restore_on_transition() already reset weights and thawed all processes;
    // there is nothing left to do.
    if mode == Mode::Normal {
        info!("apply_mode: Normal — no-op");
        return Ok(());
    }

    info!("applying mode: {}", mode.as_str());

    // ── 0. Make sure cgroup directories exist (idempotent). ───────────────
    // This guards against a previous teardown_cgroups() having removed them.
    // (P0.2 fix: re-init before every apply so writes can never fail with
    //  "no such file or directory".)
    cgroup::ensure_cgroups()
        .unwrap_or_else(|e| warn!("ensure_cgroups: {}", e));

    // ── 1. CPU weights ────────────────────────────────────────────────────
    cgroup::set_focus_cpu_weight(mode.focus_cpu_weight())
        .unwrap_or_else(|e| warn!("focus cpu weight: {}", e));
    cgroup::set_bg_cpu_weight(mode.bg_cpu_weight())
        .unwrap_or_else(|e| warn!("bg cpu weight: {}", e));

    // ── 2. I/O weights ────────────────────────────────────────────────────
    cgroup::set_focus_io_weight(mode.focus_io_weight())
        .unwrap_or_else(|e| warn!("focus io weight: {}", e));
    cgroup::set_bg_io_weight(mode.bg_io_weight())
        .unwrap_or_else(|e| warn!("bg io weight: {}", e));

    // ── 3. Memory.high for background ─────────────────────────────────────
    let mem_high = mode.bg_memory_high_percent().map(|pct| {
        let total = cgroup::total_memory_bytes();
        total * pct / 100
    });
    cgroup::set_bg_memory_high(mem_high)
        .unwrap_or_else(|e| warn!("bg memory.high: {}", e));

    // ── 4. Move focus process *tree* to focus cgroup (P0.3 fix) ──────────
    if let Some(pid) = focus_pid {
        cgroup::move_tree_to_focus(pid);
    }

    // ── 5. Populate background cgroup with all other user processes (P0.4) ─
    cgroup::populate_background(focus_pid, our_pids);

    // ── 6. Workspace lock ─────────────────────────────────────────────────
    if mode.should_lock_workspace() {
        hyprland::lock_workspace_switching()
            .unwrap_or_else(|e| { warn!("workspace lock: {}", e); false });
    } else {
        hyprland::unlock_workspace_switching()
            .unwrap_or_else(|e| { warn!("workspace unlock: {}", e); false });
    }

    // ── 7. Freeze / thaw background ───────────────────────────────────────
    if mode.should_freeze_background() {
        freezer::freeze_all_background(focus_pid, frozen, our_pids);
    } else {
        freezer::thaw_all(frozen);
    }

    Ok(())
}

/// Light restore for **mode transitions** (not shutdown).
///
/// Thaws all frozen processes and unlocks workspace switching, but does NOT
/// tear down the cgroup directory hierarchy.  The directories remain so that
/// the next apply_mode() call can write to them without re-creating them first.
/// (P0.2 fix: calling restore_all() on a mode switch was the root cause of
///  apply_mode() failing with ENOENT after the mode transition.)
pub fn restore_on_transition(frozen: &FrozenSet) {
    info!("restore_on_transition: thawing and unlocking (cgroups kept)");

    freezer::thaw_all(frozen);

    // Reset weights to defaults so processes get fair scheduling while the
    // new mode is being applied.
    let _ = cgroup::set_focus_cpu_weight(100);
    let _ = cgroup::set_bg_cpu_weight(100);
    let _ = cgroup::set_focus_io_weight(100);
    let _ = cgroup::set_bg_io_weight(100);
    let _ = cgroup::set_bg_memory_high(None);

    hyprland::unlock_workspace_switching()
        .unwrap_or_else(|e| { warn!("workspace unlock during transition: {}", e); false });
}

/// Full restore for **daemon shutdown or emergency**.
///
/// Thaws processes, unlocks workspace, and tears down cgroup directories.
/// Do NOT call this on a mode transition — use restore_on_transition() instead.
pub fn restore_all(frozen: &FrozenSet) {
    info!("restore_all: full teardown");

    freezer::thaw_all(frozen);

    cgroup::teardown_cgroups()
        .unwrap_or_else(|e| warn!("cgroup teardown: {}", e));

    hyprland::unlock_workspace_switching()
        .unwrap_or_else(|e| { warn!("workspace unlock during restore: {}", e); false });
}
