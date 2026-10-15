#![forbid(unsafe_code)]

//! Runs `tauri_build` for Windows targets only: it reads `tauri.conf.json` and
//! the capabilities, and compiles the icon and version resource. Other targets
//! build the control thread alone, which needs none of it.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        tauri_build::build();
    } else {
        // Without this, any change under the crate (the screen's sources
        // included) would rerun the script.
        println!("cargo:rerun-if-changed=build.rs");
    }
}
