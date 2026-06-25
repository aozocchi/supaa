//! Hyprland IPC integration.
//!
//! Workspace lock strategy
//! ───────────────────────
//! Two methods are supported and tried in order:
//!
//!   1. Submap (cherry-picked from supaa1, preferred)
//!      Requires the user to source assets/hypr-supaa.conf from their Hyprland
//!      config.  On lock we dispatch into the `supaa_lock` submap which
//!      contains no binds, swallowing all key events.  On unlock we reset the
//!      submap.  This is the cleanest approach: it uses a Hyprland-native
//!      mechanism designed for exactly this purpose.
//!
//!   2. Keyword bind/unbind (supaa2 fallback, works without any user config)
//!      Overrides the standard workspace-switch binds with no-op exec,true
//!      commands.  Less elegant but works out-of-the-box.
//!
//! Socket discovery
//! ────────────────
//! The daemon runs as root (via systemd).  Hyprland's socket is created by the
//! *user* compositor and lives under:
//!   $XDG_RUNTIME_DIR/hypr/<SIG>/.socket.sock   (modern Hyprland ≥ 0.40)
//!   /tmp/hypr/<SIG>/.socket.sock               (older / root)
//!
//! We try all known paths so the daemon works regardless of Hyprland version.

use anyhow::{Context, Result};
use log::{debug, info, warn};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

// ── Socket discovery ─────────────────────────────────────────────────────────

fn candidate_hypr_dirs() -> Vec<PathBuf> {
    let mut candidates = vec![];

    // 1. Explicit env var (most reliable).
    if let Ok(sig) = std::env::var("HYPRLAND_INSTANCE_SIGNATURE") {
        candidates.push(PathBuf::from(format!("/tmp/hypr/{}", sig)));
        // Also check XDG_RUNTIME_DIR paths for all users (e.g. uid 1000, 1001 …).
        for uid in 1000..1100u32 {
            candidates.push(PathBuf::from(format!("/run/user/{}/hypr/{}", uid, sig)));
        }
    }

    // 2. Enumerate /tmp/hypr/*.
    if let Ok(rd) = std::fs::read_dir("/tmp/hypr") {
        for e in rd.flatten() {
            candidates.push(e.path());
        }
    }

    // 3. Enumerate /run/user/*/hypr/*.
    if let Ok(rd) = std::fs::read_dir("/run/user") {
        for uid_entry in rd.flatten() {
            let hypr = uid_entry.path().join("hypr");
            if let Ok(rd2) = std::fs::read_dir(hypr) {
                for sig_entry in rd2.flatten() {
                    candidates.push(sig_entry.path());
                }
            }
        }
    }

    candidates
}

fn hyprland_socket() -> Result<PathBuf> {
    for dir in candidate_hypr_dirs() {
        let sock = dir.join(".socket.sock");
        if sock.exists() {
            return Ok(sock);
        }
    }
    anyhow::bail!(
        "Hyprland socket not found. Is Hyprland running? \
         Set HYPRLAND_INSTANCE_SIGNATURE if needed."
    )
}

// ── Low-level IPC ─────────────────────────────────────────────────────────────

fn dispatch(cmd: &str) -> Result<String> {
    let sock_path = hyprland_socket()?;
    let mut stream = UnixStream::connect(&sock_path)
        .with_context(|| format!("connect to {:?}", sock_path))?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;

    stream.write_all(cmd.as_bytes()).context("write to Hyprland socket")?;
    stream.shutdown(std::net::Shutdown::Write).context("shutdown write")?;

    let mut reply = String::new();
    stream.read_to_string(&mut reply).context("read reply")?;
    debug!("hyprland dispatch `{}` → `{}`", cmd, reply.trim());
    Ok(reply)
}

// ── Workspace key list (fallback method) ─────────────────────────────────────

fn workspace_binds() -> Vec<(String, String)> {
    let mut binds: Vec<(String, String)> = Vec::new();
    for i in 1..=9u8 { binds.push(("SUPER".into(), format!("{}", i))); }
    binds.push(("SUPER".into(), "0".into()));
    binds.push(("SUPER".into(), "left".into()));
    binds.push(("SUPER".into(), "right".into()));
    binds.push(("SUPER CTRL".into(), "left".into()));
    binds.push(("SUPER CTRL".into(), "right".into()));
    binds
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Lock workspace switching.
///
/// Tries the submap method first (clean, no user config churn).  Falls back to
/// keyword bind overrides.  Returns Ok(true) if any method succeeded.
pub fn lock_workspace_switching() -> Result<bool> {
    // Method 1: submap (cherry-picked from supaa1).
    // The submap is defined in assets/hypr-supaa.conf which the user sources.
    // If it's not available the dispatch will return an error — that's fine.
    if dispatch("dispatch submap supaa_lock").is_ok() {
        info!("workspace switching locked via submap");
        return Ok(true);
    }

    // Method 2: keyword bind/unbind fallback.
    let binds = workspace_binds();
    let mut any_ok = false;
    for (mods, key) in &binds {
        let cmd = format!("keyword bind {},{},exec,true", mods, key);
        match dispatch(&cmd) {
            Ok(_)  => { any_ok = true; }
            Err(e) => { warn!("workspace lock bind {}+{}: {}", mods, key, e); }
        }
    }
    if any_ok {
        info!("workspace switching locked via keyword bind ({} binds)", binds.len());
    }
    Ok(any_ok)
}

/// Unlock workspace switching.  Mirrors lock_workspace_switching().
pub fn unlock_workspace_switching() -> Result<bool> {
    // Method 1: exit submap.
    if dispatch("dispatch submap reset").is_ok() {
        info!("workspace switching unlocked via submap reset");
        return Ok(true);
    }

    // Method 2: keyword unbind fallback.
    let binds = workspace_binds();
    let mut any_ok = false;
    for (mods, key) in &binds {
        let cmd = format!("keyword unbind {},{}", mods, key);
        match dispatch(&cmd) {
            Ok(_)  => { any_ok = true; }
            Err(e) => { warn!("workspace unlock unbind {}+{}: {}", mods, key, e); }
        }
    }
    if any_ok {
        info!("workspace switching unlocked via keyword unbind");
    }
    Ok(any_ok)
}

/// Return the active window's (pid, class, title) or None if unavailable.
#[allow(dead_code)]
pub fn active_window() -> Option<(u32, String, String)> {
    let reply = dispatch("j/activewindow").ok()?;
    let pid   = extract_json_u32(&reply, "pid")?;
    let class = extract_json_str(&reply, "class").unwrap_or_default();
    let title = extract_json_str(&reply, "title").unwrap_or_default();
    Some((pid, class, title))
}

fn extract_json_u32(json: &str, key: &str) -> Option<u32> {
    let needle = format!("\"{}\":", key);
    let start  = json.find(&needle)? + needle.len();
    let rest   = json[start..].trim_start();
    let end    = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    rest[..end].parse().ok()
}

fn extract_json_str(json: &str, key: &str) -> Option<String> {
    let needle = format!("\"{}\":\"", key);
    let start  = json.find(&needle)? + needle.len();
    let end    = json[start..].find('"')?;
    Some(json[start..start + end].to_owned())
}
