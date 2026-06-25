//! Process whitelist — gates every freeze / cgroup-move operation.
//!
//! Cherry-picked from supaa1: ancestor-chain walk (depth-8) guards against
//! freezing a child of a whitelisted process.  Removed hardcoded UID-1000
//! assumption — session scopes are detected by cgroup path pattern instead.

use std::collections::HashSet;
use std::fs;
use log::warn;

/// Well-known process names that must never be touched.
const WHITELISTED_NAMES: &[&str] = &[
    "systemd", "init",
    "dbus-daemon", "dbus-broker",
    "login", "sddm", "gdm", "lightdm", "greetd", "systemd-logind",
    "Hyprland", "hyprland", "sway", "river", "weston",
    "xdg-desktop-portal", "xdg-desktop-portal-hyprland",
    "xdg-desktop-portal-gtk", "xdg-document-portal",
    "xdg-permission-store",
    "pipewire", "pipewire-media-session", "wireplumber", "pulseaudio",
    "NetworkManager", "systemd-networkd", "systemd-resolved",
    "wpa_supplicant", "iwd", "dhclient", "dhcpcd",
    "gpg-agent", "ssh-agent", "polkit",
    // Supaa's own binaries — never freeze these
    "supaad", "supaa", "supaa-ui",
    // Kernel helpers
    "kthreadd", "migration", "rcu_", "ksoftirqd", "kworker",
    "kdevtmpfs", "khungtaskd", "oom_reaper", "writeback",
    "kcompactd", "kswapd",
];

// ── /proc helpers ────────────────────────────────────────────────────────────

/// Read /proc/<pid>/comm (short process name, trimmed).
pub fn proc_comm(pid: u32) -> Option<String> {
    fs::read_to_string(format!("/proc/{}/comm", pid))
        .ok()
        .map(|s| s.trim().to_owned())
}

/// Read /proc/<pid>/cgroup and return the v2 path (the "0::" line's 3rd field).
fn proc_cgroup(pid: u32) -> Option<String> {
    let content = fs::read_to_string(format!("/proc/{}/cgroup", pid)).ok()?;
    for line in content.lines() {
        let parts: Vec<&str> = line.splitn(3, ':').collect();
        if parts.len() == 3 && parts[0] == "0" {
            return Some(parts[2].to_owned());
        }
    }
    None
}

/// Read the parent PID of `pid` from /proc/<pid>/status.
pub fn get_ppid(pid: u32) -> Option<u32> {
    let content = fs::read_to_string(format!("/proc/{}/status", pid)).ok()?;
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("PPid:") {
            return rest.trim().parse().ok();
        }
    }
    None
}

// ── Whitelist check ──────────────────────────────────────────────────────────

/// Returns true if this process is protected and must not be touched.
pub fn is_whitelisted(pid: u32, our_pids: &HashSet<u32>) -> bool {
    // 1. Our own daemon and any workers we spawned.
    if our_pids.contains(&pid) {
        return true;
    }

    // 2. PID 1 (init) and PID 2 (kthreadd) are always protected.
    if pid <= 2 {
        return true;
    }

    // 3. Kernel threads have no /proc/<pid>/exe symlink.
    if !std::path::Path::new(&format!("/proc/{}/exe", pid)).exists() {
        return true;
    }

    // 4. Check comm name against static list.
    if let Some(comm) = proc_comm(pid) {
        for &wl in WHITELISTED_NAMES {
            if comm == wl || comm.starts_with(wl) {
                return true;
            }
        }
    }

    // 5. Check cgroup path.
    //    System services live under /system.slice or /init.scope — always protected.
    //    Session managers live in scopes matching "session-N.scope" — protect those
    //    too, regardless of UID (fixes the hardcoded user-1000 bug from supaa2).
    if let Some(cg) = proc_cgroup(pid) {
        if cg.starts_with("/system.slice") || cg.starts_with("/init.scope") {
            return true;
        }
        // Protect login session scope (e.g. /user.slice/user-1000.slice/session-3.scope)
        // without hardcoding the UID.
        if cg.contains("/session-") && cg.ends_with(".scope") {
            return true;
        }
        // Processes already in one of our managed cgroups are handled by us — not
        // whitelisted (we still need to control where they sit).
    }

    // 6. Ancestor-chain check (cherry-picked from supaa1's exclusions.py).
    //    Don't freeze a child of a whitelisted process.
    if has_whitelisted_ancestor(pid) {
        return true;
    }

    false
}

/// Walk up to 8 levels of parent PIDs.  If any ancestor has a whitelisted
/// comm name, the process is considered protected.
fn has_whitelisted_ancestor(pid: u32) -> bool {
    let mut current = pid;
    let mut seen = HashSet::new();
    seen.insert(pid);

    for _ in 0..8 {
        let ppid = match get_ppid(current) {
            Some(p) => p,
            None => break,
        };
        if ppid <= 1 || seen.contains(&ppid) {
            break;
        }
        seen.insert(ppid);

        if let Some(comm) = proc_comm(ppid) {
            for &wl in WHITELISTED_NAMES {
                if comm == wl || comm.starts_with(wl) {
                    return true;
                }
            }
        }
        current = ppid;
    }
    false
}

// ── Process enumeration ──────────────────────────────────────────────────────

/// Collect all PIDs currently visible in /proc.
pub fn all_pids() -> Vec<u32> {
    let Ok(rd) = fs::read_dir("/proc") else {
        warn!("cannot read /proc");
        return vec![];
    };
    rd.flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .collect()
}

// ── Process-tree BFS (unchanged from supaa2 — already correct) ───────────────

/// Return every PID in the process tree rooted at `root_pid` (inclusive).
/// BFS over parent→children adjacency map built from a single /proc scan.
pub fn get_process_tree(root_pid: u32) -> HashSet<u32> {
    use std::collections::HashMap;

    let all = all_pids();

    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for &pid in &all {
        if let Some(ppid) = get_ppid(pid) {
            children.entry(ppid).or_default().push(pid);
        }
    }

    let mut tree = HashSet::new();
    let mut queue = vec![root_pid];
    while let Some(pid) = queue.pop() {
        if tree.insert(pid) {
            if let Some(kids) = children.get(&pid) {
                queue.extend(kids.iter().copied());
            }
        }
    }
    tree
}
