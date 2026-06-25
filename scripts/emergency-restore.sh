#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# Supaa Emergency Restore
#
# Runs independently of the daemon.  Safe to execute at any time.
# Can be bound to a key in Hyprland as a last-resort escape hatch.
#
# Usage:
#   supaa-emergency-restore            # restore and keep going
#   supaa-emergency-restore --reboot   # restore then reboot
#
# Cherry-picked from supaa1: also tries submap reset for Hyprland unlock.
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

STATE_FILE="/var/lib/supaa/state.json"
LOG_FILE="/var/log/supaa/supaa.log"
SOCKET="/run/supaad.sock"

log() {
    local ts; ts=$(date +%s)
    echo "[${ts}] $*" | tee -a "$LOG_FILE" 2>/dev/null || echo "[${ts}] $*"
}

# ── 1. Try the daemon first (fastest path) ────────────────────────────────────
try_daemon() {
    if [[ -S "$SOCKET" ]]; then
        log "Sending EmergencyRestore via IPC…"
        if command -v supaa &>/dev/null; then
            supaa emergency-restore 2>/dev/null && {
                log "Daemon restore succeeded."
                return 0
            }
        fi
    fi
    return 1
}

# ── 2. Thaw PIDs from state file ─────────────────────────────────────────────
thaw_from_state() {
    if [[ ! -f "$STATE_FILE" ]]; then
        log "No state file found — nothing to thaw from file."
        return
    fi
    log "Reading frozen PIDs from state file…"
    local pids
    # serde_json::to_string_pretty produces multi-line arrays, e.g.:
    #   "frozen_pids": [
    #       1234,
    #       5678
    #   ]
    # Prefer jq for correct parsing; fall back to a grep that handles both
    # compact and pretty-printed JSON (note the optional space after the colon
    # and that numbers may appear on lines after the opening bracket).
    if command -v jq &>/dev/null; then
        pids=$(jq -r '.frozen_pids[]?' "$STATE_FILE" 2>/dev/null \
               | grep -E '^[0-9]+$' || true)
    else
        pids=$(grep -oP '(?<="frozen_pids":\s*\[)[^\]]*' "$STATE_FILE" 2>/dev/null \
               | tr ',' '\n' | tr -d ' "\t' | grep -E '^[0-9]+$' || true)
    fi
    local count=0
    for pid in $pids; do
        if [[ -d "/proc/$pid" ]]; then
            if kill -CONT "$pid" 2>/dev/null; then
                log "  Thawed PID $pid"
                (( count++ )) || true
            fi
        fi
    done
    log "Thawed $count processes from state file."
}

# ── 3. Broad thaw scan (belt-and-suspenders) ─────────────────────────────────
thaw_all_stopped() {
    log "Scanning /proc for SIGSTOP'd processes…"
    local count=0
    for pid_dir in /proc/[0-9]*/; do
        local pid="${pid_dir%/}"; pid="${pid##*/proc/}"
        [[ "$pid" =~ ^[0-9]+$ ]] || continue
        local status_file="/proc/${pid}/status"
        [[ -f "$status_file" ]] || continue
        if grep -qP '^State:\s+T' "$status_file" 2>/dev/null; then
            local comm; comm=$(cat "/proc/${pid}/comm" 2>/dev/null || echo "?")
            case "$comm" in
                systemd|init|dbus*|Hyprland|pipewire|wireplumber|supaad) continue ;;
            esac
            if kill -CONT "$pid" 2>/dev/null; then
                log "  Thawed PID $pid ($comm)"
                (( count++ )) || true
            fi
        fi
    done
    log "Broadcast thaw: $count processes resumed."
}

# ── 4. Reset cgroup weights and tear down directories ────────────────────────
reset_cgroups() {
    log "Resetting cgroup weights…"
    for cg_path in \
        /sys/fs/cgroup/supaa/focus \
        /sys/fs/cgroup/supaa/background \
        /sys/fs/cgroup/supaa
    do
        [[ -d "$cg_path" ]] || continue
        [[ -f "${cg_path}/cpu.weight"  ]] && echo 100           > "${cg_path}/cpu.weight"  2>/dev/null || true
        [[ -f "${cg_path}/io.weight"   ]] && echo "default 100" > "${cg_path}/io.weight"   2>/dev/null || true
        [[ -f "${cg_path}/memory.high" ]] && echo max           > "${cg_path}/memory.high" 2>/dev/null || true
        if [[ -f "${cg_path}/cgroup.procs" ]]; then
            while IFS= read -r pid; do
                [[ "$pid" =~ ^[0-9]+$ ]] || continue
                echo "$pid" > /sys/fs/cgroup/cgroup.procs 2>/dev/null || true
            done < "${cg_path}/cgroup.procs"
        fi
    done
    for dir in \
        /sys/fs/cgroup/supaa/focus \
        /sys/fs/cgroup/supaa/background \
        /sys/fs/cgroup/supaa
    do
        rmdir "$dir" 2>/dev/null || true
    done
    log "cgroup cleanup done."
}

# ── 5. Unlock Hyprland workspace switching ───────────────────────────────────
# Tries submap reset first (from supaa1), then keyword unbind fallback.
# Searches both /tmp/hypr and /run/user/*/hypr (from hyprland.rs fix).
find_hypr_socket() {
    local sig="${HYPRLAND_INSTANCE_SIGNATURE:-}"
    local candidates=()

    if [[ -n "$sig" ]]; then
        candidates+=("/tmp/hypr/${sig}/.socket.sock")
        for uid_dir in /run/user/*/; do
            candidates+=("${uid_dir}hypr/${sig}/.socket.sock")
        done
    fi

    for f in /tmp/hypr/*/.socket.sock; do
        candidates+=("$f")
    done
    for f in /run/user/*/hypr/*/.socket.sock; do
        candidates+=("$f")
    done

    for f in "${candidates[@]}"; do
        [[ -S "$f" ]] && echo "$f" && return 0
    done
    return 1
}

unlock_workspaces() {
    local hypr_sock
    hypr_sock=$(find_hypr_socket) || {
        log "Hyprland socket not found — skipping workspace unlock."
        return
    }

    log "Unlocking Hyprland workspace binds via $hypr_sock…"

    # Method 1: submap reset (cherry-picked from supaa1 — cleanest approach).
    if command -v socat &>/dev/null; then
        printf "dispatch submap reset" | socat - "UNIX-CONNECT:${hypr_sock}" 2>/dev/null && {
            log "Workspace unlock: submap reset sent."
            return
        }
    fi

    # Method 2: keyword unbind fallback.
    for key in 1 2 3 4 5 6 7 8 9 0 left right; do
        if command -v socat &>/dev/null; then
            printf "keyword unbind SUPER,%s"      "$key" | socat - "UNIX-CONNECT:${hypr_sock}" 2>/dev/null || true
            printf "keyword unbind SUPER CTRL,%s" "$key" | socat - "UNIX-CONNECT:${hypr_sock}" 2>/dev/null || true
        fi
    done
    log "Workspace unlock: keyword unbind sent."
}

# ── 6. Mark state clean ───────────────────────────────────────────────────────
mark_clean() {
    if [[ -f "$STATE_FILE" ]]; then
        local ts; ts=$(date +%s)
        cat > "$STATE_FILE" << EOF
{
  "version": 1,
  "mode": "normal",
  "frozen_pids": [],
  "workspace_locked": false,
  "timestamp": ${ts},
  "clean_shutdown": true,
  "focus_app_pid": null,
  "focus_app_name": null
}
EOF
        log "State file marked clean."
    fi
}

# ── Main ──────────────────────────────────────────────────────────────────────
mkdir -p "$(dirname "$LOG_FILE")" 2>/dev/null || true
log "=== Supaa Emergency Restore started ==="

try_daemon || {
    log "Daemon unreachable — running manual restore."
    thaw_from_state
    thaw_all_stopped
    reset_cgroups
    unlock_workspaces
    mark_clean
}

log "=== Emergency Restore complete ==="
echo "Supaa emergency restore complete. Check $LOG_FILE for details."

if [[ "${1:-}" == "--reboot" ]]; then
    log "Rebooting as requested."
    sleep 2
    reboot
fi
