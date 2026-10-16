//! Upmixers per endpoint (ADR 0035 with its 2026-10-16 correction, ADR 0021).
//!
//! The main, music and PASS endpoints (0, 1 and 2) each choose an upmixer:
//! the first-party one (not built yet, ADR 0020), a VST upmixer plugin, or
//! none. The choices, the plugins and each output kind's "apply upmixing"
//! switch are [`UpmixSettings`]; the supervisor keeps them in its
//! [`crate::Settings`].
//!
//! A VST upmixer runs in its own host process (ADR 0021), reached through the
//! [`PluginHost`] port: the Windows adapter drives `ssr-plughost`, the tests a
//! fake. While the engine runs, every endpoint whose effective choice is a
//! plugin has a host ([`PluginInstance`]), and the engine holds an
//! [`UpmixStage`] built on the control thread around the host's upmixer.
//!
//! * A host starts at the endpoint's source rate (the last `FormatChanged` of
//!   mono or stereo PCM, or a bitstream; 48 kHz before any) and restarts when
//!   that rate changes. The engine is transparent meanwhile
//!   ([`UpmixBypass::RateChange`]), so nothing waits for it.
//! * The host stays up while the output kind in use does not apply
//!   upmixing: the engine only goes around it ([`LiveChange::UpmixApply`]).
//! * The adapter's watch decides restarts and the three-crash disable
//!   (ADR 0021); the supervisor restarts on [`PluginEvent::NeedsRespawn`] and
//!   on a new plugin latency (the stage's delay depends on it), and turns the
//!   plugin off for the session on [`PluginEvent::Disabled`].
//! * A host that is let go is asked to stop and polled until it exits (its
//!   last state is saved on the way out), for at most five seconds.
//!
//! Nothing here touches the engine's real-time threads: stages are built and
//! dropped on the control thread, and a change the engine's full queue
//! refuses is kept and handed over again on the next tick.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use engine::{LiveChange, UpmixMode, UpmixStage, Upmixer};
use ssr_types::app::{
    EndpointUpmixView, Language, PluginFormat, PluginView, ProfileKind, UpmixBypass, UpmixState,
    UpmixView, UpmixerChoice,
};
use ssr_types::{SourceFormat, Speaker};

use crate::machine::EngineHandle;
use crate::session::SessionError;
use crate::text::Message;

/// What an endpoint that never chose follows (ADR 0035): no upmixing until
/// the first-party upmixer exists, then that one (ADR 0020).
pub const DEFAULT_UPMIXER: UpmixerChoice = UpmixerChoice::Off;

/// Endpoints that choose an upmixer: main (0), music (1) and PASS (2).
pub const UPMIX_ENDPOINTS: usize = engine::UPMIX_ENDPOINTS;

/// The rate a host starts at before its endpoint reported one.
pub const DEFAULT_RATE: u32 = 48_000;

/// Source rates an upmixer can run at: the engine's supported source rates.
const UPMIX_RATES: [u32; 6] = [44_100, 48_000, 88_200, 96_000, 176_400, 192_000];

/// How long a host that was asked to stop is polled before it is dropped
/// (which ends its process). The plugin host's own watch kills a host that
/// has not stopped after two seconds.
const RETIRE_GRACE: Duration = Duration::from_secs(5);

/// How long Quit waits for stopping hosts to save their state and exit.
const QUIT_GRACE: Duration = Duration::from_millis(2_500);

/// The upmix settings the shell persists (ADR 0035).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct UpmixSettings {
    /// Main, music and PASS, in endpoint order.
    pub endpoints: [EndpointUpmixSettings; UPMIX_ENDPOINTS],
    /// Headphones apply upmixing (on by default).
    pub headphones: bool,
    /// Speakers apply upmixing (on by default).
    pub speakers: bool,
}

impl Default for UpmixSettings {
    /// What settings written before ADR 0035 load as: every endpoint follows
    /// [`DEFAULT_UPMIXER`] without a plugin, and both kinds apply upmixing.
    fn default() -> Self {
        Self {
            endpoints: Default::default(),
            headphones: true,
            speakers: true,
        }
    }
}

impl UpmixSettings {
    /// Whether the output kind `kind` applies upmixing.
    pub fn applies(&self, kind: ProfileKind) -> bool {
        match kind {
            ProfileKind::Headphones => self.headphones,
            ProfileKind::Speakers => self.speakers,
        }
    }

    /// What `endpoint`'s choice resolves to, the default included.
    pub fn effective(&self, endpoint: usize) -> UpmixerChoice {
        self.endpoints
            .get(endpoint)
            .and_then(|stored| stored.choice)
            .unwrap_or(DEFAULT_UPMIXER)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct EndpointUpmixSettings {
    /// `None` follows [`DEFAULT_UPMIXER`].
    pub choice: Option<UpmixerChoice>,
    /// The VST upmixer chosen for this endpoint, kept while another choice is
    /// in use.
    pub plugin: Option<PluginSetting>,
}

/// A scanned upmixer plugin. Its saved state is the [`PluginHost`]'s to keep
/// (up to 64 MiB, ADR 0021), not the settings'.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PluginSetting {
    /// The `.vst3` bundle or file, or the VST 2 `.dll`.
    pub path: PathBuf,
    pub format: PluginFormat,
    /// The VST 3 class the scan found first; `None` for VST 2.
    #[serde(default)]
    pub class: Option<[u8; 16]>,
    /// The plugin's own name and vendor, as its scan reported them.
    pub name: String,
    pub vendor: String,
    /// The input speakers by name (`"L"`, `"R"`) as the plugin's host last
    /// reported them; empty until it first ran. The scan does not read buses.
    #[serde(default)]
    pub inputs: Vec<String>,
    /// The output speakers by name (`"L"`, `"C"`, `"LFE"`), likewise.
    #[serde(default)]
    pub outputs: Vec<String>,
}

impl PluginSetting {
    /// The same plugin: the same file, format and class. Its saved state
    /// belongs to it.
    pub fn same_plugin(&self, other: &Self) -> bool {
        self.path == other.path && self.format == other.format && self.class == other.class
    }
}

/// One class a plugin file offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginClass {
    /// The VST 3 class id (zero for VST 2).
    pub id: [u8; 16],
    pub name: String,
    pub vendor: String,
}

/// Why a plugin host could not scan, start or answer.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PluginHostError {
    /// This build cannot host plugins (only the Windows app can).
    #[error("plugin hosting is not available on this platform")]
    Unavailable,
    /// The host process or the plugin failed; the detail is theirs.
    #[error("{0}")]
    Failed(String),
}

/// What a running [`PluginInstance`] reports when polled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PluginEvent {
    /// The host is up (first start, respawn or restart):
    /// [`PluginInstance::take_upmixer`] now hands over its engine side.
    Running,
    /// The host stopped delivering (it died, hung or is waiting to restart)
    /// and the engine plays the fallback (`true`), or delivers again (`false`).
    Fallback(bool),
    /// The plugin reported a new latency in frames. The stage's delay depends
    /// on it, so the host is restarted (ADR 0021).
    LatencyChanged(usize),
    /// The watch waited out the backoff after a failure: call
    /// [`PluginInstance::restart`].
    NeedsRespawn,
    /// Three failures within ten minutes: the plugin is off for this session.
    Disabled,
    /// A start or restart that was under way failed; the detail is the host's.
    StartFailed(String),
    /// The process ended after [`PluginInstance::shutdown`].
    Exited,
}

/// The plugin host port (ADR 0035): the Windows adapter runs
/// `plughost::PluginProcess`, the tests a fake. Calls happen on the control
/// thread.
pub trait PluginHost {
    /// One-shot scan of a chosen file or bundle. May block the control thread
    /// up to its own deadline.
    fn scan(
        &mut self,
        path: &Path,
        format: PluginFormat,
    ) -> Result<Vec<PluginClass>, PluginHostError>;

    /// Starts a host for `plugin` on `endpoint` at `rate` (Hz), restoring the
    /// saved state of that endpoint. Must not block: an adapter whose host
    /// takes time to come up returns at once and reports
    /// [`PluginEvent::Running`] (or [`PluginEvent::StartFailed`]) from a later
    /// poll.
    fn start(
        &mut self,
        endpoint: u16,
        plugin: &PluginSetting,
        rate: u32,
    ) -> Result<Box<dyn PluginInstance>, PluginHostError>;

    /// Forgets the saved state of `endpoint`: another plugin was chosen there.
    fn forget_state(&mut self, endpoint: u16);
}

/// One endpoint's plugin host, from start to exit. Nothing here blocks.
pub trait PluginInstance {
    /// The engine side of the host now up, taken once after each start,
    /// respawn or restart; `None` while the host is still coming up.
    fn take_upmixer(&mut self) -> Option<Box<dyn Upmixer>>;
    /// The input speakers of the host now up.
    fn inputs(&self) -> &[Speaker];
    /// The output speakers of the host now up.
    fn outputs(&self) -> &[Speaker];
    /// The plugin's latency in frames of the host now up.
    fn latency_frames(&self) -> usize;
    /// Drives the host's watch. `now_ms` is the supervisor's clock
    /// (milliseconds since it started); an adapter whose watch keeps its own
    /// clock may ignore it.
    fn poll(&mut self, now_ms: u64) -> Vec<PluginEvent>;
    /// Starts the host again at `rate`. After [`PluginEvent::NeedsRespawn`]
    /// this is a respawn that keeps the failure window and restores the saved
    /// state (the one before it when the last restore crashed); otherwise the
    /// running host saves its state and stops first. The new host reports
    /// [`PluginEvent::Running`] like a first start, unless
    /// [`PluginInstance::take_upmixer`] has its upmixer at once.
    fn restart(&mut self, rate: u32) -> Result<(), PluginHostError>;
    /// Opens the plugin's editor as the host's own window.
    fn open_editor(&mut self) -> Result<(), PluginHostError>;
    /// Asks the host to save its state and stop; reports
    /// [`PluginEvent::Exited`] when it has.
    fn shutdown(&mut self);
}

/// What the ticks changed on their own since they were last taken
/// ([`crate::Supervisor::take_tick_changes`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TickChanges {
    /// An upmixer's state changed: a host came up, fell back or failed.
    pub snapshot: bool,
    /// The settings changed: a plugin's buses were learned.
    pub settings: bool,
}

/// The upmix view of a supervisor that has never run: every endpoint follows
/// the default without a plugin, and both kinds apply upmixing.
pub fn default_upmix_view() -> UpmixView {
    UpmixView {
        endpoints: (0..UPMIX_ENDPOINTS)
            .map(|endpoint| EndpointUpmixView {
                endpoint: endpoint as u16,
                choice: None,
                effective: DEFAULT_UPMIXER,
                plugin: None,
                state: UpmixState::Stopped,
                bypass: None,
                latency_ms: None,
                inputs: Vec::new(),
                outputs: Vec::new(),
            })
            .collect(),
        headphones: true,
        speakers: true,
        first_party_available: false,
    }
}

/// The plugin format a chosen path names: a `.vst3` file or bundle directory
/// is VST 3, a `.dll` file VST 2 (any case).
pub fn plugin_format(path: &Path) -> Option<PluginFormat> {
    let extension = path.extension()?.to_str()?;
    if extension.eq_ignore_ascii_case("vst3") {
        Some(PluginFormat::Vst3)
    } else if extension.eq_ignore_ascii_case("dll") && !path.is_dir() {
        Some(PluginFormat::Vst2)
    } else {
        None
    }
}

/// The rate an upmixer for `format` would run at, when the format can be
/// upmixed at all: mono or stereo PCM (at most two channels besides LFE) at
/// its sample rate, or a bitstream at its encoded rate (48 kHz when the
/// format does not say). `None` for native-surround PCM, which the stage
/// never upmixes, and for rates the engine cannot upmix.
pub fn upmix_rate(format: &SourceFormat) -> Option<u32> {
    let rate = match format {
        SourceFormat::Pcm(pcm) => {
            let lfe = u16::from(pcm.channel_mask & 0x8 != 0);
            if pcm.channels.saturating_sub(lfe) > 2 {
                return None;
            }
            pcm.sample_rate
        }
        SourceFormat::Bitstream(bitstream) if bitstream.encoded_rate != 0 => bitstream.encoded_rate,
        SourceFormat::Bitstream(_) => DEFAULT_RATE,
    };
    UPMIX_RATES.contains(&rate).then_some(rate)
}

/// The supervisor's upmixers: the port, a host per endpoint while the engine
/// runs, and the hosts on their way out.
pub(crate) struct Upmixers {
    plugins: Box<dyn PluginHost>,
    /// The supervisor's clock for [`PluginInstance::poll`].
    epoch: Instant,
    slots: [Slot; UPMIX_ENDPOINTS],
    /// Hosts asked to stop, polled until they exit.
    retiring: Vec<(Box<dyn PluginInstance>, Instant)>,
    /// The engine's "apply upmixing" as last handed over; `None` while no
    /// engine runs.
    applied: Option<bool>,
    /// What the engine's switch should be.
    wanted: bool,
    changes: TickChanges,
}

/// One endpoint's upmixer at run time.
#[derive(Default)]
struct Slot {
    host: Option<Host>,
    /// The endpoint's source rate while the engine runs.
    rate: Option<u32>,
    /// Off for this session until the user chooses again.
    failed: Option<Message>,
    /// The engine holds a stage for this endpoint.
    staged: bool,
    /// A removal the engine's full queue refused, handed over again on tick.
    unstage: bool,
}

struct Host {
    instance: Box<dyn PluginInstance>,
    /// The rate the host runs or starts at.
    rate: u32,
    /// The engine took a stage around the host now up.
    up: bool,
    /// A stage the engine's full queue refused, handed over again on tick.
    pending: Option<Box<UpmixStage>>,
    /// The host reported it stopped delivering.
    fallback: bool,
}

impl Upmixers {
    pub(crate) fn new(plugins: Box<dyn PluginHost>) -> Self {
        Self {
            plugins,
            epoch: Instant::now(),
            slots: Default::default(),
            retiring: Vec::new(),
            applied: None,
            wanted: true,
            changes: TickChanges::default(),
        }
    }

    pub(crate) fn take_changes(&mut self) -> TickChanges {
        std::mem::take(&mut self.changes)
    }

    pub(crate) fn scan(
        &mut self,
        path: &Path,
        format: PluginFormat,
    ) -> Result<Vec<PluginClass>, PluginHostError> {
        self.plugins.scan(path, format)
    }

    pub(crate) fn forget_state(&mut self, endpoint: usize) {
        self.plugins.forget_state(endpoint as u16);
    }

    /// The user chose again for `endpoint`: an earlier failure no longer
    /// holds.
    pub(crate) fn chose(&mut self, endpoint: usize) {
        self.slots[endpoint].failed = None;
    }

    /// Whether `endpoint`'s host is up: running, or falling back.
    pub(crate) fn host_up(&self, endpoint: usize) -> bool {
        self.slots
            .get(endpoint)
            .and_then(|slot| slot.host.as_ref())
            .is_some_and(|host| host.up)
    }

    pub(crate) fn open_editor(&mut self, endpoint: usize) -> Result<(), PluginHostError> {
        match self.slots[endpoint].host.as_mut() {
            Some(host) if host.up => host.instance.open_editor(),
            _ => Err(PluginHostError::Unavailable),
        }
    }

    /// The engine started: it applies upmixing and has no stage. Hands it the
    /// switch of the kind in use and starts the hosts the settings ask for.
    pub(crate) fn engine_started(
        &mut self,
        engine: &mut dyn EngineHandle,
        settings: &mut UpmixSettings,
        kind: ProfileKind,
    ) {
        self.applied = Some(true);
        for slot in &mut self.slots {
            slot.rate = None;
            slot.staged = false;
            slot.unstage = false;
        }
        self.set_apply(engine, settings.applies(kind));
        for endpoint in 0..UPMIX_ENDPOINTS {
            self.sync(engine, settings, endpoint);
        }
    }

    /// The engine stopped, and its stages with it: every host is let go.
    pub(crate) fn engine_stopped(&mut self) {
        for endpoint in 0..UPMIX_ENDPOINTS {
            let slot = &mut self.slots[endpoint];
            slot.rate = None;
            slot.staged = false;
            slot.unstage = false;
            if let Some(host) = slot.host.take() {
                self.retire(host.instance);
                self.changes.snapshot = true;
            }
        }
        self.applied = None;
    }

    /// The switch of the kind in use, handed to the running engine when it
    /// differs from what the engine has.
    pub(crate) fn set_apply(&mut self, engine: &mut dyn EngineHandle, on: bool) {
        self.wanted = on;
        self.retry_apply(engine);
    }

    fn retry_apply(&mut self, engine: &mut dyn EngineHandle) {
        if self.applied.is_some_and(|applied| applied != self.wanted)
            && engine.apply(LiveChange::UpmixApply(self.wanted)).is_ok()
        {
            self.applied = Some(self.wanted);
        }
    }

    /// Starts or stops `endpoint`'s host to match the settings: a host while
    /// the effective choice is a plugin, one is chosen and it has not failed.
    pub(crate) fn sync(
        &mut self,
        engine: &mut dyn EngineHandle,
        settings: &mut UpmixSettings,
        endpoint: usize,
    ) {
        let slot = &self.slots[endpoint];
        let wanted = settings.effective(endpoint) == UpmixerChoice::Plugin
            && settings.endpoints[endpoint].plugin.is_some()
            && slot.failed.is_none();
        match (wanted, slot.host.is_some()) {
            (true, false) => self.start_host(engine, settings, endpoint),
            (false, true) => self.stop_host(Some(engine), endpoint),
            _ => {}
        }
    }

    /// Lets `endpoint`'s host go and takes its stage out of the engine.
    pub(crate) fn stop_host(&mut self, engine: Option<&mut dyn EngineHandle>, endpoint: usize) {
        let Some(host) = self.slots[endpoint].host.take() else {
            return;
        };
        let slot = &self.slots[endpoint];
        if let Some(engine) = engine
            && (slot.staged || slot.unstage)
        {
            self.unstage(engine, endpoint);
        }
        self.retire(host.instance);
        self.changes.snapshot = true;
    }

    fn start_host(
        &mut self,
        engine: &mut dyn EngineHandle,
        settings: &mut UpmixSettings,
        endpoint: usize,
    ) {
        let Some(plugin) = settings.endpoints[endpoint].plugin.clone() else {
            return;
        };
        let rate = self.slots[endpoint].rate.unwrap_or(DEFAULT_RATE);
        self.changes.snapshot = true;
        match self.plugins.start(endpoint as u16, &plugin, rate) {
            Ok(instance) => {
                self.slots[endpoint].host = Some(Host {
                    instance,
                    rate,
                    up: false,
                    pending: None,
                    fallback: false,
                });
                self.install(engine, settings, endpoint);
            }
            Err(error) => self.fail(
                engine,
                endpoint,
                Message::PluginScanFailed {
                    detail: error.to_string(),
                },
            ),
        }
    }

    /// Builds a stage around the upmixer of `endpoint`'s host, once the host
    /// has one, and hands it to the engine. A plugin with no more outputs
    /// than inputs is no upmixer.
    fn install(
        &mut self,
        engine: &mut dyn EngineHandle,
        settings: &mut UpmixSettings,
        endpoint: usize,
    ) {
        let Some(host) = self.slots[endpoint].host.as_mut() else {
            return;
        };
        let Some(upmixer) = host.instance.take_upmixer() else {
            return;
        };
        let names = |speakers: &[Speaker]| -> Vec<String> {
            speakers
                .iter()
                .map(|speaker| speaker.name().to_owned())
                .collect()
        };
        let inputs = names(host.instance.inputs());
        let outputs = names(host.instance.outputs());
        let plugin = settings.endpoints[endpoint].plugin.as_mut();
        if outputs.len() <= inputs.len() {
            let name = plugin.map(|plugin| plugin.name.clone()).unwrap_or_default();
            self.fail(engine, endpoint, Message::PluginNotUpmixer { name });
            return;
        }
        if let Some(plugin) = plugin
            && (plugin.inputs != inputs || plugin.outputs != outputs)
        {
            plugin.inputs = inputs;
            plugin.outputs = outputs;
            self.changes.settings = true;
        }
        match UpmixStage::new(endpoint as u16, upmixer, engine.qpc_frequency()) {
            Ok(stage) => {
                host.pending = Some(Box::new(stage));
                self.hand_over(engine, endpoint);
            }
            Err(error) => self.fail(
                engine,
                endpoint,
                Message::PluginScanFailed {
                    detail: error.to_string(),
                },
            ),
        }
    }

    /// Hands `endpoint`'s pending stage to the engine; a full queue keeps it
    /// for the next tick.
    fn hand_over(&mut self, engine: &mut dyn EngineHandle, endpoint: usize) {
        let slot = &mut self.slots[endpoint];
        let Some(stage) = slot.host.as_mut().and_then(|host| host.pending.take()) else {
            return;
        };
        let change = LiveChange::Upmixer {
            endpoint: endpoint as u16,
            stage: Some(stage),
        };
        match engine.apply_retaining(change) {
            Ok(()) => {
                if let Some(host) = slot.host.as_mut() {
                    host.up = true;
                    host.fallback = false;
                }
                slot.staged = true;
                slot.unstage = false;
                self.changes.snapshot = true;
            }
            Err((
                SessionError::Rejected,
                LiveChange::Upmixer {
                    stage: Some(stage), ..
                },
            )) => {
                if let Some(host) = slot.host.as_mut() {
                    host.pending = Some(stage);
                }
            }
            Err((error, _)) => self.fail(
                engine,
                endpoint,
                Message::PluginScanFailed {
                    detail: error.to_string(),
                },
            ),
        }
    }

    /// Takes `endpoint`'s stage out of the engine, or leaves it for the next
    /// tick when the queue is full.
    fn unstage(&mut self, engine: &mut dyn EngineHandle, endpoint: usize) {
        let removed = engine
            .apply(LiveChange::Upmixer {
                endpoint: endpoint as u16,
                stage: None,
            })
            .is_ok();
        let slot = &mut self.slots[endpoint];
        slot.unstage = !removed;
        if removed {
            slot.staged = false;
        }
    }

    /// Turns `endpoint`'s upmixer off for this session with `message`.
    fn fail(&mut self, engine: &mut dyn EngineHandle, endpoint: usize, message: Message) {
        self.stop_host(Some(engine), endpoint);
        self.slots[endpoint].failed = Some(message);
        self.changes.snapshot = true;
    }

    fn retire(&mut self, mut instance: Box<dyn PluginInstance>) {
        instance.shutdown();
        self.retiring.push((instance, Instant::now()));
    }

    /// Starts `endpoint`'s host again at `rate` and installs its stage once
    /// it is up.
    fn restart(
        &mut self,
        engine: &mut dyn EngineHandle,
        settings: &mut UpmixSettings,
        endpoint: usize,
        rate: u32,
    ) {
        let Some(host) = self.slots[endpoint].host.as_mut() else {
            return;
        };
        host.up = false;
        host.pending = None;
        host.fallback = false;
        self.changes.snapshot = true;
        match host.instance.restart(rate) {
            Ok(()) => {
                host.rate = rate;
                self.install(engine, settings, endpoint);
            }
            Err(error) => self.fail(
                engine,
                endpoint,
                Message::PluginScanFailed {
                    detail: error.to_string(),
                },
            ),
        }
    }

    /// `endpoint` reported a new source format. A host running at another
    /// rate restarts at this one.
    pub(crate) fn format_changed(
        &mut self,
        engine: &mut dyn EngineHandle,
        settings: &mut UpmixSettings,
        endpoint: usize,
        format: &SourceFormat,
    ) {
        let Some(rate) = upmix_rate(format) else {
            return;
        };
        let Some(slot) = self.slots.get_mut(endpoint) else {
            return;
        };
        slot.rate = Some(rate);
        if slot.host.as_ref().is_some_and(|host| host.rate != rate) {
            self.restart(engine, settings, endpoint, rate);
        }
    }

    /// One tick while the engine runs: hands over what a full queue refused,
    /// then polls every host and acts on what it reports.
    pub(crate) fn poll(
        &mut self,
        engine: &mut dyn EngineHandle,
        settings: &mut UpmixSettings,
        now: Instant,
    ) {
        self.retry_apply(engine);
        for endpoint in 0..UPMIX_ENDPOINTS {
            if self.slots[endpoint].unstage {
                self.unstage(engine, endpoint);
            }
            if self.slots[endpoint]
                .host
                .as_ref()
                .is_some_and(|host| host.pending.is_some())
            {
                self.hand_over(engine, endpoint);
            }
        }
        let now_ms = u64::try_from(now.saturating_duration_since(self.epoch).as_millis())
            .unwrap_or(u64::MAX);
        for endpoint in 0..UPMIX_ENDPOINTS {
            let events = match self.slots[endpoint].host.as_mut() {
                Some(host) => host.instance.poll(now_ms),
                None => continue,
            };
            for event in events {
                let Some(host) = self.slots[endpoint].host.as_mut() else {
                    break;
                };
                match event {
                    PluginEvent::Running => self.install(engine, settings, endpoint),
                    PluginEvent::Fallback(on) => {
                        if host.fallback != on {
                            host.fallback = on;
                            self.changes.snapshot = true;
                        }
                    }
                    PluginEvent::LatencyChanged(_) | PluginEvent::NeedsRespawn => {
                        let rate = host.rate;
                        self.restart(engine, settings, endpoint, rate);
                    }
                    PluginEvent::Disabled => {
                        let name = settings.endpoints[endpoint]
                            .plugin
                            .as_ref()
                            .map(|plugin| plugin.name.clone())
                            .unwrap_or_default();
                        self.fail(engine, endpoint, Message::PluginDisabled { name });
                    }
                    PluginEvent::StartFailed(detail) => {
                        self.fail(engine, endpoint, Message::PluginScanFailed { detail });
                    }
                    // A running host's exit is the watch's to judge: it
                    // reports fallback, a respawn or the disable.
                    PluginEvent::Exited => {}
                }
            }
        }
    }

    /// Polls the hosts asked to stop and lets go of those that exited or
    /// outstayed [`RETIRE_GRACE`].
    pub(crate) fn poll_retiring(&mut self) {
        let now = Instant::now();
        let now_ms = u64::try_from(now.saturating_duration_since(self.epoch).as_millis())
            .unwrap_or(u64::MAX);
        self.retiring.retain_mut(|(instance, since)| {
            let exited = instance.poll(now_ms).contains(&PluginEvent::Exited);
            !exited && now.saturating_duration_since(*since) < RETIRE_GRACE
        });
    }

    /// Before the app quits: gives the stopping hosts up to [`QUIT_GRACE`] to
    /// save their state and exit, then lets go of the rest.
    pub(crate) fn drain(&mut self) {
        let deadline = Instant::now() + QUIT_GRACE;
        loop {
            self.poll_retiring();
            if self.retiring.is_empty() || Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        self.retiring.clear();
    }

    /// The screen's view: the settings, each host's state and, while a stage
    /// is in the running engine, its bypass and latency.
    pub(crate) fn view(
        &self,
        settings: &UpmixSettings,
        engine: Option<&dyn EngineHandle>,
        kind: ProfileKind,
        language: Language,
    ) -> UpmixView {
        let endpoints = (0..UPMIX_ENDPOINTS)
            .map(|endpoint| {
                let stored = &settings.endpoints[endpoint];
                let slot = &self.slots[endpoint];
                let effective = settings.effective(endpoint);
                let mut view = EndpointUpmixView {
                    endpoint: endpoint as u16,
                    choice: stored.choice,
                    effective,
                    plugin: stored.plugin.as_ref().map(|plugin| PluginView {
                        path: plugin.path.to_string_lossy().into_owned(),
                        format: plugin.format,
                        name: plugin.name.clone(),
                        vendor: plugin.vendor.clone(),
                    }),
                    state: UpmixState::Stopped,
                    bypass: None,
                    latency_ms: None,
                    inputs: stored
                        .plugin
                        .as_ref()
                        .map(|plugin| plugin.inputs.clone())
                        .unwrap_or_default(),
                    outputs: stored
                        .plugin
                        .as_ref()
                        .map(|plugin| plugin.outputs.clone())
                        .unwrap_or_default(),
                };
                if effective != UpmixerChoice::Plugin {
                    return view;
                }
                if let Some(message) = &slot.failed {
                    view.state = UpmixState::Failed(message.render(language));
                    return view;
                }
                let (Some(host), Some(engine)) = (&slot.host, engine) else {
                    return view;
                };
                if !host.up {
                    view.state = UpmixState::Starting;
                    return view;
                }
                let status = engine
                    .upmix_status(endpoint as u16)
                    .filter(|status| status.installed);
                let missing = status
                    .as_ref()
                    .is_some_and(|status| status.upmixing && status.mode == UpmixMode::Fallback);
                view.state = if host.fallback || missing {
                    UpmixState::Fallback
                } else {
                    UpmixState::Running
                };
                if let Some(status) = &status {
                    view.latency_ms = Some(status.delay_ms());
                    view.bypass = status.bypass;
                }
                // Nothing plays: the engine names no bypass yet, but the kind
                // in use already says the sound will go around.
                if view.bypass.is_none() && !settings.applies(kind) {
                    view.bypass = Some(UpmixBypass::KindOff);
                }
                view
            })
            .collect();
        UpmixView {
            endpoints,
            headphones: settings.headphones,
            speakers: settings.speakers,
            first_party_available: false,
        }
    }
}
