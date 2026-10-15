//! `settings.json` in the state directory: the supervisor's [`Settings`]
//! under a format version.
//!
//! ```json
//! { "version": 1, "settings": { "output": { "id": "...", "name": "..." } } }
//! ```
//!
//! A missing, broken or foreign file starts the app fresh (the wizard finds
//! the output again); it is never an error the screen has to show.

use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use supervisor::Settings;

use crate::control::SettingsStore;
use crate::log;

/// The settings file's name in the state directory.
pub const FILE: &str = "settings.json";

/// The format this build reads and writes.
pub const VERSION: u64 = 1;

/// The settings file at `path`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JsonSettingsFile {
    pub path: PathBuf,
}

impl JsonSettingsFile {
    /// `settings.json` in `state_dir`.
    pub fn in_dir(state_dir: &Path) -> Self {
        Self {
            path: state_dir.join(FILE),
        }
    }

    /// Reads the file: `Err` says why it cannot be used (missing, unreadable,
    /// not JSON, another version, or not settings).
    pub fn read(&self) -> Result<Settings, String> {
        let text = fs::read_to_string(&self.path).map_err(|error| match error.kind() {
            io::ErrorKind::NotFound => "there is no settings file yet".to_owned(),
            _ => format!("the settings file cannot be read: {error}"),
        })?;
        let mut file: serde_json::Value = serde_json::from_str(&text)
            .map_err(|error| format!("the settings file is not JSON: {error}"))?;
        let version = file.get("version").and_then(serde_json::Value::as_u64);
        if version != Some(VERSION) {
            return Err(format!(
                "the settings file has version {}, not {VERSION}",
                file.get("version")
                    .map_or_else(|| "none".to_owned(), ToString::to_string)
            ));
        }
        let settings = file
            .get_mut("settings")
            .map(serde_json::Value::take)
            .ok_or_else(|| "the settings file has no settings".to_owned())?;
        serde_json::from_value(settings)
            .map_err(|error| format!("the settings file's settings do not parse: {error}"))
    }

    /// Writes the file whole: into a temporary file beside it, then renamed
    /// over it, so a crash leaves the old file or the new one. Creates the
    /// directory when it is missing.
    pub fn write(&self, settings: &Settings) -> io::Result<()> {
        let dir = self
            .path
            .parent()
            .filter(|dir| !dir.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::create_dir_all(dir)?;
        let json = serde_json::json!({ "version": VERSION, "settings": settings });
        let text = serde_json::to_string_pretty(&json).map_err(io::Error::other)?;
        let mut temporary = self.path.clone().into_os_string();
        temporary.push(".tmp");
        let temporary = PathBuf::from(temporary);
        let mut file = fs::File::create(&temporary)?;
        file.write_all(text.as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, &self.path)
    }
}

impl SettingsStore for JsonSettingsFile {
    /// The settings in the file, or the defaults with a line in the log when
    /// the file cannot be used.
    fn load(&self) -> Settings {
        match self.read() {
            Ok(settings) => settings,
            Err(reason) => {
                log::line(&format!(
                    "settings: {reason} ({}); starting with the defaults",
                    self.path.display()
                ));
                Settings::default()
            }
        }
    }

    /// A failed write goes to the log; the app keeps running with the
    /// settings in memory.
    fn save(&self, settings: &Settings) {
        if let Err(error) = self.write(settings) {
            log::line(&format!(
                "settings: could not write {}: {error}",
                self.path.display()
            ));
        }
    }
}
