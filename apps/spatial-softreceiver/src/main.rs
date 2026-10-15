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
    run(args);
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
