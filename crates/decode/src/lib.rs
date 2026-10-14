#![forbid(unsafe_code)]

//! Source-format adapters. Every adapter writes [`SceneBlock`]s (ADR 0010).
//!
//! | adapter | input | block |
//! | --- | --- | --- |
//! | [`pcm::PcmDecoder`] | interleaved PCM bytes | `rate / 100` frames (10 ms), or the complete frames so far on [`Decoders::emit_buffered`] |
//! | [`mat::MatDecoder`] | MAT bursts | 480 frames (half a 20 ms burst) |
//! | [`ddp::DdpDecoder`] | E-AC-3 bursts | 1536 frames total, split at OAMD updates/ramps |
//!
//! [`Decoders`] holds all of them, created once with every buffer they will
//! need, and switches between them on a format change without allocating.
//! Each adapter lends its own buffers to the scene it emits; the scene is valid
//! until the callback returns.

pub mod ddp;
pub mod direct;
pub mod mat;
pub mod pcm;
mod scene;

use bitstream::Burst;
use ssr_types::{SceneBlock, SourceFormat};

pub use scene::SceneBuffers;

/// Largest supported PCM scene block: 192 kHz x 10 ms.
pub const MAX_SCENE_FRAMES: usize = 1920;

/// Sizes every adapter allocates for up front.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecodeConfig {
    /// Largest scene block any adapter may emit, in frames.
    pub max_frames: usize,
    /// Channel orders used when PCM has a zero WAVE channel mask.
    pub direct: direct::DirectLayouts,
}

impl Default for DecodeConfig {
    fn default() -> Self {
        Self {
            max_frames: MAX_SCENE_FRAMES,
            direct: direct::DirectLayouts::default(),
        }
    }
}

/// Why an adapter refused input. Every variant is `Copy` and carries only
/// static text, so returning one from a real-time push never allocates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DecodeError {
    /// The format-change record names a codec no adapter decodes.
    #[error("no decoder handles {0:?}")]
    UnsupportedCodec(ssr_types::BitstreamCodec),
    /// The stream is valid but uses something this adapter does not support.
    #[error("unsupported {codec} stream: {detail}")]
    Unsupported {
        codec: &'static str,
        detail: &'static str,
    },
    #[error("the {codec} payload is corrupt: {detail}")]
    Corrupt {
        codec: &'static str,
        detail: &'static str,
    },
    /// PCM with channel mask zero whose channel count has no direct layout
    /// ([`direct::DirectLayouts`]); the caller can set one for that count.
    #[error("PCM with channel mask 0 has no direct layout for {0} channels")]
    NoDirectLayout(u16),
    #[error("data arrived before any format was selected")]
    NoFormat,
    /// Data of the wrong kind for the selected route (PCM in a bitstream epoch
    /// or the reverse).
    #[error("{0}")]
    WrongRoute(&'static str),
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
        use ssr_types::BitstreamCodec as C;
        self.epoch = epoch;
        let (active, route) = match format {
            SourceFormat::Pcm(p) => {
                self.pcm.configure(*p)?;
                (Active::Pcm, Route::Pcm)
            }
            SourceFormat::Bitstream(b) => match b.codec {
                C::Mat20 | C::Mat21 if mat::AVAILABLE => (Active::Mat, Route::Bitstream),
                C::Mat20 | C::Mat21 => {
                    self.active = Active::None;
                    return Err(DecodeError::Unsupported {
                        codec: "MAT 2.1",
                        detail: "the decoder is built from ring dumps, none recorded yet",
                    });
                }
                C::Eac3 | C::Eac3Joc => (Active::Ddp, Route::Bitstream),
                other => {
                    self.active = Active::None;
                    return Err(DecodeError::UnsupportedCodec(other));
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
            _ => Err(DecodeError::WrongRoute("PCM data in a bitstream epoch")),
        }
    }

    /// Decodes a complete burst. `qpc` is the source-clock time of its first
    /// carrier byte (the IEC 61937 Pa preamble at [`Burst::start`]), not the
    /// payload completion time or the enclosing ABI record's end timestamp.
    /// The adapter carries this timestamp into the emitted scene.
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
            Active::Pcm => Err(DecodeError::WrongRoute("a burst in a PCM epoch")),
        }
    }

    /// Emits the complete PCM frames received so far as a short scene (see
    /// [`pcm::PcmDecoder::emit_buffered`]); burst decoders emit whole bursts and
    /// hold nothing to emit. Does not allocate.
    pub fn emit_buffered(&mut self, out: &mut dyn FnMut(&SceneBlock<'_>)) {
        if self.active == Active::Pcm {
            self.pcm.emit_buffered(out);
        }
    }

    /// Emits buffered complete input at an orderly stream end. Only PCM holds
    /// a sub-block remainder; burst decoders have already emitted complete bursts.
    pub fn flush(&mut self, out: &mut dyn FnMut(&SceneBlock<'_>)) -> Result<(), DecodeError> {
        match self.active {
            Active::Pcm => self.pcm.flush(out),
            Active::Mat | Active::Ddp | Active::None => Ok(()),
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
