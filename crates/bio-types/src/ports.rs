//! Port traits: the seams the engine and the supervisor are wired through.
//!
//! The traits live here, not in `engine`, so `bio-sys` can implement them
//! without depending on the engine. Each has more than one adapter:
//!
//! | seam | adapters |
//! | --- | --- |
//! | [`CaptureSource`] | driver ring, SAL ring (development only), dump file |
//! | [`OutputOpener`] / [`OutputSink`] | WASAPI, WAV file, null |
//! | [`Host`] | Windows (`bio-sys`), fake (tests) |

use std::time::Duration;

/// Where the engine reads the driver's record stream from.
///
/// [`read`](CaptureSource::read) is the only call in the engine allowed to
/// block, and only the capture thread makes it.
pub trait CaptureSource: Send + 'static {
    /// Fills `buf` with whole records (the `bio_abi` wire format) and returns
    /// the number of bytes written. Waits at most `wait` for data; `Ok(0)` means
    /// the wait ran out.
    fn read(&mut self, buf: &mut [u8], wait: Duration) -> Result<usize, CaptureError>;

    /// Asks the source to produce `ppm` parts per million faster (negative:
    /// slower). Adapters without a rate control (dump, SAL ring) refuse, and the
    /// engine falls back to resampling.
    fn set_rate_trim(&mut self, ppm: f64) -> Result<(), TrimUnsupported>;
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

/// The shape of the periods an output sink asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutputFormat {
    pub rate: u32,
    /// Always 2 today (the binaural signal); kept explicit for sinks that pad.
    pub channels: u16,
    /// Frames per period the device asks for at a time.
    pub period_frames: usize,
}

/// Opens an output sink on the thread that will use it.
///
/// WASAPI objects belong to the thread that created them, so the engine moves
/// the opener to its output thread and opens there. `open` errors still reach
/// `engine::start` as a start error.
pub trait OutputOpener: Send + 'static {
    type Sink: OutputSink;

    fn open(self) -> Result<Self::Sink, OutputError>;
}

/// An open output device, used only from the thread that opened it.
pub trait OutputSink {
    fn format(&self) -> OutputFormat;

    /// Waits until the device wants the next period, lets `fill` write it
    /// (interleaved, `period_frames * channels` samples), and returns the QPC at
    /// which the period's first frame will play.
    fn next_period(&mut self, fill: &mut dyn FnMut(&mut [f32])) -> Result<u64, OutputError>;
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OutputError {
    #[error("no output device matches the selection")]
    NotFound,
    #[error("the output device was removed")]
    Removed,
    #[error("the output device rejected every format tried: {0}")]
    FormatRejected(String),
    #[error("the output device is in exclusive use by another application")]
    Busy,
    #[error("output failed: {0}")]
    Io(String),
}

/// What the supervisor asks the operating system for (draft; M4 settles it).
///
/// The Windows adapter lives in `bio-sys::host`; tests use a fake. Kept minimal
/// until the supervisor exists, so the shape follows its needs rather than a
/// guess.
pub trait Host {
    type Device: Clone + std::fmt::Debug;

    /// Render endpoints that can play the binaural signal.
    fn outputs(&self) -> Vec<Self::Device>;
}

/// The longest a capture read may wait before the engine checks for a stop
/// request; keeps shutdown prompt without polling.
pub const MAX_CAPTURE_WAIT: Duration = Duration::from_millis(100);
