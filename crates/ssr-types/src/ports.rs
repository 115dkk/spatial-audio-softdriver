//! Port traits: the seams the engine and the supervisor are wired through.
//!
//! The traits live here, not in `engine`, so `ssr-sys` can implement them
//! without depending on the engine. The first two have more than one adapter;
//! [`Host`] is a draft with none yet:
//!
//! | seam | adapters |
//! | --- | --- |
//! | [`CaptureSource`] | driver ring and SAL ring (`ssr-sys`), paced and looping dump file (`engine::adapters`), test fakes |
//! | [`OutputOpener`] / [`OutputSink`] | WASAPI (`ssr-sys`), WAV file and null (`engine::adapters`), test fakes |
//! | [`Host`] | none until the supervisor (M4) needs one |

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
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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

/// What the supervisor asks the operating system for (draft; M4 settles it).
///
/// Nothing implements it yet: the Windows adapter and a test fake come with
/// the supervisor, so the shape follows its needs rather than a guess.
pub trait Host {
    type Device: Clone + std::fmt::Debug;

    /// Render endpoints that can play the binaural signal.
    fn outputs(&self) -> Vec<Self::Device>;
}

/// The longest a capture read may wait before the engine checks for a stop
/// request; keeps shutdown prompt without polling.
pub const MAX_CAPTURE_WAIT: Duration = Duration::from_millis(100);
