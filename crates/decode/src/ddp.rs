//! DD+ (E-AC-3) adapter, with JOC objects when the stream carries them.
//! Wraps harletty-bridge's `eac3` crate (Apache-2.0, ADR 0007).

use bio_types::SceneBlock;
use bitstream::Burst;

use crate::{DecodeConfig, DecodeError, SceneBuffers};

#[derive(Debug)]
pub struct DdpDecoder {
    buffers: SceneBuffers,
}

impl DdpDecoder {
    pub fn new(config: DecodeConfig) -> Self {
        Self {
            buffers: SceneBuffers::new(config.max_frames),
        }
    }

    /// Decodes the E-AC-3 syncframes in one burst payload and emits one scene
    /// per access unit (1536 frames): bed channels as objects at their
    /// standard positions, JOC objects at the positions their OAMD gives.
    pub fn push(
        &mut self,
        burst: &Burst<'_>,
        qpc: u64,
        epoch: u32,
        out: &mut dyn FnMut(&SceneBlock<'_>),
    ) -> Result<(), DecodeError> {
        let _ = (burst, qpc, epoch, out, &self.buffers);
        todo!("M1: DD+ worker")
    }

    pub fn reset(&mut self) {
        todo!("M1: DD+ worker")
    }
}
