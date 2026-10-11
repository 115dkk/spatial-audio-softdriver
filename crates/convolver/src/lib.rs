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

use std::fmt;
use std::sync::Arc;

use bio_types::Feeds;
use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};

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
    inputs: usize,
    partition: usize,
    partitions: usize,
    bins: usize,
    // [input][ear][partition][bin]
    spectra: Vec<Complex<f32>>,
}

impl FilterBank {
    /// `irs[i] = [left, right]` impulse responses of input `i` (the two ears
    /// may differ in length; both are zero-padded to the longer).
    pub fn new(irs: &[[&[f32]; 2]], partition: usize) -> Result<Self, ConvolverError> {
        if irs.is_empty() {
            return Err(ConvolverError::NoInputs);
        }
        if partition < 16 || !partition.is_multiple_of(2) {
            return Err(ConvolverError::BadPartition(partition));
        }

        let mut longest = 0;
        for (input, ir) in irs.iter().enumerate() {
            let input_len = ir[0].len().max(ir[1].len());
            if input_len == 0 {
                return Err(ConvolverError::EmptyIr(input));
            }
            longest = longest.max(input_len);
        }

        let partitions = longest.div_ceil(partition);
        let fft_len = partition * 2;
        let bins = partition + 1;
        let mut planner = RealFftPlanner::<f32>::new();
        let forward = planner.plan_fft_forward(fft_len);
        let mut time = forward.make_input_vec();
        let mut transformed = forward.make_output_vec();
        let mut scratch = forward.make_scratch_vec();
        let mut spectra = vec![Complex::new(0.0, 0.0); irs.len() * 2 * partitions * bins];

        for (input, ir) in irs.iter().enumerate() {
            for (ear, samples) in ir.iter().enumerate() {
                for part in 0..partitions {
                    time.fill(0.0);
                    let source_start = part * partition;
                    if source_start < samples.len() {
                        let count = partition.min(samples.len() - source_start);
                        time[..count].copy_from_slice(&samples[source_start..source_start + count]);
                    }
                    forward
                        .process_with_scratch(&mut time, &mut transformed, &mut scratch)
                        .expect("buffers were sized by the real FFT plan");
                    let destination = ((input * 2 + ear) * partitions + part) * bins;
                    spectra[destination..destination + bins].copy_from_slice(&transformed);
                }
            }
        }

        Ok(Self {
            inputs: irs.len(),
            partition,
            partitions,
            bins,
            spectra,
        })
    }

    pub fn inputs(&self) -> usize {
        self.inputs
    }

    pub fn partition(&self) -> usize {
        self.partition
    }

    /// Partitions per filter (`ceil(longest IR / partition)`).
    pub fn partitions(&self) -> usize {
        self.partitions
    }

    fn spectrum(&self, input: usize, ear: usize, partition: usize) -> &[Complex<f32>] {
        let start = ((input * 2 + ear) * self.partitions + partition) * self.bins;
        &self.spectra[start..start + self.bins]
    }
}

/// Streaming state for one bank: input history and the delay line.
pub struct Binauralizer {
    bank: Arc<FilterBank>,
    max_block: usize,
    forward: Arc<dyn RealToComplex<f32>>,
    inverse: Arc<dyn ComplexToReal<f32>>,
    forward_scratch: Vec<Complex<f32>>,
    inverse_scratch: Vec<Complex<f32>>,
    time: Vec<f32>,
    inverse_time: Vec<f32>,
    input_history: Vec<f32>,
    // [input][delay-line slot][bin]
    delay_line: Vec<Complex<f32>>,
    accumulator: Vec<Complex<f32>>,
    current_slot: usize,
}

impl fmt::Debug for Binauralizer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Binauralizer")
            .field("bank", &self.bank)
            .field("max_block", &self.max_block)
            .field("current_slot", &self.current_slot)
            .finish_non_exhaustive()
    }
}

impl Binauralizer {
    /// Allocates everything `process` needs for blocks up to `max_block`
    /// frames (a multiple of the bank's partition).
    pub fn new(bank: Arc<FilterBank>, max_block: usize) -> Self {
        let partition = bank.partition;
        assert!(
            max_block > 0 && max_block.is_multiple_of(partition),
            "maximum block length {max_block} must be a positive multiple of partition {partition}"
        );

        let fft_len = partition * 2;
        let mut planner = RealFftPlanner::<f32>::new();
        let forward = planner.plan_fft_forward(fft_len);
        let inverse = planner.plan_fft_inverse(fft_len);
        let forward_scratch = forward.make_scratch_vec();
        let inverse_scratch = inverse.make_scratch_vec();
        let time = forward.make_input_vec();
        let inverse_time = inverse.make_output_vec();
        let input_history = vec![0.0; bank.inputs * partition];
        let delay_line = vec![Complex::new(0.0, 0.0); bank.inputs * bank.partitions * bank.bins];
        let accumulator = vec![Complex::new(0.0, 0.0); bank.bins];

        Self {
            bank,
            max_block,
            forward,
            inverse,
            forward_scratch,
            inverse_scratch,
            time,
            inverse_time,
            input_history,
            delay_line,
            accumulator,
            current_slot: 0,
        }
    }

    pub fn bank(&self) -> &Arc<FilterBank> {
        &self.bank
    }

    /// Convolves `feeds` (one channel per bank input, `feeds.frames()` a
    /// multiple of the partition and at most `max_block`) and writes the ear
    /// sums into `left` and `right` (each `feeds.frames()` long, overwritten).
    /// Never allocates.
    pub fn process(&mut self, feeds: &Feeds, left: &mut [f32], right: &mut [f32]) {
        let frames = feeds.frames();
        let partition = self.bank.partition;
        assert_eq!(
            feeds.directions(),
            self.bank.inputs,
            "feed direction count must match the filter bank input count"
        );
        assert!(
            frames.is_multiple_of(partition),
            "block length {frames} must be a multiple of partition {partition}"
        );
        assert!(
            frames <= self.max_block,
            "block length {frames} exceeds maximum {}",
            self.max_block
        );
        assert_eq!(left.len(), frames, "left output length must match feeds");
        assert_eq!(right.len(), frames, "right output length must match feeds");

        for hop_start in (0..frames).step_by(partition) {
            self.transform_inputs(feeds, hop_start);
            self.synthesize_ear(0, &mut left[hop_start..hop_start + partition]);
            self.synthesize_ear(1, &mut right[hop_start..hop_start + partition]);
            self.current_slot = (self.current_slot + 1) % self.bank.partitions;
        }
    }

    /// Zeroes the history, as after a format change.
    pub fn reset(&mut self) {
        self.input_history.fill(0.0);
        self.delay_line.fill(Complex::new(0.0, 0.0));
        self.time.fill(0.0);
        self.inverse_time.fill(0.0);
        self.accumulator.fill(Complex::new(0.0, 0.0));
        self.current_slot = 0;
    }

    fn transform_inputs(&mut self, feeds: &Feeds, hop_start: usize) {
        let partition = self.bank.partition;
        let partitions = self.bank.partitions;
        let bins = self.bank.bins;

        for input in 0..self.bank.inputs {
            let history_start = input * partition;
            self.time[..partition]
                .copy_from_slice(&self.input_history[history_start..history_start + partition]);
            let input_hop = &feeds.channel(input)[hop_start..hop_start + partition];
            self.time[partition..].copy_from_slice(input_hop);
            self.input_history[history_start..history_start + partition].copy_from_slice(input_hop);

            let delay_start = (input * partitions + self.current_slot) * bins;
            self.forward
                .process_with_scratch(
                    &mut self.time,
                    &mut self.delay_line[delay_start..delay_start + bins],
                    &mut self.forward_scratch,
                )
                .expect("buffers were sized by the real FFT plan");
        }
    }

    fn synthesize_ear(&mut self, ear: usize, output: &mut [f32]) {
        self.accumulator.fill(Complex::new(0.0, 0.0));
        let partitions = self.bank.partitions;
        let bins = self.bank.bins;

        for input in 0..self.bank.inputs {
            for filter_partition in 0..partitions {
                let slot = (self.current_slot + partitions - filter_partition) % partitions;
                let delay_start = (input * partitions + slot) * bins;
                let input_spectrum = &self.delay_line[delay_start..delay_start + bins];
                let filter_spectrum = self.bank.spectrum(input, ear, filter_partition);
                for ((sum, signal), filter) in self
                    .accumulator
                    .iter_mut()
                    .zip(input_spectrum)
                    .zip(filter_spectrum)
                {
                    *sum += *signal * *filter;
                }
            }
        }

        self.inverse
            .process_with_scratch(
                &mut self.accumulator,
                &mut self.inverse_time,
                &mut self.inverse_scratch,
            )
            .expect("accumulated spectra and scratch match the real FFT plan");
        let scale = 1.0 / (2 * self.bank.partition) as f32;
        for (destination, sample) in output
            .iter_mut()
            .zip(&self.inverse_time[self.bank.partition..])
        {
            *destination = *sample * scale;
        }
    }
}
