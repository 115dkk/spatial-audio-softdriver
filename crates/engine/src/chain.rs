//! The DSP chain: records → (deframe) → decode → render → convolve → gain →
//! limiter. Runs on whatever thread pushes records: the replay tool's only
//! thread, or the live capture thread (M2). Never allocates while pushing.

use std::sync::Arc;

use bio_abi::{AbiError, RECORD_FLAG_DISCONTINUITY, Record};
use bio_types::{BitstreamCodec, Feeds, SceneBlock, SourceFormat};
use brir::PreparedBrir;
use convolver::Binauralizer;
use decode::{DecodeConfig, DecodeError, Decoders, Route};
use render::{Panner, PannerConfig};

use crate::limiter::Limiter;

/// Sizes fixed when the chain is built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChainConfig {
    /// Largest scene block in frames (DD+: 1536).
    pub max_block: usize,
}

impl Default for ChainConfig {
    fn default() -> Self {
        Self { max_block: 1536 }
    }
}

/// The partition size a stream whose blocks are `block` frames long should be
/// prepared with: `block` itself up to 512, else its largest divisor not above
/// 512 (1536 → 512). Blocks are then always whole partitions, so the
/// convolver adds no latency.
pub fn partition_for(block: usize) -> usize {
    if block == 0 {
        return 0;
    }
    if block <= 512 {
        return block;
    }
    (1..=512)
        .rev()
        .find(|candidate| block.is_multiple_of(*candidate))
        .expect("one divides every positive block")
}

/// The scene rate and block length a format produces, which decide the BRIR
/// rate and partition the control thread must prepare before data arrives:
/// PCM at its own rate in 10 ms blocks, MAT at 48 kHz in 480-frame half
/// bursts, DD+ at its encoded rate (48 kHz when unknown) in 1536-frame access
/// units. `None` for formats no adapter decodes.
pub fn scene_shape(format: &SourceFormat) -> Option<(u32, usize)> {
    match format {
        SourceFormat::Pcm(pcm) => Some((pcm.sample_rate, (pcm.sample_rate / 100) as usize)),
        SourceFormat::Bitstream(stream) => match stream.codec {
            BitstreamCodec::Mat20 | BitstreamCodec::Mat21 => Some((48_000, 480)),
            BitstreamCodec::Eac3 | BitstreamCodec::Eac3Atmos => Some((
                if stream.encoded_rate == 0 {
                    48_000
                } else {
                    stream.encoded_rate
                },
                1536,
            )),
            _ => None,
        },
    }
}

/// A record or payload the chain could not use. The chain resets the affected
/// stage and goes on. `Copy`-cheap for decode errors, so reporting one from the
/// real-time thread does not allocate.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChainError {
    #[error(transparent)]
    Decode(#[from] DecodeError),
    #[error(transparent)]
    Record(#[from] AbiError),
}

#[derive(Debug, thiserror::Error)]
pub enum PrepareError {
    #[error(transparent)]
    Render(#[from] render::RenderError),
    #[error("max block {max_block} is not a multiple of the BRIR partition {partition}")]
    Block { max_block: usize, partition: usize },
}

/// A BRIR set with everything built from it: panner, convolver state and feed
/// buffers. Built on the control thread and handed to the chain in one box,
/// so swapping profiles is a pointer swap (the old box goes back to the
/// control thread to be dropped there).
#[derive(Debug)]
pub struct Prepared {
    pub brir: Arc<PreparedBrir>,
    max_block: usize,
    panner: Panner,
    binauralizer: Binauralizer,
    feeds: Feeds,
}

impl Prepared {
    /// Allocates for blocks up to `max_block` frames (a multiple of
    /// `brir.partition`).
    pub fn new(brir: Arc<PreparedBrir>, max_block: usize) -> Result<Self, PrepareError> {
        if !max_block.is_multiple_of(brir.partition) {
            return Err(PrepareError::Block {
                max_block,
                partition: brir.partition,
            });
        }
        let panner = Panner::new(
            &brir.layout,
            PannerConfig {
                lfe_route: brir.lfe,
                ramp_frames: 64,
            },
        )?;
        let binauralizer = Binauralizer::new(Arc::clone(&brir.bank), max_block);
        let feeds = Feeds::new(brir.layout.len(), max_block);
        Ok(Self {
            brir,
            max_block,
            panner,
            binauralizer,
            feeds,
        })
    }
}

/// One binaural block.
#[derive(Clone, Copy, Debug)]
pub struct BlockOut<'a> {
    pub epoch: u32,
    pub rate: u32,
    /// QPC of the first source sample (from the scene), for latency.
    pub source_qpc: u64,
    pub left: &'a [f32],
    pub right: &'a [f32],
}

#[derive(Debug)]
pub enum ChainEvent<'a> {
    Block(BlockOut<'a>),
    FormatChanged {
        epoch: u32,
        format: SourceFormat,
    },
    /// The prepared BRIR does not fit the scene (another rate, or a block that
    /// is not a whole number of partitions), or none is installed. Blocks are
    /// dropped until [`Chain::set_prepared`] supplies one that fits. Emitted
    /// once per change of (rate, block), not once per dropped block.
    NeedsBrir {
        rate: u32,
        block: usize,
    },
    Overrun {
        lost_bytes: u64,
    },
    Error(ChainError),
}

#[derive(Debug)]
pub struct Chain {
    config: ChainConfig,
    deframer: bitstream::Deframer,
    decoders: Decoders,
    prepared: Option<Box<Prepared>>,
    limiter: Limiter,
    left: Vec<f32>,
    right: Vec<f32>,
    route: Option<Route>,
    last_needs_brir: Option<(u32, usize)>,
}

impl Chain {
    /// Allocates every buffer the chain will use; no BRIR yet.
    pub fn new(config: ChainConfig) -> Self {
        Self {
            config,
            deframer: bitstream::Deframer::new(),
            decoders: Decoders::new(DecodeConfig {
                max_frames: config.max_block,
            }),
            prepared: None,
            limiter: Limiter::new(),
            left: vec![0.0; config.max_block],
            right: vec![0.0; config.max_block],
            route: None,
            last_needs_brir: None,
        }
    }

    /// Installs a prepared BRIR set and returns the previous one.
    pub fn set_prepared(&mut self, mut prepared: Box<Prepared>) -> Option<Box<Prepared>> {
        prepared.panner.reset();
        prepared.binauralizer.reset();
        self.limiter.reset(
            prepared.brir.rate,
            prepared.brir.gain,
            prepared.brir.limiter_ceiling,
        );
        self.last_needs_brir = None;
        self.prepared.replace(prepared)
    }

    /// Feeds one record.
    pub fn push_record(&mut self, record: Record<'_>, out: &mut dyn FnMut(ChainEvent<'_>)) {
        match record {
            Record::FormatChange { epoch, format, .. } => {
                self.deframer.reset();
                match self.decoders.select(&format, epoch) {
                    Ok(route) => self.route = Some(route),
                    Err(error) => {
                        self.route = None;
                        out(ChainEvent::Error(ChainError::Decode(error)));
                    }
                }
                out(ChainEvent::FormatChanged { epoch, format });
            }
            Record::Data {
                flags, bytes, qpc, ..
            } => {
                if flags & RECORD_FLAG_DISCONTINUITY != 0 {
                    self.deframer.reset();
                    self.decoders.reset();
                }
                match self.route {
                    Some(Route::Pcm) => self.push_pcm(bytes, qpc, out),
                    Some(Route::Bitstream) => self.push_bitstream(bytes, qpc, out),
                    None => {}
                }
            }
            Record::Overrun { lost_bytes, .. } => {
                self.deframer.reset();
                self.decoders.reset();
                out(ChainEvent::Overrun { lost_bytes });
            }
            Record::StreamStart { .. } | Record::StreamStop { .. } => {
                self.deframer.reset();
                self.decoders.reset();
            }
        }
    }

    /// Feeds a buffer of whole records (a READ result or a dump's stream).
    pub fn push_records(&mut self, stream: &[u8], out: &mut dyn FnMut(ChainEvent<'_>)) {
        for record in bio_abi::records(stream) {
            match record {
                Ok(record) => self.push_record(record, out),
                Err(error) => out(ChainEvent::Error(ChainError::Record(error))),
            }
        }
    }

    fn push_pcm(&mut self, bytes: &[u8], qpc: u64, out: &mut dyn FnMut(ChainEvent<'_>)) {
        let Self {
            config,
            decoders,
            prepared,
            limiter,
            left,
            right,
            last_needs_brir,
            ..
        } = self;
        let result = {
            let mut scenes = |scene: &SceneBlock<'_>| {
                process_scene(
                    *config,
                    prepared,
                    limiter,
                    left,
                    right,
                    last_needs_brir,
                    scene,
                    out,
                );
            };
            decoders.push_pcm(bytes, qpc, &mut scenes)
        };
        if let Err(error) = result {
            decoders.reset();
            out(ChainEvent::Error(ChainError::Decode(error)));
        }
    }

    fn push_bitstream(&mut self, bytes: &[u8], qpc: u64, out: &mut dyn FnMut(ChainEvent<'_>)) {
        let Self {
            config,
            deframer,
            decoders,
            prepared,
            limiter,
            left,
            right,
            last_needs_brir,
            ..
        } = self;
        let mut bursts = |burst: bitstream::Burst<'_>| {
            let result = {
                let mut scenes = |scene: &SceneBlock<'_>| {
                    process_scene(
                        *config,
                        prepared,
                        limiter,
                        left,
                        right,
                        last_needs_brir,
                        scene,
                        out,
                    );
                };
                // A record can contain part or all of several bursts. The wire
                // contract has no timestamp per burst, so every burst completed
                // from this record uses the record's QPC as an approximation.
                decoders.push_burst(&burst, qpc, &mut scenes)
            };
            if let Err(error) = result {
                decoders.reset();
                out(ChainEvent::Error(ChainError::Decode(error)));
            }
        };
        deframer.push(bytes, &mut bursts);
    }
}

#[allow(clippy::too_many_arguments)]
fn process_scene(
    config: ChainConfig,
    prepared: &mut Option<Box<Prepared>>,
    limiter: &mut Limiter,
    left: &mut [f32],
    right: &mut [f32],
    last_needs_brir: &mut Option<(u32, usize)>,
    scene: &SceneBlock<'_>,
    out: &mut dyn FnMut(ChainEvent<'_>),
) {
    let fits = prepared.as_ref().is_some_and(|prepared| {
        scene.rate == prepared.brir.rate
            && scene.frames <= config.max_block
            && scene.frames <= prepared.max_block
            && scene.frames.is_multiple_of(prepared.brir.partition)
    });
    if !fits {
        let shape = (scene.rate, scene.frames);
        if *last_needs_brir != Some(shape) {
            *last_needs_brir = Some(shape);
            out(ChainEvent::NeedsBrir {
                rate: shape.0,
                block: shape.1,
            });
        }
        return;
    }

    let prepared = prepared.as_mut().expect("fit requires a prepared BRIR");
    let frames = scene.frames;
    prepared.panner.render(scene, &mut prepared.feeds);
    prepared
        .binauralizer
        .process(&prepared.feeds, &mut left[..frames], &mut right[..frames]);
    limiter.process(&mut left[..frames], &mut right[..frames]);
    out(ChainEvent::Block(BlockOut {
        epoch: scene.epoch,
        rate: scene.rate,
        source_qpc: scene.source_qpc,
        left: &left[..frames],
        right: &right[..frames],
    }));
}

#[cfg(test)]
mod tests {
    use bio_types::{BitstreamFormat, PcmFormat, SampleType};

    use super::*;

    fn bitstream(codec: BitstreamCodec, encoded_rate: u32) -> SourceFormat {
        SourceFormat::Bitstream(BitstreamFormat {
            codec,
            container_rate: 192_000,
            container_channels: 8,
            encoded_rate,
            encoded_channels: 0,
        })
    }

    #[test]
    fn partition_for_uses_the_largest_divisor_at_most_512() {
        for (block, expected) in [(0, 0), (1536, 512), (480, 480), (441, 441), (513, 171)] {
            assert_eq!(partition_for(block), expected);
        }
    }

    #[test]
    fn scene_shape_matches_decoder_output() {
        let pcm = SourceFormat::Pcm(PcmFormat {
            channels: 2,
            sample_rate: 44_100,
            bits_per_sample: 32,
            valid_bits: 32,
            sample_type: SampleType::Float,
            channel_mask: 3,
        });
        assert_eq!(scene_shape(&pcm), Some((44_100, 441)));
        assert_eq!(
            scene_shape(&bitstream(BitstreamCodec::Mat20, 96_000)),
            Some((48_000, 480))
        );
        assert_eq!(
            scene_shape(&bitstream(BitstreamCodec::Mat21, 0)),
            Some((48_000, 480))
        );
        assert_eq!(
            scene_shape(&bitstream(BitstreamCodec::Eac3, 0)),
            Some((48_000, 1536))
        );
        assert_eq!(
            scene_shape(&bitstream(BitstreamCodec::Eac3Atmos, 44_100)),
            Some((44_100, 1536))
        );
        assert_eq!(scene_shape(&bitstream(BitstreamCodec::Ac3, 48_000)), None);
        assert_eq!(
            scene_shape(&bitstream(BitstreamCodec::TrueHd, 48_000)),
            None
        );
    }
}
