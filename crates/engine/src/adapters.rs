//! Port adapters that need no Windows.
//!
//! * [`DumpCapture`]: a [`CaptureSource`] over a dump's record stream.
//! * [`WavOpener`] / `WavSink`: an output that writes a 32-bit float stereo
//!   WAV, reporting a synthetic playback QPC.
//! * [`NullOpener`] / `NullSink`: an output that discards.

use std::path::PathBuf;
use std::time::Duration;

use bio_types::{CaptureError, CaptureSource, TrimUnsupported};

/// Replays a dump's record stream, handing out whole records only.
#[derive(Debug)]
pub struct DumpCapture {
    stream: Vec<u8>,
    offset: usize,
}

impl DumpCapture {
    /// `stream` is the record stream after the dump header
    /// (`bio_abi::split_dump`).
    pub fn new(stream: Vec<u8>) -> Self {
        Self { stream, offset: 0 }
    }
}

impl CaptureSource for DumpCapture {
    /// Copies as many whole records as fit in `buf`; `Err(Ended)` once the
    /// stream is exhausted. A record larger than `buf` is an error.
    fn read(&mut self, buf: &mut [u8], wait: Duration) -> Result<usize, CaptureError> {
        let _ = (buf, wait, &self.stream, self.offset);
        todo!("M1: chain worker")
    }

    fn set_rate_trim(&mut self, _ppm: f64) -> Result<(), TrimUnsupported> {
        Err(TrimUnsupported)
    }
}

/// Opens a [`WavSink`](WavOpener::Sink) writing `path` at `rate` with
/// `period_frames` per period.
#[derive(Debug, Clone)]
pub struct WavOpener {
    pub path: PathBuf,
    pub rate: u32,
    pub period_frames: usize,
}

/// Opens a sink that discards everything, at `rate` with `period_frames`.
#[derive(Debug, Clone, Copy)]
pub struct NullOpener {
    pub rate: u32,
    pub period_frames: usize,
}
