//! MAT 2.1 adapter (clean room, ADR 0005).
//!
//! Windows spatial sound packs 11 fixed channels and 20 dynamic objects as
//! uncompressed PCM, with a position table, into 20 ms MAT 2.1 bursts of
//! 61,440 bytes. The layout inside the burst is not public, and the only
//! working implementation (SpatialAudioLab) may not be read, so this adapter is
//! written from ring dumps observed as a black box. Until dumps exist it
//! refuses every burst; `features.toml` keeps `decode.mat` planned.

use bio_types::SceneBlock;
use bitstream::Burst;

use crate::{DecodeConfig, DecodeError};

#[derive(Debug)]
pub struct MatDecoder {
    _config: DecodeConfig,
}

impl MatDecoder {
    pub fn new(config: DecodeConfig) -> Self {
        Self { _config: config }
    }

    pub fn push(
        &mut self,
        _burst: &Burst<'_>,
        _qpc: u64,
        _epoch: u32,
        _out: &mut dyn FnMut(&SceneBlock<'_>),
    ) -> Result<(), DecodeError> {
        Err(DecodeError::Unsupported(
            "MAT 2.1 (the decoder is built from ring dumps, none recorded yet)".into(),
        ))
    }

    pub fn reset(&mut self) {}
}
