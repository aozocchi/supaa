//! supaa-cli — command-line interface for supaad.
//!
//! Usage:
//!   supaa status
//!   supaa mode <normal|supaa|supaa++>
//!   supaa focus <pid> [name]
//!   supaa clear-focus
//!   supaa ps
//!   supaa restore
//!   supaa shutdown
//!   supaa emergency-restore [--quit]

use anyhow::{Context, Result};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use supaa_core::modes::Mode;
use supaa_core::protocol::{Command, Response, SOCKET_PATH};

fn send_command(cmd: &Command) -> Result<Response> {
    let mut stream = UnixStream::connect(SOCKET_PATH)
        .with_context(|| {
            format!(
                "cannot connect to supaad at {}\n  Is supaad running? \
                 Try: systemctl start supaad",
                SOCKET_PATH
            )
        })?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;

    let mut payload = serde_json::to_vec(cmd).context("serialise command")?;
    payload.push(b'\n');
    stream.write_all(&payload).context("send command")?;
    stream.shutdown(std::net::Shutdown::Write)?;

    let mut buf = String::new();
    stream.read_to_string(&mut buf).context("read response")?;

    serde_json::from_str(&buf).context("parse response")
}

fn print_response(resp: Response) {
    match resp {
        Response::Ok => println!("OK"),
        Response::Error { message } => {
            eprintln!("Error: {}", message);
            std::process::exit(1);
        }
        Response::Status(s) => {
            println!("Mode           : {}", s.mode.as_str());
            println!("Focus app      : {}",
                s.focus_app_name.as_deref().unwrap_or("—"));
            println!("Focus PID      : {}",
                s.focus_app_pid.map(|p| p.to_string()).unwrap_or("—".into()));
            println!("Frozen procs   : {}", s.frozen_count);
            println!("Workspace lock : {}", if s.workspace_locked { "YES" } else { "no" });
            println!("State clean    : {}", if s.state_file_clean  { "yes" } else { "DIRTY (recovery pending)" });
            println!("Uptime         : {}s", s.uptime_secs);
        }
        Response::ProcessList(procs) => {
            println!("{:<8} {:<6} {:<6} {:<12} {}", "PID", "FROZ", "FOCUS", "WLIST", "NAME");
            println!("{}", "─".repeat(50));
            for p in procs {
                println!(
                    "{:<8} {:<6} {:<6} {:<12} {}",
                    p.pid,
                    if p.is_frozen { "✓" } else { "" },
                    if p.is_focus  { "✓" } else { "" },
                    if p.is_whitelisted { "protected" } else { "" },
                    p.name,
                );
            }
        }
    }
}

fn usage() -> ! {
    eprintln!(
        r#"supaa — Supaa Focus Resource Manager CLI

USAGE:
  supaa status                    Show daemon status
  supaa mode <MODE>               Switch mode
  supaa focus <PID> [NAME]        Set focus application
  supaa clear-focus               Clear focus application
  supaa ps                        List all processes
  supaa restore                   Emergency restore (keep daemon running)
  supaa shutdown                  Clean daemon shutdown
  supaa emergency-restore [--quit]  Restore + optionally quit daemon

MODE values:
  normal      Baseline — no throttling or freezing
  supaa       Focus gets boosted CPU/IO; background throttled
  supaa++     Extreme focus; background frozen; workspace locked
"#
    );
    std::process::exit(1);
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        usage();
    }

    let cmd: Command = match args[1].as_str() {
        "status" => Command::GetStatus,

        "mode" => {
            let mode_str = args.get(2).map(|s| s.as_str()).unwrap_or_else(|| {
                eprintln!("Usage: supaa mode <normal|supaa|supaa++>");
                std::process::exit(1);
            });
            let mode = Mode::from_str(mode_str).unwrap_or_else(|| {
                eprintln!("Unknown mode '{}'. Valid: normal, supaa, supaa++", mode_str);
                std::process::exit(1);
            });
            Command::SetMode { mode }
        }

        "focus" => {
            let pid: u32 = args.get(2)
                .and_then(|s| s.parse().ok())
                .unwrap_or_else(|| {
                    eprintln!("Usage: supaa focus <PID> [name]");
                    std::process::exit(1);
                });
            let name = args.get(3).cloned().unwrap_or_else(|| format!("pid:{}", pid));
            Command::SetFocusApp { pid, name }
        }

        "clear-focus" => Command::ClearFocusApp,
        "ps"          => Command::GetProcessList,
        "shutdown"    => Command::Shutdown,

        "restore" => Command::EmergencyRestore { quit_after: false },

        "emergency-restore" => {
            let quit = args.get(2).map(|s| s == "--quit").unwrap_or(false);
            Command::EmergencyRestore { quit_after: quit }
        }

        _ => {
            eprintln!("Unknown command '{}'\n", args[1]);
            usage();
        }
    };

    let response = send_command(&cmd)?;
    print_response(response);
    Ok(())
}
