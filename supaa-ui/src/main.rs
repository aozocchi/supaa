//! supaa-ui — GTK4 desktop UI for Supaa Focus Resource Manager.
//!
//! Features:
//!   • Mode selector (Normal / Supaa / Supaa++)  — Priority mode removed
//!   • Active mode indicator with colour coding
//!   • Focus app picker (shows active Hyprland window)
//!   • Process list: only focus app + frozen processes (capped at 100)
//!   • Warning panel for Supaa++ mode
//!   • Restore status indicator
//!   • SUPER + SHIFT + Q → "Exit Supaa++?" confirmation dialog

use anyhow::Result;
use gtk4::prelude::*;
use gtk4::{
    gdk, gio, glib,
    Align, Application, ApplicationWindow, Box as GBox, Button, CssProvider,
    EventControllerKey, HeaderBar, Label, ListBox, ListBoxRow, Orientation,
    PolicyType, ScrolledWindow, SelectionMode, Separator,
    StyleContext,
};
use std::cell::RefCell;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::rc::Rc;
use std::time::Duration;

use supaa_core::modes::Mode;
use supaa_core::protocol::{Command, DaemonStatus, ProcessInfo, Response, SOCKET_PATH};

const APP_ID: &str = "dev.supaa.Supaa";
const POLL_INTERVAL_MS: u32 = 2000;
/// Maximum number of processes shown in the list (frozen + focus).
const MAX_PROC_LIST: usize = 100;

// ─────────────────────────────────────────────────────────────────────────────
//  IPC helpers (synchronous — called from glib timeout, not blocking the UI)
// ─────────────────────────────────────────────────────────────────────────────

fn ipc_send(cmd: &Command) -> Option<Response> {
    let mut stream = UnixStream::connect(SOCKET_PATH)
        .map_err(|e| log::warn!("IPC connect: {}", e))
        .ok()?;
    // Short timeouts so the GTK main thread is never blocked more than 500 ms.
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(500)));

    let mut payload = serde_json::to_vec(cmd).ok()?;
    payload.push(b'\n');
    stream.write_all(&payload)
        .map_err(|e| log::warn!("IPC write: {}", e))
        .ok()?;
    let _ = stream.shutdown(std::net::Shutdown::Write);

    let mut buf = String::new();
    stream.read_to_string(&mut buf)
        .map_err(|e| log::warn!("IPC read: {}", e))
        .ok()?;
    serde_json::from_str(&buf)
        .map_err(|e| log::warn!("IPC parse: {}", e))
        .ok()
}

fn get_status() -> Option<DaemonStatus> {
    match ipc_send(&Command::GetStatus)? {
        Response::Status(s) => Some(s),
        _ => None,
    }
}

/// Fetch process list but only return interesting entries:
/// frozen processes and the focus app, capped at MAX_PROC_LIST.
/// This avoids rebuilding a ListBox row for every process on the system
/// (which could be hundreds) every 2 seconds.
fn get_interesting_processes() -> Vec<ProcessInfo> {
    let all = match ipc_send(&Command::GetProcessList) {
        Some(Response::ProcessList(v)) => v,
        _ => return vec![],
    };
    all.into_iter()
        .filter(|p| p.is_frozen || p.is_focus)
        .take(MAX_PROC_LIST)
        .collect()
}

fn set_mode(mode: Mode) {
    let _ = ipc_send(&Command::SetMode { mode });
}

fn emergency_restore() {
    let _ = ipc_send(&Command::EmergencyRestore { quit_after: false });
}

// ─────────────────────────────────────────────────────────────────────────────
//  Exit Supaa++ confirmation dialog
// ─────────────────────────────────────────────────────────────────────────────

fn show_exit_supaa_plus_dialog(parent: &ApplicationWindow) {
    // Use AlertDialog (GTK 4.10+) — available in gtk4-rs 0.9 with v4_12 feature.
    let dialog = gtk4::AlertDialog::builder()
        .modal(true)
        .message("Exit Supaa++?")
        .detail(
            "Leaving Supaa++ will restore all system resource policies \
             and unfreeze background applications.",
        )
        .buttons(["Cancel", "Quit Supaa++"])
        .cancel_button(0)
        .default_button(0)
        .build();

    let parent_ref = parent.clone();
    dialog.choose(Some(&parent_ref), gio::Cancellable::NONE, move |result| {
        // Button index 1 = "Quit Supaa++"
        if result == Ok(1) {
            let _ = ipc_send(&Command::EmergencyRestore { quit_after: false });
            let _ = ipc_send(&Command::SetMode { mode: Mode::Normal });
        }
        // Index 0 (Cancel) or Err → do nothing, dialog already dismissed.
    });
}

// ─────────────────────────────────────────────────────────────────────────────
//  Mode indicator badge
// ─────────────────────────────────────────────────────────────────────────────

fn mode_css_class(mode: Mode) -> &'static str {
    match mode {
        Mode::Normal    => "mode-normal",
        Mode::Supaa     => "mode-supaa",
        Mode::SupaaPlus => "mode-supaaplus",
    }
}

/// All CSS class names used for mode badges.
const MODE_CSS_CLASSES: &[&str] = &["mode-normal", "mode-supaa", "mode-supaaplus"];

// ─────────────────────────────────────────────────────────────────────────────
//  Build the UI
// ─────────────────────────────────────────────────────────────────────────────

fn build_ui(app: &Application) {
    // ── CSS ─────────────────────────────────────────────────────────────────
    let css = CssProvider::new();
    css.load_from_data(
        r#"
        .mode-badge {
            font-weight: bold;
            font-size: 1.1em;
            padding: 4px 12px;
            border-radius: 999px;
        }
        .mode-normal    { background: #3d3d3d; color: #ccc; }
        .mode-supaa     { background: #1a4e8a; color: #cdf; }
        .mode-supaaplus { background: #8a1a1a; color: #fcd; }

        .warning-panel {
            background: #3d1a1a;
            border-left: 4px solid #cc3333;
            padding: 8px 12px;
        }
        .daemon-offline {
            color: #cc4444;
            font-style: italic;
        }
        .section-title {
            font-weight: bold;
            font-size: 0.9em;
            color: alpha(currentColor, 0.6);
            text-transform: uppercase;
            letter-spacing: 1px;
            padding: 4px 0;
        }
        .proc-frozen { color: #ff8888; }
        .proc-focus  { color: #88ff88; font-weight: bold; }
        .proc-white  { color: alpha(currentColor, 0.5); }
        "#,
    );
    StyleContext::add_provider_for_display(
        &gdk::Display::default().unwrap(),
        &css,
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    // ── Window ───────────────────────────────────────────────────────────────
    let window = ApplicationWindow::new(app);
    window.set_title(Some("Supaa"));
    window.set_default_size(420, 580);
    window.set_resizable(true);

    // ── Header bar ───────────────────────────────────────────────────────────
    let header = HeaderBar::new();
    let mode_badge = Label::new(Some("Normal"));
    mode_badge.add_css_class("mode-badge");
    mode_badge.add_css_class("mode-normal");
    header.pack_end(&mode_badge);
    window.set_titlebar(Some(&header));

    // ── Root layout ──────────────────────────────────────────────────────────
    let root = GBox::new(Orientation::Vertical, 0);
    window.set_child(Some(&root));

    // ── Daemon status bar ────────────────────────────────────────────────────
    let status_bar = GBox::new(Orientation::Horizontal, 8);
    status_bar.set_margin_start(12);
    status_bar.set_margin_end(12);
    status_bar.set_margin_top(6);
    status_bar.set_margin_bottom(6);
    let daemon_label = Label::new(Some("● Connecting…"));
    daemon_label.set_halign(Align::Start);
    daemon_label.set_hexpand(true);
    let focus_label = Label::new(Some("No focus app"));
    focus_label.set_halign(Align::End);
    status_bar.append(&daemon_label);
    status_bar.append(&focus_label);
    root.append(&status_bar);
    root.append(&Separator::new(Orientation::Horizontal));

    // ── Supaa++ warning panel ────────────────────────────────────────────────
    let warning_panel = GBox::new(Orientation::Horizontal, 8);
    warning_panel.add_css_class("warning-panel");
    warning_panel.set_margin_all(8);
    let warn_icon  = Label::new(Some("⚠"));
    let warn_label = Label::new(Some(
        "Supaa++ active — workspace switching locked. \
         Press Super+Shift+Q to exit.",
    ));
    warn_label.set_wrap(true);
    warn_label.set_halign(Align::Start);
    warning_panel.append(&warn_icon);
    warning_panel.append(&warn_label);
    warning_panel.set_visible(false);
    root.append(&warning_panel);

    // ── Mode buttons ─────────────────────────────────────────────────────────
    let mode_box = GBox::new(Orientation::Vertical, 4);
    mode_box.set_margin_all(12);

    let mode_title = Label::new(Some("MODE"));
    mode_title.add_css_class("section-title");
    mode_title.set_halign(Align::Start);
    mode_box.append(&mode_title);

    let btn_row = GBox::new(Orientation::Horizontal, 6);
    // Three modes only — Priority has been removed.
    let modes = [
        ("Normal",   Mode::Normal),
        ("Supaa",    Mode::Supaa),
        ("Supaa++",  Mode::SupaaPlus),
    ];

    let mode_badge_clone  = mode_badge.clone();
    let warning_panel_ref = warning_panel.clone();

    for (label, mode) in modes {
        let btn = Button::with_label(label);
        btn.set_hexpand(true);
        let mb  = mode_badge_clone.clone();
        let wp  = warning_panel_ref.clone();
        btn.connect_clicked(move |_| {
            set_mode(mode);
            // Update badge immediately (confirmed on next poll).
            for cls in MODE_CSS_CLASSES {
                mb.remove_css_class(cls);
            }
            mb.set_text(mode.as_str());
            mb.add_css_class(mode_css_class(mode));
            wp.set_visible(mode == Mode::SupaaPlus);
        });
        btn_row.append(&btn);
    }
    mode_box.append(&btn_row);
    root.append(&mode_box);
    root.append(&Separator::new(Orientation::Horizontal));

    // ── Restore button ───────────────────────────────────────────────────────
    let restore_row = GBox::new(Orientation::Horizontal, 8);
    restore_row.set_margin_start(12);
    restore_row.set_margin_end(12);
    restore_row.set_margin_top(6);
    restore_row.set_margin_bottom(6);
    let restore_btn = Button::with_label("⟳ Emergency Restore");
    restore_btn.set_tooltip_text(Some(
        "Immediately thaw all processes and reset resource policies",
    ));
    let wp2 = warning_panel.clone();
    let mb2 = mode_badge.clone();
    restore_btn.connect_clicked(move |_| {
        emergency_restore();
        wp2.set_visible(false);
        for cls in MODE_CSS_CLASSES {
            mb2.remove_css_class(cls);
        }
        mb2.set_text("Normal");
        mb2.add_css_class("mode-normal");
    });
    restore_row.append(&restore_btn);
    root.append(&restore_row);
    root.append(&Separator::new(Orientation::Horizontal));

    // ── Process list ─────────────────────────────────────────────────────────
    // Only shows frozen processes and the focus app (capped at MAX_PROC_LIST).
    // Rebuilding hundreds of rows every 2 s causes visible GTK jank.
    let proc_box = GBox::new(Orientation::Vertical, 4);
    proc_box.set_margin_all(12);
    proc_box.set_vexpand(true);

    let proc_title = Label::new(Some("FROZEN / FOCUS PROCESSES"));
    proc_title.add_css_class("section-title");
    proc_title.set_halign(Align::Start);
    proc_box.append(&proc_title);

    let list_box = ListBox::new();
    list_box.set_selection_mode(SelectionMode::None);
    let scroll = ScrolledWindow::new();
    scroll.set_policy(PolicyType::Never, PolicyType::Automatic);
    scroll.set_vexpand(true);
    scroll.set_min_content_height(150);
    scroll.set_child(Some(&list_box));
    proc_box.append(&scroll);
    root.append(&proc_box);

    // ── SUPER + SHIFT + Q key handler ────────────────────────────────────────
    // This fires only when the Supaa UI window is focused.
    // For a global escape hatch (window not focused), the Hyprland bind
    // in hypr-supaa.conf runs `supaa mode normal` directly.
    let key_ctrl = EventControllerKey::new();
    let window_ref = window.clone();
    let state_mode: Rc<RefCell<Mode>> = Rc::new(RefCell::new(Mode::Normal));
    let state_mode_key = state_mode.clone();
    key_ctrl.connect_key_pressed(move |_, key, _, modifiers| {
        use gdk::Key;
        use gdk::ModifierType;

        let is_super = modifiers.contains(ModifierType::SUPER_MASK);
        let is_shift = modifiers.contains(ModifierType::SHIFT_MASK);
        let is_q     = key == Key::q || key == Key::Q;

        if is_super && is_shift && is_q {
            let mode = *state_mode_key.borrow();
            if mode == Mode::SupaaPlus {
                show_exit_supaa_plus_dialog(&window_ref);
                return glib::Propagation::Stop;
            }
        }
        glib::Propagation::Proceed
    });
    window.add_controller(key_ctrl);

    // ── Poll daemon status ────────────────────────────────────────────────────
    let daemon_label   = daemon_label.clone();
    let focus_label    = focus_label.clone();
    let mode_badge     = mode_badge.clone();
    let warning_panel  = warning_panel.clone();
    let list_box       = list_box.clone();
    let state_mode_poll = state_mode.clone();

    glib::timeout_add_local(Duration::from_millis(POLL_INTERVAL_MS as u64), move || {
        // ── Update status ─────────────────────────────────────────────────
        match get_status() {
            Some(s) => {
                *state_mode_poll.borrow_mut() = s.mode;

                daemon_label.remove_css_class("daemon-offline");
                let clean_txt = if s.state_file_clean { "" } else { " ⚠ dirty state" };
                daemon_label.set_text(&format!(
                    "● supaad  uptime {}s{}",
                    s.uptime_secs, clean_txt
                ));

                focus_label.set_text(
                    s.focus_app_name.as_deref().unwrap_or("No focus app"),
                );

                for cls in MODE_CSS_CLASSES {
                    mode_badge.remove_css_class(cls);
                }
                mode_badge.set_text(s.mode.as_str());
                mode_badge.add_css_class(mode_css_class(s.mode));
                warning_panel.set_visible(s.mode == Mode::SupaaPlus);
            }
            None => {
                daemon_label.add_css_class("daemon-offline");
                daemon_label.set_text("● supaad offline");
                focus_label.set_text("");
            }
        }

        // ── Update process list (frozen + focus only) ─────────────────────
        while let Some(child) = list_box.first_child() {
            list_box.remove(&child);
        }

        let procs = get_interesting_processes();
        if procs.is_empty() {
            let row  = ListBoxRow::new();
            let lbl  = Label::new(Some("— no frozen or focused processes —"));
            lbl.set_margin_top(8);
            lbl.set_margin_bottom(8);
            row.set_child(Some(&lbl));
            list_box.append(&row);
        } else {
            for proc in &procs {
                let row   = ListBoxRow::new();
                let hbox  = GBox::new(Orientation::Horizontal, 8);
                hbox.set_margin_start(8);
                hbox.set_margin_end(8);
                hbox.set_margin_top(4);
                hbox.set_margin_bottom(4);

                let pid_lbl = Label::new(Some(&format!("{}", proc.pid)));
                pid_lbl.set_width_chars(7);
                pid_lbl.set_xalign(0.0);

                let name_lbl = Label::new(Some(&proc.name));
                name_lbl.set_hexpand(true);
                name_lbl.set_xalign(0.0);
                name_lbl.set_ellipsize(gtk4::pango::EllipsizeMode::End);

                let state_lbl = if proc.is_frozen {
                    let l = Label::new(Some("❄ frozen"));
                    l.add_css_class("proc-frozen");
                    l
                } else if proc.is_focus {
                    let l = Label::new(Some("★ focus"));
                    l.add_css_class("proc-focus");
                    l
                } else {
                    Label::new(Some(""))
                };

                hbox.append(&pid_lbl);
                hbox.append(&name_lbl);
                hbox.append(&state_lbl);
                row.set_child(Some(&hbox));
                list_box.append(&row);
            }
        }

        glib::ControlFlow::Continue
    });

    window.present();
}

// ─────────────────────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Warn)
        .init();

    let app = Application::builder()
        .application_id(APP_ID)
        .build();

    app.connect_activate(build_ui);

    let exit_code = app.run();
    std::process::exit(exit_code.value());
}
