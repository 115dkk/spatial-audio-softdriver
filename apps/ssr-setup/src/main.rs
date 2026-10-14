#![forbid(unsafe_code)]

//! Administrator one-shot helper (M4): install, uninstall, set-formats,
//! restart-device and repair, each answering with one JSON line on stdout.
//! Until then it accepts no task.

fn main() {
    println!(r#"{{"ok":false,"error":"no tasks before M4"}}"#);
    std::process::exit(2);
}
