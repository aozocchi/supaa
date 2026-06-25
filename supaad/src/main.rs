mod cgroup;
mod daemon;
mod freezer;
mod hyprland;
mod policy;
mod recovery;
mod whitelist;

use anyhow::Result;
use log::{error, info, warn};
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::daemon::Daemon;
use crate::recovery::run_recovery;

#[tokio::main]
async fn main() -> Result<()> {
    // ── Logging ────────────────────────────────────────────────────────────
    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Info)
        .init();

    info!("supaad starting — pid {}", std::process::id());

    // ── Ensure runtime dirs exist ──────────────────────────────────────────
    for dir in &[
        "/var/lib/supaa",
        "/var/log/supaa",
        "/sys/fs/cgroup/supaa",
    ] {
        if let Err(e) = std::fs::create_dir_all(dir) {
            // cgroup dir will fail if cgroup v2 is not mounted — that is OK;
            // we will detect and report it later in cgroup.rs.
            warn!("could not create {}: {}", dir, e);
        }
    }

    // ── Recovery pass ─────────────────────────────────────────────────────
    // If the previous run left a dirty state file, restore everything before
    // we start accepting new commands.
    if let Err(e) = run_recovery() {
        error!("recovery pass failed: {}", e);
        // Non-fatal — continue startup; operator can run emergency-restore.sh
    }

    // ── Build daemon and run ───────────────────────────────────────────────
    let daemon = Arc::new(Mutex::new(Daemon::new()?));
    daemon::run(daemon).await
}
