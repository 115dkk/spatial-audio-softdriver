//! The control thread: the one thread that owns the [`Supervisor`] (ADR 0001,
//! ADR 0013).
//!
//! The Windows host joins the multithreaded apartment for its COM calls, which
//! the UI thread's single-threaded apartment cannot, so the supervisor never
//! runs on the UI thread. The shell talks to it through a [`Bridge`] (intents
//! in, results and snapshots back) and hears from it through an [`Emitter`]:
//!
//! * every intent is handled, the settings are saved when it succeeds, the
//!   result is sent back, and a fresh snapshot goes out whatever the result
//!   (a failure's sentence is in the snapshot);
//! * every tick (33 ms) the engine is polled; notices or a new phase send a
//!   snapshot, and while Running the engine status goes out as meters (30 Hz).
//!   When the engine asked for its output again, the tick prepares it
//!   between a not-Running heartbeat and a fresh one, as a running profile
//!   import does (ADR 0033);
//! * every refresh (1 s) a snapshot is taken and sent only when it differs
//!   from the last one sent, the engine status left out of the comparison:
//!   this is how a plugged or removed output reaches the screen;
//! * Quit ends the thread after one [`Emitter::quit`]; so does the shell
//!   dropping every [`Bridge`], after the thread quits the supervisor itself
//!   so the default roles go back;
//! * a panic while serving (a host or launcher that panics) is caught: the
//!   thread logs it, tries Quit once more so the roles go back, and ends with
//!   [`Emitter::quit`]. A panic outside that (creating the supervisor, or
//!   `quit` itself) ends the thread with the panic, which the shell watches
//!   for.

use std::any::Any;
use std::panic::{self, AssertUnwindSafe};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use ssr_types::app::{Heartbeat, Intent, NextAction, Phase, Snapshot, UserError};
use ssr_types::{EngineStatus, Host};
use supervisor::machine::Config;
use supervisor::{Launcher, Settings, Supervisor};

use crate::log;

/// What the shell asks the control thread for.
#[derive(Debug)]
pub enum Request {
    Intent {
        intent: Intent,
        reply: Sender<Result<(), UserError>>,
    },
    Snapshot {
        reply: Sender<Snapshot>,
    },
}

/// What the control thread sends to the screen. The Tauri adapter and the
/// tests' recorder implement it.
pub trait Emitter: Send + 'static {
    fn snapshot(&self, snapshot: &Snapshot);
    fn meters(&self, status: &EngineStatus);
    /// One line per refresh for ADR 0033's parent process.
    fn heartbeat(&self, heartbeat: &Heartbeat);
    /// Once, when the supervisor has entered Quit and the control thread
    /// ends.
    fn quit(&self);
}

/// Where the supervisor's settings persist.
pub trait SettingsStore: Send + 'static {
    fn load(&self) -> Settings;
    /// A failure is logged and swallowed.
    fn save(&self, settings: &Settings);
}

/// The shell's handle on the control thread. Clones share the thread; when the
/// last one is dropped, the thread quits the supervisor and ends.
#[derive(Clone, Debug)]
pub struct Bridge {
    tx: Sender<Request>,
}

impl Bridge {
    /// Sends `intent` and waits for its result. Once the control thread has
    /// ended this is an error that says the app is quitting.
    pub fn dispatch(&self, intent: Intent) -> Result<(), UserError> {
        let (reply, result) = mpsc::channel();
        self.tx
            .send(Request::Intent { intent, reply })
            .map_err(|_| quitting())?;
        result.recv().unwrap_or_else(|_| Err(quitting()))
    }

    /// The supervisor's snapshot now. Once the control thread has ended, an
    /// empty snapshot in [`Phase::Quit`].
    pub fn snapshot(&self) -> Snapshot {
        let (reply, snapshot) = mpsc::channel();
        if self.tx.send(Request::Snapshot { reply }).is_err() {
            return quit_snapshot();
        }
        snapshot.recv().unwrap_or_else(|_| quit_snapshot())
    }
}

/// The control thread's two periods.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ControlConfig {
    /// How often the engine is polled and the meters go out.
    pub tick: Duration,
    /// How often a snapshot is taken to catch changes no intent or notice
    /// announces (outputs plugged or removed).
    pub refresh: Duration,
}

impl ControlConfig {
    /// 30 Hz meters.
    pub const TICK: Duration = Duration::from_millis(33);
    pub const REFRESH: Duration = Duration::from_secs(1);
}

impl Default for ControlConfig {
    fn default() -> Self {
        Self {
            tick: Self::TICK,
            refresh: Self::REFRESH,
        }
    }
}

/// Starts the control thread: it reads the settings from `store`, creates the
/// supervisor (on that thread, since the host's first calls happen in
/// [`Supervisor::new`]), saves the settings `new` may have rewritten, sends
/// the first snapshot at once, and then serves the returned [`Bridge`].
///
/// The thread ends with a panic only when one escapes the serving loop's
/// catch (see the module documentation); joining the handle tells.
pub fn spawn<H, L, E, S>(
    host: H,
    launcher: L,
    config: Config,
    control: ControlConfig,
    store: S,
    emitter: E,
    initial_notes: Vec<String>,
) -> (Bridge, JoinHandle<()>)
where
    H: Host + Send + 'static,
    L: Launcher + Send + 'static,
    E: Emitter,
    S: SettingsStore,
{
    let (tx, rx) = mpsc::channel();
    let thread = thread::Builder::new()
        .name("ssr-control".to_owned())
        .spawn(move || {
            let mut supervisor = Supervisor::new(host, launcher, config, store.load());
            for note in initial_notes {
                supervisor.note(note);
            }
            store.save(supervisor.settings());
            let mut thread = ControlThread {
                supervisor,
                store,
                emitter,
                control,
                last: None,
            };
            if let Err(panic) = panic::catch_unwind(AssertUnwindSafe(|| thread.run(&rx))) {
                thread.after_panic(panic.as_ref());
            }
        })
        .expect("the operating system starts the control thread");
    (Bridge { tx }, thread)
}

struct ControlThread<H: Host, L: Launcher, E: Emitter, S: SettingsStore> {
    supervisor: Supervisor<H, L>,
    store: S,
    emitter: E,
    control: ControlConfig,
    /// The last snapshot sent, without its engine status.
    last: Option<Snapshot>,
}

impl<H: Host, L: Launcher, E: Emitter, S: SettingsStore> ControlThread<H, L, E, S> {
    fn run(&mut self, rx: &Receiver<Request>) {
        self.publish();
        self.heartbeat();
        let started = Instant::now();
        let mut next_tick = started + self.control.tick;
        let mut next_refresh = started + self.control.refresh;
        loop {
            let now = Instant::now();
            if now >= next_tick {
                self.tick(now);
                next_tick = later(next_tick, self.control.tick, now);
            }
            if now >= next_refresh {
                self.refresh();
                next_refresh = later(next_refresh, self.control.refresh, now);
            }
            let wait = next_tick
                .min(next_refresh)
                .saturating_duration_since(Instant::now());
            match rx.recv_timeout(wait) {
                Ok(Request::Intent { intent, reply }) => {
                    if self.intent(intent, &reply) {
                        return;
                    }
                }
                Ok(Request::Snapshot { reply }) => {
                    let _ = reply.send(self.supervisor.snapshot());
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    log::line("control: the shell let go of the control thread; quitting");
                    if let Err(error) = self.supervisor.handle(Intent::Quit) {
                        log::line(&format!("control: quit failed: {error}"));
                    }
                    self.publish();
                    self.emitter.quit();
                    return;
                }
            }
        }
    }

    /// The serving loop panicked: the supervisor may be half way through an
    /// intent (the roles taken, say), so Quit is tried once more, a second
    /// panic swallowed, and the shell told the thread ends. Requests still
    /// queued are dropped with the receiver, which their senders see as the
    /// app quitting.
    fn after_panic(&mut self, panic: &(dyn Any + Send)) {
        log::line(&format!(
            "control: the control thread panicked ({}); quitting the supervisor",
            panic_message(panic)
        ));
        let supervisor = &mut self.supervisor;
        match panic::catch_unwind(AssertUnwindSafe(|| supervisor.handle(Intent::Quit))) {
            Ok(Ok(())) => log::line("control: quit after the panic"),
            Ok(Err(error)) => log::line(&format!(
                "control: quit after the panic failed: {}",
                error.message
            )),
            Err(again) => log::line(&format!(
                "control: quit after the panic panicked too ({})",
                panic_message(again.as_ref())
            )),
        }
        self.emitter.quit();
    }

    /// Handles one intent; true when the supervisor has quit and the thread
    /// ends.
    fn intent(&mut self, intent: Intent, reply: &Sender<Result<(), UserError>>) -> bool {
        let described = format!("{intent:?}");
        // A running engine prepares an imported profile on this thread
        // (decode, resample, fold, FFT), which can outlast the guard's 2 s
        // stall limit. Announce the step as not Running, as Disable does, so
        // the guard counts its silence against the 15 s takeover limit
        // instead of restarting the app; a fresh line follows the step.
        let long_step = *self.supervisor.phase() == Phase::Running
            && matches!(
                intent,
                Intent::ImportBrir(_) | Intent::ImportHeadphoneFir(_)
            );
        if long_step || matches!(intent, Intent::Disable | Intent::Quit) {
            // Disable and Quit stop the engine and give the roles back in one
            // step, during which no heartbeat goes out: the guard must not
            // take that for a stalled, Running app and bring the engine back
            // (ADR 0033).
            self.idle_heartbeat();
        }
        let result = self.supervisor.handle(intent);
        match &result {
            Ok(()) => {
                self.store.save(self.supervisor.settings());
                log::line(&format!("control: {described}: done"));
            }
            Err(error) => log::line(&format!("control: {described}: {}", error.message)),
        }
        if long_step {
            self.heartbeat();
        }
        // The reply goes before the snapshot: a shell waiting for it must not
        // wait for the screen as well.
        let _ = reply.send(result);
        self.publish();
        if *self.supervisor.phase() == Phase::Quit {
            self.emitter.quit();
            return true;
        }
        false
    }

    fn tick(&mut self, now: Instant) {
        let before = self.supervisor.phase().clone();
        let mut notices = self.supervisor.tick(now);
        if self.supervisor.needs_prepared() {
            // Only slow preparation is bracketed. If the change queue refused
            // an already prepared output, later ticks retry the handoff while
            // leaving the guard's Running stall clock untouched.
            let slow = self.supervisor.prepare_work_pending();
            if slow {
                self.idle_heartbeat();
            }
            notices.extend(self.supervisor.prepare_again());
            if slow {
                self.heartbeat();
            }
        }
        let phase = self.supervisor.phase();
        if *phase != before {
            log::line(&format!("control: {before:?} -> {phase:?}"));
        }
        if !notices.is_empty() || *phase != before {
            self.publish();
        }
        if *self.supervisor.phase() == Phase::Running
            && let Some(status) = self.supervisor.engine_status()
        {
            self.emitter.meters(&status);
        }
    }

    /// A heartbeat that says the app is not Running, before a step that
    /// sends none for longer than the guard's stall limit allows: the guard
    /// counts that silence against its 15 s takeover limit instead.
    fn idle_heartbeat(&self) {
        self.emitter.heartbeat(&Heartbeat {
            running: false,
            output_periods: 0,
            capture_reads: 0,
        });
    }

    /// One heartbeat line for the guard (ADR 0033): whether the supervisor
    /// is Running and the engine's progress counters.
    fn heartbeat(&mut self) {
        let status = self.supervisor.engine_status();
        self.emitter
            .heartbeat(&heartbeat_of(self.supervisor.phase(), status.as_ref()));
    }

    /// Sends a snapshot only when it differs from the last one sent, engine
    /// status aside.
    fn refresh(&mut self) {
        self.supervisor.watch_profile_file();
        let snapshot = self.supervisor.snapshot();
        self.emitter
            .heartbeat(&heartbeat_of(&snapshot.phase, snapshot.engine.as_ref()));
        if self.last.as_ref() != Some(&without_engine(&snapshot)) {
            self.send(snapshot);
        }
    }

    fn publish(&mut self) {
        let snapshot = self.supervisor.snapshot();
        self.send(snapshot);
    }

    fn send(&mut self, snapshot: Snapshot) {
        self.emitter.snapshot(&snapshot);
        self.last = Some(without_engine(&snapshot));
    }
}

/// The next deadline after `deadline` by `period`, skipping the ones a slow
/// iteration already missed.
fn later(deadline: Instant, period: Duration, now: Instant) -> Instant {
    let next = deadline + period;
    if next > now { next } else { now + period }
}

/// A panic's message when it has one (`panic!` with a literal or a format).
fn panic_message(panic: &(dyn Any + Send)) -> &str {
    panic
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("no message")
}

fn heartbeat_of(phase: &Phase, engine: Option<&EngineStatus>) -> Heartbeat {
    let (output_periods, capture_reads) = engine.map_or((0, 0), |status| {
        (status.output_periods, status.capture_reads)
    });
    Heartbeat {
        running: *phase == Phase::Running,
        output_periods,
        capture_reads,
    }
}

fn without_engine(snapshot: &Snapshot) -> Snapshot {
    Snapshot {
        engine: None,
        ..snapshot.clone()
    }
}

fn quitting() -> UserError {
    UserError {
        message: "앱이 종료되는 중입니다.".to_owned(),
        next: NextAction::None,
    }
}

fn quit_snapshot() -> Snapshot {
    Snapshot {
        phase: Phase::Quit,
        profile: supervisor::default_profile_view(),
        engine: None,
        outputs: Vec::new(),
        chosen_output: None,
        driver: None,
        platform: None,
        roles_taken: false,
        error: None,
        notes: Vec::new(),
    }
}
