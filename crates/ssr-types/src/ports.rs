//! Port traits: the seams the engine and the supervisor are wired through.
//!
//! The traits live here, not in `engine`, so `ssr-sys` can implement them
//! without depending on the engine. Each seam has two adapters or more:
//!
//! | seam | adapters |
//! | --- | --- |
//! | [`CaptureSource`] | driver ring and SAL ring (`ssr-sys`), paced and looping dump file (`engine::adapters`), test fakes |
//! | [`OutputOpener`] / [`OutputSink`] | WASAPI (`ssr-sys`), WAV file and null (`engine::adapters`), test fakes |
//! | [`Host`] | Windows (`ssr-sys`, the next M4 slice), the scripted fake in `supervisor`'s tests |

use std::time::Duration;

/// Where the engine reads the driver's record stream from.
///
/// [`read`](CaptureSource::read) is the only call in the engine allowed to
/// block, and only the capture thread makes it.
pub trait CaptureSource: Send + 'static {
    /// Fills `buf` with whole records (the `ssr_abi` wire format) and returns
    /// the number of bytes written. Waits at most `wait` for data; `Ok(0)` means
    /// the wait ran out.
    fn read(&mut self, buf: &mut [u8], wait: Duration) -> Result<usize, CaptureError>;

    /// Current source-clock QPC after the latest successful read. Sources that
    /// cannot expose the clock return `None`.
    fn now_qpc(&self) -> Option<u64> {
        None
    }

    /// Asks the source to produce `ppm` parts per million faster (negative:
    /// slower). Adapters without a rate control (dump, SAL ring) refuse, and the
    /// engine falls back to resampling.
    fn set_rate_trim(&mut self, ppm: f64) -> Result<(), TrimUnsupported>;

    /// Reopens a lost capture source in place. Live device adapters override
    /// this; finite files and synthetic sources keep the unsupported default.
    fn reconnect(&mut self) -> Result<(), CaptureError> {
        Err(CaptureError::NotPresent)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CaptureError {
    #[error("the capture device is not present")]
    NotPresent,
    #[error("the capture device is open in another process")]
    Busy,
    #[error("access to the capture device was denied")]
    AccessDenied,
    #[error("the capture stream ended")]
    Ended,
    #[error("capture failed: {0}")]
    Io(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("this capture source cannot trim its rate")]
pub struct TrimUnsupported;

/// The channel layout the engine asks an output for (ADR 0030): two for
/// headphones, the speaker profile's device channels for speakers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutputLayout {
    pub channels: u16,
    /// WAVEFORMATEXTENSIBLE `dwChannelMask`. 0 is direct out: channel n goes to
    /// the device's output n with no speaker meaning.
    pub channel_mask: u32,
}

impl OutputLayout {
    pub const STEREO: Self = Self {
        channels: 2,
        channel_mask: 0x3,
    };
}

/// The mode in which an output sink was actually opened.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum OutputMode {
    #[default]
    Exclusive,
    Shared,
}

/// The shape of the periods an output sink asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutputFormat {
    pub rate: u32,
    /// Interleaved channels per frame, as opened.
    pub channels: u16,
    /// The channel mask the device was opened with (0: direct out).
    pub channel_mask: u32,
    /// Frames per period the device asks for at a time.
    pub period_frames: usize,
    pub mode: OutputMode,
    /// The caller requested exclusive mode, but explicitly allowed shared mode.
    pub fell_back: bool,
    /// Channels in the shared engine's mix format. Equal to `channels` unless
    /// an exclusive request fell back to the shared engine.
    pub mix_channels: u16,
}

impl OutputFormat {
    pub const fn exclusive(
        rate: u32,
        channels: u16,
        channel_mask: u32,
        period_frames: usize,
    ) -> Self {
        Self {
            rate,
            channels,
            channel_mask,
            period_frames,
            mode: OutputMode::Exclusive,
            fell_back: false,
            mix_channels: channels,
        }
    }
}

/// Opens an output sink on the thread that will use it.
///
/// WASAPI objects belong to the thread that created them, so the engine moves
/// the opener to its output thread and opens there. `open` errors still reach
/// `engine::start` as a start error. The engine opens again, after dropping
/// the previous sink, when the output layout changes (a profile with another
/// channel count) and to reconnect a lost device.
pub trait OutputOpener: Send + 'static {
    type Sink: OutputSink;

    fn open(&mut self, layout: OutputLayout) -> Result<Self::Sink, OutputError>;

    /// Changes the requested period for the next `open`. Adapters that cannot
    /// choose a hardware period return false.
    fn set_period(&mut self, _period: Duration) -> bool {
        false
    }
}

/// An open output device, used only from the thread that opened it.
pub trait OutputSink {
    fn format(&self) -> OutputFormat;

    /// Waits until the device wants the next period, lets `fill` write it
    /// (interleaved, `period_frames * channels` samples), and returns the QPC at
    /// which the period's first frame will play.
    fn next_period(&mut self, fill: &mut dyn FnMut(&mut [f32])) -> Result<u64, OutputError>;

    /// Times the sink restarted its stream after the device stalled; sinks
    /// that never restart keep the default.
    fn restarts(&self) -> u64 {
        0
    }

    /// Device frames played at the latest period boundary, when the adapter can
    /// expose a hardware position. The runtime pairs deltas with playback QPC.
    fn played_frames(&self) -> Option<u64> {
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OutputError {
    #[error("no output device matches the selection")]
    NotFound,
    #[error("the output device was removed")]
    Removed,
    #[error("the output device rejected every format tried: {0}")]
    FormatRejected(String),
    #[error("exclusive output is unavailable: {0}")]
    ExclusiveUnavailable(String),
    #[error("the output device is in exclusive use by another application")]
    Busy,
    #[error("output failed: {0}")]
    Io(String),
}

/// A render endpoint as the operating system lists it. `endpoint` is `Some(n)`
/// for our driver's endpoint n (found through the KS filter reference, never
/// through the display name).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputDevice {
    pub id: DeviceId,
    pub name: String,
    pub endpoint: Option<u16>,
    pub state: DeviceState,
}

/// The operating system's identifier of a render endpoint (the `IMMDevice`
/// id). It can change across reboots; the supervisor keeps the name as a
/// fallback.
#[derive(
    Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[cfg_attr(feature = "ts", derive(specta::Type))]
pub struct DeviceId(pub String);

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum DeviceState {
    Active,
    Disabled,
    NotPresent,
    Unplugged,
}

/// The three roles Windows keeps a default render device for (eConsole,
/// eMultimedia, eCommunications).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Console,
    Multimedia,
    Communications,
}

impl Role {
    /// Every role, in the order the supervisor takes them.
    pub const ALL: [Role; 3] = [Role::Console, Role::Multimedia, Role::Communications];
}

/// The default device of each role; `None` when Windows has none for it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DefaultRoles {
    pub console: Option<DeviceId>,
    pub multimedia: Option<DeviceId>,
    pub communications: Option<DeviceId>,
}

impl DefaultRoles {
    pub fn get(&self, role: Role) -> Option<&DeviceId> {
        match role {
            Role::Console => self.console.as_ref(),
            Role::Multimedia => self.multimedia.as_ref(),
            Role::Communications => self.communications.as_ref(),
        }
    }

    pub fn set(&mut self, role: Role, id: Option<DeviceId>) {
        let slot = match role {
            Role::Console => &mut self.console,
            Role::Multimedia => &mut self.multimedia,
            Role::Communications => &mut self.communications,
        };
        *slot = id;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum SecureBoot {
    Enabled,
    Disabled,
    /// The firmware has no Secure Boot (legacy BIOS).
    Unsupported,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum Arch {
    X64,
    #[serde(rename = "arm64")]
    Arm64,
    Other,
}

/// What the machine learns about the PC once per run.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(specta::Type))]
pub struct Platform {
    pub os_build: u32,
    pub arch: Arch,
    pub test_signing: bool,
    pub secure_boot: SecureBoot,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(specta::Type))]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum DriverState {
    NotInstalled,
    /// `problem_code` is DEVPKEY_Device_ProblemCode (0 is fine, 52 is an
    /// unsigned driver).
    Installed {
        problem_code: u32,
        service_running: bool,
        exclusive_policy: PolicyState,
    },
    RebootRequired,
}

/// Whether our endpoints carry the exclusive-mode policy of ADR 0028: two
/// private endpoint property keys (`ssr_abi::exclusive_policy`) that keep
/// other apps from opening the primary and music endpoints exclusively and
/// let them open the passthrough endpoint so. The host reads them; only
/// `ssr-setup` writes them (install, repair, set-formats).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum PolicyState {
    /// Every active endpoint of ours that the policy covers has the values
    /// `ssr-setup` writes.
    Applied,
    /// One of them lacks them, as after an install that finished with a
    /// reboot: a repair applies them.
    Missing,
    /// No endpoint of ours is active yet, or a read failed. The supervisor
    /// treats it as fine.
    Unknown,
}

/// The one task an elevated `ssr-setup` run performs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SetupTask {
    Install,
    Uninstall,
    SetFormats { endpoint_mask: u8 },
    RestartDevice,
    Repair,
}

/// The JSON line `ssr-setup` prints when its task succeeded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetupOutcome {
    pub reboot_required: bool,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum HostError {
    #[error("the administrator prompt was declined")]
    Declined,
    #[error("the setup helper failed: {0}")]
    Setup(String),
    #[error("the host refused: {0}")]
    Failed(String),
    #[error("not supported on this host")]
    Unsupported,
}

/// What the supervisor asks the operating system for (ADR 0013). Every call
/// may block briefly; none is called from a real-time thread. The Windows
/// adapter and the test fake are the two adapters.
pub trait Host {
    fn platform(&mut self) -> Platform;

    fn driver(&mut self) -> DriverState;

    /// Every render endpoint, ours included (with `endpoint` set), in any state.
    fn outputs(&mut self) -> Vec<OutputDevice>;

    fn default_roles(&mut self) -> DefaultRoles;

    fn set_default_role(&mut self, role: Role, device: &DeviceId) -> Result<(), HostError>;

    /// Whether `output` accepts `layout` in exclusive mode. `Err(HostError::Unsupported)`
    /// when this host cannot tell; the supervisor then goes ahead.
    fn supports_layout(
        &mut self,
        output: &DeviceId,
        layout: OutputLayout,
    ) -> Result<bool, HostError>;

    /// Runs `ssr-setup` elevated (one UAC prompt) and waits for its one JSON
    /// line.
    fn run_setup(&mut self, task: &SetupTask) -> Result<SetupOutcome, HostError>;

    /// Writes (`Some`) or removes (`None`) the restore point `ssr-guard`
    /// reads (`supervisor::restore_point`).
    fn write_restore_point(&mut self, json: Option<&str>) -> Result<(), HostError>;
}

/// The longest a capture read may wait before the engine checks for a stop
/// request; keeps shutdown prompt without polling.
pub const MAX_CAPTURE_WAIT: Duration = Duration::from_millis(100);

/// Which engine thread `engine::ThreadHook` runs on. `Capture` and `Output`
/// are the real-time threads: they never allocate, lock or block. `Decode`
/// (`ssr-decode`, ADR 0031) is not real-time: it runs the allocating upstream
/// decoders for the capture thread, which never waits for it.
///
/// It lives here, not in `engine`, so `ssr-sys` can provide the hook that
/// registers each thread with MMCSS without depending on the engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineThread {
    Capture,
    Output,
    Decode,
}
