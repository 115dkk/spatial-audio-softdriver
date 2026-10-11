//! PCM adapter: interleaved samples to one object per channel at the standard
//! position of its channel-mask bit; LFE goes to the scene's LFE signal.

use bio_types::{PcmFormat, Position, SceneBlock};

use crate::{DecodeConfig, DecodeError, SceneBuffers};

/// Position of one `dwChannelMask` speaker bit (`SPEAKER_FRONT_LEFT` = 0x1 ...),
/// or `None` for LFE (0x8) and bits with no standard position.
///
/// Angles (azimuth positive to the left, elevation up): FL 30, FR -30, FC 0,
/// BL 150, BR -150, FLC 15, FRC -15, BC 180, SL 90, SR -90, TC (0, 90),
/// TFL (45, 45), TFC (0, 45), TFR (-45, 45), TBL (135, 45), TBC (180, 45),
/// TBR (-135, 45).
pub fn speaker_position(mask_bit: u32) -> Option<Position> {
    let _ = mask_bit;
    todo!("M1: PCM worker")
}

/// The mask Windows assumes when a format gives none: 1 FC; 2 FL FR;
/// 3 FL FR FC; 4 FL FR BL BR; 5 FL FR FC BL BR; 6 FL FR FC LFE BL BR (0x3F);
/// 8 FL FR FC LFE BL BR SL SR (0x63F). Other counts have no default.
pub fn default_mask(channels: u16) -> Option<u32> {
    let _ = channels;
    todo!("M1: PCM worker")
}

#[derive(Debug)]
pub struct PcmDecoder {
    buffers: SceneBuffers,
}

impl PcmDecoder {
    pub fn new(config: DecodeConfig) -> Self {
        Self {
            buffers: SceneBuffers::new(config.max_frames),
        }
    }

    /// Accepts the epoch's format: 1 to 8 channels, 16/24/32-bit integer or
    /// 32-bit float, any rate whose 10 ms block fits `max_frames`. Channels
    /// beyond the mask's set bits, or a mask with more bits than channels, are
    /// errors. Does not allocate.
    pub fn configure(&mut self, format: PcmFormat) -> Result<(), DecodeError> {
        let _ = format;
        todo!("M1: PCM worker")
    }

    /// Converts `bytes` (may end mid-frame or mid-sample; the remainder is
    /// kept for the next call) and emits a scene for every complete block of
    /// `rate / 100` frames. `qpc` is the arrival time of `bytes`' first byte;
    /// each block's `source_qpc` is the QPC of the record that held its first
    /// frame.
    pub fn push(
        &mut self,
        bytes: &[u8],
        qpc: u64,
        epoch: u32,
        out: &mut dyn FnMut(&SceneBlock<'_>),
    ) -> Result<(), DecodeError> {
        let _ = (bytes, qpc, epoch, out, &self.buffers);
        todo!("M1: PCM worker")
    }

    pub fn reset(&mut self) {
        todo!("M1: PCM worker")
    }
}
