#![forbid(unsafe_code)]

//! A bounded, allocation-free shared-memory audio block ring.
//!
//! This is a Rust port of EqualizerAPO-XT `runtime/ipc/StreamRing.{h,cpp}` at
//! commit `1432f61cc437c99fde8e8e84e10e648fe516d4ce`. Copyright 2026 115dkk.
//! It is ported to MIT with the copyright holder's permission (ADR 0021).
//!
//! The wire image is little-endian. Every value is accessed atomically. The
//! mapping is split into a prefix of [`AtomicU32`] words and a 128-byte suffix
//! of [`AtomicU64`] words. This keeps f32 samples in `AtomicU32`, gives the
//! counters their natural alignment, and never accesses one byte at two widths.
//! A platform adapter must create those two non-overlapping slices and keep the
//! mapping alive longer than the [`Region`].
//!
//! # Header layout
//!
//! | Offset | Bytes | Field |
//! |---:|---:|---|
//! | 0 | 4 | magic `SSRR` |
//! | 4 | 4 | layout version |
//! | 8 | 4 | total bytes, including the counter suffix |
//! | 12 | 4 | lane count |
//! | 16 | 4 | input channels |
//! | 20 | 4 | output channels |
//! | 24 | 4 | maximum frames |
//! | 28 | 4 | sample rate, Hz |
//! | 32 | 4 | latency bound, frames |
//! | 36 | 4 | state |
//! | 40 | 4 | fault code |
//! | 44 | 4 | producer pid |
//! | 48 | 4 | consumer pid |
//! | 64 | 24 | lane geometry: two slot offsets and slot bytes, per lane |
//! | 128 | 8 | lane sequences (one u32 per lane; own cache line) |
//! | 192 | 8 | lane completions (one u32 per lane; own cache line) |
//! | 512 | variable | two 64-byte-aligned sample slots per lane |
//! | rounded slot end | 64 | heartbeat u64 and padding |
//! | +64 | 64 | last error u64 and padding |

use std::array;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

pub const MAGIC: u32 = u32::from_le_bytes(*b"SSRR");
pub const LAYOUT_VERSION: u32 = 1;
pub const HEADER_BYTES: usize = 512;
pub const CACHE_LINE: usize = 64;
pub const MAX_CHANNELS: u32 = 64;
pub const MAX_FRAMES: u32 = 65_536;
pub const MAX_REGION_BYTES: usize = 128 * 1024 * 1024;
const COUNTER_BYTES: usize = 2 * CACHE_LINE;
const HEADER_WORDS: usize = HEADER_BYTES / 4;
const COUNTER_WORDS: usize = COUNTER_BYTES / 8;

const MAGIC_I: usize = 0;
const VERSION_I: usize = 1;
const TOTAL_I: usize = 2;
const LANES_I: usize = 3;
const INPUTS_I: usize = 4;
const OUTPUTS_I: usize = 5;
const FRAMES_I: usize = 6;
const RATE_I: usize = 7;
const LATENCY_I: usize = 8;
const STATE_I: usize = 9;
const FAULT_I: usize = 10;
const PRODUCER_PID_I: usize = 11;
const CONSUMER_PID_I: usize = 12;
const LANE_GEOMETRY_I: usize = 16;
const SEQUENCE_I: usize = 32;
const COMPLETED_I: usize = 48;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum State {
    Empty = 0,
    Announced = 1,
    Ready = 2,
    Closing = 3,
    Fault = 4,
}

impl State {
    fn from_wire(value: u32) -> Option<Self> {
        Some(match value {
            0 => Self::Empty,
            1 => Self::Announced,
            2 => Self::Ready,
            3 => Self::Closing,
            4 => Self::Fault,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Fault {
    None = 0,
    LayoutMismatch = 1,
    EngineFailed = 2,
    HostShuttingDown = 3,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Geometry {
    pub lanes: u32,
    pub input_channels: u32,
    pub output_channels: u32,
    pub max_frames: u32,
    pub sample_rate: u32,
    pub latency_bound_frames: u32,
}

impl Geometry {
    pub fn validate(self) -> Result<Self, GeometryError> {
        if !(1..=2).contains(&self.lanes) {
            return Err(GeometryError::LaneCount);
        }
        if self.input_channels > MAX_CHANNELS || self.output_channels > MAX_CHANNELS {
            return Err(GeometryError::Channels);
        }
        if self.input_channels == 0 && self.output_channels == 0 {
            return Err(GeometryError::Channels);
        }
        if !(1..=MAX_FRAMES).contains(&self.max_frames) {
            return Err(GeometryError::Frames);
        }
        if !(1_000..=1_000_000).contains(&self.sample_rate) {
            return Err(GeometryError::SampleRate);
        }
        if layout(self).is_none() {
            return Err(GeometryError::RegionTooLarge);
        }
        Ok(self)
    }

    pub fn planes(self) -> usize {
        self.input_channels.max(self.output_channels) as usize
    }

    pub fn samples_per_slot(self) -> usize {
        self.planes() * self.max_frames as usize
    }

    pub fn total_bytes(self) -> Result<usize, GeometryError> {
        self.validate()?;
        Ok(layout(self)
            .expect("validated geometry has a layout")
            .total_bytes)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GeometryError {
    LaneCount,
    Channels,
    Frames,
    SampleRate,
    RegionTooLarge,
}

#[derive(Clone, Copy, Debug)]
struct Layout {
    slot_bytes: usize,
    slot_offsets: [[usize; 2]; 2],
    prefix_bytes: usize,
    total_bytes: usize,
}

fn round_up(value: usize, alignment: usize) -> Option<usize> {
    value
        .checked_add(alignment - 1)
        .map(|v| v / alignment * alignment)
}

fn layout(geometry: Geometry) -> Option<Layout> {
    let slot_bytes = round_up(geometry.samples_per_slot().checked_mul(4)?, CACHE_LINE)?;
    let mut offset = HEADER_BYTES;
    let mut slot_offsets = [[0; 2]; 2];
    for lane_slots in slot_offsets.iter_mut().take(geometry.lanes as usize) {
        for slot_offset in lane_slots {
            *slot_offset = offset;
            offset = offset.checked_add(slot_bytes)?;
        }
    }
    let prefix_bytes = round_up(offset, CACHE_LINE)?;
    let total_bytes = prefix_bytes.checked_add(COUNTER_BYTES)?;
    (total_bytes < MAX_REGION_BYTES).then_some(Layout {
        slot_bytes,
        slot_offsets,
        prefix_bytes,
        total_bytes,
    })
}

pub trait Signal: Send + Sync {
    fn signal(&self);
}

pub trait Wait: Send + Sync {
    fn wait(&self, timeout: Duration) -> Waited;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Waited {
    Signalled,
    TimedOut,
    PeerGone,
}

#[derive(Debug)]
struct EventInner {
    generation: AtomicU64,
    gone: AtomicBool,
    observed: Mutex<u64>,
    changed: Condvar,
}

/// Same-process event for tests and adapters. `signal` only performs an atomic
/// increment and a condition-variable notification; it neither locks nor
/// allocates.
#[derive(Clone, Debug)]
pub struct InProcessEvent(Arc<EventInner>);

impl InProcessEvent {
    pub fn new() -> Self {
        Self(Arc::new(EventInner {
            generation: AtomicU64::new(0),
            gone: AtomicBool::new(false),
            observed: Mutex::new(0),
            changed: Condvar::new(),
        }))
    }

    pub fn peer_gone(&self) {
        self.0.gone.store(true, Ordering::Release);
        self.0.changed.notify_all();
    }
}

impl Default for InProcessEvent {
    fn default() -> Self {
        Self::new()
    }
}

impl Signal for InProcessEvent {
    fn signal(&self) {
        self.0.generation.fetch_add(1, Ordering::Release);
        self.0.changed.notify_one();
    }
}

impl Wait for InProcessEvent {
    fn wait(&self, timeout: Duration) -> Waited {
        if self.0.gone.load(Ordering::Acquire) {
            return Waited::PeerGone;
        }
        let mut observed = self.0.observed.lock().unwrap_or_else(|e| e.into_inner());
        let current = self.0.generation.load(Ordering::Acquire);
        if current != *observed {
            *observed = current;
            return Waited::Signalled;
        }
        let (mut observed, result) = self
            .0
            .changed
            .wait_timeout_while(observed, timeout, |seen| {
                !self.0.gone.load(Ordering::Acquire)
                    && self.0.generation.load(Ordering::Acquire) == *seen
            })
            .unwrap_or_else(|e| e.into_inner());
        if self.0.gone.load(Ordering::Acquire) {
            Waited::PeerGone
        } else {
            let current = self.0.generation.load(Ordering::Acquire);
            if current != *observed {
                *observed = current;
                Waited::Signalled
            } else if result.timed_out() {
                Waited::TimedOut
            } else {
                Waited::Signalled
            }
        }
    }
}

/// Borrowed shared mapping. The u32 prefix contains the header and samples;
/// the u64 suffix contains only heartbeat and last-error cache lines.
#[derive(Clone, Copy, Debug)]
pub struct Region<'a> {
    words32: &'a [AtomicU32],
    words64: &'a [AtomicU64],
}

impl<'a> Region<'a> {
    pub fn new(words32: &'a [AtomicU32], words64: &'a [AtomicU64]) -> Self {
        Self { words32, words64 }
    }

    pub fn state(self) -> Option<State> {
        State::from_wire(self.load32(STATE_I, Ordering::Acquire))
    }

    pub fn fault_code(self) -> u32 {
        self.load32(FAULT_I, Ordering::Acquire)
    }

    pub fn heartbeat(self) -> u64 {
        self.words64
            .first()
            .map_or(0, |v| v.load(Ordering::Acquire))
    }

    pub fn last_error(self) -> u64 {
        self.words64
            .get(CACHE_LINE / 8)
            .map_or(0, |v| v.load(Ordering::Acquire))
    }

    fn load32(self, index: usize, ordering: Ordering) -> u32 {
        self.words32.get(index).map_or(0, |v| v.load(ordering))
    }

    fn store32(self, index: usize, value: u32, ordering: Ordering) {
        if let Some(word) = self.words32.get(index) {
            word.store(value, ordering);
        }
    }
}

#[derive(Debug)]
pub struct RegionBuf {
    words32: Box<[AtomicU32]>,
    words64: Box<[AtomicU64]>,
}

impl RegionBuf {
    pub fn new(geometry: Geometry) -> Result<Self, GeometryError> {
        let geometry = geometry.validate()?;
        let layout = layout(geometry).expect("validated geometry has a layout");
        Ok(Self {
            words32: (0..layout.prefix_bytes / 4)
                .map(|_| AtomicU32::new(0))
                .collect(),
            words64: (0..COUNTER_WORDS).map(|_| AtomicU64::new(0)).collect(),
        })
    }

    pub fn region(&self) -> Region<'_> {
        Region::new(&self.words32, &self.words64)
    }
}

/// Memory that owns a mapping and lends it as a [`Region`]: an in-process
/// [`RegionBuf`], or a platform adapter's shared-memory view. An owner that
/// outlives the borrow lets a [`Publisher`] travel to another thread without
/// the [`Producer`] that announced the header.
pub trait RegionMemory: Send + Sync {
    fn region(&self) -> Region<'_>;
}

impl RegionMemory for RegionBuf {
    fn region(&self) -> Region<'_> {
        self.region()
    }
}

impl<T: RegionMemory + ?Sized> RegionMemory for Arc<T> {
    fn region(&self) -> Region<'_> {
        T::region(self)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HeaderError {
    TooShort,
    Magic,
    Version,
    State,
    Geometry(GeometryError),
    TotalBytes,
    LaneGeometry,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Publish {
    Published,
    Dropped,
}

/// The producer's real-time half: the announced geometry and slot layout,
/// cached so the per-block path needs no [`Producer`] and no borrow of the
/// mapping beyond each call. Every method takes the [`Region`] it works on and
/// neither blocks, locks nor allocates; `work` is only signalled.
#[derive(Clone, Copy, Debug)]
pub struct Publisher {
    geometry: Geometry,
    layout: Layout,
}

impl Publisher {
    pub fn geometry(&self) -> Geometry {
        self.geometry
    }

    pub fn state(&self, region: Region<'_>) -> Option<State> {
        region.state()
    }

    /// The last sequence the consumer completed on `lane` (0 outside the
    /// geometry).
    pub fn completed(&self, region: Region<'_>, lane: usize) -> u32 {
        if lane >= self.geometry.lanes as usize {
            return 0;
        }
        u32::from_le(region.load32(COMPLETED_I + lane, Ordering::Acquire))
    }

    /// Writes `sequence` (never 0) into its slot and publishes it, then
    /// signals `work`. `Dropped`, touching nothing, unless the consumer is
    /// `Ready` and has completed `sequence - 2`, whose slot this reuses.
    pub fn try_publish<F>(
        &self,
        region: Region<'_>,
        lane: usize,
        sequence: u32,
        work: Option<&dyn Signal>,
        write: F,
    ) -> Publish
    where
        F: FnOnce(&mut SlotWriter<'_>),
    {
        if region.state() != Some(State::Ready)
            || lane >= self.geometry.lanes as usize
            || sequence == 0
        {
            return Publish::Dropped;
        }
        let completed = self.completed(region, lane);
        if sequence > 2 && !sequence_at_least(completed, sequence.wrapping_sub(2)) {
            return Publish::Dropped;
        }
        let offset = self.layout.slot_offsets[lane][sequence as usize & 1] / 4;
        let Some(samples) = region
            .words32
            .get(offset..offset + self.geometry.samples_per_slot())
        else {
            return Publish::Dropped;
        };
        let mut writer = SlotWriter {
            samples,
            frames: self.geometry.max_frames as usize,
            planes: self.geometry.planes(),
        };
        write(&mut writer);
        region.store32(SEQUENCE_I + lane, sequence.to_le(), Ordering::Release);
        if let Some(signal) = work {
            signal.signal();
        }
        Publish::Published
    }

    /// Lends `sequence`'s slot to `read` once the consumer has completed it.
    pub fn try_read<F>(&self, region: Region<'_>, lane: usize, sequence: u32, read: F) -> bool
    where
        F: FnOnce(SlotReader<'_>),
    {
        if lane >= self.geometry.lanes as usize
            || !sequence_at_least(self.completed(region, lane), sequence)
        {
            return false;
        }
        let offset = self.layout.slot_offsets[lane][sequence as usize & 1] / 4;
        let Some(samples) = region
            .words32
            .get(offset..offset + self.geometry.samples_per_slot())
        else {
            return false;
        };
        read(SlotReader {
            samples,
            frames: self.geometry.max_frames as usize,
            planes: self.geometry.planes(),
        });
        true
    }
}

pub struct Producer<'a> {
    region: Region<'a>,
    geometry: Option<Geometry>,
    layout: Option<Layout>,
    work: [Option<&'a dyn Signal>; 2],
    ready: &'a dyn Wait,
}

impl fmt::Debug for Producer<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Producer")
            .field("region", &self.region)
            .field("geometry", &self.geometry)
            .field("layout", &self.layout)
            .finish_non_exhaustive()
    }
}

impl<'a> Producer<'a> {
    pub fn new(region: Region<'a>, work: [Option<&'a dyn Signal>; 2], ready: &'a dyn Wait) -> Self {
        Self {
            region,
            geometry: None,
            layout: None,
            work,
            ready,
        }
    }

    pub fn announce(&mut self, geometry: Geometry, producer_pid: u32) -> Result<(), GeometryError> {
        let geometry = geometry.validate()?;
        let layout = layout(geometry).expect("validated geometry has a layout");
        if self.region.words32.len() * 4 < layout.prefix_bytes
            || self.region.words64.len() < COUNTER_WORDS
        {
            return Err(GeometryError::RegionTooLarge);
        }
        for word in self.region.words32.iter().take(HEADER_WORDS) {
            word.store(0, Ordering::Relaxed);
        }
        for word in self.region.words64 {
            word.store(0, Ordering::Relaxed);
        }
        let fields = [
            (MAGIC_I, MAGIC),
            (VERSION_I, LAYOUT_VERSION),
            (TOTAL_I, layout.total_bytes as u32),
            (LANES_I, geometry.lanes),
            (INPUTS_I, geometry.input_channels),
            (OUTPUTS_I, geometry.output_channels),
            (FRAMES_I, geometry.max_frames),
            (RATE_I, geometry.sample_rate),
            (LATENCY_I, geometry.latency_bound_frames),
            (PRODUCER_PID_I, producer_pid),
        ];
        for (index, value) in fields {
            self.region.store32(index, value.to_le(), Ordering::Relaxed);
        }
        for lane in 0..geometry.lanes as usize {
            let base = LANE_GEOMETRY_I + lane * 3;
            self.region.store32(
                base,
                (layout.slot_offsets[lane][0] as u32).to_le(),
                Ordering::Relaxed,
            );
            self.region.store32(
                base + 1,
                (layout.slot_offsets[lane][1] as u32).to_le(),
                Ordering::Relaxed,
            );
            self.region.store32(
                base + 2,
                (layout.slot_bytes as u32).to_le(),
                Ordering::Relaxed,
            );
        }
        self.region
            .store32(FAULT_I, Fault::None as u32, Ordering::Release);
        self.region
            .store32(STATE_I, State::Announced as u32, Ordering::Release);
        self.geometry = Some(geometry);
        self.layout = Some(layout);
        Ok(())
    }

    pub fn wait_ready(&self, timeout: Duration) -> Waited {
        let deadline = Instant::now() + timeout;
        loop {
            match self.region.state() {
                Some(State::Ready) => return Waited::Signalled,
                Some(State::Fault | State::Closing) | None => return Waited::PeerGone,
                _ => {}
            }
            let now = Instant::now();
            if now >= deadline {
                return Waited::TimedOut;
            }
            match self.ready.wait(deadline.saturating_duration_since(now)) {
                Waited::Signalled => {}
                other => return other,
            }
        }
    }

    /// The real-time half, once [`Producer::announce`] has succeeded.
    pub fn publisher(&self) -> Option<Publisher> {
        Some(Publisher {
            geometry: self.geometry?,
            layout: self.layout?,
        })
    }

    pub fn completed(&self, lane: usize) -> u32 {
        self.publisher()
            .map_or(0, |publisher| publisher.completed(self.region, lane))
    }

    pub fn try_publish<F>(&self, lane: usize, sequence: u32, write: F) -> Publish
    where
        F: FnOnce(&mut SlotWriter<'_>),
    {
        let Some(publisher) = self.publisher() else {
            return Publish::Dropped;
        };
        publisher.try_publish(self.region, lane, sequence, self.work[lane], write)
    }

    pub fn try_read<F>(&self, lane: usize, sequence: u32, read: F) -> bool
    where
        F: FnOnce(SlotReader<'_>),
    {
        self.publisher()
            .is_some_and(|publisher| publisher.try_read(self.region, lane, sequence, read))
    }

    pub fn close(&self) {
        self.region
            .store32(STATE_I, State::Closing as u32, Ordering::Release);
        for signal in self.work.iter().flatten() {
            signal.signal();
        }
    }
}

#[derive(Debug)]
pub struct SlotWriter<'a> {
    samples: &'a [AtomicU32],
    frames: usize,
    planes: usize,
}

impl SlotWriter<'_> {
    pub fn frames(&self) -> usize {
        self.frames
    }

    pub fn planes(&self) -> usize {
        self.planes
    }

    pub fn write(&mut self, plane: usize, frame: usize, sample: f32) {
        self.samples[plane * self.frames + frame].store(sample.to_bits(), Ordering::Relaxed);
    }

    pub fn copy_from(&mut self, samples: &[f32]) -> bool {
        if samples.len() != self.samples.len() {
            return false;
        }
        for (destination, sample) in self.samples.iter().zip(samples) {
            destination.store(sample.to_bits(), Ordering::Relaxed);
        }
        true
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SlotReader<'a> {
    samples: &'a [AtomicU32],
    frames: usize,
    planes: usize,
}

impl SlotReader<'_> {
    pub fn frames(self) -> usize {
        self.frames
    }

    pub fn planes(self) -> usize {
        self.planes
    }

    pub fn read(self, plane: usize, frame: usize) -> f32 {
        f32::from_bits(self.samples[plane * self.frames + frame].load(Ordering::Relaxed))
    }

    pub fn copy_to(self, samples: &mut [f32]) -> bool {
        if samples.len() != self.samples.len() {
            return false;
        }
        for (destination, sample) in samples.iter_mut().zip(self.samples) {
            *destination = f32::from_bits(sample.load(Ordering::Relaxed));
        }
        true
    }
}

pub struct Consumer<'a> {
    region: Region<'a>,
    geometry: Option<Geometry>,
    layout: Option<Layout>,
    ready: &'a dyn Signal,
    done: [Option<&'a dyn Signal>; 2],
}

impl fmt::Debug for Consumer<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Consumer")
            .field("region", &self.region)
            .field("geometry", &self.geometry)
            .field("layout", &self.layout)
            .finish_non_exhaustive()
    }
}

impl<'a> Consumer<'a> {
    pub fn new(
        region: Region<'a>,
        ready: &'a dyn Signal,
        done: [Option<&'a dyn Signal>; 2],
    ) -> Self {
        Self {
            region,
            geometry: None,
            layout: None,
            ready,
            done,
        }
    }

    pub fn validate_and_ready(&mut self, consumer_pid: u32) -> Result<Geometry, HeaderError> {
        let geometry = validate_header(self.region)?;
        let layout = layout(geometry).expect("validated geometry has a layout");
        self.geometry = Some(geometry);
        self.layout = Some(layout);
        self.region
            .store32(CONSUMER_PID_I, consumer_pid.to_le(), Ordering::Relaxed);
        self.region
            .store32(FAULT_I, Fault::None as u32, Ordering::Relaxed);
        self.region
            .store32(STATE_I, State::Ready as u32, Ordering::Release);
        self.ready.signal();
        Ok(geometry)
    }

    pub fn fault(&self, code: u32) {
        self.region.store32(FAULT_I, code, Ordering::Relaxed);
        self.region
            .store32(STATE_I, State::Fault as u32, Ordering::Release);
        self.ready.signal();
        for signal in self.done.iter().flatten() {
            signal.signal();
        }
    }

    pub fn next_published(&self, lane: usize) -> Option<u32> {
        let geometry = self.geometry?;
        if lane >= geometry.lanes as usize || self.region.state() != Some(State::Ready) {
            return None;
        }
        let published = u32::from_le(self.region.load32(SEQUENCE_I + lane, Ordering::Acquire));
        let completed = u32::from_le(self.region.load32(COMPLETED_I + lane, Ordering::Acquire));
        let next = completed.wrapping_add(1);
        (published != completed && sequence_at_least(published, next)).then_some(next)
    }

    /// Copies one slot to `buffer`, lets the caller process ordinary f32s, and
    /// copies it back. No plugin ever receives shared-memory atomics.
    pub fn slot_mut<F>(&self, lane: usize, sequence: u32, buffer: &mut [f32], process: F) -> bool
    where
        F: FnOnce(&mut [f32]),
    {
        let Some((geometry, layout)) = self.geometry.zip(self.layout) else {
            return false;
        };
        if lane >= geometry.lanes as usize || buffer.len() != geometry.samples_per_slot() {
            return false;
        }
        let offset = layout.slot_offsets[lane][sequence as usize & 1] / 4;
        let samples = &self.region.words32[offset..offset + buffer.len()];
        for (destination, sample) in buffer.iter_mut().zip(samples) {
            *destination = f32::from_bits(sample.load(Ordering::Relaxed));
        }
        process(buffer);
        for (destination, sample) in samples.iter().zip(buffer) {
            destination.store(sample.to_bits(), Ordering::Relaxed);
        }
        true
    }

    pub fn complete(&self, lane: usize, sequence: u32) {
        self.region
            .store32(COMPLETED_I + lane, sequence.to_le(), Ordering::Release);
        if let Some(signal) = self.done[lane] {
            signal.signal();
        }
    }

    pub fn beat(&self) -> u64 {
        self.region
            .words64
            .first()
            .map_or(0, |v| v.fetch_add(1, Ordering::Release).wrapping_add(1))
    }

    pub fn set_last_error(&self, error: u64) {
        if let Some(word) = self.region.words64.get(CACHE_LINE / 8) {
            word.store(error, Ordering::Release);
        }
    }
}

fn validate_header(region: Region<'_>) -> Result<Geometry, HeaderError> {
    if region.words32.len() < HEADER_WORDS || region.words64.len() < COUNTER_WORDS {
        return Err(HeaderError::TooShort);
    }
    if u32::from_le(region.load32(MAGIC_I, Ordering::Relaxed)) != MAGIC {
        return Err(HeaderError::Magic);
    }
    if u32::from_le(region.load32(VERSION_I, Ordering::Relaxed)) != LAYOUT_VERSION {
        return Err(HeaderError::Version);
    }
    if region.state() != Some(State::Announced) {
        return Err(HeaderError::State);
    }
    let geometry = Geometry {
        lanes: u32::from_le(region.load32(LANES_I, Ordering::Relaxed)),
        input_channels: u32::from_le(region.load32(INPUTS_I, Ordering::Relaxed)),
        output_channels: u32::from_le(region.load32(OUTPUTS_I, Ordering::Relaxed)),
        max_frames: u32::from_le(region.load32(FRAMES_I, Ordering::Relaxed)),
        sample_rate: u32::from_le(region.load32(RATE_I, Ordering::Relaxed)),
        latency_bound_frames: u32::from_le(region.load32(LATENCY_I, Ordering::Relaxed)),
    }
    .validate()
    .map_err(HeaderError::Geometry)?;
    let expected = layout(geometry).expect("validated geometry has a layout");
    if u32::from_le(region.load32(TOTAL_I, Ordering::Relaxed)) as usize != expected.total_bytes
        || region.words32.len() * 4 < expected.prefix_bytes
    {
        return Err(HeaderError::TotalBytes);
    }
    for lane in 0..geometry.lanes as usize {
        let base = LANE_GEOMETRY_I + lane * 3;
        let actual = [
            u32::from_le(region.load32(base, Ordering::Relaxed)) as usize,
            u32::from_le(region.load32(base + 1, Ordering::Relaxed)) as usize,
            u32::from_le(region.load32(base + 2, Ordering::Relaxed)) as usize,
        ];
        if actual
            != [
                expected.slot_offsets[lane][0],
                expected.slot_offsets[lane][1],
                expected.slot_bytes,
            ]
        {
            return Err(HeaderError::LaneGeometry);
        }
    }
    Ok(geometry)
}

fn sequence_at_least(value: u32, reference: u32) -> bool {
    value.wrapping_sub(reference) as i32 >= 0
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct PipelineEntry {
    sequence: u32,
    app_block: u64,
    generation: u64,
    valid: bool,
}

/// Two-slot metadata enforcing ADR 0021's one-block pipeline rule.
#[derive(Debug, Default)]
pub struct Pipeline {
    entries: [PipelineEntry; 2],
}

impl Pipeline {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn published(&mut self, sequence: u32, app_block: u64, generation: u64) {
        self.entries[sequence as usize & 1] = PipelineEntry {
            sequence,
            app_block,
            generation,
            valid: true,
        };
    }

    /// Returns the sequence for `app_block - 1` only when that exact block was
    /// published, completed, and belongs to the current generation.
    pub fn output_sequence(
        &mut self,
        app_block: u64,
        generation: u64,
        completed: u32,
    ) -> Option<u32> {
        let wanted = app_block.checked_sub(1)?;
        let found = self.entries.iter_mut().find(|entry| {
            entry.valid && entry.app_block == wanted && entry.generation == generation
        })?;
        if sequence_at_least(completed, found.sequence) {
            found.valid = false;
            Some(found.sequence)
        } else {
            // Once its output instant passes, a late result must never be used.
            found.valid = false;
            None
        }
    }

    pub fn clear(&mut self) {
        self.entries = array::from_fn(|_| PipelineEntry::default());
    }
}
