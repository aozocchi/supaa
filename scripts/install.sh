#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# Supaa installer
# Merged from supaa1 + supaa2:
#   * all fixes from supaa2 (group setup, systemd, binaries)
#   * optional Hyprland config installation (cherry-picked from supaa1)
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PREFIX="${PREFIX:-/usr/local}"
SYSTEMD_DIR="/etc/systemd/system"

need_root() {
    if [[ $EUID -ne 0 ]]; then
        echo "ERROR: this installer must be run as root (sudo install.sh)" >&2
        exit 1
    fi
}

check_deps() {
    local missing=()
    for cmd in cargo pkg-config; do
        command -v "$cmd" &>/dev/null || missing+=("$cmd")
    done
    pkg-config --exists gtk4 2>/dev/null || missing+=("gtk4 (install libgtk-4-dev / gtk4)")
    if [[ ${#missing[@]} -gt 0 ]]; then
        echo "ERROR: missing dependencies: ${missing[*]}" >&2
        echo "On Arch Linux:  sudo pacman -S rust gtk4 pkg-config base-devel" >&2
        echo "On Ubuntu/Deb:  sudo apt install cargo pkg-config libgtk-4-dev" >&2
        exit 1
    fi
}

build() {
    echo "→ Building Supaa (release)…"
    cd "$REPO_DIR"
    cargo build --release 2>&1
}

install_binaries() {
    echo "→ Installing binaries to ${PREFIX}/bin…"
    install -Dm 755 "$REPO_DIR/target/release/supaad"    "${PREFIX}/bin/supaad"
    install -Dm 755 "$REPO_DIR/target/release/supaa"     "${PREFIX}/bin/supaa"
    install -Dm 755 "$REPO_DIR/target/release/supaa-ui"  "${PREFIX}/bin/supaa-ui"
    install -Dm 755 "$REPO_DIR/scripts/emergency-restore.sh" \
                    "${PREFIX}/bin/supaa-emergency-restore"
}

install_service() {
    echo "→ Installing systemd service…"
    install -Dm 644 "$REPO_DIR/assets/supaa.service" "${SYSTEMD_DIR}/supaa.service"
    systemctl daemon-reload
}

create_dirs() {
    echo "→ Creating runtime directories…"
    install -dm 755 /var/lib/supaa
    install -dm 755 /var/log/supaa
}

setup_group() {
    echo "→ Setting up 'supaa' group for IPC access…"

    if ! getent group supaa > /dev/null 2>&1; then
        groupadd --system supaa
        echo "  Created system group 'supaa'."
    else
        echo "  Group 'supaa' already exists."
    fi

    local user="${SUDO_USER:-}"
    if [[ -z "$user" ]]; then
        user=$(logname 2>/dev/null || true)
    fi

    if [[ -n "$user" && "$user" != "root" ]]; then
        usermod -aG supaa "$user"
        echo "  Added '$user' to group 'supaa'."
        echo "  ⚠  Log out and back in (or run 'newgrp supaa') for the group to take effect."
    else
        echo "  Could not determine desktop user automatically."
        echo "  Add yourself manually: sudo usermod -aG supaa \$USER"
    fi
}

# ── Hyprland config integration (cherry-picked from supaa1) ──────────────────
#
# assets/hypr-supaa.conf defines the `supaa_lock` submap which enables the
# cleaner workspace-lock mechanism.  The .lua variant is for Hyprland setups
# using Lua-based config (hyprland.conf + require()).
#
# Installation is non-destructive: we copy to ~/.config/hypr/ and add a
# single `source` line to hyprland.conf.  The user can remove it at any time.

install_hyprland_config() {
    local user="${SUDO_USER:-$(logname 2>/dev/null || echo '')}"
    if [[ -z "$user" || "$user" == "root" ]]; then
        return 0
    fi

    local hypr_cfg
    hypr_cfg="$(eval echo "~$user")/.config/hypr/hyprland.conf"

    if [[ ! -f "$hypr_cfg" ]]; then
        # Not a Hyprland user (or non-standard config path) — skip silently.
        return 0
    fi

    echo ""
    echo "→ Hyprland config detected at $hypr_cfg"
    echo "  Supaa can install a Hyprland config snippet that enables a cleaner"
    echo "  workspace-lock mechanism (submap-based, cherry-picked from supaa1)."
    echo "  This is optional — Supaa works without it, using a keyword-bind"
    echo "  fallback."
    echo ""
    read -rp "  Install Hyprland config snippet? [y/N] " ans
    if [[ "${ans,,}" != "y" ]]; then
        echo "  Skipping Hyprland config installation."
        return 0
    fi

    local hypr_dir
    hypr_dir="$(dirname "$hypr_cfg")"
    local conf_dst="${hypr_dir}/hypr-supaa.conf"

    # Copy config snippet (as the desktop user, preserving ownership).
    install -Dm 644 -o "$user" "$REPO_DIR/assets/hypr-supaa.conf" "$conf_dst"
    echo "  Installed: $conf_dst"

    # Also install .lua variant if a .lua config is present.
    if ls "${hypr_dir}"/*.lua &>/dev/null 2>&1; then
        install -Dm 644 -o "$user" "$REPO_DIR/assets/hypr-supaa.lua" \
                         "${hypr_dir}/hypr-supaa.lua"
        echo "  Installed: ${hypr_dir}/hypr-supaa.lua"
    fi

    # Add source line to hyprland.conf if not already present.
    local source_line="source = ~/.config/hypr/hypr-supaa.conf"
    if ! grep -qF "$source_line" "$hypr_cfg"; then
        echo "" >> "$hypr_cfg"
        echo "# Supaa workspace-lock submap (added by install.sh)" >> "$hypr_cfg"
        echo "$source_line" >> "$hypr_cfg"
        chown "$user" "$hypr_cfg"
        echo "  Added source line to $hypr_cfg"
    else
        echo "  Source line already present in $hypr_cfg — no change."
    fi

    echo "  ✓ Hyprland config updated.  Reload Hyprland (Super+Shift+C or"
    echo "    hyprctl reload) for the submap to take effect."
}

enable_service() {
    echo "→ Enabling and starting supaad service…"
    systemctl enable supaa.service
    systemctl restart supaa.service
    sleep 1
    if systemctl is-active --quiet supaa.service; then
        echo "  supaad is running."
    else
        echo "  WARNING: supaad did not start cleanly. Check: journalctl -u supaa" >&2
    fi
}

print_success() {
    echo ""
    echo "╔══════════════════════════════════════════╗"
    echo "║        Supaa installed successfully      ║"
    echo "╚══════════════════════════════════════════╝"
    echo ""
    echo "  CLI:        supaa status"
    echo "  UI:         supaa-ui"
    echo "  Restore:    supaa-emergency-restore"
    echo "  Service:    systemctl status supaa"
    echo "  Logs:       journalctl -u supaa -f"
    echo ""
    echo "  IPC access: users in group 'supaa' can run supaa + supaa-ui"
    echo "              Log out and back in (or 'newgrp supaa') to activate."
    echo ""
}

main() {
    need_root
    check_deps
    build
    install_binaries
    install_service
    create_dirs
    setup_group
    install_hyprland_config
    enable_service
    print_success
}

main "$@"
