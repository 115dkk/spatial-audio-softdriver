#![forbid(unsafe_code)]
// Like the app: a release build has no console window (the app would open
// one for a console child); a debug build keeps the console it was started
// from.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::path::PathBuf;

#[derive(Debug)]
struct Args {
    state_dir: PathBuf,
    app: PathBuf,
    minimized: bool,
    extra: Vec<String>,
}

fn parse() -> Result<Args, String> {
    let mut state_dir = None;
    let mut app = None;
    let mut minimized = false;
    let mut extra = Vec::new();
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--state-dir" {
            state_dir = Some(PathBuf::from(
                args.next().ok_or("--state-dir needs a directory")?,
            ));
        } else if arg == "--app" {
            app = Some(PathBuf::from(
                args.next().ok_or("--app needs an executable")?,
            ));
        } else if arg == "--minimized" {
            minimized = true;
        } else if arg == "--" {
            extra.extend(args.map(|value| value.to_string_lossy().into_owned()));
            break;
        } else {
            return Err(format!("unknown argument {arg:?}"));
        }
    }
    Ok(Args {
        state_dir: state_dir.ok_or("--state-dir is required")?,
        app: app.ok_or("--app is required")?,
        minimized,
        extra,
    })
}

fn main() {
    let args = match parse() {
        Ok(args) => args,
        Err(error) => {
            eprintln!("ssr-guard: {error}");
            std::process::exit(2);
        }
    };
    run(args);
}

#[cfg(windows)]
fn run(args: Args) {
    use ssr_guard::runner::Exit;
    use ssr_guard::windows::{self, WindowsPlatform};

    windows::log(&args.state_dir, "guard: starts");
    let mut platform = WindowsPlatform::new(args.state_dir, args.app, args.extra, args.minimized);
    match windows::run(&mut platform) {
        Exit::Clean => std::process::exit(0),
        Exit::Restored(reason) => {
            eprintln!("ssr-guard: {reason:?}");
            std::process::exit(1);
        }
    }
}

#[cfg(not(windows))]
fn run(args: Args) {
    let _ = (args.state_dir, args.app, args.minimized, args.extra);
    eprintln!("Windows에서만 돕니다");
    std::process::exit(1);
}
