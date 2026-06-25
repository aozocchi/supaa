//! Daemon core: IPC socket server + state machine.
//!
//! Key fixes vs supaa2:
//!
//!   P0.2  handle_set_mode() now calls policy::restore_on_transition() instead
//!         of policy::restore_all() when switching modes.  restore_on_transition
//!         thaws and unlocks workspace but does NOT tear down cgroup directories,
//!         so the subsequent apply_mode() can write to them without ENOENT.
//!
//!   P0.3  handle_set_focus() moves the entire process tree (via
//!         cgroup::move_tree_to_focus) rather than a single PID.
//!
//!   P1    Focus cgroup is properly drained (process tree released) on
//!         handle_clear_focus() so stale PIDs don't linger in the focus cgroup.
//!
//!   P2    handle_connection() now sends a Response::Error on JSON parse failure
//!         instead of silently closing the connection (fixes CLI hanging on bad
//!         input and makes errors visible to callers).

use anyhow::{Context, Result};
use log::{error, info, warn};
use nix::unistd::{chown, Gid};
use std::collections::HashSet;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Mutex;

use supaa_core::modes::Mode;
use supaa_core::protocol::{Command, DaemonStatus, ProcessInfo, Response, SOCKET_PATH};
use supaa_core::state::PersistedState;

use crate::{cgroup, freezer, policy, recovery, whitelist};
use crate::freezer::FrozenSet;

// ─────────────────────────────────────────────────────────────────────────────

pub struct Daemon {
    pub mode:             Mode,
    pub focus_pid:        Option<u32>,
    pub focus_name:       Option<String>,
    pub frozen:           FrozenSet,
    pub our_pids:         HashSet<u32>,
    pub state:            PersistedState,
    pub workspace_locked: bool,
    pub start:            Instant,
}

impl Daemon {
    pub fn new() -> Result<Self> {
        let mut our_pids = HashSet::new();
        our_pids.insert(std::process::id());

        if let Err(e) = cgroup::init_cgroups() {
            warn!("cgroup init failed (continuing without cgroup support): {}", e);
        }

        Ok(Self {
            mode:             Mode::Normal,
            focus_pid:        None,
            focus_name:       None,
            frozen:           freezer::new_frozen_set(),
            our_pids,
            state:            PersistedState::new(),
            workspace_locked: false,
            start:            Instant::now(),
        })
    }

    // ── Command handlers ─────────────────────────────────────────────────────

    fn handle_set_mode(&mut self, mode: Mode) -> Response {
        // ── Same-mode guard ───────────────────────────────────────────────
        // Re-applying the current mode is a no-op.  This prevents a spurious
        // mark_dirty / mark_clean cycle and avoids redundant cgroup writes.
        if self.mode == mode {
            info!("handle_set_mode: already in {} — skipping", mode.as_str());
            return Response::Ok;
        }

        info!("mode change: {} → {}", self.mode.as_str(), mode.as_str());
        recovery::mark_dirty(&mut self.state);

        // ── Transition out of the current mode ────────────────────────────
        // If we are leaving a non-Normal mode: thaw all frozen processes and
        // unlock workspace switching, but keep the cgroup directory hierarchy
        // alive so apply_mode() can write to it without ENOENT.
        // (P0.2 fix: the old code called restore_all() here which tore down
        //  cgroup directories, causing apply_mode() to fail.)
        if self.mode != Mode::Normal {
            policy::restore_on_transition(&self.frozen);

            // When the *destination* is Normal we go the extra step and tear
            // down the cgroup dirs so that no process remains classified in a
            // Supaa-managed cgroup.  This makes Normal truly off.
            // apply_mode(Normal) is a no-op, so the dirs are not needed again
            // until the user activates Supaa or Supaa++ — at which point
            // ensure_cgroups() re-creates them idempotently.
            if mode == Mode::Normal {
                if let Err(e) = cgroup::teardown_cgroups() {
                    warn!("cgroup teardown on Normal transition: {}", e);
                }
            }
        }

        self.mode = mode;
        self.state.mode = mode;

        // apply_mode(Normal) is a documented no-op.  For Supaa / Supaa++ it
        // calls ensure_cgroups() internally, so torn-down dirs are recreated.
        if let Err(e) = policy::apply_mode(
            mode,
            self.focus_pid,
            &self.frozen,
            &self.our_pids,
        ) {
            error!("apply_mode failed: {}", e);
        }

        self.workspace_locked = mode.should_lock_workspace();
        self.state.workspace_locked = self.workspace_locked;
        self.state.frozen_pids = self.frozen.lock().unwrap().clone();

        if mode == Mode::Normal {
            recovery::mark_clean(&mut self.state);
        } else {
            let _ = recovery::save_state(&self.state);
        }

        recovery::write_log(&format!("Mode set to {}", mode.as_str()));
        Response::Ok
    }

    fn handle_set_focus(&mut self, pid: u32, name: String) -> Response {
        info!("focus app: {} (pid {})", name, pid);
        self.focus_pid  = Some(pid);
        self.focus_name = Some(name.clone());
        self.state.focus_app_pid  = Some(pid);
        self.state.focus_app_name = Some(name);

        // Move the *entire process tree* into the focus cgroup so that all
        // threads and child processes benefit from the boosted weights.
        // (P0.3 fix: old code only moved the single registered PID.)
        cgroup::move_tree_to_focus(pid);

        Response::Ok
    }

    fn handle_clear_focus(&mut self) -> Response {
        if let Some(fpid) = self.focus_pid.take() {
            // Release the entire tree, not just the root PID.  (P1 fix: old
            // code left child processes stranded in the focus cgroup.)
            let tree = whitelist::get_process_tree(fpid);
            for pid in tree {
                cgroup::release_pid(pid);
            }
        }
        self.focus_name               = None;
        self.state.focus_app_pid      = None;
        self.state.focus_app_name     = None;
        Response::Ok
    }

    fn handle_get_status(&self) -> Response {
        Response::Status(DaemonStatus {
            mode:             self.mode,
            focus_app_pid:    self.focus_pid,
            focus_app_name:   self.focus_name.clone(),
            frozen_count:     self.frozen.lock().unwrap().len(),
            workspace_locked: self.workspace_locked,
            state_file_clean: !self.state.is_dirty(),
            uptime_secs:      self.start.elapsed().as_secs(),
        })
    }

    fn handle_get_process_list(&self) -> Response {
        let frozen_set = self.frozen.lock().unwrap();
        let list: Vec<ProcessInfo> = whitelist::all_pids()
            .into_iter()
            .filter_map(|pid| {
                let name = whitelist::proc_comm(pid)?;
                Some(ProcessInfo {
                    pid,
                    name,
                    is_frozen:      frozen_set.contains(&pid),
                    is_focus:       self.focus_pid == Some(pid),
                    is_whitelisted: whitelist::is_whitelisted(pid, &self.our_pids),
                    cpu_weight:     None,
                })
            })
            .collect();
        Response::ProcessList(list)
    }

    fn handle_emergency_restore(&mut self, quit_after: bool) -> Response {
        info!("emergency restore requested (quit_after={})", quit_after);
        recovery::write_log("Emergency restore triggered");
        recovery::mark_dirty(&mut self.state);

        // Full restore on emergency: thaw, unlock, tear down cgroups.
        policy::restore_all(&self.frozen);

        self.mode             = Mode::Normal;
        self.state.mode       = Mode::Normal;
        self.workspace_locked = false;
        self.state.workspace_locked = false;
        self.focus_pid        = None;
        self.focus_name       = None;
        self.state.focus_app_pid  = None;
        self.state.focus_app_name = None;
        self.state.frozen_pids.clear();

        recovery::mark_clean(&mut self.state);
        recovery::write_log("Emergency restore complete");

        if quit_after {
            info!("quitting after emergency restore");
            std::process::exit(0);
        }
        Response::Ok
    }

    pub fn dispatch(&mut self, cmd: Command) -> Response {
        match cmd {
            Command::SetMode { mode }              => self.handle_set_mode(mode),
            Command::SetFocusApp { pid, name }     => self.handle_set_focus(pid, name),
            Command::ClearFocusApp                 => self.handle_clear_focus(),
            Command::GetStatus                     => self.handle_get_status(),
            Command::GetProcessList                => self.handle_get_process_list(),
            Command::Shutdown                      => {
                info!("clean shutdown requested via IPC");
                self.shutdown();
                std::process::exit(0);
            }
            Command::EmergencyRestore { quit_after } => {
                self.handle_emergency_restore(quit_after)
            }
        }
    }

    pub fn shutdown(&mut self) {
        info!("supaad shutting down cleanly");
        recovery::write_log("Clean shutdown started");
        recovery::mark_dirty(&mut self.state);

        policy::restore_all(&self.frozen);

        self.state.mode             = Mode::Normal;
        self.state.workspace_locked = false;
        self.state.frozen_pids.clear();
        recovery::mark_clean(&mut self.state);
        recovery::write_log("Clean shutdown complete");
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  Top-level async run loop
// ─────────────────────────────────────────────────────────────────────────────

pub async fn run(daemon: Arc<Mutex<Daemon>>) -> Result<()> {
    let _ = std::fs::remove_file(SOCKET_PATH);

    if let Some(parent) = std::path::Path::new(SOCKET_PATH).parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create socket dir {:?}", parent))?;
    }

    let listener = UnixListener::bind(SOCKET_PATH)
        .with_context(|| format!("bind to {}", SOCKET_PATH))?;

    std::fs::set_permissions(SOCKET_PATH, std::fs::Permissions::from_mode(0o660))?;

    for group_name in &["supaa", "wheel"] {
        if let Some(gid) = lookup_gid(group_name) {
            match chown(
                std::path::Path::new(SOCKET_PATH),
                None,
                Some(Gid::from_raw(gid)),
            ) {
                Ok(()) => {
                    info!("socket group set to '{}' (gid {})", group_name, gid);
                }
                Err(e) => {
                    warn!("chown socket to {}: {}", group_name, e);
                }
            }
            break;
        }
    }

    info!("supaad listening on {} (mode 0660, group supaa)", SOCKET_PATH);

    // ── Signal handling ────────────────────────────────────────────────────
    let daemon_sig = daemon.clone();
    tokio::spawn(async move {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm = signal(SignalKind::terminate()).expect("SIGTERM handler");
        let mut sigint  = signal(SignalKind::interrupt()).expect("SIGINT handler");
        tokio::select! {
            _ = sigterm.recv() => info!("received SIGTERM"),
            _ = sigint.recv()  => info!("received SIGINT"),
        }
        daemon_sig.lock().await.shutdown();
        std::process::exit(0);
    });

    // ── Accept loop ───────────────────────────────────────────────────────
    loop {
        let (stream, _) = listener.accept().await.context("accept connection")?;
        let daemon = daemon.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, daemon).await {
                warn!("client connection error: {}", e);
            }
        });
    }
}

async fn handle_connection(
    mut stream: UnixStream,
    daemon: Arc<Mutex<Daemon>>,
) -> Result<()> {
    let mut buf = Vec::with_capacity(512);
    let mut tmp = [0u8; 512];
    loop {
        let n = tokio::time::timeout(
            Duration::from_secs(5),
            stream.read(&mut tmp),
        )
        .await
        .context("read timeout")??;

        if n == 0 { break; }
        buf.extend_from_slice(&tmp[..n]);
        if buf.len() > 64 * 1024 { break; }
        if buf.contains(&b'\n') { break; }
    }

    // P2 fix: send a proper error response instead of silently closing the
    // connection when the client sends malformed JSON.
    let cmd: Command = match serde_json::from_slice(&buf) {
        Ok(c) => c,
        Err(e) => {
            warn!("JSON parse error from client: {}", e);
            let err_resp = Response::Error {
                message: format!("command parse error: {}", e),
            };
            if let Ok(mut reply) = serde_json::to_vec(&err_resp) {
                reply.push(b'\n');
                let _ = stream.write_all(&reply).await;
            }
            return Ok(());
        }
    };

    let response = daemon.lock().await.dispatch(cmd);

    let mut reply = serde_json::to_vec(&response).context("serialise response")?;
    reply.push(b'\n');
    stream.write_all(&reply).await.context("write response")?;
    Ok(())
}

fn lookup_gid(name: &str) -> Option<u32> {
    let content = std::fs::read_to_string("/etc/group").ok()?;
    for line in content.lines() {
        let mut fields = line.splitn(4, ':');
        let gname = fields.next()?;
        if gname != name { continue; }
        let _ = fields.next();
        let gid_str = fields.next()?;
        return gid_str.trim().parse().ok();
    }
    None
}
