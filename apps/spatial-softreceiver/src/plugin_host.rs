//! The supervisor's [`PluginHost`] port (ADR 0035): on Windows,
//! `ssr-plughost.exe` beside the app through `plughost::PluginProcess`; on
//! other systems, a port that hosts nothing.
//!
//! The Windows adapter never blocks the control thread for a start: each
//! start, restart and respawn runs `PluginProcess::start` on a thread of its
//! own, and the instance reports
//! [`PluginEvent::Running`](supervisor::upmix::PluginEvent::Running) from the
//! poll that finds it done. A scan does block, up to the host's own
//! deadlines, as the port allows.
//!
//! Plugin states (ADR 0021) are saved as the host returns them: the plugin
//! host's watch asks for one after loading and restoring, when the editor
//! closes, two seconds after the last parameter change and before a host
//! stops (`PluginControl` sends the request, the adapter keeps the reply).
//! Each endpoint has two files in the state directory,
//! `upmix-<endpoint>.state` and `upmix-<endpoint>.previous.state`: a new
//! state is written beside them and renamed over the newest, which becomes
//! the previous one. A start restores the newest and falls back to the
//! previous when that fails; a respawn keeps the session's own watch, with
//! its failure window and its two states, as `plughost` documents.

use std::fs;
use std::hash::{DefaultHasher, Hash as _, Hasher as _};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use plughost_proto::StateHistory;
use ssr_types::Speaker;
use ssr_types::app::PluginFormat;
use supervisor::upmix::{PluginClass, PluginHost, PluginHostError, PluginInstance, PluginSetting};

use crate::log;
use crate::paths;

/// The port where plugins cannot be hosted: every scan and start fails with
/// [`PluginHostError::Unavailable`].
#[derive(Clone, Copy, Debug, Default)]
pub struct UnavailablePluginHost;

impl PluginHost for UnavailablePluginHost {
    fn scan(
        &mut self,
        _path: &Path,
        _format: PluginFormat,
    ) -> Result<Vec<PluginClass>, PluginHostError> {
        Err(PluginHostError::Unavailable)
    }

    fn start(
        &mut self,
        _endpoint: u16,
        _plugin: &PluginSetting,
        _rate: u32,
    ) -> Result<Box<dyn PluginInstance>, PluginHostError> {
        Err(PluginHostError::Unavailable)
    }

    fn forget_state(&mut self, _endpoint: u16) {}
}

/// The plugin host of this build: on Windows `ssr-plughost.exe` in
/// `exe_dir`, saving states in `state_dir`; elsewhere
/// [`UnavailablePluginHost`].
pub fn platform_plugin_host(exe_dir: &Path, state_dir: &Path) -> Box<dyn PluginHost + Send> {
    #[cfg(windows)]
    {
        Box::new(windows::WindowsPluginHost::new(
            paths::plughost_exe(exe_dir),
            state_dir.to_path_buf(),
        ))
    }
    #[cfg(not(windows))]
    {
        let _ = (exe_dir, state_dir);
        Box::new(UnavailablePluginHost)
    }
}

/// The speakers of a plugin bus, from the Windows channel-mask bits the host
/// reports in channel order. A bit no speaker has is refused with its value.
pub fn speakers_of(bits: &[u32]) -> Result<Vec<Speaker>, String> {
    bits.iter()
        .map(|&bit| Speaker::from_wave_bit(bit).ok_or_else(|| format!("speaker bit {bit:#x}")))
        .collect()
}

/// The state a respawned host restores (ADR 0021): the newest, unless the
/// host died again right after restoring it, then the one before. The
/// history remembers which one is being restored.
pub fn respawn_state(states: &mut StateHistory) -> Option<Vec<u8>> {
    let after_crash = states.crash_after_restore().map(<[u8]>::to_vec);
    if after_crash.as_deref() == states.current() {
        states.begin_restore().map(<[u8]>::to_vec)
    } else {
        after_crash
    }
}

/// One endpoint's two saved plugin states (ADR 0021).
#[derive(Debug)]
pub struct StateStore {
    current: PathBuf,
    previous: PathBuf,
    /// The newest state on disk, by hash: the same state again is not
    /// saved, so it does not push the previous one out.
    saved: Option<u64>,
}

impl StateStore {
    pub fn new(state_dir: &Path, endpoint: u16) -> Self {
        Self {
            current: paths::upmix_state(state_dir, endpoint),
            previous: paths::upmix_previous_state(state_dir, endpoint),
            saved: None,
        }
    }

    /// The newest and the previous state, each when it can be read.
    pub fn load(&mut self) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
        let read = |path: &Path| match fs::read(path) {
            Ok(blob) => Some(blob),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => {
                log::line(&format!(
                    "plugin host: {} cannot be read: {error}",
                    path.display()
                ));
                None
            }
        };
        let current = read(&self.current);
        self.saved = current.as_deref().map(state_hash);
        (current, read(&self.previous))
    }

    /// Keeps `blob` as the newest state, the newest so far becoming the
    /// previous: written beside them, then renamed. A failure is logged.
    pub fn save(&mut self, blob: &[u8]) {
        let hash = state_hash(blob);
        if self.saved == Some(hash) {
            return;
        }
        match self.write(blob) {
            Ok(()) => self.saved = Some(hash),
            Err(error) => log::line(&format!(
                "plugin host: {} could not be saved: {error}",
                self.current.display()
            )),
        }
    }

    fn write(&self, blob: &[u8]) -> io::Result<()> {
        if let Some(dir) = self.current.parent() {
            fs::create_dir_all(dir)?;
        }
        let new = self.new_file();
        let mut file = fs::File::create(&new)?;
        file.write_all(blob)?;
        file.sync_all()?;
        drop(file);
        match fs::rename(&self.current, &self.previous) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        fs::rename(&new, &self.current)
    }

    /// Removes both states: another plugin was chosen.
    pub fn clear(&mut self) {
        for path in [&self.current, &self.previous, &self.new_file()] {
            match fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => log::line(&format!(
                    "plugin host: {} could not be removed: {error}",
                    path.display()
                )),
            }
        }
        self.saved = None;
    }

    fn new_file(&self) -> PathBuf {
        let mut name = self.current.clone().into_os_string();
        name.push(".new");
        PathBuf::from(name)
    }
}

fn state_hash(blob: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    blob.hash(&mut hasher);
    hasher.finish()
}

#[cfg(windows)]
mod windows {
    use std::path::{Path, PathBuf};
    use std::sync::mpsc::{self, Receiver, TryRecvError};
    use std::thread;
    use std::time::{Duration, Instant};

    use engine::{RingUpmixer, Upmixer};
    use plughost::watch::ControlWatch;
    use plughost::{ControlEvent, PluginControl, PluginProcess, SharedEvent, StartError, Started};
    use plughost_proto::{Action, Frame, Message};
    use ssr_types::Speaker;
    use ssr_types::app::PluginFormat;
    use supervisor::upmix::{
        PluginClass, PluginEvent, PluginHost, PluginHostError, PluginInstance, PluginSetting,
    };

    use super::{StateStore, respawn_state, speakers_of};

    /// How long a stopping host is polled for its last state before it is
    /// dropped (which ends its process). Its watch kills it after two seconds.
    const LEAVE_GRACE: Duration = Duration::from_secs(3);

    /// `ssr-plughost.exe` as the supervisor's plugin host port.
    #[derive(Clone, Debug)]
    pub struct WindowsPluginHost {
        exe: PathBuf,
        state_dir: PathBuf,
    }

    impl WindowsPluginHost {
        pub fn new(exe: PathBuf, state_dir: PathBuf) -> Self {
            Self { exe, state_dir }
        }
    }

    fn proto_format(format: PluginFormat) -> plughost::PluginFormat {
        match format {
            PluginFormat::Vst2 => plughost::PluginFormat::Vst2,
            PluginFormat::Vst3 => plughost::PluginFormat::Vst3,
        }
    }

    fn failed(error: impl std::fmt::Display) -> PluginHostError {
        PluginHostError::Failed(error.to_string())
    }

    impl PluginHost for WindowsPluginHost {
        fn scan(
            &mut self,
            path: &Path,
            format: PluginFormat,
        ) -> Result<Vec<PluginClass>, PluginHostError> {
            let classes =
                PluginProcess::scan(&self.exe, path, proto_format(format)).map_err(failed)?;
            Ok(classes
                .into_iter()
                .map(|class| PluginClass {
                    id: class.id,
                    name: class.name,
                    vendor: class.vendor,
                })
                .collect())
        }

        fn start(
            &mut self,
            endpoint: u16,
            plugin: &PluginSetting,
            rate: u32,
        ) -> Result<Box<dyn PluginInstance>, PluginHostError> {
            let mut instance = Instance {
                exe: self.exe.clone(),
                plugin: plugin.clone(),
                store: StateStore::new(&self.state_dir, endpoint),
                phase: Phase::Stopped,
                leaving: Vec::new(),
                stopping: false,
                exited: false,
            };
            let (current, previous) = instance.store.load();
            instance.begin_start(rate, current, previous, None)?;
            Ok(Box::new(instance))
        }

        fn forget_state(&mut self, endpoint: u16) {
            StateStore::new(&self.state_dir, endpoint).clear();
        }
    }

    /// The failure window and the session's states, carried from a dead
    /// host's control to its successor's.
    struct CarriedWatch {
        watch: ControlWatch,
        /// The old control's clock when it was taken.
        at_ms: u64,
        taken: Instant,
    }

    enum Phase {
        /// A thread is starting the host.
        Starting {
            result: Receiver<Result<Started, StartError>>,
            watch: Option<Box<CarriedWatch>>,
        },
        /// The old host was asked for its state and to stop; the new one
        /// starts at `rate` once it is gone.
        Handoff {
            rate: u32,
        },
        Up(Box<Up>),
        /// Nothing runs: shut down, or a start failed.
        Stopped,
    }

    /// A host that is up, and its engine side until the supervisor takes it.
    struct Up {
        control: PluginControl,
        inputs: Vec<Speaker>,
        outputs: Vec<Speaker>,
        latency: usize,
        upmixer: Option<Box<dyn Upmixer>>,
    }

    struct Instance {
        exe: PathBuf,
        plugin: PluginSetting,
        store: StateStore,
        phase: Phase,
        /// Hosts asked to stop, polled for their last state until they exit.
        leaving: Vec<(PluginControl, Instant)>,
        stopping: bool,
        exited: bool,
    }

    impl Instance {
        /// Starts a host on a thread of its own, restoring `state` (and
        /// `fallback` when that start fails).
        fn begin_start(
            &mut self,
            rate: u32,
            state: Option<Vec<u8>>,
            fallback: Option<Vec<u8>>,
            watch: Option<Box<CarriedWatch>>,
        ) -> Result<(), PluginHostError> {
            let (sender, result) = mpsc::channel();
            let exe = self.exe.clone();
            let path = self.plugin.path.clone();
            let format = proto_format(self.plugin.format);
            let class = self.plugin.class;
            thread::Builder::new()
                .name("ssr-plughost-start".to_owned())
                .spawn(move || {
                    let mut started =
                        PluginProcess::start(&exe, &path, format, class, rate, state.as_deref());
                    if started.is_err()
                        && let Some(previous) = fallback.as_deref()
                    {
                        started =
                            PluginProcess::start(&exe, &path, format, class, rate, Some(previous));
                    }
                    // Nobody waits any more when the instance was let go:
                    // the host then ends with its job.
                    let _ = sender.send(started);
                })
                .map_err(failed)?;
            self.phase = Phase::Starting { result, watch };
            Ok(())
        }

        /// A started host becomes the one up, with its upmixer for the engine.
        fn up(&mut self, started: Started, watch: Option<Box<CarriedWatch>>) -> Result<(), String> {
            let Started {
                region,
                work,
                publisher,
                input_wave_bits,
                output_wave_bits,
                latency_frames,
                mut control,
                ..
            } = started;
            if let Some(carried) = watch {
                let CarriedWatch {
                    watch,
                    at_ms,
                    taken,
                } = *carried;
                let elapsed = u64::try_from(taken.elapsed().as_millis()).unwrap_or(0);
                control.restore_watch(watch, at_ms.saturating_add(elapsed));
            }
            let inputs = speakers_of(&input_wave_bits)?;
            let outputs = speakers_of(&output_wave_bits)?;
            let latency = latency_frames as usize;
            let upmixer = RingUpmixer::new(
                Box::new(region),
                Box::new(SharedEvent(work)),
                publisher,
                inputs.clone(),
                outputs.clone(),
                latency,
            )
            .map_err(|error| error.to_string())?;
            self.phase = Phase::Up(Box::new(Up {
                control,
                inputs,
                outputs,
                latency,
                upmixer: Some(Box::new(upmixer)),
            }));
            Ok(())
        }

        /// Saves what the stopping hosts return and lets go of those that
        /// exited or outstayed [`LEAVE_GRACE`].
        fn poll_leaving(&mut self) {
            let store = &mut self.store;
            self.leaving.retain_mut(|(control, since)| {
                let gone = !matches!(control.exit_code(), Ok(None));
                for event in control.poll(control.elapsed_ms()) {
                    if let ControlEvent::Message(Frame {
                        message: Message::State { blob },
                        ..
                    }) = event
                    {
                        store.save(&blob);
                    }
                }
                !gone && since.elapsed() < LEAVE_GRACE
            });
        }
    }

    impl PluginInstance for Instance {
        fn take_upmixer(&mut self) -> Option<Box<dyn Upmixer>> {
            match &mut self.phase {
                Phase::Up(up) => up.upmixer.take(),
                _ => None,
            }
        }

        fn inputs(&self) -> &[Speaker] {
            match &self.phase {
                Phase::Up(up) => &up.inputs,
                _ => &[],
            }
        }

        fn outputs(&self) -> &[Speaker] {
            match &self.phase {
                Phase::Up(up) => &up.outputs,
                _ => &[],
            }
        }

        fn latency_frames(&self) -> usize {
            match &self.phase {
                Phase::Up(up) => up.latency,
                _ => 0,
            }
        }

        /// The host's watch keeps its own clock (`PluginControl::elapsed_ms`,
        /// carried across respawns), so the supervisor's is not used.
        fn poll(&mut self, _now_ms: u64) -> Vec<PluginEvent> {
            let mut events = Vec::new();
            self.poll_leaving();

            let finished = match &mut self.phase {
                Phase::Starting { result, watch } => match result.try_recv() {
                    Ok(started) => Some((started, watch.take())),
                    Err(TryRecvError::Empty) => None,
                    Err(TryRecvError::Disconnected) => Some((
                        Err(StartError("the thread starting the host ended".to_owned())),
                        None,
                    )),
                },
                _ => None,
            };
            if let Some((started, watch)) = finished {
                match started
                    .map_err(|error| error.to_string())
                    .and_then(|started| self.up(started, watch))
                {
                    Ok(()) => events.push(PluginEvent::Running),
                    Err(detail) => {
                        self.phase = Phase::Stopped;
                        events.push(PluginEvent::StartFailed(detail));
                    }
                }
            }

            if let Phase::Handoff { rate } = self.phase
                && self.leaving.is_empty()
            {
                let (current, previous) = self.store.load();
                if let Err(error) = self.begin_start(rate, current, previous, None) {
                    self.phase = Phase::Stopped;
                    events.push(PluginEvent::StartFailed(error.to_string()));
                }
            }

            if let Phase::Up(up) = &mut self.phase {
                for event in up.control.poll(up.control.elapsed_ms()) {
                    match event {
                        ControlEvent::Message(Frame {
                            message: Message::State { blob },
                            ..
                        }) => self.store.save(&blob),
                        ControlEvent::Message(Frame {
                            message: Message::LatencyChanged { frames },
                            ..
                        }) => {
                            let frames = frames as usize;
                            if frames != up.latency {
                                events.push(PluginEvent::LatencyChanged(frames));
                            }
                        }
                        ControlEvent::Action(Action::EnterFallback | Action::Kill { .. }) => {
                            events.push(PluginEvent::Fallback(true));
                        }
                        ControlEvent::Action(Action::LeaveFallback) => {
                            events.push(PluginEvent::Fallback(false));
                        }
                        ControlEvent::Action(Action::Disable { .. }) => {
                            events.push(PluginEvent::Disabled);
                        }
                        ControlEvent::NeedsRespawn => events.push(PluginEvent::NeedsRespawn),
                        _ => {}
                    }
                }
            }

            if self.stopping
                && !self.exited
                && matches!(self.phase, Phase::Stopped)
                && self.leaving.is_empty()
            {
                self.exited = true;
                events.push(PluginEvent::Exited);
            }
            events
        }

        fn restart(&mut self, rate: u32) -> Result<(), PluginHostError> {
            match std::mem::replace(&mut self.phase, Phase::Stopped) {
                // The host died and the watch waited out the backoff: a new
                // process that carries the watch on.
                Phase::Up(up) if up.control.needs_respawn => {
                    let mut control = up.control;
                    let at_ms = control.elapsed_ms();
                    let mut watch = control.take_watch();
                    let state = respawn_state(&mut watch.states);
                    drop(control);
                    let carried = Box::new(CarriedWatch {
                        watch,
                        at_ms,
                        taken: Instant::now(),
                    });
                    self.begin_start(rate, state, None, Some(carried))
                }
                // A new rate or latency: the running host saves its state
                // and stops, and the next one restores that.
                Phase::Up(up) => {
                    let mut control = up.control;
                    // A broken pipe leaves nothing to wait for but the exit.
                    let _ = control.shutdown();
                    self.leaving.push((control, Instant::now()));
                    self.phase = Phase::Handoff { rate };
                    Ok(())
                }
                Phase::Handoff { .. } => {
                    self.phase = Phase::Handoff { rate };
                    Ok(())
                }
                // A start under way is dropped with its result; its host
                // ends with its job.
                Phase::Starting { .. } | Phase::Stopped => {
                    let (current, previous) = self.store.load();
                    self.begin_start(rate, current, previous, None)
                }
            }
        }

        fn open_editor(&mut self) -> Result<(), PluginHostError> {
            match &mut self.phase {
                Phase::Up(up) => up.control.open_editor().map(drop).map_err(failed),
                _ => Err(PluginHostError::Failed(
                    "the plugin host is not up".to_owned(),
                )),
            }
        }

        fn shutdown(&mut self) {
            self.stopping = true;
            if let Phase::Up(up) = std::mem::replace(&mut self.phase, Phase::Stopped) {
                let mut control = up.control;
                let _ = control.shutdown();
                self.leaving.push((control, Instant::now()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wave_bits_map_to_speakers_and_unknown_bits_are_refused() {
        assert_eq!(
            speakers_of(&[0x1, 0x2, 0x4, 0x8, 0x10, 0x20]),
            Ok(vec![
                Speaker::L,
                Speaker::R,
                Speaker::C,
                Speaker::Lfe,
                Speaker::Lrs,
                Speaker::Rrs
            ])
        );
        assert_eq!(
            speakers_of(&[0x1, 0x40000]),
            Err("speaker bit 0x40000".into())
        );
    }

    #[test]
    fn a_respawn_restores_the_newest_state_then_the_one_before() {
        let mut states = StateHistory::new();
        assert_eq!(respawn_state(&mut states), None);
        states.save(vec![1]);
        states.save(vec![2]);
        // The first crash restores the newest...
        assert_eq!(respawn_state(&mut states), Some(vec![2]));
        // ...and a crash right after that restore, the one before.
        assert_eq!(respawn_state(&mut states), Some(vec![1]));
        // A state saved since starts over from the newest.
        states.save(vec![3]);
        assert_eq!(respawn_state(&mut states), Some(vec![3]));
    }

    #[test]
    fn states_rotate_through_two_files_and_clear() {
        let dir = std::env::temp_dir().join(format!(
            "ssr-plugin-states-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut store = StateStore::new(&dir, 2);
        assert_eq!(store.load(), (None, None));
        store.save(b"one");
        assert_eq!(fs::read(dir.join("upmix-2.state")).unwrap(), b"one");
        assert!(!dir.join("upmix-2.previous.state").exists());
        store.save(b"two");
        // The same state again keeps the one before it.
        store.save(b"two");
        assert_eq!(store.load(), (Some(b"two".to_vec()), Some(b"one".to_vec())));
        store.save(b"three");
        assert_eq!(
            StateStore::new(&dir, 2).load(),
            (Some(b"three".to_vec()), Some(b"two".to_vec()))
        );
        assert_eq!(StateStore::new(&dir, 1).load(), (None, None));
        store.clear();
        assert_eq!(store.load(), (None, None));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn hosting_is_unavailable_off_windows() {
        let mut plugins = UnavailablePluginHost;
        assert_eq!(
            plugins.scan(Path::new("Example.vst3"), PluginFormat::Vst3),
            Err(PluginHostError::Unavailable)
        );
        let plugin = PluginSetting {
            path: "Example.vst3".into(),
            format: PluginFormat::Vst3,
            class: None,
            name: "Example".into(),
            vendor: "Vendor".into(),
            inputs: Vec::new(),
            outputs: Vec::new(),
        };
        assert!(matches!(
            plugins.start(0, &plugin, 48_000),
            Err(PluginHostError::Unavailable)
        ));
    }
}
