#![forbid(unsafe_code)]

//! Direction feeds to binaural signal: non-uniformly partitioned
//! frequency-domain convolution, a Rust rewrite of EqualizerAPO-XT's
//! MultiConvolution (Copyright 2026 115dkk, MIT; ADR 0006).
//!
//! Filters use three segments. The first `[0, 16P)` taps use the caller's
//! partition `P`, `[16P, 128P)` uses `8P`, and the remaining taps use `64P`.
//! A filter that ends before a boundary has no later segment, so short filters
//! retain the original uniformly partitioned path. The two larger segments
//! start two of their own partitions after zero: while input window `w + 1`
//! arrives, their FFT, multiply-accumulate, and inverse-FFT work for completed
//! window `w` is divided deterministically among the `Q / P` blocks. Its first
//! output is therefore ready when window `w + 2` starts. Blocks remain whole
//! multiples of `P`, so this scheduling adds no latency.

use std::fmt;
use std::sync::Arc;

use bio_types::Feeds;
use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};

const SEGMENT_ZERO_END_PARTITIONS: usize = 16;
const SEGMENT_ONE_PARTITION_MULTIPLIER: usize = 8;
const SEGMENT_TWO_START_PARTITIONS: usize = 128;
const SEGMENT_TWO_PARTITION_MULTIPLIER: usize = 64;
const INPUT_WINDOW_SLOTS: usize = 3;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ConvolverError {
    #[error("a filter bank needs at least one input")]
    NoInputs,
    #[error("input {0} has an empty impulse response")]
    EmptyIr(usize),
    #[error("partition size {0} must be even and at least 16")]
    BadPartition(usize),
}

#[derive(Debug)]
struct FilterSegment {
    partition: usize,
    offset: usize,
    partitions: usize,
    bins: usize,
    // [input][ear][partition][bin]
    spectra: Vec<Complex<f32>>,
}

impl FilterSegment {
    fn spectrum(&self, input: usize, ear: usize, partition: usize) -> &[Complex<f32>] {
        let start = ((input * 2 + ear) * self.partitions + partition) * self.bins;
        &self.spectra[start..start + self.bins]
    }
}

/// The filters of every input and ear, cut into transformed non-uniform
/// partitions. Built once on the control thread; shared read-only by the
/// real-time thread.
#[derive(Debug)]
pub struct FilterBank {
    inputs: usize,
    partition: usize,
    partitions: usize,
    quiet_hops: usize,
    segments: Vec<FilterSegment>,
}

impl FilterBank {
    /// `irs[i] = [left, right]` impulse responses of input `i` (the two ears
    /// may differ in length; a missing tail is represented by zero partitions).
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

        let first_end = partition * SEGMENT_ZERO_END_PARTITIONS;
        let second_end = partition * SEGMENT_TWO_START_PARTITIONS;
        let mut descriptions = Vec::with_capacity(3);
        descriptions.push((0, partition, longest.min(first_end).div_ceil(partition)));
        if longest > first_end {
            let segment_partition = partition * SEGMENT_ONE_PARTITION_MULTIPLIER;
            descriptions.push((
                first_end,
                segment_partition,
                (longest.min(second_end) - first_end).div_ceil(segment_partition),
            ));
        }
        if longest > second_end {
            let segment_partition = partition * SEGMENT_TWO_PARTITION_MULTIPLIER;
            descriptions.push((
                second_end,
                segment_partition,
                (longest - second_end).div_ceil(segment_partition),
            ));
        }

        let mut planner = RealFftPlanner::<f32>::new();
        let mut segments = Vec::with_capacity(descriptions.len());
        for (offset, segment_partition, segment_partitions) in descriptions {
            segments.push(build_filter_segment(
                irs,
                offset,
                segment_partition,
                segment_partitions,
                &mut planner,
            ));
        }
        let partitions = segments.iter().map(|segment| segment.partitions).sum();
        // A scheduled segment can still be collecting one large input window
        // and spread that window's work across the next. Reserve two complete
        // windows at the largest segment ratio; `+ 2` is sufficient only for
        // the immediate P-sized segment.
        let largest_ratio = segments
            .iter()
            .map(|segment| segment.partition / partition)
            .max()
            .unwrap_or(1);
        let quiet_hops = longest
            .div_ceil(partition)
            .saturating_add(2 * largest_ratio);

        Ok(Self {
            inputs: irs.len(),
            partition,
            partitions,
            quiet_hops,
            segments,
        })
    }

    pub fn inputs(&self) -> usize {
        self.inputs
    }

    /// The smallest partition, which is also the required process hop.
    pub fn partition(&self) -> usize {
        self.partition
    }

    /// Total partition count across all non-uniform segments.
    ///
    /// This is the sum of the `P`, `8P`, and `64P` partition counts that exist
    /// for the longest filter, rather than `ceil(length / P)`.
    pub fn partitions(&self) -> usize {
        self.partitions
    }
}

fn build_filter_segment(
    irs: &[[&[f32]; 2]],
    offset: usize,
    partition: usize,
    partitions: usize,
    planner: &mut RealFftPlanner<f32>,
) -> FilterSegment {
    let fft_len = partition * 2;
    let bins = partition + 1;
    let forward = planner.plan_fft_forward(fft_len);
    let mut time = forward.make_input_vec();
    let mut transformed = forward.make_output_vec();
    let mut scratch = forward.make_scratch_vec();
    let mut spectra = vec![Complex::new(0.0, 0.0); irs.len() * 2 * partitions * bins];

    for (input, ir) in irs.iter().enumerate() {
        for (ear, samples) in ir.iter().enumerate() {
            for part in 0..partitions {
                time.fill(0.0);
                let source_start = offset + part * partition;
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

    FilterSegment {
        partition,
        offset,
        partitions,
        bins,
        spectra,
    }
}

struct ImmediateState {
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

impl ImmediateState {
    fn new(segment: &FilterSegment, inputs: usize) -> Self {
        let fft_len = segment.partition * 2;
        let mut planner = RealFftPlanner::<f32>::new();
        let forward = planner.plan_fft_forward(fft_len);
        let inverse = planner.plan_fft_inverse(fft_len);
        let forward_scratch = forward.make_scratch_vec();
        let inverse_scratch = inverse.make_scratch_vec();
        let time = forward.make_input_vec();
        let inverse_time = inverse.make_output_vec();
        let input_history = vec![0.0; inputs * segment.partition];
        let delay_line = vec![Complex::new(0.0, 0.0); inputs * segment.partitions * segment.bins];
        let accumulator = vec![Complex::new(0.0, 0.0); segment.bins];

        Self {
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

    fn process_hop(
        &mut self,
        segment: &FilterSegment,
        inputs: usize,
        feeds: &Feeds,
        hop_start: usize,
        left: &mut [f32],
        right: &mut [f32],
    ) {
        self.transform_inputs(segment, inputs, feeds, hop_start);
        self.synthesize_ear(segment, inputs, 0, left);
        self.synthesize_ear(segment, inputs, 1, right);
        self.current_slot = (self.current_slot + 1) % segment.partitions;
    }

    fn transform_inputs(
        &mut self,
        segment: &FilterSegment,
        inputs: usize,
        feeds: &Feeds,
        hop_start: usize,
    ) {
        let partition = segment.partition;
        for input in 0..inputs {
            let history_start = input * partition;
            self.time[..partition]
                .copy_from_slice(&self.input_history[history_start..history_start + partition]);
            let input_hop = &feeds.channel(input)[hop_start..hop_start + partition];
            self.time[partition..].copy_from_slice(input_hop);
            self.input_history[history_start..history_start + partition].copy_from_slice(input_hop);

            let delay_start = (input * segment.partitions + self.current_slot) * segment.bins;
            self.forward
                .process_with_scratch(
                    &mut self.time,
                    &mut self.delay_line[delay_start..delay_start + segment.bins],
                    &mut self.forward_scratch,
                )
                .expect("buffers were sized by the real FFT plan");
        }
    }

    fn synthesize_ear(
        &mut self,
        segment: &FilterSegment,
        inputs: usize,
        ear: usize,
        output: &mut [f32],
    ) {
        self.accumulator.fill(Complex::new(0.0, 0.0));

        for input in 0..inputs {
            for filter_partition in 0..segment.partitions {
                let slot = (self.current_slot + segment.partitions - filter_partition)
                    % segment.partitions;
                let delay_start = (input * segment.partitions + slot) * segment.bins;
                let input_spectrum = &self.delay_line[delay_start..delay_start + segment.bins];
                let filter_spectrum = segment.spectrum(input, ear, filter_partition);
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
        let scale = 1.0 / (2 * segment.partition) as f32;
        for (destination, sample) in output
            .iter_mut()
            .zip(&self.inverse_time[segment.partition..])
        {
            *destination += *sample * scale;
        }
    }

    fn reset(&mut self) {
        self.forward_scratch.fill(Complex::new(0.0, 0.0));
        self.inverse_scratch.fill(Complex::new(0.0, 0.0));
        self.time.fill(0.0);
        self.inverse_time.fill(0.0);
        self.input_history.fill(0.0);
        self.delay_line.fill(Complex::new(0.0, 0.0));
        self.accumulator.fill(Complex::new(0.0, 0.0));
        self.current_slot = 0;
    }
}

#[derive(Debug, Clone, Copy)]
enum WorkItem {
    Forward {
        input: usize,
    },
    MultiplyAccumulate {
        input: usize,
        ear: usize,
        filter_partition: usize,
        bin_start: usize,
        bin_end: usize,
    },
    Inverse {
        ear: usize,
    },
}

struct ScheduledState {
    forward: Arc<dyn RealToComplex<f32>>,
    inverse: Arc<dyn ComplexToReal<f32>>,
    forward_scratch: Vec<Complex<f32>>,
    inverse_scratch: Vec<Complex<f32>>,
    time: Vec<f32>,
    inverse_time: Vec<f32>,
    // Three complete windows permit reading w - 2 and w - 1 while w arrives.
    input_windows: Vec<f32>,
    input_spectra: Vec<Complex<f32>>,
    // Frequency-domain output windows, indexed by the absolute due window.
    output_spectra: Vec<Complex<f32>>,
    spectrum_slots: usize,
    // Completed time-domain output retained until its due window.
    output_windows: Vec<f32>,
    output_slots: usize,
    work_items: Vec<WorkItem>,
    // Cumulative work-item count after each base hop in a large window.
    work_ends: Vec<usize>,
    ratio: usize,
    delay_windows: usize,
    window_index: usize,
    hop_in_window: usize,
}

impl ScheduledState {
    fn new(segment: &FilterSegment, inputs: usize, base_partition: usize) -> Self {
        let fft_len = segment.partition * 2;
        let mut planner = RealFftPlanner::<f32>::new();
        let forward = planner.plan_fft_forward(fft_len);
        let inverse = planner.plan_fft_inverse(fft_len);
        let forward_scratch = forward.make_scratch_vec();
        let inverse_scratch = inverse.make_scratch_vec();
        let time = forward.make_input_vec();
        let inverse_time = inverse.make_output_vec();
        let input_windows = vec![0.0; INPUT_WINDOW_SLOTS * inputs * segment.partition];
        let input_spectra = vec![Complex::new(0.0, 0.0); inputs * segment.bins];
        let delay_windows = segment.offset / segment.partition;
        debug_assert!(delay_windows >= 2);
        let spectrum_slots = delay_windows + segment.partitions;
        let output_spectra = vec![Complex::new(0.0, 0.0); 2 * spectrum_slots * segment.bins];
        let output_slots = delay_windows + 1;
        let output_windows = vec![0.0; 2 * output_slots * segment.partition];
        let ratio = segment.partition / base_partition;
        let (work_items, work_ends) = build_work_plan(segment, inputs, base_partition, ratio);

        Self {
            forward,
            inverse,
            forward_scratch,
            inverse_scratch,
            time,
            inverse_time,
            input_windows,
            input_spectra,
            output_spectra,
            spectrum_slots,
            output_windows,
            output_slots,
            work_items,
            work_ends,
            ratio,
            delay_windows,
            window_index: 0,
            hop_in_window: 0,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn process_hop(
        &mut self,
        segment: &FilterSegment,
        inputs: usize,
        base_partition: usize,
        feeds: &Feeds,
        hop_start: usize,
        left: &mut [f32],
        right: &mut [f32],
    ) {
        self.emit_output(segment.partition, base_partition, left, right);
        self.collect_input(segment.partition, inputs, base_partition, feeds, hop_start);

        if self.window_index != 0 {
            let source_window = self.window_index - 1;
            self.run_work(segment, inputs, source_window, self.hop_in_window);
        }

        self.hop_in_window += 1;
        if self.hop_in_window == self.ratio {
            self.hop_in_window = 0;
            self.window_index = self.window_index.wrapping_add(1);
        }
    }

    fn emit_output(
        &mut self,
        partition: usize,
        base_partition: usize,
        left: &mut [f32],
        right: &mut [f32],
    ) {
        let slot = self.window_index % self.output_slots;
        let window_offset = self.hop_in_window * base_partition;
        let left_start = slot * partition + window_offset;
        add_and_clear(
            left,
            &mut self.output_windows[left_start..left_start + base_partition],
        );
        let right_start = (self.output_slots + slot) * partition + window_offset;
        add_and_clear(
            right,
            &mut self.output_windows[right_start..right_start + base_partition],
        );
    }

    fn collect_input(
        &mut self,
        partition: usize,
        inputs: usize,
        base_partition: usize,
        feeds: &Feeds,
        hop_start: usize,
    ) {
        let slot = self.window_index % INPUT_WINDOW_SLOTS;
        let window_offset = self.hop_in_window * base_partition;
        for input in 0..inputs {
            let destination = (slot * inputs + input) * partition + window_offset;
            self.input_windows[destination..destination + base_partition]
                .copy_from_slice(&feeds.channel(input)[hop_start..hop_start + base_partition]);
        }
    }

    fn run_work(
        &mut self,
        segment: &FilterSegment,
        inputs: usize,
        source_window: usize,
        work_hop: usize,
    ) {
        let work_start = if work_hop == 0 {
            0
        } else {
            self.work_ends[work_hop - 1]
        };
        let work_end = self.work_ends[work_hop];
        for item_index in work_start..work_end {
            match self.work_items[item_index] {
                WorkItem::Forward { input } => {
                    self.transform_input(segment, inputs, source_window, input);
                }
                WorkItem::MultiplyAccumulate {
                    input,
                    ear,
                    filter_partition,
                    bin_start,
                    bin_end,
                } => {
                    self.multiply_accumulate(
                        segment,
                        source_window,
                        input,
                        ear,
                        filter_partition,
                        bin_start,
                        bin_end,
                    );
                }
                WorkItem::Inverse { ear } => {
                    self.inverse_output(segment, source_window, ear);
                }
            }
        }
    }

    fn transform_input(
        &mut self,
        segment: &FilterSegment,
        inputs: usize,
        source_window: usize,
        input: usize,
    ) {
        let source_slot = source_window % INPUT_WINDOW_SLOTS;
        let source_start = (source_slot * inputs + input) * segment.partition;
        if source_window == 0 {
            self.time[..segment.partition].fill(0.0);
        } else {
            let previous_slot = (source_window - 1) % INPUT_WINDOW_SLOTS;
            let previous_start = (previous_slot * inputs + input) * segment.partition;
            self.time[..segment.partition].copy_from_slice(
                &self.input_windows[previous_start..previous_start + segment.partition],
            );
        }
        self.time[segment.partition..]
            .copy_from_slice(&self.input_windows[source_start..source_start + segment.partition]);

        let spectrum_start = input * segment.bins;
        self.forward
            .process_with_scratch(
                &mut self.time,
                &mut self.input_spectra[spectrum_start..spectrum_start + segment.bins],
                &mut self.forward_scratch,
            )
            .expect("buffers were sized by the real FFT plan");
    }

    #[allow(clippy::too_many_arguments)]
    fn multiply_accumulate(
        &mut self,
        segment: &FilterSegment,
        source_window: usize,
        input: usize,
        ear: usize,
        filter_partition: usize,
        bin_start: usize,
        bin_end: usize,
    ) {
        let due_window = source_window
            .wrapping_add(self.delay_windows)
            .wrapping_add(filter_partition);
        let slot = due_window % self.spectrum_slots;
        let source_start = input * segment.bins + bin_start;
        let destination_start = (ear * self.spectrum_slots + slot) * segment.bins + bin_start;
        let filter = segment.spectrum(input, ear, filter_partition);
        for ((sum, signal), coefficient) in self.output_spectra
            [destination_start..destination_start + (bin_end - bin_start)]
            .iter_mut()
            .zip(&self.input_spectra[source_start..source_start + (bin_end - bin_start)])
            .zip(&filter[bin_start..bin_end])
        {
            *sum += *signal * *coefficient;
        }
    }

    fn inverse_output(&mut self, segment: &FilterSegment, source_window: usize, ear: usize) {
        let due_window = source_window.wrapping_add(self.delay_windows);
        let spectrum_slot = due_window % self.spectrum_slots;
        let spectrum_start = (ear * self.spectrum_slots + spectrum_slot) * segment.bins;
        let spectrum = &mut self.output_spectra[spectrum_start..spectrum_start + segment.bins];
        self.inverse
            .process_with_scratch(spectrum, &mut self.inverse_time, &mut self.inverse_scratch)
            .expect("accumulated spectra and scratch match the real FFT plan");

        let output_slot = due_window % self.output_slots;
        let output_start = (ear * self.output_slots + output_slot) * segment.partition;
        let scale = 1.0 / (2 * segment.partition) as f32;
        for (destination, sample) in self.output_windows
            [output_start..output_start + segment.partition]
            .iter_mut()
            .zip(&self.inverse_time[segment.partition..])
        {
            *destination = *sample * scale;
        }
        spectrum.fill(Complex::new(0.0, 0.0));
    }

    fn reset(&mut self) {
        self.forward_scratch.fill(Complex::new(0.0, 0.0));
        self.inverse_scratch.fill(Complex::new(0.0, 0.0));
        self.time.fill(0.0);
        self.inverse_time.fill(0.0);
        self.input_windows.fill(0.0);
        self.input_spectra.fill(Complex::new(0.0, 0.0));
        self.output_spectra.fill(Complex::new(0.0, 0.0));
        self.output_windows.fill(0.0);
        self.window_index = 0;
        self.hop_in_window = 0;
    }
}

fn add_and_clear(destination: &mut [f32], source: &mut [f32]) {
    for (destination, source) in destination.iter_mut().zip(source) {
        *destination += *source;
        *source = 0.0;
    }
}

fn build_work_plan(
    segment: &FilterSegment,
    inputs: usize,
    base_partition: usize,
    ratio: usize,
) -> (Vec<WorkItem>, Vec<usize>) {
    let chunks = segment.bins.div_ceil(base_partition);
    let item_capacity = inputs * (1 + 2 * segment.partitions * chunks) + 2;
    let mut items = Vec::with_capacity(item_capacity);
    let mut weights = Vec::with_capacity(item_capacity);
    let fft_weight = estimated_fft_weight(segment.partition);

    for input in 0..inputs {
        items.push(WorkItem::Forward { input });
        weights.push(fft_weight);
    }
    for filter_partition in 0..segment.partitions {
        for ear in 0..2 {
            for input in 0..inputs {
                for bin_start in (0..segment.bins).step_by(base_partition) {
                    let bin_end = (bin_start + base_partition).min(segment.bins);
                    items.push(WorkItem::MultiplyAccumulate {
                        input,
                        ear,
                        filter_partition,
                        bin_start,
                        bin_end,
                    });
                    weights.push((bin_end - bin_start) as u64);
                }
            }
            if filter_partition == 0 {
                items.push(WorkItem::Inverse { ear });
                weights.push(fft_weight);
            }
        }
    }

    let total_weight: u64 = weights.iter().sum();
    let mut counts = vec![0_usize; ratio];
    let mut completed_weight = 0_u64;
    for weight in weights {
        let midpoint = completed_weight + weight / 2;
        let hop = (((midpoint as u128) * (ratio as u128)) / (total_weight as u128))
            .min((ratio - 1) as u128) as usize;
        counts[hop] += 1;
        completed_weight += weight;
    }
    let mut work_ends = Vec::with_capacity(ratio);
    let mut item_end = 0;
    for count in counts {
        item_end += count;
        work_ends.push(item_end);
    }
    debug_assert_eq!(item_end, items.len());

    (items, work_ends)
}

fn estimated_fft_weight(partition: usize) -> u64 {
    let fft_len = partition * 2;
    let stages = usize::BITS - fft_len.leading_zeros();
    (fft_len as u64 * u64::from(stages)).div_ceil(4)
}

/// Streaming state for one bank: immediate input history plus scheduled work
/// and output windows for every larger segment.
pub struct Binauralizer {
    bank: Arc<FilterBank>,
    max_block: usize,
    immediate: ImmediateState,
    scheduled: Vec<ScheduledState>,
    silent_hops: usize,
}

impl fmt::Debug for Binauralizer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Binauralizer")
            .field("bank", &self.bank)
            .field("max_block", &self.max_block)
            .field("scheduled_segments", &self.scheduled.len())
            .finish_non_exhaustive()
    }
}

impl Binauralizer {
    /// Allocates everything `process` needs for blocks up to `max_block`
    /// frames (a multiple of the bank's smallest partition).
    pub fn new(bank: Arc<FilterBank>, max_block: usize) -> Self {
        let partition = bank.partition;
        assert!(
            max_block > 0 && max_block.is_multiple_of(partition),
            "maximum block length {max_block} must be a positive multiple of partition {partition}"
        );

        let immediate = ImmediateState::new(&bank.segments[0], bank.inputs);
        let silent_hops = bank.quiet_hops;
        let scheduled = bank.segments[1..]
            .iter()
            .map(|segment| ScheduledState::new(segment, bank.inputs, bank.partition))
            .collect();

        Self {
            bank,
            max_block,
            immediate,
            scheduled,
            silent_hops,
        }
    }

    pub fn bank(&self) -> &Arc<FilterBank> {
        &self.bank
    }

    /// Whether enough zero input has passed to empty every convolution state.
    pub fn is_quiet(&self) -> bool {
        self.silent_hops >= self.bank.quiet_hops
    }

    /// Accounts for a known zero-input block after it has been processed.
    pub fn note_silent_input(&mut self, frames: usize) {
        debug_assert!(frames.is_multiple_of(self.bank.partition));
        self.silent_hops = self
            .silent_hops
            .saturating_add(frames / self.bank.partition)
            .min(self.bank.quiet_hops);
    }

    /// Marks the next processed block as containing signal.
    pub fn note_active_input(&mut self) {
        self.silent_hops = 0;
    }

    /// Convolves `feeds` (one channel per bank input, `feeds.frames()` a
    /// multiple of the smallest partition and at most `max_block`) and writes
    /// the ear sums into `left` and `right` (each `feeds.frames()` long,
    /// overwritten). Never allocates.
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
            let left_hop = &mut left[hop_start..hop_start + partition];
            let right_hop = &mut right[hop_start..hop_start + partition];
            left_hop.fill(0.0);
            right_hop.fill(0.0);

            self.immediate.process_hop(
                &self.bank.segments[0],
                self.bank.inputs,
                feeds,
                hop_start,
                left_hop,
                right_hop,
            );
            for (state, segment) in self.scheduled.iter_mut().zip(&self.bank.segments[1..]) {
                state.process_hop(
                    segment,
                    self.bank.inputs,
                    partition,
                    feeds,
                    hop_start,
                    left_hop,
                    right_hop,
                );
            }
        }
    }

    /// Zeroes every segment's history and any partially completed scheduled
    /// work, as after a format change.
    pub fn reset(&mut self) {
        self.immediate.reset();
        for state in &mut self.scheduled {
            state.reset();
        }
        self.silent_hops = self.bank.quiet_hops;
    }
}
