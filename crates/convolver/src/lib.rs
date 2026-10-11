#![forbid(unsafe_code)]

//! Direction feeds to binaural signal: uniformly partitioned frequency-domain
//! convolution, a Rust rewrite of EqualizerAPO-XT's MultiConvolution
//! (Copyright 2026 115dkk, MIT; ADR 0006).
//!
//! Each input is transformed once per partition-sized hop; for each ear the
//! spectra of all inputs are multiplied with the matching filter partitions
//! and accumulated in a frequency-domain delay line, so a block costs one
//! forward FFT per input and one inverse FFT per ear and hop. Blocks must be a
//! multiple of the partition size, which `engine` chooses as a divisor of the
//! stream's block, so no latency is added.

use std::sync::Arc;

use bio_types::Feeds;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ConvolverError {
    #[error("a filter bank needs at least one input")]
    NoInputs,
    #[error("input {0} has an empty impulse response")]
    EmptyIr(usize),
    #[error("partition size {0} must be even and at least 16")]
    BadPartition(usize),
}

/// The filters of every input and ear, cut into partitions and transformed.
/// Built once on the control thread; shared read-only by the real-time thread.
#[derive(Debug)]
pub struct FilterBank {
    // Implementation detail: spectra laid out [input][ear][partition][bin].
}

impl FilterBank {
    /// `irs[i] = [left, right]` impulse responses of input `i` (the two ears
    /// may differ in length; both are zero-padded to the longer).
    pub fn new(irs: &[[&[f32]; 2]], partition: usize) -> Result<Self, ConvolverError> {
        let _ = (irs, partition);
        todo!("M1: convolver worker")
    }

    pub fn inputs(&self) -> usize {
        todo!("M1: convolver worker")
    }

    pub fn partition(&self) -> usize {
        todo!("M1: convolver worker")
    }

    /// Partitions per filter (`ceil(longest IR / partition)`).
    pub fn partitions(&self) -> usize {
        todo!("M1: convolver worker")
    }
}

/// Streaming state for one bank: input history and the delay line.
#[derive(Debug)]
pub struct Binauralizer {
    // Implementation detail: FFT plans, scratch, delay lines sized in `new`.
}

impl Binauralizer {
    /// Allocates everything `process` needs for blocks up to `max_block`
    /// frames (a multiple of the bank's partition).
    pub fn new(bank: Arc<FilterBank>, max_block: usize) -> Self {
        let _ = (bank, max_block);
        todo!("M1: convolver worker")
    }

    pub fn bank(&self) -> &Arc<FilterBank> {
        todo!("M1: convolver worker")
    }

    /// Convolves `feeds` (one channel per bank input, `feeds.frames()` a
    /// multiple of the partition and at most `max_block`) and writes the ear
    /// sums into `left` and `right` (each `feeds.frames()` long, overwritten).
    /// Never allocates.
    pub fn process(&mut self, feeds: &Feeds, left: &mut [f32], right: &mut [f32]) {
        let _ = (feeds, left, right);
        todo!("M1: convolver worker")
    }

    /// Zeroes the history, as after a format change.
    pub fn reset(&mut self) {
        todo!("M1: convolver worker")
    }
}
