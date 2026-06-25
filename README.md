# Supaa
Focus Mode for Hyprland

Supaa is a Rust daemon that maximises CPU, I/O, and memory resources for one
focus application while throttling (and optionally freezing) everything else.

---

## Modes

| Mode | CPU focus / bg | Freeze bg | Workspace lock |
|------|----------------|-----------|----------------|
| **Normal** | 100 / 100 | ✗ | ✗ |
| **Supaa** | 800 / 20 | ✗ | ✗ |
| **Supaa++** | 10000 / 1 | ✓ (SIGSTOP) | ✓ |

- **Normal** — Supaa inactive; no throttling, no freezing.
- **Supaa** — Focus app gets 8× CPU and I/O priority; background throttled.
  Background memory capped at 75 % of RAM (soft limit, no OOM-kill).
- **Supaa++** — Extreme mode. Background processes frozen (SIGSTOP), workspace
  switching locked in Hyprland. Exit via `Super+Shift+Q`, the UI popup, or
  `supaa mode normal` from a terminal.

> **Note:** the `Priority` mode present in earlier branches has been removed.
> The final design uses exactly these three levels.

---

## Quick start

```bash
sudo ./scripts/install.sh    # builds, installs binaries + systemd service
supaa status                 # check daemon status
supaa-ui                     # graphical mode switcher
```

---

## Components

| Binary | Role |
|--------|------|
| `supaad` | Root daemon — manages cgroups, freezer, Hyprland IPC |
| `supaa` | CLI client — send commands to the daemon |
| `supaa-ui` | GTK4 floating window — select mode + focus app |
| `supaa-emergency-restore` | Standalone shell script — works without daemon |

---

## Architecture

```
supaa-ui / supaa CLI
      │  JSON over Unix socket (/run/supaad.sock, mode 0660, group supaa)
      ▼
  supaad (root)
      ├── cgroup.rs     — cgroup v2 setup, CPU/IO/memory weights
      ├── policy.rs     — mode → cgroup + freeze + lock
      ├── freezer.rs    — SIGSTOP / SIGCONT
      ├── whitelist.rs  — protects system processes (ancestor-chain aware)
      ├── hyprland.rs   — workspace lock (submap → keyword-bind fallback)
      └── recovery.rs   — crash detection + safe restart
```

### Cgroup layout

```
/sys/fs/cgroup/supaa/
  focus/       ← focused application's full process tree
  background/  ← all other user processes
```

CPU and I/O weights are applied at the cgroup level, so they work even if
the focus app spawns many threads or child processes.

---

## Hyprland integration

The installer copies `assets/hypr-supaa.conf` (or `.lua` for Hyprland ≥ 0.55)
to `~/.config/hypr/` and adds a single `source=` line to `hyprland.conf`.

### Workspace lock

When Supaa++ is active the daemon engages the `supaa_lock` submap, which
has no binds — so all workspace switching keys (`Super+1…9`, `Super+arrow`,
etc.) are silently swallowed.

### Escape hatch — Super+Shift+Q

Two layers of escape:

| Where | What happens |
|-------|-------------|
| Global Hyprland bind (in `hypr-supaa.conf`) | Runs `supaa mode normal` — **immediate** restore, no popup |
| Supaa UI window (when it has focus) | Shows confirmation popup before restoring |

The global bind works even when the Supaa UI window is not focused or is
minimised, which is the primary escape hatch during Supaa++ mode.

---

## IPC access

The daemon socket is `root:supaa 0660`.  Users in the `supaa` group can run
the CLI and UI without sudo.  The installer adds your desktop user
automatically.  Re-login (or `newgrp supaa`) for the change to take effect.

---

## Emergency restore

If Supaa++ freezes something it shouldn't, or you lose keyboard access:

```bash
supaa mode normal              # via CLI (requires daemon running)
supaa-emergency-restore        # standalone script, works without daemon
supaa-emergency-restore --reboot
```

From a TTY (e.g. `Ctrl+Alt+F2`):
```bash
supaa-emergency-restore
```

---

## Patch history

| Fix | Description |
|-----|-------------|
| P0.2 | `restore_on_transition()` — mode switch without cgroup teardown |
| P0.3 | `move_tree_to_focus()` — moves full process tree, not just root PID |
| P0.4 | `populate_background()` — background cgroup actually populated |
| P1 | Recovery resets cgroup weights; removed hardcoded `user-1000` |
| P2 | Daemon sends `Response::Error` on JSON parse failure (not silent close) |
| Design | Removed `Priority` mode; final modes are Normal / Supaa / Supaa++ |
| Design | Hyprland config uses `supaa mode normal` (not `supaactl request-quit`) |
| UI | Process list shows only frozen/focus processes (performance fix) |
| UI | IPC timeout reduced to 500 ms to prevent GTK main-thread stall |
