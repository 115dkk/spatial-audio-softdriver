#![forbid(unsafe_code)]

//! Source-format adapters. Every adapter writes [`SceneBlock`]s (ADR 0010).
//!
//! | adapter | input | block |
//! | --- | --- | --- |
//! | [`pcm::PcmDecoder`] | interleaved PCM bytes | `rate / 100` frames (10 ms) |
//! | [`mat::MatDecoder`] | MAT bursts | 480 frames (half a 20 ms burst) |
//! | [`ddp::DdpDecoder`] | E-AC-3 bursts | 1536 frames (one access unit) |
//!
//! [`Decoders`] holds all of them, created once with every buffer they will
//! need, and switches between them on a format change without allocating.
//! Each adapter lends its own buffers to the scene it emits; the scene is valid
//! until the callback returns.

pub mod ddp;
pub mod mat;
pub mod pcm;
mod scene;

use bio_types::{SceneBlock, SourceFormat};
use bitstream::Burst;

pub use scene::SceneBuffers;

/// Sizes every adapter allocates for up front.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecodeConfig {
    /// Largest scene block any adapter may emit, in frames.
    pub max_frames: usize,
}

impl Default for DecodeConfig {
    fn default() -> Self {
        // DD+ access units are the largest block: 6 audio blocks x 256.
        Self { max_frames: 1536 }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DecodeError {
    #[error("no decoder handles {0}")]
    Unsupported(String),
    #[error("the {codec} payload is corrupt: {detail}")]
    Corrupt { codec: &'static str, detail: String },
    #[error("no format selected yet")]
    NoFormat,
}

/// How the engine feeds the selected adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    /// Data records go straight to [`Decoders::push_pcm`].
    Pcm,
    /// Data records go through `bitstream::Deframer`, bursts to
    /// [`Decoders::push_burst`].
    Bitstream,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Active {
    None,
    Pcm,
    Mat,
    Ddp,
}

/// Every adapter, preallocated; one is active at a time.
#[derive(Debug)]
pub struct Decoders {
    pcm: pcm::PcmDecoder,
    mat: mat::MatDecoder,
    ddp: ddp::DdpDecoder,
    active: Active,
    epoch: u32,
}

impl Decoders {
    pub fn new(config: DecodeConfig) -> Self {
        Self {
            pcm: pcm::PcmDecoder::new(config),
            mat: mat::MatDecoder::new(config),
            ddp: ddp::DdpDecoder::new(config),
            active: Active::None,
            epoch: 0,
        }
    }

    /// Activates the adapter for `format` (a format-change record), resetting
    /// it. Does not allocate.
    pub fn select(&mut self, format: &SourceFormat, epoch: u32) -> Result<Route, DecodeError> {
        use bio_types::BitstreamCodec as C;
        self.epoch = epoch;
        let (active, route) = match format {
            SourceFormat::Pcm(p) => {
                self.pcm.configure(*p)?;
                (Active::Pcm, Route::Pcm)
            }
            SourceFormat::Bitstream(b) => match b.codec {
                C::Mat20 | C::Mat21 => (Active::Mat, Route::Bitstream),
                C::Eac3 | C::Eac3Atmos => (Active::Ddp, Route::Bitstream),
                other => {
                    self.active = Active::None;
                    return Err(DecodeError::Unsupported(format!("{other:?}")));
                }
            },
        };
        self.active = active;
        self.reset();
        Ok(route)
    }

    pub fn push_pcm(
        &mut self,
        bytes: &[u8],
        qpc: u64,
        out: &mut dyn FnMut(&SceneBlock<'_>),
    ) -> Result<(), DecodeError> {
        match self.active {
            Active::Pcm => self.pcm.push(bytes, qpc, self.epoch, out),
            Active::None => Err(DecodeError::NoFormat),
            _ => Err(DecodeError::Unsupported(
                "PCM data in a bitstream epoch".into(),
            )),
        }
    }

    pub fn push_burst(
        &mut self,
        burst: &Burst<'_>,
        qpc: u64,
        out: &mut dyn FnMut(&SceneBlock<'_>),
    ) -> Result<(), DecodeError> {
        match self.active {
            Active::Mat => self.mat.push(burst, qpc, self.epoch, out),
            Active::Ddp => self.ddp.push(burst, qpc, self.epoch, out),
            Active::None => Err(DecodeError::NoFormat),
            Active::Pcm => Err(DecodeError::Unsupported("a burst in a PCM epoch".into())),
        }
    }

    /// Drops buffered state of the active adapter (overrun, discontinuity).
    pub fn reset(&mut self) {
        match self.active {
            Active::Pcm => self.pcm.reset(),
            Active::Mat => self.mat.reset(),
            Active::Ddp => self.ddp.reset(),
            Active::None => {}
        }
    }
}
