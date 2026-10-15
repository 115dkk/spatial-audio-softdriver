//! A second launch asks the running app for its window through a file in
//! the state directory (ADR 0033): the guard that could not take the lock
//! writes it, and the app's shell takes it once a second.

use std::fs;
use std::io;
use std::path::Path;

pub const FILE: &str = "show-window";

pub fn request(state_dir: &Path) -> io::Result<()> {
    fs::create_dir_all(state_dir)?;
    fs::write(state_dir.join(FILE), b"")
}

/// Takes the pending request, if any: true when there was one.
pub fn take(state_dir: &Path) -> bool {
    fs::remove_file(state_dir.join(FILE)).is_ok()
}
