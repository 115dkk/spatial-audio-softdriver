//! The installed app's updates (Velopack): the startup hooks, an update
//! downloaded earlier that waits for a restart, and the thread that looks at
//! the GitHub releases.
//!
//! `release.yml` packs the app with `vpk`: `Setup.exe` puts the app, the
//! guard, the setup tool and the driver package under
//! `%LOCALAPPDATA%\SpatialSoftReceiver`, and gives the Start Menu shortcut
//! the AUMID the toasts need. Velopack runs the app with `--veloapp-*`
//! arguments for its hooks; [`startup`] answers those and exits inside.
//! Updates come from the repository's GitHub releases, prereleases included
//! (every 0.x is one): the newest release that carries `releases.win.json`.
//! A downloaded update is applied when the user asks (the settings screen),
//! or at the next plain launch, never behind a running session.
//!
//! The real module needs the `updater` feature (see Cargo.toml); without it
//! the same names are stubs.

use std::sync::Arc;

use ssr_types::app::{Language, NextAction, UserError};

use crate::log;

pub use real::*;

/// What the shell's `apply_update` command does once Tauri handed over its
/// state: without a downloaded update, or when the updater does not start,
/// the error in `language`; otherwise `quit` runs, since the updater is now
/// waiting for the exit.
pub fn apply(
    language: Language,
    pending: Option<Arc<Pending>>,
    quit: impl FnOnce(),
) -> Result<(), UserError> {
    let Some(pending) = pending else {
        return Err(user_error(ssr_i18n::shell::update_none_downloaded(
            language,
        )));
    };
    finish(
        language,
        pending.apply_after_exit(),
        pending.version(),
        quit,
    )
}

fn finish(
    language: Language,
    applied: Result<(), ApplyError>,
    version: &str,
    quit: impl FnOnce(),
) -> Result<(), UserError> {
    applied.map_err(|error| user_error(&error.render(language)))?;
    log::line(&format!(
        "update: the updater waits for the exit; quitting to apply {version}"
    ));
    quit();
    Ok(())
}

fn user_error(message: &str) -> UserError {
    UserError {
        message: message.to_owned(),
        next: NextAction::None,
    }
}

/// Why a downloaded update was not applied; rendered in the user's language
/// by the shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApplyError {
    /// This build was made without the `updater` feature.
    NotBuilt,
    /// Velopack's updater could not be started.
    Failed(String),
}

impl ApplyError {
    pub fn render(&self, language: Language) -> String {
        match self {
            Self::NotBuilt => ssr_i18n::shell::update_not_built(language).to_owned(),
            Self::Failed(detail) => ssr_i18n::shell::update_apply_failed(language, detail),
        }
    }
}

#[cfg(feature = "updater")]
mod real {
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    use velopack::sources::{GithubSource, NoneSource};
    use velopack::{UpdateCheck, UpdateManager, VelopackApp, VelopackAsset};

    use crate::log;

    /// Where the releases are.
    pub const REPO_URL: &str = "https://github.com/115dkk/spatial-audio-softreceiver";
    /// The Application User Model ID of the app, its shortcut (`vpk --aumid`)
    /// and the toasts: the same as `tauri.conf.json`'s identifier.
    pub const APP_USER_MODEL_ID: &str = "io.github.115dkk.spatial-softreceiver";
    /// The first check waits for the app to settle.
    const FIRST_CHECK: Duration = Duration::from_secs(20);
    const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

    /// Velopack's startup: the hooks (the process exits inside for those) and
    /// the process AUMID. The first thing `main` does. The uninstall hook gives
    /// the default roles back from the restore point, so an app being removed
    /// does not leave them on its endpoint.
    pub fn startup(state_dir: &Path) {
        let restore_dir = state_dir.to_path_buf();
        VelopackApp::build()
            .set_auto_apply_on_startup(false)
            .set_app_user_model_id(APP_USER_MODEL_ID)
            .on_before_uninstall_fast_callback(move |_| {
                // On a thread of its own: the host's COM calls want one.
                let _ =
                    thread::spawn(move || drop(crate::shell::last_resort_restore(&restore_dir)))
                        .join();
            })
            .run();
    }

    /// Applies an update downloaded by an earlier run and starts the app again
    /// without arguments (the new instance starts its own guard). Returns only
    /// when there is none, the app is not an installed one, or the apply failed
    /// (logged): the launch then goes on as usual.
    pub fn apply_pending() {
        let Ok(manager) = UpdateManager::new(NoneSource {}, None, None) else {
            return;
        };
        let Some(asset) = manager.get_update_pending_restart() else {
            return;
        };
        log::line(&format!(
            "update: applying the downloaded {} and restarting",
            asset.Version
        ));
        if let Err(error) =
            manager.apply_updates_and_restart_with_args(&asset, Vec::<String>::new())
        {
            log::line(&format!(
                "update: {} could not be applied: {error}",
                asset.Version
            ));
        }
    }

    /// What the settings screen shows.
    #[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
    pub struct UpdateState {
        /// The running version.
        pub current: String,
        /// The version downloaded and waiting for a restart.
        pub ready: Option<String>,
    }

    /// A downloaded update, ready to apply.
    pub struct Pending {
        manager: UpdateManager,
        asset: VelopackAsset,
    }

    impl std::fmt::Debug for Pending {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter
                .debug_struct("Pending")
                .field("version", &self.asset.Version)
                .field("file", &self.asset.FileName)
                .finish_non_exhaustive()
        }
    }

    impl Pending {
        pub fn version(&self) -> &str {
            &self.asset.Version
        }

        /// Starts Velopack's updater, which waits for this process to exit (at
        /// most 60 s), applies the update and starts the app again without
        /// arguments. The caller quits the app after this returns.
        pub fn apply_after_exit(&self) -> Result<(), super::ApplyError> {
            self.manager
                .wait_exit_then_apply_updates(&self.asset, false, true, Vec::<String>::new())
                .map_err(|error| super::ApplyError::Failed(error.to_string()))
        }
    }

    /// The update check thread's handle. Clones share it.
    #[derive(Clone, Debug)]
    pub struct Updater {
        state_dir: PathBuf,
        pending: Arc<Mutex<Option<Arc<Pending>>>>,
    }

    impl Updater {
        /// Starts the thread: a first check after [`FIRST_CHECK`], then one
        /// every [`CHECK_EVERY`]. An available update is downloaded at once and
        /// `notify` is called with the new state. The thread ends quietly when
        /// the app is not an installed one (a development run, the CI smoke
        /// test).
        pub fn spawn(state_dir: PathBuf, notify: impl Fn(&UpdateState) + Send + 'static) -> Self {
            let updater = Self {
                state_dir,
                pending: Arc::new(Mutex::new(None)),
            };
            let shared = updater.clone();
            thread::spawn(move || shared.check_loop(notify));
            updater
        }

        pub fn state_dir(&self) -> &Path {
            &self.state_dir
        }

        pub fn state(&self) -> UpdateState {
            UpdateState {
                current: env!("CARGO_PKG_VERSION").to_owned(),
                ready: self.pending().map(|pending| pending.version().to_owned()),
            }
        }

        pub fn pending(&self) -> Option<Arc<Pending>> {
            self.pending.lock().ok().and_then(|pending| pending.clone())
        }

        fn check_loop(&self, notify: impl Fn(&UpdateState)) {
            thread::sleep(FIRST_CHECK);
            let manager =
                match UpdateManager::new(GithubSource::new(REPO_URL, None, true), None, None) {
                    Ok(manager) => manager,
                    Err(error) => {
                        log::line(&format!(
                            "update: not an installed app ({error}); no update checks"
                        ));
                        return;
                    }
                };
            loop {
                match manager.check_for_updates() {
                    Ok(UpdateCheck::UpdateAvailable(update)) => {
                        let version = update.TargetFullRelease.Version.clone();
                        log::line(&format!("update: {version} is available; downloading"));
                        match manager.download_updates(&update, None) {
                            Ok(()) => {
                                if let Ok(mut pending) = self.pending.lock() {
                                    *pending = Some(Arc::new(Pending {
                                        manager: manager.clone(),
                                        asset: update.TargetFullRelease.clone(),
                                    }));
                                }
                                log::line(&format!(
                                    "update: {version} downloaded; waiting for a restart"
                                ));
                                notify(&self.state());
                            }
                            Err(error) => {
                                log::line(&format!(
                                    "update: {version} could not be downloaded: {error}"
                                ));
                            }
                        }
                    }
                    Ok(UpdateCheck::NoUpdateAvailable | UpdateCheck::RemoteIsEmpty) => {}
                    Err(error) => log::line(&format!("update: the check failed: {error}")),
                }
                thread::sleep(CHECK_EVERY);
            }
        }
    }
}

#[cfg(not(feature = "updater"))]
mod real {
    use std::path::{Path, PathBuf};

    use crate::log;

    /// Where the releases are.
    pub const REPO_URL: &str = "https://github.com/115dkk/spatial-audio-softreceiver";
    /// The Application User Model ID of the app, its shortcut and the toasts.
    pub const APP_USER_MODEL_ID: &str = "io.github.115dkk.spatial-softreceiver";

    /// Nothing: this build has no updater.
    pub fn startup(_state_dir: &Path) {}

    /// Nothing: this build has no updater.
    pub fn apply_pending() {}

    /// What the settings screen shows.
    #[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
    pub struct UpdateState {
        /// The running version.
        pub current: String,
        /// The version downloaded and waiting for a restart.
        pub ready: Option<String>,
    }

    /// A downloaded update, ready to apply; never exists in this build.
    #[derive(Debug)]
    pub struct Pending(());

    impl Pending {
        pub fn version(&self) -> &str {
            ""
        }

        pub fn apply_after_exit(&self) -> Result<(), super::ApplyError> {
            Err(super::ApplyError::NotBuilt)
        }
    }

    /// The update check thread's handle; no thread in this build.
    #[derive(Clone, Debug)]
    pub struct Updater {
        state_dir: PathBuf,
    }

    impl Updater {
        pub fn spawn(state_dir: PathBuf, _notify: impl Fn(&UpdateState) + Send + 'static) -> Self {
            log::line("update: built without the updater feature; no update checks");
            Self { state_dir }
        }

        pub fn state_dir(&self) -> &Path {
            &self.state_dir
        }

        pub fn state(&self) -> UpdateState {
            UpdateState {
                current: env!("CARGO_PKG_VERSION").to_owned(),
                ready: None,
            }
        }

        pub fn pending(&self) -> Option<std::sync::Arc<Pending>> {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_errors_render_in_both_languages_and_keep_the_detail() {
        assert_eq!(
            ApplyError::NotBuilt.render(Language::Ko),
            "이 빌드에는 업데이트 기능이 없습니다."
        );
        assert_eq!(
            ApplyError::NotBuilt.render(Language::En),
            "This build has no updater."
        );
        let failed = ApplyError::Failed("exit code 3".to_owned());
        assert_eq!(
            failed.render(Language::Ko),
            "업데이트를 적용하지 못했습니다: exit code 3"
        );
        assert_eq!(
            failed.render(Language::En),
            "Could not apply the update: exit code 3"
        );
    }

    #[test]
    fn an_update_error_speaks_the_language_and_never_quits() {
        let mut quit = false;
        let error = apply(Language::En, None, || quit = true).unwrap_err();
        assert_eq!(error.message, "There is no downloaded update.");
        assert_eq!(error.next, NextAction::None);
        let error = apply(Language::Ko, None, || quit = true).unwrap_err();
        assert_eq!(error.message, "받아 둔 업데이트가 없습니다.");
        let failed = || Err(ApplyError::Failed("exit code 3".to_owned()));
        let error = finish(Language::En, failed(), "1.2.3", || quit = true).unwrap_err();
        assert_eq!(error.message, "Could not apply the update: exit code 3");
        let error = finish(Language::Ko, failed(), "1.2.3", || quit = true).unwrap_err();
        assert_eq!(error.message, "업데이트를 적용하지 못했습니다: exit code 3");
        assert!(!quit, "a failed apply must not quit the app");
    }

    #[test]
    fn a_started_updater_quits_the_app() {
        let mut quit = false;
        finish(Language::Ko, Ok(()), "1.2.3", || quit = true).unwrap();
        assert!(quit);
    }
}
