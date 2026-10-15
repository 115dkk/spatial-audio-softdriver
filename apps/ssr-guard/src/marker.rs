use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

pub const FILE: &str = "guard-restored.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarkerReason {
    /// The app exited with its restore point still there.
    AppExited,
    /// The engine stalled; the app was restarted once.
    StallRestarted,
    /// It stalled again after the restart, or the restart did not start.
    RestartFailed,
    /// The app held the roles but never said it was Running (ADR 0033's
    /// takeover silence).
    Stuck,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marker {
    pub version: u32,
    pub reason: MarkerReason,
    pub at_unix: u64,
    pub notes: Vec<String>,
}

impl Marker {
    pub const VERSION: u32 = 1;

    pub fn new(reason: MarkerReason, at_unix: u64, notes: Vec<String>) -> Self {
        Self {
            version: Self::VERSION,
            reason,
            at_unix,
            notes,
        }
    }
}

pub fn read(path: &Path) -> io::Result<Marker> {
    let text = fs::read_to_string(path)?;
    let marker: Marker = serde_json::from_str(&text).map_err(io::Error::other)?;
    if marker.version != Marker::VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported guard marker version {}", marker.version),
        ));
    }
    Ok(marker)
}

pub fn write(path: &Path, marker: &Marker) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("json.tmp");
    let text = serde_json::to_vec(marker).map_err(io::Error::other)?;
    fs::write(&temporary, text)?;
    if let Err(error) = fs::rename(&temporary, path) {
        if !path.exists() {
            return Err(error);
        }
        fs::remove_file(path)?;
        fs::rename(temporary, path)?;
    }
    Ok(())
}
