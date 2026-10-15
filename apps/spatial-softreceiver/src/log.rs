//! `app.log` in the state directory: one line per event, UNIX seconds first.
//!
//! The control thread and the shell write here; the real-time threads never
//! do. A log that cannot be opened or written is dropped without a word: the
//! app runs the same without it.

use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// The log file's name in the state directory.
pub const FILE: &str = "app.log";

static LOG: OnceLock<Log> = OnceLock::new();

/// The process's log, opened once by [`Log::open`].
#[derive(Debug)]
pub struct Log {
    file: Mutex<File>,
}

impl Log {
    /// Opens `state_dir/app.log` for appending (creating the directory) and
    /// makes it the log [`line()`] writes to. Only the first successful call
    /// counts; a failure leaves the process without a log.
    pub fn open(state_dir: &Path) {
        if LOG.get().is_some() {
            return;
        }
        let opened = fs::create_dir_all(state_dir).and_then(|()| {
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(state_dir.join(FILE))
        });
        if let Ok(file) = opened {
            let _ = LOG.set(Self {
                file: Mutex::new(file),
            });
        }
    }
}

/// Appends `message` as one line after the UNIX time in seconds. Does nothing
/// before [`Log::open`] succeeded.
pub fn line(message: &str) {
    let Some(log) = LOG.get() else {
        return;
    };
    let since_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let message = message.replace(['\r', '\n'], " ");
    if let Ok(mut file) = log.file.lock() {
        let _ = writeln!(
            file,
            "{}.{:03} {message}",
            since_epoch.as_secs(),
            since_epoch.subsec_millis()
        );
    }
}
