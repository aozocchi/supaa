#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# Supaa uninstaller
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

PREFIX="${PREFIX:-/usr/local}"
SYSTEMD_DIR="/etc/systemd/system"

need_root() {
    if [[ $EUID -ne 0 ]]; then
        echo "ERROR: must be run as root (sudo uninstall.sh)" >&2
        exit 1
    fi
}

stop_service() {
    echo "→ Stopping and disabling supaad…"
    systemctl stop supaa.service   2>/dev/null || true
    systemctl disable supaa.service 2>/dev/null || true
}

emergency_restore() {
    echo "→ Running emergency restore before uninstall…"
    if command -v supaa &>/dev/null; then
        supaa emergency-restore --quit 2>/dev/null || true
    fi
    # Belt-and-suspenders: thaw via the shell script if daemon is gone
    if [[ -f "${PREFIX}/bin/supaa-emergency-restore" ]]; then
        "${PREFIX}/bin/supaa-emergency-restore" 2>/dev/null || true
    fi
}

remove_binaries() {
    echo "→ Removing binaries…"
    rm -f "${PREFIX}/bin/supaad"
    rm -f "${PREFIX}/bin/supaa"
    rm -f "${PREFIX}/bin/supaa-ui"
    rm -f "${PREFIX}/bin/supaa-emergency-restore"
}

remove_service() {
    echo "→ Removing systemd service…"
    rm -f "${SYSTEMD_DIR}/supaa.service"
    systemctl daemon-reload
}

remove_data() {
    read -rp "Remove runtime data (/var/lib/supaa, /var/log/supaa)? [y/N] " ans
    if [[ "${ans,,}" == "y" ]]; then
        rm -rf /var/lib/supaa /var/log/supaa
        echo "  Data removed."
    else
        echo "  Data kept."
    fi
}

remove_cgroup() {
    echo "→ Cleaning up cgroup hierarchy…"
    # Move all processes out before rmdir
    for cg in /sys/fs/cgroup/supaa/focus /sys/fs/cgroup/supaa/background; do
        if [[ -f "${cg}/cgroup.procs" ]]; then
            while IFS= read -r pid; do
                echo "$pid" > /sys/fs/cgroup/cgroup.procs 2>/dev/null || true
            done < "${cg}/cgroup.procs"
            rmdir "$cg" 2>/dev/null || true
        fi
    done
    rmdir /sys/fs/cgroup/supaa 2>/dev/null || true
}

main() {
    need_root
    stop_service
    emergency_restore
    remove_cgroup
    remove_binaries
    remove_service
    remove_data
    echo ""
    echo "Supaa has been uninstalled."
}

main "$@"
