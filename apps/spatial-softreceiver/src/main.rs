#![forbid(unsafe_code)]
// A release build has no console window; a debug build keeps one, so the CI's
// smoke test can show what it printed.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

//! Spatial SoftReceiver: the Tauri 2 shell around the supervisor (ADR 0001).

use spatial_softreceiver::paths::Args;

fn main() {
    let args = match Args::parse(std::env::args_os().skip(1)) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("Spatial SoftReceiver: {message}");
            std::process::exit(2);
        }
    };
    #[cfg(windows)]
    if args.smoke.is_none() && !args.guarded {
        match start_guard(args) {
            Ok(()) => return,
            Err(message) => {
                eprintln!("Spatial SoftReceiver: {message}");
                spatial_softreceiver::log::line(&format!("guard: {message}; running directly"));
            }
        }
    }
    run(args);
}

#[cfg(windows)]
fn start_guard(args: Args) -> Result<(), String> {
    use std::process::{Command, Stdio};

    let app = std::env::current_exe().map_err(|error| format!("app path: {error}"))?;
    let exe_dir = app.parent().ok_or("the app has no parent directory")?;
    let state_dir = spatial_softreceiver::paths::state_dir(std::env::var_os("LOCALAPPDATA"));
    spatial_softreceiver::log::Log::open(&state_dir);
    let mut command = Command::new(spatial_softreceiver::paths::guard_exe(exe_dir));
    command
        .arg("--state-dir")
        .arg(state_dir)
        .arg("--app")
        .arg(app)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if args.minimized {
        command.arg("--minimized");
    }
    command
        .spawn()
        .map(drop)
        .map_err(|error| format!("the guard did not start: {error}"))
}

#[cfg(windows)]
fn run(args: Args) {
    if let Err(message) = spatial_softreceiver::shell::run(args) {
        eprintln!("Spatial SoftReceiver: {message}");
        spatial_softreceiver::log::line(&format!("shell: {message}"));
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn run(_args: Args) {
    eprintln!("Spatial SoftReceiver는 Windows에서만 돕니다.");
    std::process::exit(1);
}
