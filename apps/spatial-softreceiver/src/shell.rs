//! The Tauri 2 wiring (ADR 0001 and its 2026-10-15 addendum).
//!
//! * Two commands: `dispatch(intent)` returns `Result<(), UserError>`, and
//!   `snapshot()` hands the screen the current state when it starts. Both run
//!   on Tauri's thread pool, never the UI thread, since they wait for the
//!   control thread (an `Enable` takes hundreds of milliseconds).
//! * Two events: `snapshot` when the state changes, `meters` with the engine
//!   status at 30 Hz while Running.
//! * The tray (open, on/off, quit). Closing the window hides it; the app stays
//!   in the tray and the sound goes on.
//! * A second launch brings the window forward (single instance); the login
//!   autostart passes `--minimized`.
//! * Any exit the app did not start itself first quits the supervisor, so the
//!   default roles go back, then lets the app end. An exit request waits on a
//!   thread of its own; Windows ending the session (`WM_ENDSESSION`) ends the
//!   event loop without a request, so the `Exit` event quits the supervisor
//!   before the process ends.
//! * When the supervisor does not quit in time, or its thread dies with a
//!   panic, the roles go back from the restore point it wrote before taking
//!   them, with the guard's rule (`last_resort_restore`); a dead control
//!   thread then ends the app with 1.
//! * `--smoke [seconds]`: CI's check that the window, the screen's bundle and
//!   the IPC work. The app quits with 0 once the screen has asked for its
//!   first snapshot, or with 2 when it has not in time.
//!
//! The control thread never waits for the UI thread (events and menu changes
//! are posted to it), and the UI thread waits for the control thread only in
//! that last `Exit`: at most three seconds for Quit, then two for the restore
//! point.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use ssr_sys::WindowsHost;
use ssr_sys::host::RESTORE_POINT_FILE;
use ssr_types::EngineStatus;
use ssr_types::app::{Intent, Phase, Snapshot, UserError};
use supervisor::machine::Config;
use supervisor::restore_point::{self, RestorePoint};
use tauri::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter as _, Manager as _, RunEvent, WindowEvent, Wry};
use tauri_plugin_autostart::MacosLauncher;

use crate::control::{self, Bridge, ControlConfig};
use crate::launcher::WindowsLauncher;
use crate::log::{self, Log};
use crate::paths::{self, Args};
use crate::settings::JsonSettingsFile;

/// The window `tauri.conf.json` creates.
const MAIN_WINDOW: &str = "main";
const TRAY: &str = "main";
const MENU_OPEN: &str = "open";
const MENU_TOGGLE: &str = "toggle";
const MENU_QUIT: &str = "quit";

/// The event that carries a [`Snapshot`].
pub const SNAPSHOT_EVENT: &str = "snapshot";
/// The event that carries an [`EngineStatus`] while Running.
pub const METERS_EVENT: &str = "meters";

/// How long an exit the app did not start waits for the supervisor to quit.
const QUIT_WAIT: Duration = Duration::from_secs(3);

/// How long the UI thread waits for the restore point to be given back when
/// the event loop has ended.
const LAST_RESORT_WAIT: Duration = Duration::from_secs(2);

/// Set by the `snapshot` command: the screen is up and has asked for its
/// state. The smoke test waits for it.
#[derive(Clone, Debug, Default)]
pub struct SmokeFlag(Arc<AtomicBool>);

/// Runs the app until it exits. `Err` is a window or plugin that could not be
/// created.
pub fn run(args: Args) -> Result<(), String> {
    let state_dir = paths::state_dir(std::env::var_os("LOCALAPPDATA"));
    Log::open(&state_dir);
    log::line(&format!(
        "shell: Spatial SoftReceiver {} starts with {args:?}",
        env!("CARGO_PKG_VERSION")
    ));
    let smoke = SmokeFlag::default();
    let ending = Ending::new(state_dir);
    let setup_smoke = smoke.clone();
    let setup_ending = ending.clone();
    let app = tauri::Builder::default()
        // First, so a second launch is handed over before any other plugin
        // starts (the plugin's documentation asks for this).
        .plugin(tauri_plugin_single_instance::init(|handle, _argv, _cwd| {
            show_main(handle);
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec!["--minimized"]),
        ))
        .manage(smoke)
        .invoke_handler(tauri::generate_handler![dispatch, snapshot])
        .setup(move |app| setup(app, args, &setup_smoke, &setup_ending))
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                if let Err(error) = window.hide() {
                    log::line(&format!("shell: the window could not be hidden: {error}"));
                }
            }
        })
        .build(tauri::generate_context!())
        .map_err(|error| format!("the app could not start: {error}"))?;
    app.run(move |handle, event| match &event {
        RunEvent::ExitRequested { code, api, .. } => {
            if ending.allowed.load(Ordering::SeqCst) {
                return;
            }
            // Before setup there is no supervisor and nothing to give back.
            let Some(bridge) = handle.try_state::<Bridge>() else {
                return;
            };
            api.prevent_exit();
            log::line(&format!(
                "shell: an exit was requested ({code:?}); quitting the supervisor first"
            ));
            let bridge = bridge.inner().clone();
            let handle = handle.clone();
            let ending = ending.clone();
            let code = code.unwrap_or(0);
            thread::spawn(move || ending.quit_then_exit(&bridge, &handle, code));
        }
        RunEvent::Exit => {
            if let Some(bridge) = handle.try_state::<Bridge>() {
                ending.loop_ends(&bridge);
            }
        }
        _ => {}
    });
    Ok(())
}

/// The screen sends an intent; the result is the supervisor's.
#[tauri::command(async)]
fn dispatch(intent: Intent, bridge: tauri::State<'_, Bridge>) -> Result<(), UserError> {
    bridge.dispatch(intent)
}

/// The screen asks for the current state, once when it starts.
#[tauri::command(async)]
fn snapshot(bridge: tauri::State<'_, Bridge>, smoke: tauri::State<'_, SmokeFlag>) -> Snapshot {
    let snapshot = bridge.snapshot();
    smoke.0.store(true, Ordering::SeqCst);
    snapshot
}

fn setup(
    app: &mut tauri::App,
    args: Args,
    smoke: &SmokeFlag,
    ending: &Ending,
) -> Result<(), Box<dyn std::error::Error>> {
    let state_dir = ending.state_dir.clone();
    let host = windows_host(&state_dir);
    let launcher = WindowsLauncher {
        state_dir: state_dir.clone(),
    };
    let store = JsonSettingsFile::in_dir(&state_dir);

    let open = MenuItem::with_id(app, MENU_OPEN, "열기", true, None::<&str>)?;
    let toggle = MenuItem::with_id(
        app,
        MENU_TOGGLE,
        Toggle::Unavailable.text(),
        false,
        None::<&str>,
    )?;
    let separator = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, MENU_QUIT, "종료", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &toggle, &separator, &quit])?;

    let last = Arc::new(Mutex::new(None));
    let emitter = TauriEmitter {
        handle: app.handle().clone(),
        last: Arc::clone(&last),
        toggle,
        ending: ending.clone(),
    };
    let (bridge, control) = control::spawn(
        host,
        launcher,
        Config::default(),
        ControlConfig::default(),
        store,
        emitter,
    );
    app.manage(bridge.clone());
    let watched = ending.clone();
    let handle = app.handle().clone();
    thread::spawn(move || {
        if control.join().is_err() {
            watched.control_died(&handle);
        }
    });

    let menu_bridge = bridge.clone();
    let mut tray = TrayIconBuilder::with_id(TRAY)
        .tooltip("Spatial SoftReceiver")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(move |handle, event| on_menu(handle, &event, &menu_bridge, &last))
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon().cloned() {
        tray = tray.icon(icon);
    }
    tray.build(app)?;

    // The window starts hidden (tauri.conf.json), so `--minimized` never
    // flashes it; the smoke test always shows it, since a hidden window's
    // WebView2 is not known to load the screen on every runner.
    if !args.minimized || args.smoke.is_some() {
        show_main(app.handle());
    }
    if let Some(limit) = args.smoke {
        watch_smoke(
            app.handle().clone(),
            bridge,
            smoke.clone(),
            ending.clone(),
            limit,
        );
    }
    Ok(())
}

/// The control thread's view of the screen: events, the tray's on/off item,
/// and the end of the app.
struct TauriEmitter {
    handle: AppHandle,
    /// The last snapshot sent, which the tray's on/off item acts on.
    last: Arc<Mutex<Option<Snapshot>>>,
    toggle: MenuItem<Wry>,
    ending: Ending,
}

impl control::Emitter for TauriEmitter {
    fn snapshot(&self, snapshot: &Snapshot) {
        if let Err(error) = self.handle.emit(SNAPSHOT_EVENT, snapshot) {
            log::line(&format!("shell: the snapshot event failed: {error}"));
        }
        let toggle = Toggle::of(&snapshot.phase);
        let previous = self
            .last
            .lock()
            .ok()
            .and_then(|mut last| last.replace(snapshot.clone()))
            .map(|last| Toggle::of(&last.phase));
        if previous != Some(toggle) {
            // Menu items change on the UI thread; posting there keeps this
            // thread from waiting for it.
            let item = self.toggle.clone();
            let posted = self.handle.run_on_main_thread(move || {
                if let Err(error) = item
                    .set_text(toggle.text())
                    .and_then(|()| item.set_enabled(toggle.intent().is_some()))
                {
                    log::line(&format!("shell: the tray menu could not change: {error}"));
                }
            });
            if let Err(error) = posted {
                log::line(&format!("shell: the tray menu could not change: {error}"));
            }
        }
    }

    fn meters(&self, status: &EngineStatus) {
        if let Err(error) = self.handle.emit(METERS_EVENT, status) {
            log::line(&format!("shell: the meters event failed: {error}"));
        }
    }

    fn quit(&self) {
        log::line("shell: the supervisor quit");
        self.ending.supervisor_quit(&self.handle);
    }
}

/// The tray's on/off item for a phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Toggle {
    Disable,
    Enable,
    Unavailable,
}

impl Toggle {
    fn of(phase: &Phase) -> Self {
        match phase {
            Phase::Running => Self::Disable,
            Phase::Ready | Phase::Faulted => Self::Enable,
            Phase::Setup(_) | Phase::Quit => Self::Unavailable,
        }
    }

    fn text(self) -> &'static str {
        match self {
            Self::Disable => "끄기",
            Self::Enable | Self::Unavailable => "켜기",
        }
    }

    fn intent(self) -> Option<Intent> {
        match self {
            Self::Disable => Some(Intent::Disable),
            Self::Enable => Some(Intent::Enable),
            Self::Unavailable => None,
        }
    }
}

fn on_menu(handle: &AppHandle, event: &MenuEvent, bridge: &Bridge, last: &Mutex<Option<Snapshot>>) {
    let id = event.id();
    if id == MENU_OPEN {
        show_main(handle);
    } else if id == MENU_TOGGLE {
        let intent = last
            .lock()
            .ok()
            .and_then(|last| last.as_ref().map(|snapshot| Toggle::of(&snapshot.phase)))
            .and_then(Toggle::intent);
        if let Some(intent) = intent {
            send(bridge, intent);
        }
    } else if id == MENU_QUIT {
        send(bridge, Intent::Quit);
    }
}

/// Sends `intent` from a thread of its own: the tray's callbacks run on the
/// UI thread, and an `Enable` starts the engine.
fn send(bridge: &Bridge, intent: Intent) {
    let bridge = bridge.clone();
    thread::spawn(move || {
        if let Err(error) = bridge.dispatch(intent) {
            log::line(&format!(
                "shell: the tray's intent failed: {}",
                error.message
            ));
        }
    });
}

fn show_main(handle: &AppHandle) {
    let Some(window) = handle.get_webview_window(MAIN_WINDOW) else {
        return;
    };
    if let Err(error) = window
        .unminimize()
        .and_then(|()| window.show())
        .and_then(|()| window.set_focus())
    {
        log::line(&format!("shell: the window could not be shown: {error}"));
    }
}

/// How the app ends.
///
/// `started` is set by whoever ends it first: the supervisor quitting (the
/// screen's or the tray's Quit), or an exit that waits for the supervisor to
/// quit (one the app did not start, or the smoke test). `allowed` is set once
/// the supervisor has quit (or was given its time to): the next exit request
/// goes through, and the one who set it asks for the exit.
#[derive(Clone, Debug)]
struct Ending {
    started: Arc<AtomicBool>,
    allowed: Arc<AtomicBool>,
    /// Where the restore point is, for [`last_resort_restore`].
    state_dir: PathBuf,
}

impl Ending {
    fn new(state_dir: PathBuf) -> Self {
        Self {
            started: Arc::default(),
            allowed: Arc::default(),
            state_dir,
        }
    }

    /// The supervisor has quit: the app exits with 0, unless an exit that is
    /// waiting for this ends it itself.
    fn supervisor_quit(&self, handle: &AppHandle) {
        if !self.started.swap(true, Ordering::SeqCst) {
            self.allowed.store(true, Ordering::SeqCst);
            handle.exit(0);
        }
    }

    /// Quits the supervisor, waiting at most [`QUIT_WAIT`], then exits with
    /// `code`. Nothing happens when the app is already ending.
    ///
    /// When the supervisor did not quit, the restore point gives the roles
    /// back here, on this (not the UI) thread.
    fn quit_then_exit(&self, bridge: &Bridge, handle: &AppHandle, code: i32) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }
        if quit_within(bridge, QUIT_WAIT) != QuitOutcome::Done {
            last_resort_restore(&self.state_dir);
        }
        if !self.allowed.swap(true, Ordering::SeqCst) {
            handle.exit(code);
        }
    }

    /// The control thread ended with a panic its own catch did not hold: the
    /// roles go back from the restore point and the app exits with 1.
    fn control_died(&self, handle: &AppHandle) {
        log::line("shell: the control thread died; giving the roles back from the restore point");
        self.started.store(true, Ordering::SeqCst);
        last_resort_restore(&self.state_dir);
        if !self.allowed.swap(true, Ordering::SeqCst) {
            handle.exit(1);
        }
    }

    /// The event loop has ended. When the app did not arrange it (Windows
    /// ends the session), quits the supervisor here, on the UI thread, since
    /// the process exits when this returns. A quit already under way is
    /// waited for: this Quit is handled after it. When the supervisor does
    /// not quit, the restore point is given back from another thread (the
    /// host's COM calls cannot run on the UI thread), waited for at most
    /// [`LAST_RESORT_WAIT`].
    fn loop_ends(&self, bridge: &Bridge) {
        self.started.store(true, Ordering::SeqCst);
        if self.allowed.swap(true, Ordering::SeqCst) {
            return;
        }
        log::line("shell: the event loop ended without an exit request; quitting the supervisor");
        if quit_within(bridge, QUIT_WAIT) == QuitOutcome::Done {
            return;
        }
        let state_dir = self.state_dir.clone();
        let (done, finished) = mpsc::channel();
        thread::spawn(move || {
            last_resort_restore(&state_dir);
            let _ = done.send(());
        });
        if finished.recv_timeout(LAST_RESORT_WAIT).is_err() {
            log::line(&format!(
                "shell: the restore point was not given back within {} s",
                LAST_RESORT_WAIT.as_secs()
            ));
        }
    }
}

/// How a Quit sent at the end went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum QuitOutcome {
    /// The supervisor quit and gave the roles back.
    Done,
    /// The supervisor refused (it had quit already) or its thread has ended.
    Failed,
    TimedOut,
}

/// Sends Quit and waits for its result at most `limit`, writing anything but
/// success to the log. A Quit after the supervisor has quit is answered at
/// once.
fn quit_within(bridge: &Bridge, limit: Duration) -> QuitOutcome {
    let (done, finished) = mpsc::channel();
    let quitting = bridge.clone();
    thread::spawn(move || {
        let _ = done.send(quitting.dispatch(Intent::Quit));
    });
    match finished.recv_timeout(limit) {
        Ok(Ok(())) => QuitOutcome::Done,
        Ok(Err(error)) => {
            log::line(&format!("shell: quit: {}", error.message));
            QuitOutcome::Failed
        }
        Err(_) => {
            log::line(&format!(
                "shell: the supervisor did not quit within {} s",
                limit.as_secs()
            ));
            QuitOutcome::TimedOut
        }
    }
}

/// The last way back for the default roles, when the supervisor did not
/// quit (in time, or at all) or its thread died: the restore point it wrote
/// before taking them is given back with the guard's rule
/// ([`restore_point::restore`]: `restore_roles`, then the file is removed).
/// Without a restore point the roles are not ours and nothing happens. Makes
/// the host's COM calls, so never on the UI thread.
fn last_resort_restore(state_dir: &Path) {
    let path = state_dir.join(RESTORE_POINT_FILE);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            log::line("shell: last resort: there is no restore point; the roles are not ours");
            return;
        }
        Err(error) => {
            log::line(&format!(
                "shell: last resort: {} cannot be read: {error}",
                path.display()
            ));
            return;
        }
    };
    let point = match RestorePoint::from_json(&text) {
        Ok(point) => point,
        Err(error) => {
            log::line(&format!("shell: last resort: {}: {error}", path.display()));
            return;
        }
    };
    let notes = restore_point::restore(&point, &mut windows_host(state_dir));
    log::line("shell: last resort: gave the roles back from the restore point");
    for note in notes {
        log::line(&format!("shell: last resort: {note}"));
    }
}

/// The host adapter with `ssr-setup.exe` beside this executable.
fn windows_host(state_dir: &Path) -> WindowsHost {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .unwrap_or_default();
    WindowsHost::new(paths::setup_exe(&exe_dir), state_dir.to_path_buf())
}

/// `--smoke`: waits for the screen's first `snapshot` command, then quits with
/// 0; after `limit` without one, quits with 2.
fn watch_smoke(
    handle: AppHandle,
    bridge: Bridge,
    smoke: SmokeFlag,
    ending: Ending,
    limit: Duration,
) {
    thread::spawn(move || {
        let started = Instant::now();
        while !smoke.0.load(Ordering::SeqCst) {
            if started.elapsed() >= limit {
                let message = format!(
                    "smoke: the screen did not ask for a snapshot within {} s",
                    limit.as_secs()
                );
                eprintln!("{message}");
                log::line(&message);
                ending.quit_then_exit(&bridge, &handle, 2);
                return;
            }
            thread::sleep(Duration::from_millis(50));
        }
        let message = format!(
            "smoke: the screen asked for a snapshot after {:.1} s; the phase is {:?}",
            started.elapsed().as_secs_f64(),
            bridge.snapshot().phase
        );
        eprintln!("{message}");
        log::line(&message);
        ending.quit_then_exit(&bridge, &handle, 0);
    });
}
