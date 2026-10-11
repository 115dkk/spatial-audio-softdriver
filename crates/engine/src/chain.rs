//! The DSP chain: records → (deframe) → decode → render → convolve → gain →
//! limiter. Runs on whatever thread pushes records: the replay tool's only
//! thread, or the live capture thread (M2). Never allocates while pushing.

use std::sync::Arc;

use bio_abi::Record;
use bio_types::SourceFormat;
use brir::PreparedBrir;
use convolver::Binauralizer;
use render::Panner;

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
    let _ = block;
    todo!("M1: chain worker")
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
    // Implementation detail: Panner, Binauralizer, Feeds, scratch.
    _panner: Option<Panner>,
    _binauralizer: Option<Binauralizer>,
}

impl Prepared {
    /// Allocates for blocks up to `max_block` frames (a multiple of
    /// `brir.partition`).
    pub fn new(brir: Arc<PreparedBrir>, max_block: usize) -> Result<Self, PrepareError> {
        let _ = (brir, max_block);
        todo!("M1: chain worker")
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
    /// is not a whole number of partitions). Blocks are dropped until
    /// [`Chain::set_prepared`] supplies one that fits.
    NeedsBrir {
        rate: u32,
        block: usize,
    },
    Overrun {
        lost_bytes: u64,
    },
    /// A record or payload the chain could not use; the chain resets the
    /// affected stage and goes on.
    Error(String),
}

#[derive(Debug)]
pub struct Chain {
    // Implementation detail: Deframer, Decoders, Option<Box<Prepared>>,
    // limiter state, output buffers, current epoch and route.
    _config: ChainConfig,
}

impl Chain {
    /// Allocates every buffer the chain will use; no BRIR yet.
    pub fn new(config: ChainConfig) -> Self {
        let _ = config;
        todo!("M1: chain worker")
    }

    /// Installs a prepared BRIR set and returns the previous one.
    pub fn set_prepared(&mut self, prepared: Box<Prepared>) -> Option<Box<Prepared>> {
        let _ = prepared;
        todo!("M1: chain worker")
    }

    /// Feeds one record.
    pub fn push_record(&mut self, record: Record<'_>, out: &mut dyn FnMut(ChainEvent<'_>)) {
        let _ = (record, out);
        todo!("M1: chain worker")
    }

    /// Feeds a buffer of whole records (a READ result or a dump's stream).
    pub fn push_records(&mut self, stream: &[u8], out: &mut dyn FnMut(ChainEvent<'_>)) {
        let _ = (stream, out);
        todo!("M1: chain worker")
    }
}
