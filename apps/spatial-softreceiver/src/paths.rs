//! Where the app keeps its state, where `ssr-setup.exe` is, and the command
//! line.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The state directory's name under `%LOCALAPPDATA%`.
pub const STATE_DIR: &str = "Spatial SoftReceiver";

/// The administrator helper, next to the app's executable.
pub const SETUP_EXE: &str = "ssr-setup.exe";

/// `%LOCALAPPDATA%\Spatial SoftReceiver`, given the value of `LOCALAPPDATA`;
/// without one (a test, or a session that lacks it) the same name under the
/// temporary directory.
///
/// The Windows host gets this directory (`WindowsHost::new`), and the restore
/// point (`restore-point.json`), `settings.json`, `app.log` and
/// `profile.toml` live in it.
pub fn state_dir(local_app_data: Option<OsString>) -> PathBuf {
    match local_app_data.filter(|value| !value.is_empty()) {
        Some(dir) => PathBuf::from(dir).join(STATE_DIR),
        None => std::env::temp_dir().join(STATE_DIR),
    }
}

/// `ssr-setup.exe` beside the app's executable. The driver package ships
/// beside it too, and the Windows host looks for the package in the setup
/// executable's directory.
pub fn setup_exe(exe_dir: &Path) -> PathBuf {
    exe_dir.join(SETUP_EXE)
}

/// The app's command line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Args {
    /// `--smoke [seconds]`: quit with 0 as soon as the screen asks for its
    /// first snapshot, or with 2 when it has not within this time (CI).
    pub smoke: Option<Duration>,
    /// `--minimized`: start in the tray without showing the window (the login
    /// autostart passes it).
    pub minimized: bool,
}

impl Args {
    /// How long `--smoke` waits without a number after it.
    pub const DEFAULT_SMOKE: Duration = Duration::from_secs(30);

    /// Parses the arguments after the program name. `--smoke` takes the
    /// number of seconds that follows it, if one does (a positive whole
    /// number); anything else is an error that names the argument.
    pub fn parse<I, S>(args: I) -> Result<Self, String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut parsed = Self::default();
        let mut args = args.into_iter().peekable();
        while let Some(arg) = args.next() {
            let arg = arg.as_ref();
            if arg == "--minimized" {
                parsed.minimized = true;
            } else if arg == "--smoke" {
                let seconds = args
                    .peek()
                    .and_then(|next| next.as_ref().to_str())
                    .filter(|next| !next.starts_with('-'))
                    .map(|next| match next.parse::<u64>() {
                        Ok(seconds) if seconds > 0 => Ok(seconds),
                        _ => Err(format!(
                            "--smoke takes a positive whole number of seconds, not {next:?}"
                        )),
                    })
                    .transpose()?;
                if seconds.is_some() {
                    args.next();
                }
                parsed.smoke = Some(seconds.map_or(Self::DEFAULT_SMOKE, Duration::from_secs));
            } else {
                return Err(format!("unknown argument {arg:?}"));
            }
        }
        Ok(parsed)
    }
}
