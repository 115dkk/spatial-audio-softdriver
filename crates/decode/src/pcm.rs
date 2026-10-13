//! PCM adapter: interleaved samples to one object per channel at the standard
//! position of its channel-mask bit; LFE goes to the scene's LFE signal.

use bio_types::{PcmFormat, Position, SampleType, SceneBlock, Speaker};

use crate::{DecodeConfig, DecodeError, SceneBuffers};

pub const MAX_PCM_CHANNELS: usize = 32;

/// Position of one `dwChannelMask` speaker bit (`SPEAKER_FRONT_LEFT` = 0x1 ...),
/// or `None` for LFE (0x8) and bits with no standard position.
///
/// Angles (azimuth positive to the left, elevation up): FL 30, FR -30, FC 0,
/// BL 150, BR -150, FLC 15, FRC -15, BC 180, SL 90, SR -90, TC (0, 90),
/// TFL (45, 45), TFC (0, 45), TFR (-45, 45), TBL (135, 45), TBC (180, 45),
/// TBR (-135, 45).
pub fn speaker_position(mask_bit: u32) -> Option<Position> {
    Speaker::from_wave_bit(mask_bit).and_then(Speaker::position)
}

/// The mask Windows assumes when a format gives none: 1 FC; 2 FL FR;
/// 3 FL FR FC; 4 FL FR BL BR; 5 FL FR FC BL BR; 6 FL FR FC LFE BL BR (0x3F);
/// 8 FL FR FC LFE BL BR SL SR (0x63F). Other counts have no default.
pub fn default_mask(channels: u16) -> Option<u32> {
    match channels {
        1 => Some(0x4),
        2 => Some(0x3),
        3 => Some(0x7),
        4 => Some(0x33),
        5 => Some(0x37),
        6 => Some(0x3f),
        8 => Some(0x63f),
        _ => None,
    }
}

#[derive(Debug)]
pub struct PcmDecoder {
    buffers: SceneBuffers,
    format: Option<PcmFormat>,
    direct: crate::direct::DirectLayouts,
    // None is LFE; all other channels index the object buffers.
    channels: [Option<usize>; MAX_PCM_CHANNELS],
    frame: [u8; MAX_PCM_CHANNELS * 4],
    pending: usize,
    frames: usize,
    source_qpc: u64,
    epoch: Option<u32>,
}

impl PcmDecoder {
    pub fn new(config: DecodeConfig) -> Self {
        Self {
            buffers: SceneBuffers::new(config.max_frames),
            format: None,
            direct: config.direct,
            channels: [None; MAX_PCM_CHANNELS],
            frame: [0; MAX_PCM_CHANNELS * 4],
            pending: 0,
            frames: 0,
            source_qpc: 0,
            epoch: None,
        }
    }

    /// Accepts the epoch's format: 1 to 32 channels, 16/24/32-bit integer or
    /// 32-bit float, any rate whose nonempty 10 ms block fits `max_frames`.
    /// Integer valid bits must be in `1..=bits_per_sample` and are left aligned
    /// as in WAVEFORMATEXTENSIBLE; floats require 32 valid bits.
    /// Unknown speaker bits and masks with a different channel count fail.
    /// Failure deselects the old format. Does not allocate, including errors.
    pub fn configure(&mut self, format: PcmFormat) -> Result<(), DecodeError> {
        self.reset();
        self.format = None;
        let invalid = |detail| DecodeError::Unsupported {
            codec: "PCM",
            detail,
        };
        let frames = (format.sample_rate / 100) as usize;
        if !(1..=MAX_PCM_CHANNELS as u16).contains(&format.channels)
            || frames == 0
            || frames > self.buffers.max_frames()
            || !matches!(format.bits_per_sample, 16 | 24 | 32)
            || format.valid_bits == 0
            || format.valid_bits > format.bits_per_sample
            || (format.sample_type == SampleType::Float
                && (format.bits_per_sample != 32 || format.valid_bits != 32))
        {
            return Err(invalid(
                "channels, rate or sample width outside 1-32 ch, 16/24/32-bit int or 32-bit float",
            ));
        }
        let mut objects = 0;
        let mut has_lfe = false;
        if format.channel_mask == 0 {
            let layout = self
                .direct
                .get(format.channels)
                .ok_or(DecodeError::NoDirectLayout(format.channels))?;
            for (channel, &speaker) in layout.speakers().iter().enumerate() {
                self.channels[channel] = if matches!(speaker, Speaker::Lfe | Speaker::Lfe2) {
                    if has_lfe {
                        return Err(invalid("direct layout has more than one LFE channel"));
                    }
                    has_lfe = true;
                    None
                } else {
                    let position = speaker
                        .position()
                        .ok_or_else(|| invalid("direct layout speaker has no position"))?;
                    self.buffers.set_object(objects, position, 1.0, 0.0);
                    objects += 1;
                    Some(objects - 1)
                };
            }
        } else {
            let mask = format.channel_mask;
            if mask.count_ones() != u32::from(format.channels) || mask & !0x3ffff != 0 {
                return Err(invalid("channel mask does not match the channel count"));
            }
            let mut remaining = mask;
            for channel in 0..usize::from(format.channels) {
                let bit = 1 << remaining.trailing_zeros();
                remaining &= !bit;
                self.channels[channel] = if bit == 0x8 {
                    has_lfe = true;
                    None
                } else {
                    let position = speaker_position(bit)
                        .ok_or_else(|| invalid("channel mask has a speaker without a position"))?;
                    self.buffers.set_object(objects, position, 1.0, 0.0);
                    objects += 1;
                    Some(objects - 1)
                };
            }
        }
        self.buffers.set_layout(objects, has_lfe);
        self.format = Some(format);
        Ok(())
    }

    /// Converts `bytes` (may end mid-frame or mid-sample; the remainder is
    /// kept for the next call) and emits a scene for every complete block of
    /// `rate / 100` frames. `qpc` is the arrival time of `bytes`' first byte;
    /// each block's `source_qpc` is the QPC of the record that held the first
    /// byte of its first frame, even when that frame spans records.
    /// Epoch changes discard incomplete blocks. NaNs become silence and all
    /// samples are clamped to `[-1, 1)` (including rounded i32 maxima).
    pub fn push(
        &mut self,
        mut bytes: &[u8],
        qpc: u64,
        epoch: u32,
        out: &mut dyn FnMut(&SceneBlock<'_>),
    ) -> Result<(), DecodeError> {
        let format = self.format.ok_or(DecodeError::NoFormat)?;
        if self.epoch != Some(epoch) {
            self.reset();
            self.epoch = Some(epoch);
        }
        let block = (format.sample_rate / 100) as usize;
        let frame_bytes = format.bytes_per_frame();
        let width = usize::from(format.bits_per_sample / 8);
        while !bytes.is_empty() {
            if self.frames == 0 && self.pending == 0 {
                self.source_qpc = qpc;
            }
            let take = (frame_bytes - self.pending).min(bytes.len());
            self.frame[self.pending..self.pending + take].copy_from_slice(&bytes[..take]);
            self.pending += take;
            bytes = &bytes[take..];
            if self.pending != frame_bytes {
                continue;
            }
            for channel in 0..usize::from(format.channels) {
                let start = channel * width;
                let value = sample(&self.frame[start..start + width], format);
                match self.channels[channel] {
                    Some(object) => self.buffers.signal_mut(object, block)[self.frames] = value,
                    None => self.buffers.lfe_mut(block)[self.frames] = value,
                }
            }
            self.pending = 0;
            self.frames += 1;
            if self.frames == block {
                self.buffers
                    .emit(block, epoch, format.sample_rate, self.source_qpc, out);
                self.frames = 0;
            }
        }
        Ok(())
    }

    /// Emits the final complete PCM frames as a short scene. A trailing partial
    /// interleaved frame is incomplete input and is discarded. Does not allocate.
    pub fn flush(&mut self, out: &mut dyn FnMut(&SceneBlock<'_>)) -> Result<(), DecodeError> {
        let format = self.format.ok_or(DecodeError::NoFormat)?;
        if self.frames != 0 {
            self.buffers.emit(
                self.frames,
                self.epoch.unwrap_or(0),
                format.sample_rate,
                self.source_qpc,
                out,
            );
        }
        self.pending = 0;
        self.frames = 0;
        self.source_qpc = 0;
        Ok(())
    }

    pub fn reset(&mut self) {
        self.pending = 0;
        self.frames = 0;
        self.source_qpc = 0;
        self.epoch = None;
    }
}

fn sample(bytes: &[u8], format: PcmFormat) -> f32 {
    let value = if format.sample_type == SampleType::Float {
        f32::from_le_bytes(bytes.try_into().expect("validated float width"))
    } else {
        let raw = match bytes {
            [a, b] => i32::from(i16::from_le_bytes([*a, *b])),
            [a, b, c] => i32::from_le_bytes([0, *a, *b, *c]) >> 8,
            [a, b, c, d] => i32::from_le_bytes([*a, *b, *c, *d]),
            _ => unreachable!("validated integer width"),
        };
        let signed = raw >> (format.bits_per_sample - format.valid_bits);
        (f64::from(signed) / (1_u64 << (format.valid_bits - 1)) as f64) as f32
    };
    if value.is_nan() {
        0.0
    } else {
        value.clamp(-1.0, f32::from_bits(1.0_f32.to_bits() - 1))
    }
}
