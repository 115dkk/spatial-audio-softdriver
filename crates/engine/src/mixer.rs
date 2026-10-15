//! Preallocated common timeline mixer (ADR 0028).
//!
//! Every endpoint owns a lane on a 48 kHz timeline. Each lane pans its scenes
//! into the current speaker bed before the common timeline adds lanes together.
//! No lane drives, resets, or changes the panning state of another.

use render::{PanGeometry, Ramp};
use ssr_abi::MAX_ENDPOINTS;
use ssr_types::{Layout, MAX_OBJECTS, Object, Position, SceneBlock};

pub const MIX_RATE: u32 = 48_000;
pub const MIX_FRAMES: usize = 480;
/// Largest retained late-arrival window: 200 ms at 48 kHz.
pub const MAX_ARRIVAL_SLACK_FRAMES: u64 = 9_600;

const BASE_RESAMPLE_TAPS: usize = 256;
const MAX_RESAMPLE_TAPS: usize = 512;
const KAISER_BETA: f64 = 7.856_2;
const FADE_FRAMES: usize = 64;
const VOLUME_RAMP_FRAMES: u64 = 64;
const VOLUME_TRANSITIONS: usize = 16;
const CHANNELS: usize = MAX_OBJECTS + 1;
const MAX_DELAY_FRAMES: u64 = MIX_RATE as u64 / 5;
const MAX_SCENE_FRAMES: u64 = 1_536;
const MAX_OUTPUT_FRAMES: usize = (MAX_SCENE_FRAMES * MIX_RATE as u64).div_ceil(44_100) as usize + 1;
const CONVERSION_RING: usize = 4;
const MAX_DIRECTIONS: usize = Layout::MAX_DIRECTIONS;
const MATRIX_VALUES: usize = MAX_DIRECTIONS * MAX_DIRECTIONS;

#[inline]
fn renderer_pan_ramp_gain(from: f32, to: f32, index: usize, frames: usize) -> f32 {
    if index < frames {
        from + (to - from) / frames as f32 * (index + 1) as f32
    } else {
        to
    }
}

#[inline]
fn discontinuity_fade_out_gain(index: usize, frames: usize) -> f32 {
    (frames - index - 1) as f32 / frames as f32
}

#[derive(Clone, Copy, Debug, Default)]
struct FrameRange {
    start: u64,
    end: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
enum Occupancy {
    #[default]
    Empty,
    Filled,
    Dropped,
}

/// Values fixed when a timeline mixer is built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MixerConfig {
    /// Time an interval waits after its end for a lane that has not placed
    /// through it yet. An interval every active lane has filled leaves sooner.
    pub arrival_slack_frames: u64,
    /// Source QPC ticks per second, used only for interval latency metadata.
    pub qpc_frequency: u64,
    /// Bit `i` reserves timeline storage for endpoint lane `i`.
    pub endpoint_mask: u32,
}

impl Default for MixerConfig {
    fn default() -> Self {
        Self {
            arrival_slack_frames: 96,
            qpc_frequency: 10_000_000,
            endpoint_mask: u32::MAX,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LaneState {
    Idle,
    Active,
    Draining,
}

#[derive(Debug)]
struct Slot {
    interval: u64,
    valid: bool,
    bed: Vec<f32>,
    lfe: Vec<f32>,
    has_lfe: bool,
    directions: usize,
    layout_epoch: u32,
    scene_epoch: u32,
    source_qpc: u64,
    occupancy: [Occupancy; MIX_FRAMES],
    filled_start: usize,
    filled_end: usize,
}

impl Slot {
    fn new() -> Self {
        Self {
            interval: 0,
            valid: false,
            bed: vec![0.0; MAX_DIRECTIONS * MIX_FRAMES],
            lfe: vec![0.0; MIX_FRAMES],
            has_lfe: false,
            directions: 0,
            layout_epoch: 0,
            scene_epoch: 0,
            source_qpc: 0,
            occupancy: [Occupancy::Empty; MIX_FRAMES],
            filled_start: MIX_FRAMES,
            filled_end: 0,
        }
    }

    fn reset_for(&mut self, interval: u64, directions: usize, layout_epoch: u32) {
        self.interval = interval;
        self.valid = true;
        self.bed[..directions * MIX_FRAMES].fill(0.0);
        self.lfe.fill(0.0);
        self.has_lfe = false;
        self.directions = directions;
        self.layout_epoch = layout_epoch;
        self.scene_epoch = 0;
        self.source_qpc = 0;
        self.occupancy.fill(Occupancy::Empty);
        self.filled_start = MIX_FRAMES;
        self.filled_end = 0;
    }

    fn clear(&mut self) {
        self.valid = false;
        self.occupancy.fill(Occupancy::Empty);
        self.filled_start = MIX_FRAMES;
        self.filled_end = 0;
    }

    fn refresh_filled_range(&mut self) {
        self.filled_start = self
            .occupancy
            .iter()
            .position(|state| *state == Occupancy::Filled)
            .unwrap_or(MIX_FRAMES);
        self.filled_end = self
            .occupancy
            .iter()
            .rposition(|state| *state == Occupancy::Filled)
            .map_or(0, |index| index + 1);
    }

    fn contains(&self, interval: u64) -> bool {
        self.valid && self.interval == interval
    }

    fn has_frames(&self) -> bool {
        self.filled_start < self.filled_end
    }
}

trait Interval {
    fn interval_valid(&self, interval: u64) -> bool;
}

impl Interval for Slot {
    fn interval_valid(&self, interval: u64) -> bool {
        self.contains(interval)
    }
}

#[derive(Debug)]
struct SlotRing<T> {
    slots: Vec<T>,
}

impl<T: Interval> SlotRing<T> {
    fn new(slots: Vec<T>) -> Self {
        Self { slots }
    }

    fn get(&self, interval: u64) -> Option<&T> {
        let slot = self.entry_ref(interval)?;
        slot.interval_valid(interval).then_some(slot)
    }

    fn get_mut(&mut self, interval: u64) -> Option<&mut T> {
        let slot = self.entry(interval)?;
        slot.interval_valid(interval).then_some(slot)
    }

    fn entry(&mut self, interval: u64) -> Option<&mut T> {
        if self.slots.is_empty() {
            return None;
        }
        let index = interval as usize % self.slots.len();
        self.slots.get_mut(index)
    }

    fn entry_ref(&self, interval: u64) -> Option<&T> {
        if self.slots.is_empty() {
            return None;
        }
        let index = interval as usize % self.slots.len();
        self.slots.get(index)
    }

    fn iter(&self) -> std::slice::Iter<'_, T> {
        self.slots.iter()
    }

    fn iter_mut(&mut self) -> std::slice::IterMut<'_, T> {
        self.slots.iter_mut()
    }

    fn len(&self) -> usize {
        self.slots.len()
    }

    fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }
}

impl<'a, T> IntoIterator for &'a mut SlotRing<T> {
    type Item = &'a mut T;
    type IntoIter = std::slice::IterMut<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.slots.iter_mut()
    }
}

#[derive(Debug)]
struct KernelBank {
    phases: usize,
    taps: usize,
    values: Vec<f32>,
}

impl KernelBank {
    fn new(rate: u32, cutoff: f64) -> Self {
        let (phases, denominator) = rate_ratio(rate).expect("kernel rate is supported");
        let decimation = denominator.div_ceil(phases);
        let taps = BASE_RESAMPLE_TAPS * decimation.div_ceil(2).max(1);
        Self {
            phases,
            taps,
            values: make_kernels(phases, taps, cutoff),
        }
    }

    fn phase(&self, phase: usize) -> &[f32] {
        let start = phase * self.taps;
        &self.values[start..start + self.taps]
    }
}

#[derive(Debug)]
struct Kernels {
    up_44_1: KernelBank,
    down_88_2: KernelBank,
    down_96: KernelBank,
    down_176_4: KernelBank,
    down_192: KernelBank,
}

impl Kernels {
    fn new() -> Self {
        Self {
            up_44_1: KernelBank::new(44_100, 21_525.0 / 22_050.0),
            down_88_2: KernelBank::new(88_200, 22_000.0 / 44_100.0),
            down_96: KernelBank::new(96_000, 22_000.0 / 48_000.0),
            down_176_4: KernelBank::new(176_400, 22_000.0 / 88_200.0),
            down_192: KernelBank::new(192_000, 22_000.0 / 96_000.0),
        }
    }

    fn for_rate(&self, rate: u32) -> Option<&KernelBank> {
        match rate {
            44_100 => Some(&self.up_44_1),
            88_200 => Some(&self.down_88_2),
            96_000 => Some(&self.down_96),
            176_400 => Some(&self.down_176_4),
            192_000 => Some(&self.down_192),
            _ => None,
        }
    }
}

fn greatest_common_divisor(mut left: usize, mut right: usize) -> usize {
    while right != 0 {
        (left, right) = (right, left % right);
    }
    left
}

fn rate_ratio(rate: u32) -> Option<(usize, usize)> {
    if !matches!(
        rate,
        44_100 | MIX_RATE | 88_200 | 96_000 | 176_400 | 192_000
    ) {
        return None;
    }
    let divisor = greatest_common_divisor(MIX_RATE as usize, rate as usize);
    Some((MIX_RATE as usize / divisor, rate as usize / divisor))
}

fn bessel_i0(value: f64) -> f64 {
    let squared = value * value / 4.0;
    let mut term = 1.0;
    let mut sum = 1.0;
    for index in 1..=32 {
        term *= squared / (index * index) as f64;
        sum += term;
        if term < sum * f64::EPSILON {
            break;
        }
    }
    sum
}

fn make_kernels(phases: usize, taps: usize, cutoff: f64) -> Vec<f32> {
    let mut kernels = vec![0.0; phases * taps];
    let delay = (taps - 1) as f64 / 2.0;
    let window_scale = bessel_i0(KAISER_BETA);
    for phase in 0..phases {
        let fraction = phase as f64 / phases as f64;
        let mut sum = 0.0_f64;
        for tap in 0..taps {
            let distance = tap as f64 - delay + fraction;
            let sinc = if distance.abs() < f64::EPSILON {
                cutoff
            } else {
                (std::f64::consts::PI * cutoff * distance).sin() / (std::f64::consts::PI * distance)
            };
            let position = 2.0 * tap as f64 / (taps - 1) as f64 - 1.0;
            let window = bessel_i0(KAISER_BETA * (1.0 - position * position).sqrt()) / window_scale;
            let value = sinc * window;
            kernels[phase * taps + tap] = value as f32;
            sum += value;
        }
        if sum.abs() > f64::EPSILON {
            for tap in 0..taps {
                kernels[phase * taps + tap] /= sum as f32;
            }
        }
    }
    kernels
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct VolumeTransition {
    frame: u64,
    gain: f32,
}

#[derive(Debug)]
struct Lane {
    state: LaneState,
    origin: u64,
    transport: u64,
    delay: Option<u64>,
    produced: u64,
    placed_end: u64,
    generation_end: u64,
    fade_in_remaining: usize,
    rate: u32,
    phase_numerator: u64,
    history: Vec<f32>,
    pan_gains: Vec<f32>,
    unit_gains: Vec<f32>,
    unit_valid: [bool; MAX_OBJECTS],
    pan_valid: bool,
    previous_object_count: usize,
    last_positions: [Position; MAX_OBJECTS],
    last_gains: [f32; MAX_OBJECTS],
    last_spreads: [f32; MAX_OBJECTS],
    last_object_count: usize,
    last_has_lfe: bool,
    last_epoch: u32,
    last_source_qpc: u64,
    pending_drop: FrameRange,
    slots: SlotRing<Slot>,
    tail_slots: SlotRing<Slot>,
    tail_end: u64,
    volume_gain: f32,
    volume_from: f32,
    volume_target: f32,
    volume_start: u64,
    volume_transitions: [VolumeTransition; VOLUME_TRANSITIONS],
    volume_transition_len: usize,
    volume_transitions_coalesced: u64,
}

impl Lane {
    fn enabled(&self) -> bool {
        !self.slots.is_empty()
    }

    fn clocked(&self) -> bool {
        self.enabled() && self.state == LaneState::Active && self.transport > 0
    }

    fn new(slot_capacity: usize) -> Self {
        Self {
            state: LaneState::Idle,
            origin: 0,
            transport: 0,
            delay: None,
            produced: 0,
            placed_end: 0,
            generation_end: 0,
            fade_in_remaining: 0,
            rate: 0,
            phase_numerator: 0,
            history: vec![0.0; CHANNELS * (MAX_RESAMPLE_TAPS - 1)],
            pan_gains: vec![0.0; MAX_OBJECTS * MAX_DIRECTIONS],
            unit_gains: vec![0.0; MAX_OBJECTS * MAX_DIRECTIONS],
            unit_valid: [false; MAX_OBJECTS],
            pan_valid: false,
            previous_object_count: 0,
            last_positions: [Position::FRONT; MAX_OBJECTS],
            last_gains: [0.0; MAX_OBJECTS],
            last_spreads: [0.0; MAX_OBJECTS],
            last_object_count: 0,
            last_has_lfe: false,
            last_epoch: 0,
            last_source_qpc: 0,
            pending_drop: FrameRange::default(),
            slots: SlotRing::new((0..slot_capacity).map(|_| Slot::new()).collect()),
            tail_slots: SlotRing::new((0..slot_capacity).map(|_| Slot::new()).collect()),
            tail_end: 0,
            volume_gain: 1.0,
            volume_from: 1.0,
            volume_target: 1.0,
            volume_start: 0,
            volume_transitions: [VolumeTransition::default(); VOLUME_TRANSITIONS],
            volume_transition_len: 0,
            volume_transitions_coalesced: 0,
        }
    }

    fn volume_ramp_gain(from: f32, target: f32, start: u64, frame: u64) -> f32 {
        if from == target || frame <= start {
            return if frame <= start { from } else { target };
        }
        let elapsed = frame.saturating_sub(start).min(VOLUME_RAMP_FRAMES);
        if elapsed == VOLUME_RAMP_FRAMES {
            target
        } else {
            Ramp {
                from,
                to: target,
                frames: VOLUME_RAMP_FRAMES as usize,
            }
            .gain(elapsed as usize - 1)
        }
    }

    fn gain_at(&self, frame: u64) -> f32 {
        let mut from = self.volume_from;
        let mut target = self.volume_target;
        let mut start = self.volume_start;
        for transition in &self.volume_transitions[..self.volume_transition_len] {
            if transition.frame > frame {
                break;
            }
            from = Self::volume_ramp_gain(from, target, start, transition.frame);
            target = transition.gain;
            start = transition.frame;
        }
        Self::volume_ramp_gain(from, target, start, frame)
    }

    fn set_volume(&mut self, gain: f32, frame: u64) {
        if self.volume_transition_len == VOLUME_TRANSITIONS {
            // Sixteen entries cover all volume records that can arrive within
            // the admitted arrival slack plus one maximum decoder block. If a
            // malformed producer still exceeds it, merge the chronologically
            // newest pair: the later frame and gain win, while earlier segments
            // remain unchanged.
            self.volume_transitions_coalesced = self.volume_transitions_coalesced.saturating_add(1);
            let mut index = self.volume_transition_len;
            while index > 0 && self.volume_transitions[index - 1].frame > frame {
                index -= 1;
            }
            if index >= self.volume_transition_len - 2 {
                let newest = self.volume_transitions[self.volume_transition_len - 1];
                if frame >= newest.frame {
                    self.volume_transitions[self.volume_transition_len - 1] =
                        VolumeTransition { frame, gain };
                }
                return;
            }
            self.volume_transitions
                .copy_within(index..self.volume_transition_len - 2, index + 1);
            self.volume_transitions[index] = VolumeTransition { frame, gain };
            return;
        }
        let mut index = self.volume_transition_len;
        while index > 0 && self.volume_transitions[index - 1].frame > frame {
            self.volume_transitions[index] = self.volume_transitions[index - 1];
            index -= 1;
        }
        self.volume_transitions[index] = VolumeTransition { frame, gain };
        self.volume_transition_len += 1;
    }

    fn retire_volume_transitions(&mut self, frame: u64) {
        let mut consumed = 0;
        while consumed < self.volume_transition_len
            && self.volume_transitions[consumed].frame <= frame
        {
            let transition = self.volume_transitions[consumed];
            self.volume_from = Self::volume_ramp_gain(
                self.volume_from,
                self.volume_target,
                self.volume_start,
                transition.frame,
            );
            self.volume_target = transition.gain;
            self.volume_start = transition.frame;
            consumed += 1;
        }
        if consumed != 0 {
            self.volume_transitions
                .copy_within(consumed..self.volume_transition_len, 0);
            self.volume_transition_len -= consumed;
        }
        self.volume_gain = self.gain_at(frame);
    }

    fn begin(&mut self, origin: u64) {
        self.state = LaneState::Active;
        self.origin = origin;
        self.transport = 0;
        self.delay = None;
        self.produced = 0;
        self.placed_end = origin;
        self.generation_end = origin;
        self.fade_in_remaining = 0;
        self.rate = 0;
        self.phase_numerator = 0;
        self.history.fill(0.0);
        self.pan_valid = false;
        self.unit_valid.fill(false);
        self.previous_object_count = 0;
        self.last_object_count = 0;
        self.last_has_lfe = false;
        self.last_epoch = 0;
        self.last_source_qpc = 0;
        self.pending_drop = FrameRange::default();
    }

    /// Moves the un-emitted part of the ended generation out of the current
    /// ring. Empty positions become underruns here, while the generation still
    /// supplies the bounds needed to distinguish them from unoccupied time.
    fn transfer_current_to_tail(&mut self, frontier: u64) -> (u64, u64) {
        if self.slots.is_empty() {
            return (0, 0);
        }
        let generation_start = self.origin.saturating_add(self.delay.unwrap_or(0));
        let occupied_start = generation_start.max(frontier);
        let mut underruns = 0_u64;
        for frame in occupied_start..self.placed_end {
            let interval = frame / MIX_FRAMES as u64;
            let offset = (frame % MIX_FRAMES as u64) as usize;
            let state = self
                .slot(interval)
                .map_or(Occupancy::Empty, |slot| slot.occupancy[offset]);
            if state == Occupancy::Empty
                && !(self.pending_drop.start..self.pending_drop.end).contains(&frame)
            {
                underruns = underruns.saturating_add(1);
            }
        }

        let tail_is_empty = !self.tail_slots.iter().any(Slot::has_frames);
        if tail_is_empty {
            std::mem::swap(&mut self.slots, &mut self.tail_slots);
            for slot in &mut self.slots {
                slot.clear();
            }
        } else {
            for source in &mut self.slots {
                if !source.valid {
                    continue;
                }
                let target = self
                    .tail_slots
                    .entry(source.interval)
                    .expect("an enabled lane has tail slots");
                if !target.contains(source.interval) {
                    target.reset_for(source.interval, source.directions, source.layout_epoch);
                }
                debug_assert_eq!(target.layout_epoch, source.layout_epoch);
                debug_assert_eq!(target.directions, source.directions);
                for offset in source.filled_start..source.filled_end {
                    if source.occupancy[offset] != Occupancy::Filled {
                        continue;
                    }
                    for direction in 0..source.directions {
                        let sample = direction * MIX_FRAMES + offset;
                        target.bed[sample] += source.bed[sample];
                    }
                    target.lfe[offset] += source.lfe[offset];
                    target.occupancy[offset] = Occupancy::Filled;
                    target.filled_start = target.filled_start.min(offset);
                    target.filled_end = target.filled_end.max(offset + 1);
                }
                target.has_lfe |= source.has_lfe;
                if target.scene_epoch == 0 {
                    target.scene_epoch = source.scene_epoch;
                    target.source_qpc = source.source_qpc;
                }
                source.clear();
            }
        }
        self.tail_end = self.tail_end.max(self.placed_end);
        // Emitted occupied positions were already observed by `emit_next`; only
        // transfer-finalised Empty positions are added to the test ledger here.
        (underruns, underruns)
    }

    fn remember_pending_drop(&mut self, start: u64, end: u64) {
        if start >= end {
            return;
        }
        if self.pending_drop.start == self.pending_drop.end {
            self.pending_drop = FrameRange { start, end };
            return;
        }
        // `produced` only advances, so a later drop starts at or beyond this
        // range. Successfully placed frames may separate the two; covering that
        // gap is deliberately conservative and prevents a discarded position
        // from being counted again as an underrun.
        debug_assert!(start >= self.pending_drop.start);
        self.pending_drop.start = self.pending_drop.start.min(start);
        self.pending_drop.end = self.pending_drop.end.max(end);
    }

    fn mark_drop(&mut self, start: u64, end: u64) {
        if start >= end {
            return;
        }
        let mut covered_end = start;
        for slot in &mut self.slots {
            if !slot.valid {
                continue;
            }
            let slot_start = slot.interval.saturating_mul(MIX_FRAMES as u64);
            let slot_end = slot_start.saturating_add(MIX_FRAMES as u64);
            let overlap_start = start.max(slot_start);
            let overlap_end = end.min(slot_end);
            if overlap_start >= overlap_end {
                continue;
            }
            let first = (overlap_start - slot_start) as usize;
            let last = (overlap_end - slot_start) as usize;
            for state in &mut slot.occupancy[first..last] {
                if *state == Occupancy::Empty {
                    *state = Occupancy::Dropped;
                }
            }
            covered_end = covered_end.max(overlap_end);
        }
        // An absent interval is represented by the pending range and applied
        // when that ring slot is prepared or emitted. Keep the full uncovered
        // suffix; existing slots may be disjoint in ring iteration order.
        if covered_end < end {
            self.remember_pending_drop(start, end);
        }
    }

    fn apply_pending_drop(&mut self, interval: u64) {
        if self.slots.is_empty() || self.pending_drop.start == self.pending_drop.end {
            return;
        }
        let slot_start = interval.saturating_mul(MIX_FRAMES as u64);
        let slot_end = slot_start.saturating_add(MIX_FRAMES as u64);
        let overlap_start = self.pending_drop.start.max(slot_start);
        let overlap_end = self.pending_drop.end.min(slot_end);
        if overlap_start < overlap_end {
            let first = (overlap_start - slot_start) as usize;
            let last = (overlap_end - slot_start) as usize;
            let slot = self
                .slots
                .entry(interval)
                .expect("an enabled lane has current slots");
            for state in &mut slot.occupancy[first..last] {
                if *state == Occupancy::Empty {
                    *state = Occupancy::Dropped;
                }
            }
        }
        if overlap_start < overlap_end {
            if self.pending_drop.end <= slot_end {
                self.pending_drop = FrameRange::default();
            } else {
                self.pending_drop.start = slot_end;
            }
        }
    }

    fn slot(&self, interval: u64) -> Option<&Slot> {
        self.slots.get(interval)
    }

    fn slot_mut(
        &mut self,
        interval: u64,
        emitted_frontier: u64,
        directions: usize,
        layout_epoch: u32,
    ) -> Option<&mut Slot> {
        if self.slots.get(interval).is_none() {
            let slot = self.slots.entry(interval)?;
            let previous_end = slot
                .interval
                .saturating_add(1)
                .saturating_mul(MIX_FRAMES as u64);
            let reusable = !slot.valid || previous_end <= emitted_frontier;
            if !reusable {
                return None;
            }
            slot.reset_for(interval, directions, layout_epoch);
            self.apply_pending_drop(interval);
        }
        self.slots.get_mut(interval)
    }

    fn tail_slot(&self, interval: u64) -> Option<&Slot> {
        self.tail_slots.get(interval)
    }

    fn clear_interval(&mut self, interval: u64) {
        if let Some(slot) = self.slots.get_mut(interval) {
            slot.clear();
        }
    }

    fn clear_tail_interval(&mut self, interval: u64) {
        if let Some(slot) = self.tail_slots.get_mut(interval) {
            slot.clear();
        }
    }

    fn clear_from(&mut self, timeline_frame: u64) -> u64 {
        let mut dropped = 0_u64;
        for slot in &mut self.slots {
            if !slot.valid {
                continue;
            }
            let start = slot.interval.saturating_mul(MIX_FRAMES as u64);
            let end = start.saturating_add(MIX_FRAMES as u64);
            if end <= timeline_frame {
                continue;
            }
            let first = timeline_frame.saturating_sub(start).min(MIX_FRAMES as u64) as usize;
            for direction in 0..slot.directions {
                slot.bed[direction * MIX_FRAMES + first..(direction + 1) * MIX_FRAMES].fill(0.0);
            }
            slot.lfe[first..].fill(0.0);
            for state in &mut slot.occupancy[first..] {
                if *state == Occupancy::Filled {
                    *state = Occupancy::Dropped;
                    dropped = dropped.saturating_add(1);
                }
            }
            slot.refresh_filled_range();
        }
        self.placed_end = self.placed_end.min(timeline_frame);
        self.generation_end = self.generation_end.min(timeline_frame);
        dropped
    }

    fn fade_out(&mut self, start: u64, end: u64) {
        let length = end.saturating_sub(start);
        if length == 0 {
            return;
        }
        for frame in start..end {
            let interval = frame / MIX_FRAMES as u64;
            let offset = (frame % MIX_FRAMES as u64) as usize;
            let scale = discontinuity_fade_out_gain((frame - start) as usize, length as usize);
            let Some(slot) = self.slots.get_mut(interval) else {
                continue;
            };
            for direction in 0..slot.directions {
                slot.bed[direction * MIX_FRAMES + offset] *= scale;
            }
            slot.lfe[offset] *= scale;
        }
    }
}

#[derive(Debug)]
struct Conversion {
    from_epoch: u32,
    from_directions: usize,
    matrix: Vec<f32>,
}

impl Conversion {
    fn new() -> Self {
        Self {
            from_epoch: 0,
            from_directions: 0,
            matrix: vec![0.0; MATRIX_VALUES],
        }
    }
}

/// One mixed speaker-bed interval.
#[derive(Clone, Copy, Debug)]
pub struct BedBlock<'a> {
    pub epoch: u32,
    pub rate: u32,
    pub frames: usize,
    pub source_qpc: u64,
    pub directions: usize,
    pub channels: &'a [f32],
    pub lfe: Option<&'a [f32]>,
    /// No lane placed sound in this interval; the bed is exactly zero.
    pub silent: bool,
    /// Emitted by [`SceneMixer::idle_to`] rather than an input lane clock.
    pub idle: bool,
}

/// Preallocated common-rate scene mixer for all render endpoints.
#[derive(Debug)]
pub struct SceneMixer {
    config: MixerConfig,
    lanes: Box<[Lane]>,
    kernels: Kernels,
    now: u64,
    next_t: Option<u64>,
    mixed_bed: Vec<f32>,
    mixed_lfe: Vec<f32>,
    target_gains: Vec<f32>,
    frame_gains: Vec<f32>,
    resample_scratch: Vec<f32>,
    active_directions: Vec<usize>,
    active_direction_counts: [usize; MAX_OBJECTS],
    #[cfg(test)]
    direction_updates: u64,
    layout_epoch: u32,
    directions: usize,
    conversions: [Conversion; CONVERSION_RING],
    conversion_count: usize,
    matrix_scratch: Vec<f32>,
    slot_scratch: Vec<f32>,
    last_source_qpc: Option<u64>,
    pending_lanes: u32,
    /// Lanes with data still to come in the READ being processed, starting
    /// before the moment just processed ([`SceneMixer::set_waiting`]).
    waiting: u32,
    lane_underrun_frames: u64,
    dropped_frames: u64,
    #[cfg(test)]
    emitted_filled_frames: [u64; MAX_ENDPOINTS],
    #[cfg(test)]
    lane_dropped_frames: [u64; MAX_ENDPOINTS],
    #[cfg(test)]
    lane_underruns: [u64; MAX_ENDPOINTS],
    #[cfg(test)]
    occupied_frames: [u64; MAX_ENDPOINTS],
}

impl SceneMixer {
    pub fn new(config: MixerConfig) -> Self {
        let config = MixerConfig {
            arrival_slack_frames: config.arrival_slack_frames.min(MAX_ARRIVAL_SLACK_FRAMES),
            ..config
        };
        let retained = MAX_DELAY_FRAMES
            .saturating_add(config.arrival_slack_frames)
            .saturating_add(MAX_SCENE_FRAMES)
            .saturating_add(2 * MIX_FRAMES as u64);
        // The delay, admitted arrival slack, largest decoded scene, and two
        // output intervals are simultaneously retained. The extra slot keeps a
        // just-emitted interval from aliasing the furthest admissible write.
        let slot_capacity = retained.div_ceil(MIX_FRAMES as u64) as usize + 1;
        Self {
            config,
            lanes: (0..MAX_ENDPOINTS)
                .map(|index| {
                    Lane::new(if config.endpoint_mask & (1_u32 << index) != 0 {
                        slot_capacity
                    } else {
                        0
                    })
                })
                .collect(),
            kernels: Kernels::new(),
            now: 0,
            next_t: None,
            mixed_bed: vec![0.0; MAX_DIRECTIONS * MIX_FRAMES],
            mixed_lfe: vec![0.0; MIX_FRAMES],
            target_gains: vec![0.0; MAX_OBJECTS * MAX_DIRECTIONS],
            frame_gains: vec![0.0; MAX_OBJECTS * MAX_DIRECTIONS],
            resample_scratch: vec![0.0; CHANNELS * MAX_OUTPUT_FRAMES],
            active_directions: vec![0; MAX_OBJECTS * MAX_DIRECTIONS],
            active_direction_counts: [0; MAX_OBJECTS],
            #[cfg(test)]
            direction_updates: 0,
            layout_epoch: 0,
            directions: 0,
            conversions: std::array::from_fn(|_| Conversion::new()),
            conversion_count: 0,
            matrix_scratch: vec![0.0; MATRIX_VALUES],
            slot_scratch: vec![0.0; MAX_DIRECTIONS * MIX_FRAMES],
            last_source_qpc: None,
            pending_lanes: 0,
            waiting: 0,
            lane_underrun_frames: 0,
            dropped_frames: 0,
            #[cfg(test)]
            emitted_filled_frames: [0; MAX_ENDPOINTS],
            #[cfg(test)]
            lane_dropped_frames: [0; MAX_ENDPOINTS],
            #[cfg(test)]
            lane_underruns: [0; MAX_ENDPOINTS],
            #[cfg(test)]
            occupied_frames: [0; MAX_ENDPOINTS],
        }
    }

    /// Changes the speaker bed without losing pending timeline frames.
    pub fn change_layout(&mut self, old: Option<&PanGeometry>, new: &PanGeometry) {
        if old.is_some_and(|old| same_layout(old, new)) {
            return;
        }
        let old_epoch = self.layout_epoch;
        let old_directions = old.map_or(0, PanGeometry::directions);
        let new_directions = new.directions();
        self.layout_epoch = self.layout_epoch.wrapping_add(1);
        self.directions = new_directions;
        for lane in &mut self.lanes {
            lane.pan_valid = false;
            lane.unit_valid.fill(false);
            lane.previous_object_count = 0;
        }

        let Some(old) = old else {
            self.conversion_count = 0;
            for lane in &mut self.lanes {
                for slot in lane.slots.iter_mut().chain(&mut lane.tail_slots) {
                    if slot.valid {
                        slot.bed[..new_directions * MIX_FRAMES].fill(0.0);
                        slot.lfe.fill(0.0);
                        slot.has_lfe = false;
                        slot.directions = new_directions;
                        slot.layout_epoch = self.layout_epoch;
                    }
                }
            }
            return;
        };

        self.matrix_scratch.fill(0.0);
        let mut row = [0.0_f32; MAX_DIRECTIONS];
        for old_direction in 0..old_directions {
            new.gains(
                old.direction_position(old_direction),
                0.0,
                &mut row[..new_directions],
            );
            for (new_direction, &gain) in row[..new_directions].iter().enumerate() {
                self.matrix_scratch[new_direction * MAX_DIRECTIONS + old_direction] = gain;
            }
        }

        for conversion in &mut self.conversions[..self.conversion_count] {
            compose_conversion(
                &self.matrix_scratch,
                new_directions,
                old_directions,
                &mut conversion.matrix,
                conversion.from_directions,
                &mut self.mixed_bed[..MATRIX_VALUES],
            );
            conversion.matrix[..MATRIX_VALUES].copy_from_slice(&self.mixed_bed[..MATRIX_VALUES]);
        }

        if self.conversion_count == CONVERSION_RING {
            let oldest_epoch = self.conversions[0].from_epoch;
            for lane in &mut self.lanes {
                for slot in lane.slots.iter_mut().chain(&mut lane.tail_slots) {
                    if slot.valid && slot.layout_epoch == oldest_epoch {
                        convert_slot(
                            slot,
                            &self.conversions[0].matrix,
                            new_directions,
                            self.layout_epoch,
                            &mut self.slot_scratch,
                        );
                    }
                }
            }
            self.conversions.rotate_left(1);
            self.conversion_count -= 1;
        }

        let conversion = &mut self.conversions[self.conversion_count];
        conversion.from_epoch = old_epoch;
        conversion.from_directions = old_directions;
        conversion.matrix.fill(0.0);
        conversion.matrix[..MATRIX_VALUES].copy_from_slice(&self.matrix_scratch);
        self.conversion_count += 1;
    }

    pub fn lane_start(&mut self, endpoint: u16, start_offset_frames: i64) {
        let index = endpoint as usize;
        if index >= MAX_ENDPOINTS || !self.lanes[index].enabled() {
            return;
        }
        self.pending_lanes |= 1_u32 << index;
        let frontier = self.next_t.unwrap_or(0);
        let floor = frontier.max(self.lane_clock(index));
        let upper = self.now.saturating_add(MIX_FRAMES as u64).max(floor);
        let origin = offset_frame(self.now, start_offset_frames).clamp(floor, upper);
        if self.lanes[index].state != LaneState::Idle {
            let (underruns, occupied) = self.lanes[index].transfer_current_to_tail(frontier);
            self.lane_underrun_frames = self.lane_underrun_frames.saturating_add(underruns);
            #[cfg(test)]
            {
                self.lane_underruns[index] = self.lane_underruns[index].saturating_add(underruns);
                self.occupied_frames[index] = self.occupied_frames[index].saturating_add(occupied);
            }
            #[cfg(not(test))]
            let _ = occupied;
        }
        self.lanes[index].begin(origin);
        if self.next_t.is_none() {
            self.next_t = Some(origin / MIX_FRAMES as u64 * MIX_FRAMES as u64);
        }
    }

    pub fn push_scene(
        &mut self,
        endpoint: u16,
        scene: &SceneBlock<'_>,
        geometry: Option<&PanGeometry>,
    ) -> Result<(), u32> {
        let lane_index = endpoint as usize;
        if lane_index >= MAX_ENDPOINTS || !self.lanes[lane_index].enabled() {
            return Ok(());
        }
        if rate_ratio(scene.rate).is_none() {
            return Err(scene.rate);
        }
        if self.lanes[lane_index].state == LaneState::Idle {
            self.lane_start(endpoint, 0);
        }
        if self.directions == 0
            && let Some(geometry) = geometry
        {
            self.change_layout(None, geometry);
        }
        let geometry = geometry.filter(|geometry| geometry.directions() == self.directions);
        let object_count = scene.objects.len().min(MAX_OBJECTS);
        // MAX_OBJECTS is the renderer precision limit, not an audio-drop policy:
        // excess metadata objects are ignored and do not affect drop statistics.

        self.ensure_delay(lane_index);
        if self.lanes[lane_index].rate != 0 && self.lanes[lane_index].rate != scene.rate {
            self.lanes[lane_index].phase_numerator = 0;
            self.lanes[lane_index].history.fill(0.0);
        }
        self.lanes[lane_index].rate = scene.rate;

        let directions = self.directions;
        self.target_gains.fill(0.0);
        if let Some(geometry) = geometry {
            let lane = &mut self.lanes[lane_index];
            for (object_index, object) in scene.objects[..object_count].iter().enumerate() {
                let start = object_index * MAX_DIRECTIONS;
                let end = start + directions;
                let same_position = object.position.x.to_bits()
                    == lane.last_positions[object_index].x.to_bits()
                    && object.position.y.to_bits() == lane.last_positions[object_index].y.to_bits()
                    && object.position.z.to_bits() == lane.last_positions[object_index].z.to_bits();
                let same_spread =
                    object.spread.to_bits() == lane.last_spreads[object_index].to_bits();
                if !lane.unit_valid[object_index] || !same_position || !same_spread {
                    geometry.gains(
                        object.position,
                        object.spread,
                        &mut lane.unit_gains[start..end],
                    );
                    lane.unit_valid[object_index] = true;
                }
                for (target, unit) in self.target_gains[start..end]
                    .iter_mut()
                    .zip(&lane.unit_gains[start..end])
                {
                    *target = *unit * object.gain;
                }
            }
        }
        let mut numerator = self.lanes[lane_index].phase_numerator;
        let block_numerator = (scene.frames as u64).saturating_mul(u64::from(MIX_RATE));
        let output_frames = if numerator < block_numerator {
            ((block_numerator - numerator - 1) / u64::from(scene.rate) + 1) as usize
        } else {
            0
        };
        let ramp_frames = FADE_FRAMES.min(output_frames);
        let can_ramp = geometry.is_some() && self.lanes[lane_index].pan_valid;
        self.prepare_active_directions(lane_index, object_count, directions);

        // `ahead_limit` is both the retained-ring boundary and the admitted
        // lead policy. Preflight the one contiguous range below it before doing
        // FIR work, so a rejected suffix is counted once and never resampled.
        let mut preflight_numerator = numerator;
        let mut preflight_produced = self.lanes[lane_index].produced;
        let mut placeable = 0_usize;
        while preflight_numerator < block_numerator {
            let target = self.timeline_target(lane_index, preflight_produced);
            if target >= self.ahead_limit(lane_index) {
                break;
            }
            let interval = target / MIX_FRAMES as u64;
            let frontier = self.next_t.unwrap_or(target);
            if !self.ensure_slot_current(lane_index, interval, frontier) {
                break;
            }
            placeable += 1;
            preflight_produced = preflight_produced.saturating_add(1);
            preflight_numerator = preflight_numerator.saturating_add(u64::from(scene.rate));
        }

        // A caller may present a larger block than the decoded-scene bound.
        // Preserve the single preflight decision, then reuse the bounded scratch
        // in chunks rather than turning its capacity into an audio-drop policy.
        let mut placed = 0_usize;
        while placed < placeable {
            let chunk = (placeable - placed).min(MAX_OUTPUT_FRAMES);
            if geometry.is_some() {
                self.resample_placeable(lane_index, scene, numerator, chunk, object_count);
            }
            for scratch_frame in 0..chunk {
                let output_frame = placed + scratch_frame;
                let source_frame = (numerator / u64::from(MIX_RATE)) as usize;
                let produced = self.lanes[lane_index].produced;
                let target = self.timeline_target(lane_index, produced);
                if !self.place_frame(
                    lane_index,
                    scene,
                    source_frame,
                    target,
                    output_frame,
                    scratch_frame,
                    ramp_frames,
                    can_ramp,
                    geometry.is_some(),
                    object_count,
                ) {
                    debug_assert!(false, "preflighted slot disappeared");
                    break;
                }
                self.lanes[lane_index].produced = produced.saturating_add(1);
                let lane = &mut self.lanes[lane_index];
                lane.placed_end = lane.placed_end.max(target.saturating_add(1));
                lane.generation_end = lane.generation_end.max(target.saturating_add(1));
                numerator = numerator.saturating_add(u64::from(scene.rate));
            }
            placed += chunk;
        }
        debug_assert_eq!(placed, placeable);
        let remaining = output_frames.saturating_sub(placeable) as u64;
        if remaining != 0 {
            self.drop_tail(lane_index, remaining);
            self.lanes[lane_index].produced =
                self.lanes[lane_index].produced.saturating_add(remaining);
            numerator = numerator.saturating_add(remaining.saturating_mul(u64::from(scene.rate)));
        }
        self.lanes[lane_index].phase_numerator = numerator.saturating_sub(block_numerator);
        update_history(&mut self.lanes[lane_index], scene);
        let lane = &mut self.lanes[lane_index];
        lane.last_object_count = object_count;
        lane.last_has_lfe = scene.lfe.is_some();
        lane.last_epoch = scene.epoch;
        lane.last_source_qpc = scene.source_qpc;
        for (index, object) in scene.objects[..object_count].iter().enumerate() {
            lane.last_positions[index] = object.position;
            lane.last_gains[index] = object.gain;
            lane.last_spreads[index] = object.spread;
        }
        if geometry.is_some() {
            for object in 0..object_count {
                let start = object * MAX_DIRECTIONS;
                lane.pan_gains[start..start + directions]
                    .copy_from_slice(&self.target_gains[start..start + directions]);
            }
            lane.pan_valid = true;
            lane.previous_object_count = object_count;
        } else {
            lane.pan_valid = false;
            lane.previous_object_count = 0;
        }
        Ok(())
    }

    pub fn flush_lane(&mut self, endpoint: u16, geometry: Option<&PanGeometry>) -> Result<(), u32> {
        let lane_index = endpoint as usize;
        if lane_index >= MAX_ENDPOINTS
            || self.lanes[lane_index].state == LaneState::Idle
            || self.lanes[lane_index].rate == 0
        {
            return Ok(());
        }
        let rate = self.lanes[lane_index].rate;
        self.lanes[lane_index].rate = 0;
        if rate == MIX_RATE {
            return Ok(());
        }
        let zeros = [0.0_f32; MAX_RESAMPLE_TAPS];
        let mut objects = [Object::SILENT; MAX_OBJECTS];
        let lane = &self.lanes[lane_index];
        let taps = self
            .kernels
            .for_rate(rate)
            .expect("a lane only retains a supported rate")
            .taps;
        for (index, object) in objects[..lane.last_object_count].iter_mut().enumerate() {
            *object = Object {
                signal: &zeros,
                position: lane.last_positions[index],
                gain: lane.last_gains[index],
                spread: lane.last_spreads[index],
            };
        }
        let scene = SceneBlock {
            epoch: lane.last_epoch,
            frames: taps,
            rate,
            source_qpc: lane.last_source_qpc,
            objects: &objects[..lane.last_object_count],
            lfe: lane.last_has_lfe.then_some(zeros.as_slice()),
            new_objects: 0,
        };
        let generation_end = self.lanes[lane_index].generation_end;
        let result = self.push_scene(endpoint, &scene, geometry);
        let lane = &mut self.lanes[lane_index];
        lane.rate = 0;
        lane.generation_end = generation_end;
        result
    }

    /// Schedules an endpoint gain at an absolute timeline frame. Gain is applied
    /// while emitting, not while placing, so already queued current and tail
    /// samples at or after that frame observe the change.
    pub fn set_lane_volume(&mut self, endpoint: u16, gain: f32, frame: u64) {
        let index = endpoint as usize;
        if index >= MAX_ENDPOINTS || !self.lanes[index].enabled() {
            return;
        }
        let frontier = self.next_t.unwrap_or(self.now);
        self.lanes[index].set_volume(gain, frame.max(frontier));
    }

    pub fn advance(
        &mut self,
        endpoint: u16,
        transport_frames: u64,
        out: &mut dyn FnMut(&BedBlock<'_>),
    ) {
        let index = endpoint as usize;
        if index >= MAX_ENDPOINTS || !self.lanes[index].enabled() {
            return;
        }
        if self.lanes[index].state == LaneState::Idle {
            self.lane_start(endpoint, 0);
        }
        let lane = &mut self.lanes[index];
        lane.transport = lane.transport.saturating_add(transport_frames);
        self.pending_lanes &= !(1_u32 << index);
        self.now = self.now.max(lane.origin.saturating_add(lane.transport));
        self.emit_ready(out);
        self.retire_drained();
    }

    /// Names the lanes (bit `i` for endpoint `i`) whose next data in the READ
    /// being processed starts before the moment just processed. An interval
    /// past its arrival slack that one of them has not filled yet waits for
    /// it instead of counting it late: the driver writes every endpoint up to
    /// the same QPC, and a late DMA tick puts several milliseconds of each in
    /// one READ, processed endpoint by endpoint. A lane whose next data starts
    /// later (it stalled) is not waited for.
    pub fn set_waiting(&mut self, lanes: u32) {
        self.waiting = lanes;
    }

    /// Ends a READ: nothing more is coming, so what is late leaves now.
    pub fn release_waiting(&mut self, out: &mut dyn FnMut(&BedBlock<'_>)) {
        self.waiting = 0;
        self.emit_ready(out);
        self.retire_drained();
    }

    fn waiting_lane_short_of(&self, end: u64) -> bool {
        self.lanes.iter().enumerate().any(|(index, lane)| {
            self.waiting & (1_u32 << index) != 0
                && lane.state == LaneState::Active
                && !(lane.delay.is_some() && lane.generation_end >= end)
        })
    }

    pub fn idle_to(&mut self, timeline_frame: u64, out: &mut dyn FnMut(&BedBlock<'_>)) {
        if self.has_clocked_lane() {
            return;
        }
        self.now = self.now.max(timeline_frame);
        if self.next_t.is_none() {
            self.next_t = Some(timeline_frame / MIX_FRAMES as u64 * MIX_FRAMES as u64);
            return;
        }
        while self
            .next_t
            .is_some_and(|next| next.saturating_add(MIX_FRAMES as u64) <= self.now)
        {
            self.emit_next(out, true);
        }
        self.retire_drained();
    }

    pub fn lane_stop(&mut self, endpoint: u16, out: &mut dyn FnMut(&BedBlock<'_>)) {
        let index = endpoint as usize;
        if index >= MAX_ENDPOINTS
            || !self.lanes[index].enabled()
            || self.lanes[index].state == LaneState::Idle
        {
            return;
        }
        self.pending_lanes &= !(1_u32 << index);
        self.lanes[index].state = LaneState::Draining;
        // Drain at once only when no other lane is active. An active lane that
        // has not advanced yet usually has its first data a moment away (a
        // start record, then data); draining now would emit the partial
        // interval it is about to fill and push its start back by up to an
        // interval. One that never advances (no decoder for its format) holds
        // the tail only until the idle clock, which ignores lanes that are not
        // clocked, plays it out in real time; offline callers without the idle
        // clock release it at the next activity or at `finish`.
        let another_active = self
            .lanes
            .iter()
            .enumerate()
            .any(|(other, lane)| other != index && lane.state == LaneState::Active);
        if !another_active {
            self.drain_remaining(out);
        }
        self.retire_drained();
    }

    pub fn lane_discontinuity(&mut self, endpoint: u16, restart_offset_frames: i64) {
        let index = endpoint as usize;
        if index >= MAX_ENDPOINTS || !self.lanes[index].enabled() {
            return;
        }
        self.pending_lanes |= 1_u32 << index;
        let state = self.lanes[index].state;
        let was_active = state != LaneState::Idle;
        let frontier = self.next_t.unwrap_or(self.now);
        let fade_end = if state == LaneState::Active {
            let old_end = self.lanes[index].placed_end;
            let fade_end = old_end.min(frontier.saturating_add(FADE_FRAMES as u64));
            self.lanes[index].fade_out(frontier, fade_end);
            let dropped = self.lanes[index].clear_from(fade_end);
            self.dropped_frames = self.dropped_frames.saturating_add(dropped);
            fade_end
        } else {
            frontier
        };
        let floor = frontier.max(self.lane_clock(index));
        let upper = self.now.saturating_add(MIX_FRAMES as u64).max(floor);
        let origin = offset_frame(self.now, restart_offset_frames)
            .clamp(floor, upper)
            .max(fade_end);
        if was_active {
            let (underruns, occupied) = self.lanes[index].transfer_current_to_tail(frontier);
            self.lane_underrun_frames = self.lane_underrun_frames.saturating_add(underruns);
            #[cfg(test)]
            {
                self.lane_underruns[index] = self.lane_underruns[index].saturating_add(underruns);
                self.occupied_frames[index] = self.occupied_frames[index].saturating_add(occupied);
            }
            #[cfg(not(test))]
            let _ = occupied;
            self.lanes[index].begin(origin);
        } else {
            let lane = &mut self.lanes[index];
            lane.origin = origin;
            lane.transport = 0;
            lane.delay = None;
            lane.produced = 0;
            lane.rate = 0;
            lane.phase_numerator = 0;
            lane.history.fill(0.0);
            lane.pan_valid = false;
            lane.previous_object_count = 0;
            lane.pending_drop = FrameRange::default();
        }
    }

    pub fn begin_finish(&mut self) {
        self.pending_lanes = 0;
        for lane in &mut self.lanes {
            if lane.state == LaneState::Active {
                lane.state = LaneState::Draining;
            }
        }
    }

    pub fn is_quiet(&self) -> bool {
        self.lanes.iter().all(|lane| {
            lane.state == LaneState::Idle
                && !lane.slots.iter().any(Slot::has_frames)
                && !lane.tail_slots.iter().any(Slot::has_frames)
        })
    }

    /// Emits at most one pending common-timeline interval.
    pub fn finish_step(&mut self, out: &mut dyn FnMut(&BedBlock<'_>)) -> bool {
        let end = self
            .lanes
            .iter()
            .map(|lane| lane.placed_end.max(lane.tail_end))
            .max()
            .unwrap_or(0);
        if self.next_t.is_some_and(|next| next < end) {
            self.emit_next(out, false);
            return false;
        }
        if let Some(next) = self.next_t {
            self.now = self.now.max(next);
        }
        for lane in &mut self.lanes {
            lane.state = LaneState::Idle;
        }
        true
    }

    pub fn finish(&mut self, out: &mut dyn FnMut(&BedBlock<'_>)) {
        self.begin_finish();
        while !self.finish_step(out) {}
    }

    pub fn lane_underrun_frames(&self) -> u64 {
        self.lane_underrun_frames
    }
    /// 48 kHz frames whose sound the mixer discarded: ahead drops and
    /// discontinuity cuts.
    pub fn dropped_frames(&self) -> u64 {
        self.dropped_frames
    }
    pub fn volume_transitions_coalesced(&self) -> u64 {
        self.lanes
            .iter()
            .map(|lane| lane.volume_transitions_coalesced)
            .sum()
    }
    pub fn now(&self) -> u64 {
        self.now
    }

    pub(crate) fn lane_is_idle(&self, endpoint: u16) -> bool {
        self.lanes
            .get(endpoint as usize)
            .is_none_or(|lane| lane.state == LaneState::Idle)
    }

    pub(crate) fn has_clocked_lane_except(&self, endpoint: u16) -> bool {
        self.lanes
            .iter()
            .enumerate()
            .any(|(index, lane)| index != endpoint as usize && lane.clocked())
    }

    pub(crate) fn has_clocked_lane(&self) -> bool {
        self.lanes.iter().any(Lane::clocked)
    }

    fn lane_clock(&self, index: usize) -> u64 {
        let lane = &self.lanes[index];
        if lane.transport == 0 {
            0
        } else {
            lane.origin.saturating_add(lane.transport)
        }
    }

    fn ensure_delay(&mut self, lane_index: usize) {
        if self.lanes[lane_index].delay.is_some() {
            return;
        }
        let lane = &self.lanes[lane_index];
        let lag = lane.transport.saturating_sub(lane.produced);
        let wanted = lag
            .saturating_add(self.config.arrival_slack_frames)
            .saturating_sub(MIX_FRAMES as u64);
        self.lanes[lane_index].delay = Some(wanted.min(MAX_DELAY_FRAMES));
    }

    fn timeline_target(&mut self, lane_index: usize, produced: u64) -> u64 {
        let lane = &self.lanes[lane_index];
        let target = lane
            .origin
            .saturating_add(lane.delay.unwrap_or(0))
            .saturating_add(produced);
        let frontier = self.next_t.unwrap_or(target);
        if target >= frontier {
            return target;
        }
        let missed = frontier - target;
        let lane = &mut self.lanes[lane_index];
        lane.delay = Some(lane.delay.unwrap_or(0).saturating_add(missed));
        frontier
    }

    /// Exclusive timeline bound for new placements in one lane.
    ///
    /// The smaller of the physical current-ring end and the admitted lead keeps
    /// `slot_mut` from aliasing an un-emitted interval while bounding latency.
    fn ahead_limit(&self, lane_index: usize) -> u64 {
        let frontier = self.next_t.unwrap_or(self.now);
        let ring_end =
            frontier.saturating_add(self.lanes[lane_index].slots.len() as u64 * MIX_FRAMES as u64);
        let policy_end = self
            .now
            .saturating_add(self.config.arrival_slack_frames)
            .saturating_add(MAX_SCENE_FRAMES)
            .saturating_add(2 * MIX_FRAMES as u64);
        ring_end.min(policy_end)
    }

    /// Accounts for the unplaceable suffix and leaves timeline placeholders so
    /// emission cannot count the same positions again as lane underruns.
    fn drop_tail(&mut self, lane_index: usize, frames: u64) {
        self.dropped_frames = self.dropped_frames.saturating_add(frames);
        let drop_start = self.timeline_target(lane_index, self.lanes[lane_index].produced);
        let drop_end = drop_start.saturating_add(frames);
        self.lanes[lane_index].mark_drop(drop_start, drop_end);
        let frontier = self.next_t.unwrap_or(self.now);
        let end = self.lanes[lane_index].generation_end;
        let start = end.saturating_sub(FADE_FRAMES as u64).max(frontier);
        self.lanes[lane_index].fade_out(start, end);
        self.lanes[lane_index].fade_in_remaining = FADE_FRAMES;
    }

    fn prepare_active_directions(
        &mut self,
        lane_index: usize,
        object_count: usize,
        directions: usize,
    ) {
        self.active_direction_counts.fill(0);
        for object in 0..object_count {
            let row = object * MAX_DIRECTIONS;
            let mut count = 0;
            for direction in 0..directions {
                if self.target_gains[row + direction] != 0.0
                    || self.lanes[lane_index].pan_gains[row + direction] != 0.0
                {
                    self.active_directions[row + count] = direction;
                    count += 1;
                }
            }
            self.active_direction_counts[object] = count;
        }
    }

    fn resample_placeable(
        &mut self,
        lane_index: usize,
        scene: &SceneBlock<'_>,
        mut numerator: u64,
        frames: usize,
        object_count: usize,
    ) {
        debug_assert!(frames <= MAX_OUTPUT_FRAMES);
        for output_frame in 0..frames {
            let source_frame = (numerator / u64::from(MIX_RATE)) as usize;
            let fraction = numerator % u64::from(MIX_RATE);
            for object in 0..object_count {
                self.resample_scratch[object * MAX_OUTPUT_FRAMES + output_frame] = sample_channel(
                    &self.lanes[lane_index],
                    &self.kernels,
                    scene,
                    object,
                    source_frame,
                    fraction,
                );
            }
            if scene.lfe.is_some() {
                self.resample_scratch[MAX_OBJECTS * MAX_OUTPUT_FRAMES + output_frame] =
                    sample_channel(
                        &self.lanes[lane_index],
                        &self.kernels,
                        scene,
                        MAX_OBJECTS,
                        source_frame,
                        fraction,
                    );
            }
            numerator = numerator.saturating_add(u64::from(scene.rate));
        }
    }

    /// Places one preflighted 48 kHz frame. `output_frame` is scene-relative
    /// for the single gain ramp; `scratch_frame` is relative to the reused FIR
    /// chunk. The slot must remain available after preflight.
    #[allow(clippy::too_many_arguments)]
    fn place_frame(
        &mut self,
        lane_index: usize,
        scene: &SceneBlock<'_>,
        source_frame: usize,
        target: u64,
        output_frame: usize,
        scratch_frame: usize,
        ramp_frames: usize,
        can_ramp: bool,
        has_geometry: bool,
        object_count: usize,
    ) -> bool {
        let interval = target / MIX_FRAMES as u64;
        let offset = (target % MIX_FRAMES as u64) as usize;
        let qpc = scene.source_qpc.saturating_add(
            (source_frame as u128 * u128::from(self.config.qpc_frequency) / u128::from(scene.rate))
                as u64,
        );
        let frontier = self.next_t.unwrap_or(target);
        let lfe = (has_geometry && scene.lfe.is_some())
            .then(|| self.resample_scratch[MAX_OBJECTS * MAX_OUTPUT_FRAMES + scratch_frame]);
        let fade_scale = if self.lanes[lane_index].fade_in_remaining == 0 {
            1.0
        } else {
            let remaining = self.lanes[lane_index].fade_in_remaining;
            self.lanes[lane_index].fade_in_remaining -= 1;
            Ramp {
                from: 0.0,
                to: 1.0,
                frames: FADE_FRAMES,
            }
            .gain(FADE_FRAMES - remaining)
        };

        if !self.ensure_slot_current(lane_index, interval, frontier) {
            return false;
        }
        let directions = self.directions;
        let previous_count = self.lanes[lane_index].previous_object_count;
        if has_geometry && output_frame < ramp_frames {
            for object in 0..object_count {
                let is_new = !can_ramp
                    || object >= previous_count
                    || scene.new_objects & (1_u64 << object) != 0;
                let row = object * MAX_DIRECTIONS;
                for active in 0..self.active_direction_counts[object] {
                    let direction = self.active_directions[row + active];
                    let target_gain = self.target_gains[row + direction];
                    let old_gain = self.lanes[lane_index].pan_gains[row + direction];
                    self.frame_gains[row + direction] = if is_new {
                        target_gain
                    } else {
                        renderer_pan_ramp_gain(old_gain, target_gain, output_frame, ramp_frames)
                    };
                    #[cfg(test)]
                    {
                        self.direction_updates = self.direction_updates.saturating_add(1);
                    }
                }
            }
        }
        let Some(slot) =
            self.lanes[lane_index].slot_mut(interval, frontier, directions, self.layout_epoch)
        else {
            debug_assert!(false, "slot was prepared");
            return false;
        };
        if !slot.has_frames() {
            slot.scene_epoch = scene.epoch;
            slot.source_qpc = qpc;
        }
        if has_geometry {
            for object in 0..object_count {
                let sample = self.resample_scratch[object * MAX_OUTPUT_FRAMES + scratch_frame];
                let row = object * MAX_DIRECTIONS;
                for active in 0..self.active_direction_counts[object] {
                    let direction = self.active_directions[row + active];
                    let gain = if output_frame < ramp_frames {
                        self.frame_gains[row + direction]
                    } else {
                        self.target_gains[row + direction]
                    };
                    if gain != 0.0 {
                        slot.bed[direction * MIX_FRAMES + offset] += sample * gain * fade_scale;
                    }
                }
            }
        }
        if let Some(sample) = lfe {
            slot.lfe[offset] += sample * fade_scale;
            slot.has_lfe = true;
        }
        slot.occupancy[offset] = if has_geometry {
            Occupancy::Filled
        } else {
            Occupancy::Dropped
        };
        if has_geometry {
            slot.filled_start = slot.filled_start.min(offset);
            slot.filled_end = slot.filled_end.max(offset + 1);
        } else {
            self.dropped_frames = self.dropped_frames.saturating_add(1);
        }
        true
    }

    fn ensure_slot_current(&mut self, lane_index: usize, interval: u64, frontier: u64) -> bool {
        let directions = self.directions;
        let epoch = self.layout_epoch;
        let Some(slot) = self.lanes[lane_index].slot_mut(interval, frontier, directions, epoch)
        else {
            return false;
        };
        if slot.layout_epoch == epoch {
            return true;
        }
        let Some(conversion) = self.conversions[..self.conversion_count]
            .iter()
            .find(|conversion| conversion.from_epoch == slot.layout_epoch)
        else {
            debug_assert!(false, "pending slot has no layout conversion");
            slot.bed[..directions * MIX_FRAMES].fill(0.0);
            slot.directions = directions;
            slot.layout_epoch = epoch;
            return true;
        };
        convert_slot(
            slot,
            &conversion.matrix,
            directions,
            epoch,
            &mut self.slot_scratch,
        );
        true
    }

    fn ensure_tail_slot_current(&mut self, lane_index: usize, interval: u64) {
        let directions = self.directions;
        let epoch = self.layout_epoch;
        let lane = &mut self.lanes[lane_index];
        let Some(slot) = lane.tail_slots.get_mut(interval) else {
            return;
        };
        if slot.layout_epoch == epoch {
            return;
        }
        let Some(conversion) = self.conversions[..self.conversion_count]
            .iter()
            .find(|conversion| conversion.from_epoch == slot.layout_epoch)
        else {
            debug_assert!(false, "pending tail slot has no layout conversion");
            slot.bed[..directions * MIX_FRAMES].fill(0.0);
            slot.directions = directions;
            slot.layout_epoch = epoch;
            return;
        };
        convert_slot(
            slot,
            &conversion.matrix,
            directions,
            epoch,
            &mut self.slot_scratch,
        );
    }

    fn emit_ready(&mut self, out: &mut dyn FnMut(&BedBlock<'_>)) {
        while let Some(next) = self.next_t {
            let end = next.saturating_add(MIX_FRAMES as u64);
            let late = self.now >= end.saturating_add(self.config.arrival_slack_frames)
                && !self.waiting_lane_short_of(end);
            if !late && !(end <= self.now && self.active_lanes_cover(end)) {
                break;
            }
            self.emit_next(out, false);
        }
    }

    fn active_lanes_cover(&self, end: u64) -> bool {
        self.lanes.iter().enumerate().all(|(index, lane)| {
            self.pending_lanes & (1_u32 << index) == 0
                && (!lane.clocked() || (lane.delay.is_some() && lane.generation_end >= end))
        })
    }

    fn drain_remaining(&mut self, out: &mut dyn FnMut(&BedBlock<'_>)) {
        let end = self
            .lanes
            .iter()
            .map(|lane| lane.placed_end.max(lane.tail_end))
            .max()
            .unwrap_or(0);
        while self.next_t.is_some_and(|next| next < end) {
            self.emit_next(out, false);
        }
        if let Some(next) = self.next_t {
            self.now = self.now.max(next);
        }
    }

    /// Emits exactly one common-timeline interval. Within a lane, current sound
    /// is accumulated before ended-generation tail sound; then the interval is
    /// cleared from both rings so neither sound nor accounting can repeat.
    fn emit_next(&mut self, out: &mut dyn FnMut(&BedBlock<'_>), idle: bool) {
        let Some(start) = self.next_t else {
            debug_assert!(false, "emission requires a timeline frontier");
            return;
        };
        let interval = start / MIX_FRAMES as u64;
        self.mixed_bed[..self.directions * MIX_FRAMES].fill(0.0);
        self.mixed_lfe.fill(0.0);
        let mut has_lfe = false;
        let mut chosen: Option<(u64, usize, u64, u32)> = None;

        for lane_index in 0..MAX_ENDPOINTS {
            if !self.lanes[lane_index].enabled() {
                continue;
            }
            if self.lanes[lane_index].slot(interval).is_none()
                && !self.ensure_slot_current(lane_index, interval, start)
            {
                self.count_missing(lane_index, start, MIX_FRAMES as u64);
                continue;
            }
            let _ = self.ensure_slot_current(lane_index, interval, start);
            let Some(slot) = self.lanes[lane_index].slot(interval) else {
                debug_assert!(false, "slot disappeared after preparation");
                self.count_missing(lane_index, start, MIX_FRAMES as u64);
                continue;
            };
            let has_frames = slot.has_frames();
            let slot_has_lfe = slot.has_lfe;
            let source_qpc = slot.source_qpc;
            let scene_epoch = slot.scene_epoch;
            let delay = self.lanes[lane_index].delay.unwrap_or(0);
            let mut run_start = None;
            for offset in 0..MIX_FRAMES {
                let occupancy = self.lanes[lane_index]
                    .slot(interval)
                    .map_or(Occupancy::Empty, |slot| slot.occupancy[offset]);
                #[cfg(test)]
                if self.lane_occupies(lane_index, start + offset as u64) {
                    self.occupied_frames[lane_index] += 1;
                    match occupancy {
                        Occupancy::Filled => self.emitted_filled_frames[lane_index] += 1,
                        Occupancy::Dropped => self.lane_dropped_frames[lane_index] += 1,
                        Occupancy::Empty => {}
                    }
                }
                let empty = occupancy == Occupancy::Empty;
                match (run_start, empty) {
                    (None, true) => run_start = Some(offset),
                    (Some(missing_start), false) => {
                        self.count_missing(
                            lane_index,
                            start + missing_start as u64,
                            (offset - missing_start) as u64,
                        );
                        run_start = None;
                    }
                    _ => {}
                }
            }
            if let Some(missing_start) = run_start {
                self.count_missing(
                    lane_index,
                    start + missing_start as u64,
                    (MIX_FRAMES - missing_start) as u64,
                );
            }
            if has_frames {
                if chosen.is_none_or(|current| (delay, lane_index) < (current.0, current.1)) {
                    chosen = Some((delay, lane_index, source_qpc, scene_epoch));
                }
                if let Some(slot) = self.lanes[lane_index].slot(interval) {
                    for direction in 0..self.directions {
                        let mixed = &mut self.mixed_bed
                            [direction * MIX_FRAMES..(direction + 1) * MIX_FRAMES];
                        let source =
                            &slot.bed[direction * MIX_FRAMES..(direction + 1) * MIX_FRAMES];
                        for (offset, (mixed, source)) in mixed.iter_mut().zip(source).enumerate() {
                            let gain = self.lanes[lane_index].gain_at(start + offset as u64);
                            if gain == 1.0 {
                                *mixed += *source;
                            } else {
                                *mixed += *source * gain;
                            }
                        }
                    }
                    if slot_has_lfe {
                        for (offset, (mixed, source)) in
                            self.mixed_lfe.iter_mut().zip(&slot.lfe).enumerate()
                        {
                            let gain = self.lanes[lane_index].gain_at(start + offset as u64);
                            if gain == 1.0 {
                                *mixed += *source;
                            } else {
                                *mixed += *source * gain;
                            }
                        }
                        has_lfe = true;
                    }
                } else {
                    debug_assert!(false, "slot disappeared before mixing");
                }
            }
            self.lanes[lane_index].clear_interval(interval);

            // Within each lane the current generation is summed first and the
            // ended-generation tail second. This fixed floating-point order is
            // part of the sample-stability contract.
            self.ensure_tail_slot_current(lane_index, interval);
            if let Some(tail) = self.lanes[lane_index].tail_slot(interval)
                && tail.has_frames()
            {
                if chosen.is_none_or(|current| (delay, lane_index) < (current.0, current.1)) {
                    chosen = Some((delay, lane_index, tail.source_qpc, tail.scene_epoch));
                }
                for direction in 0..self.directions {
                    let mixed =
                        &mut self.mixed_bed[direction * MIX_FRAMES..(direction + 1) * MIX_FRAMES];
                    let source = &tail.bed[direction * MIX_FRAMES..(direction + 1) * MIX_FRAMES];
                    for (offset, (mixed, source)) in mixed.iter_mut().zip(source).enumerate() {
                        let gain = self.lanes[lane_index].gain_at(start + offset as u64);
                        if gain == 1.0 {
                            *mixed += *source;
                        } else {
                            *mixed += *source * gain;
                        }
                    }
                }
                if tail.has_lfe {
                    for (offset, (mixed, source)) in
                        self.mixed_lfe.iter_mut().zip(&tail.lfe).enumerate()
                    {
                        let gain = self.lanes[lane_index].gain_at(start + offset as u64);
                        if gain == 1.0 {
                            *mixed += *source;
                        } else {
                            *mixed += *source * gain;
                        }
                    }
                    has_lfe = true;
                }
            }
            self.lanes[lane_index].clear_tail_interval(interval);
            self.lanes[lane_index]
                .retire_volume_transitions(start.saturating_add(MIX_FRAMES as u64));
        }

        // Silence carries the last source time on. Before any audio, or after
        // audio without one, it has none and is stamped 0 (unknown); counting
        // up from 0 made the output read the machine's uptime as latency.
        let (source_qpc, epoch) = if let Some((_, _, qpc, epoch)) = chosen {
            self.last_source_qpc = (qpc != 0).then_some(qpc);
            (qpc, epoch)
        } else if let Some(last) = self.last_source_qpc {
            let step = (MIX_FRAMES as u128 * u128::from(self.config.qpc_frequency)
                / u128::from(MIX_RATE)) as u64;
            let qpc = last.saturating_add(step);
            self.last_source_qpc = Some(qpc);
            (qpc, 0)
        } else {
            (0, 0)
        };
        out(&BedBlock {
            epoch,
            rate: MIX_RATE,
            frames: MIX_FRAMES,
            source_qpc,
            directions: self.directions,
            channels: &self.mixed_bed[..self.directions * MIX_FRAMES],
            lfe: has_lfe.then_some(self.mixed_lfe.as_slice()),
            silent: chosen.is_none(),
            idle,
        });
        let frontier = start.saturating_add(MIX_FRAMES as u64);
        self.next_t = Some(frontier);
    }

    #[cfg(test)]
    fn lane_occupies(&self, lane_index: usize, frame: u64) -> bool {
        let lane = &self.lanes[lane_index];
        if lane.state == LaneState::Idle || lane.delay.is_none() {
            return false;
        }
        let occupied_start = lane.origin.saturating_add(lane.delay.unwrap_or(0));
        let occupied_end = if lane.state == LaneState::Draining {
            lane.placed_end
        } else {
            u64::MAX
        };
        (occupied_start..occupied_end).contains(&frame)
    }

    fn count_missing(&mut self, lane_index: usize, start: u64, frames: u64) {
        if frames == 0 {
            return;
        }
        let lane = &self.lanes[lane_index];
        if lane.state == LaneState::Idle || lane.delay.is_none() {
            return;
        }
        let occupied_start = lane.origin.saturating_add(lane.delay.unwrap_or(0));
        let occupied_end = if lane.state == LaneState::Draining {
            lane.placed_end
        } else {
            u64::MAX
        };
        let end = start.saturating_add(frames);
        let overlap_start = start.max(occupied_start);
        let overlap_end = end.min(occupied_end);
        let missing = overlap_end.saturating_sub(overlap_start);
        self.lane_underrun_frames = self.lane_underrun_frames.saturating_add(missing);
        #[cfg(test)]
        {
            self.lane_underruns[lane_index] =
                self.lane_underruns[lane_index].saturating_add(missing);
        }
    }

    fn retire_drained(&mut self) {
        let frontier = self.next_t.unwrap_or(0);
        for lane in &mut self.lanes {
            if lane.state == LaneState::Draining && frontier >= lane.placed_end.max(lane.tail_end) {
                lane.state = LaneState::Idle;
            }
        }
    }
}

fn same_layout(old: &PanGeometry, new: &PanGeometry) -> bool {
    old.directions() == new.directions()
        && (0..old.directions())
            .all(|index| old.direction_position(index) == new.direction_position(index))
}

fn compose_conversion(
    next: &[f32],
    next_rows: usize,
    middle: usize,
    prior: &mut [f32],
    prior_columns: usize,
    scratch: &mut [f32],
) {
    scratch.fill(0.0);
    for row in 0..next_rows {
        for pivot in 0..middle {
            let left = next[row * MAX_DIRECTIONS + pivot];
            if left == 0.0 {
                continue;
            }
            for column in 0..prior_columns {
                scratch[row * MAX_DIRECTIONS + column] +=
                    left * prior[pivot * MAX_DIRECTIONS + column];
            }
        }
    }
}

fn convert_slot(
    slot: &mut Slot,
    matrix: &[f32],
    new_directions: usize,
    new_epoch: u32,
    scratch: &mut [f32],
) {
    scratch[..new_directions * MIX_FRAMES].fill(0.0);
    for new_direction in 0..new_directions {
        let output = &mut scratch[new_direction * MIX_FRAMES..(new_direction + 1) * MIX_FRAMES];
        for old_direction in 0..slot.directions {
            let gain = matrix[new_direction * MAX_DIRECTIONS + old_direction];
            if gain == 0.0 {
                continue;
            }
            let input = &slot.bed[old_direction * MIX_FRAMES..(old_direction + 1) * MIX_FRAMES];
            for (output, input) in output.iter_mut().zip(input) {
                *output += *input * gain;
            }
        }
    }
    slot.bed[..new_directions * MIX_FRAMES]
        .copy_from_slice(&scratch[..new_directions * MIX_FRAMES]);
    slot.directions = new_directions;
    slot.layout_epoch = new_epoch;
}

fn offset_frame(frame: u64, offset: i64) -> u64 {
    if offset >= 0 {
        frame.saturating_add(offset as u64)
    } else {
        frame.saturating_sub(offset.unsigned_abs())
    }
}

fn source_sample(scene: &SceneBlock<'_>, channel: usize, frame: usize) -> f32 {
    if channel == MAX_OBJECTS {
        scene
            .lfe
            .and_then(|lfe| lfe.get(frame))
            .copied()
            .unwrap_or(0.0)
    } else {
        scene
            .objects
            .get(channel)
            .and_then(|object| object.signal.get(frame))
            .copied()
            .unwrap_or(0.0)
    }
}

fn sample_channel(
    lane: &Lane,
    kernels: &Kernels,
    scene: &SceneBlock<'_>,
    channel: usize,
    source_frame: usize,
    fraction: u64,
) -> f32 {
    if scene.rate == MIX_RATE {
        return source_sample(scene, channel, source_frame);
    }
    let Some(bank) = kernels.for_rate(scene.rate) else {
        debug_assert!(false, "unsupported resampling rate reached the FIR path");
        return 0.0;
    };
    let phase = if bank.phases == 160 {
        ((fraction as u128 * 160_u128) / u128::from(MIX_RATE)) as usize
    } else {
        ((fraction as u128 * bank.phases as u128) / u128::from(MIX_RATE)) as usize
    };
    let kernel = bank.phase(phase.min(bank.phases - 1));
    let history_frames = MAX_RESAMPLE_TAPS - 1;
    let history = &lane.history[channel * history_frames..(channel + 1) * history_frames];
    let mut output = 0.0;
    for (tap, &coefficient) in kernel.iter().enumerate() {
        let sample = if source_frame >= tap {
            source_sample(scene, channel, source_frame - tap)
        } else {
            let before = tap - source_frame;
            if before <= history_frames {
                history[history_frames - before]
            } else {
                0.0
            }
        };
        output += sample * coefficient;
    }
    output
}

fn update_history(lane: &mut Lane, scene: &SceneBlock<'_>) {
    let history_frames = MAX_RESAMPLE_TAPS - 1;
    for channel in 0..CHANNELS {
        let history = &mut lane.history[channel * history_frames..(channel + 1) * history_frames];
        if scene.frames >= history_frames {
            for (offset, sample) in history.iter_mut().enumerate() {
                *sample = source_sample(scene, channel, scene.frames - history_frames + offset);
            }
        } else {
            history.copy_within(scene.frames.., 0);
            let start = history_frames - scene.frames;
            for frame in 0..scene.frames {
                history[start + frame] = source_sample(scene, channel, frame);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use ssr_types::Direction;

    use super::*;

    fn geometry() -> PanGeometry {
        PanGeometry::new(
            &Layout::new(vec![
                Direction::new("FL", 30.0, 0.0),
                Direction::new("FR", -30.0, 0.0),
            ])
            .unwrap(),
        )
        .unwrap()
    }

    fn scene<'a>(signal: &'a [f32], objects: &'a mut [Object<'a>; MAX_OBJECTS]) -> SceneBlock<'a> {
        objects[0] = Object {
            signal,
            position: Position::FRONT,
            gain: 1.0,
            spread: 0.0,
        };
        SceneBlock {
            epoch: 1,
            frames: signal.len(),
            rate: MIX_RATE,
            source_qpc: 0,
            objects: &objects[..1],
            lfe: None,
            new_objects: 0,
        }
    }

    #[derive(Debug, Default)]
    struct Tagged {
        interval: u64,
        valid: bool,
    }

    impl Interval for Tagged {
        fn interval_valid(&self, interval: u64) -> bool {
            self.valid && self.interval == interval
        }
    }

    #[test]
    fn slot_ring_finds_an_interval_only_in_its_own_valid_slot() {
        let mut empty = SlotRing::<Tagged>::new(Vec::new());
        assert!(empty.is_empty());
        assert!(empty.entry(3).is_none());
        assert!(empty.entry_ref(3).is_none());
        assert!(empty.get(3).is_none());

        let mut ring = SlotRing::new((0..4).map(|_| Tagged::default()).collect());
        assert_eq!(ring.len(), 4);
        *ring.entry(6).expect("a ring with slots has an entry") = Tagged {
            interval: 6,
            valid: true,
        };
        assert_eq!(ring.iter().position(|slot| slot.valid), Some(2));
        assert!(ring.get(6).is_some());
        assert!(
            ring.get(2).is_none(),
            "the same slot holds another interval"
        );
        assert!(
            ring.get(10).is_none(),
            "the same slot holds another interval"
        );
        assert!(ring.get(7).is_none(), "another slot");
        ring.entry(6).expect("entry").valid = false;
        assert!(ring.get(6).is_none());
        assert!(ring.get_mut(6).is_none());
    }

    #[test]
    fn a_colliding_interval_takes_a_slot_only_once_the_old_interval_is_emitted() {
        let mut mixer = SceneMixer::new(MixerConfig::default());
        let epoch = mixer.layout_epoch;
        let lane = &mut mixer.lanes[0];
        let capacity = lane.slots.len() as u64;
        assert!(capacity > 1);
        lane.slot_mut(0, 0, 2, epoch)
            .expect("an empty ring takes interval 0");
        let previous_end = MIX_FRAMES as u64;

        // Interval `capacity` maps to the slot that holds interval 0.
        assert!(
            lane.slot_mut(capacity, previous_end - 1, 2, epoch)
                .is_none()
        );
        assert!(
            lane.slots.get(0).is_some(),
            "a busy slot keeps its interval"
        );
        assert!(lane.slots.get(capacity).is_none());

        let slot = lane
            .slot_mut(capacity, previous_end, 2, epoch)
            .expect("an emitted slot is reused");
        assert_eq!(slot.interval, capacity);
        assert!(lane.slots.get(0).is_none());
        assert!(lane.slots.get(capacity).is_some());
        assert!(
            lane.slot_mut(1, 0, 2, epoch).is_some(),
            "a neighbour has its own slot"
        );
    }

    fn clear_frames(mixer: &mut SceneMixer, endpoint: usize, start: usize, end: usize) {
        let slot = mixer.lanes[endpoint]
            .slot_mut(0, 0, 2, mixer.layout_epoch)
            .expect("the first interval is retained");
        slot.occupancy[start..end].fill(Occupancy::Empty);
        slot.refresh_filled_range();
    }

    fn collect_bits(block: &BedBlock<'_>, output: &mut Vec<u32>) {
        output.extend(block.channels.iter().map(|sample| sample.to_bits()));
    }

    #[test]
    fn full_volume_transition_queue_merges_the_newest_two_without_changing_earlier_gain() {
        let mut lane = Lane::new(1);
        lane.set_volume(0.8, 100);
        lane.set_volume(0.2, 200);
        for index in 2..VOLUME_TRANSITIONS {
            lane.set_volume(0.9, 100 + index as u64 * 100);
        }
        lane.set_volume(1.0, 2_000);

        assert_eq!(lane.volume_transition_len, VOLUME_TRANSITIONS);
        assert_eq!(
            lane.volume_transitions[0],
            VolumeTransition {
                frame: 100,
                gain: 0.8
            }
        );
        assert_eq!(lane.volume_transitions[1].frame, 200);
        assert_eq!(
            lane.volume_transitions[VOLUME_TRANSITIONS - 1],
            VolumeTransition {
                frame: 2_000,
                gain: 1.0
            }
        );
        assert!(lane.gain_at(164) >= 0.79);
        assert_eq!(lane.volume_transitions_coalesced, 1);
    }

    #[test]
    fn queued_volume_transitions_preserve_the_gain_before_each_reservation() {
        let mut lane = Lane::new(1);
        lane.set_volume(0.0, 100);
        lane.set_volume(1.0, 200);
        assert_eq!(lane.gain_at(99), 1.0);
        assert_eq!(lane.gain_at(100), 1.0);
        assert!(lane.gain_at(132) < 1.0);
        assert_eq!(lane.gain_at(164), 0.0);
        assert_eq!(lane.gain_at(200), 0.0);
        assert!(lane.gain_at(232) > 0.0);
        assert_eq!(lane.gain_at(264), 1.0);
    }

    #[test]
    fn endpoint_volume_ramps_only_its_lane_at_emission() {
        let geometry = geometry();
        let signal_a = vec![1.0; MIX_FRAMES];
        let signal_b = vec![0.25; MIX_FRAMES];
        let mut objects_a = [Object::SILENT; MAX_OBJECTS];
        let mut objects_b = [Object::SILENT; MAX_OBJECTS];
        let block_a = scene(&signal_a, &mut objects_a);
        let block_b = scene(&signal_b, &mut objects_b);
        let mut mixer = SceneMixer::new(MixerConfig {
            arrival_slack_frames: 0,
            ..MixerConfig::default()
        });
        mixer.push_scene(0, &block_a, Some(&geometry)).unwrap();
        mixer.push_scene(1, &block_b, Some(&geometry)).unwrap();
        mixer.set_lane_volume(0, 0.0, 0);
        let mut output = Vec::new();
        mixer.advance(0, MIX_FRAMES as u64, &mut |bed| {
            output.extend_from_slice(bed.channels)
        });
        mixer.advance(1, MIX_FRAMES as u64, &mut |bed| {
            output.extend_from_slice(bed.channels)
        });
        assert_eq!(output.len(), geometry.directions() * MIX_FRAMES);
        let direction = &output[..MIX_FRAMES];
        assert!(
            direction[64..]
                .iter()
                .all(|sample| (*sample - 0.176_776_69).abs() < 1e-5)
        );
        assert!(direction[0] > direction[63]);
    }

    #[test]
    fn endpoint_volume_applies_to_ended_generation_tail_bed() {
        let mut mixer = SceneMixer::new(MixerConfig {
            arrival_slack_frames: 0,
            ..MixerConfig::default()
        });
        let geometry = geometry();
        mixer.change_layout(None, &geometry);
        mixer.next_t = Some(0);
        let epoch = mixer.layout_epoch;
        let lane = &mut mixer.lanes[0];
        lane.begin(0);
        lane.delay = Some(0);
        lane.tail_end = 1;
        let tail = lane
            .tail_slots
            .entry(0)
            .expect("enabled lane has tail slots");
        tail.reset_for(0, 7, epoch);
        tail.bed[64] = 0.75;
        tail.occupancy[64] = Occupancy::Filled;
        tail.filled_start = 64;
        tail.filled_end = 65;
        mixer.set_lane_volume(0, 0.0, 0);

        let mut sample = None;
        mixer.emit_next(&mut |block| sample = Some(block.channels[64]), false);

        assert_eq!(sample, Some(0.0));
    }

    #[test]
    fn endpoint_mask_allocates_no_slots_for_excluded_lane() {
        let mixer = SceneMixer::new(MixerConfig {
            endpoint_mask: 1,
            ..MixerConfig::default()
        });
        assert!(!mixer.lanes[0].slots.is_empty());
        assert_eq!(mixer.lanes[0].slots.len(), mixer.lanes[0].tail_slots.len());
        assert!(mixer.lanes[1].slots.is_empty());
        assert!(mixer.lanes[1].tail_slots.is_empty());
    }

    #[test]
    fn internal_unfilled_run_counts_only_the_current_occupied_generation() {
        let geometry = geometry();
        let signal = vec![1.0; MIX_FRAMES];
        let mut objects = [Object::SILENT; MAX_OBJECTS];
        let block = scene(&signal, &mut objects);
        let mut mixer = SceneMixer::new(MixerConfig::default());
        mixer.push_scene(0, &block, Some(&geometry)).unwrap();
        clear_frames(&mut mixer, 0, 120, 137);
        mixer.advance(0, MIX_FRAMES as u64, &mut |_| {});
        assert_eq!(mixer.lane_underrun_frames(), 17);
    }

    #[test]
    fn transferred_tail_is_not_counted_as_an_underrun_again() {
        let geometry = geometry();
        let signal = vec![1.0; MIX_FRAMES];
        let mut objects = [Object::SILENT; MAX_OBJECTS];
        let block = scene(&signal, &mut objects);
        let mut mixer = SceneMixer::new(MixerConfig {
            arrival_slack_frames: 0,
            ..MixerConfig::default()
        });
        mixer.push_scene(0, &block, Some(&geometry)).unwrap();
        clear_frames(&mut mixer, 0, 120, 137);
        mixer.lane_start(0, 0);
        assert_eq!(mixer.lane_underrun_frames(), 17);
        mixer.finish(&mut |_| {});
        assert_eq!(mixer.lane_underrun_frames(), 17);
    }

    #[test]
    fn tail_collision_preserves_current_then_tail_addition_order() {
        let mut mixer = SceneMixer::new(MixerConfig {
            arrival_slack_frames: 0,
            ..MixerConfig::default()
        });
        let geometry = geometry();
        mixer.change_layout(None, &geometry);
        mixer.next_t = Some(0);
        let epoch = mixer.layout_epoch;
        for lane in &mut mixer.lanes {
            lane.begin(0);
            lane.delay = Some(0);
            lane.placed_end = 1;
        }
        let prior = mixer.lanes[0]
            .slot_mut(0, 0, 2, epoch)
            .expect("prior lane interval is retained");
        prior.bed[0] = -1.0e20;
        prior.occupancy[0] = Occupancy::Filled;
        prior.filled_start = 0;
        prior.filled_end = 1;

        mixer.lanes[1].tail_end = 1;
        let current = mixer.lanes[1]
            .slot_mut(0, 0, 2, epoch)
            .expect("current interval is retained");
        current.bed[0] = 1.0e20;
        current.occupancy[0] = Occupancy::Filled;
        current.filled_start = 0;
        current.filled_end = 1;
        let tail = mixer.lanes[1]
            .tail_slots
            .entry(0)
            .expect("enabled lane has tail slots");
        tail.reset_for(0, 2, epoch);
        tail.bed[0] = 1.0;
        tail.occupancy[0] = Occupancy::Filled;
        tail.filled_start = 0;
        tail.filled_end = 1;

        let mut first = None;
        mixer.emit_next(
            &mut |block| first = Some(block.channels[0].to_bits()),
            false,
        );
        assert_eq!(first, Some(1.0_f32.to_bits()));
        assert!(mixer.lanes[1].slot(0).is_none());
        assert!(mixer.lanes[1].tail_slot(0).is_none());
    }

    #[test]
    fn pending_tail_is_converted_lazily_after_a_layout_change() {
        let old =
            PanGeometry::new(&Layout::new(vec![Direction::new("C", 0.0, 0.0)]).unwrap()).unwrap();
        let new = geometry();
        let signal = vec![1.0; MIX_FRAMES];
        let mut objects = [Object::SILENT; MAX_OBJECTS];
        let block = scene(&signal, &mut objects);
        let mut mixer = SceneMixer::new(MixerConfig {
            arrival_slack_frames: 0,
            ..MixerConfig::default()
        });
        mixer.push_scene(0, &block, Some(&old)).unwrap();
        mixer.lane_start(0, 0);
        let tail_epoch = mixer.lanes[0]
            .tail_slots
            .get(0)
            .expect("tail interval is retained")
            .layout_epoch;
        mixer.change_layout(Some(&old), &new);
        assert_eq!(
            mixer.lanes[0]
                .tail_slots
                .get(0)
                .expect("tail interval is retained")
                .layout_epoch,
            tail_epoch
        );

        let mut power = None;
        mixer.finish(&mut |mixed| {
            power = Some(mixed.channels[0].powi(2) + mixed.channels[MIX_FRAMES].powi(2));
        });
        assert!(power.is_some_and(|power| (power - 1.0).abs() < 1.0e-5));
        assert_eq!(mixer.lane_underrun_frames(), 0);
    }

    #[test]
    fn ramp_direction_updates_stop_after_active_prefix() {
        let geometry = geometry();
        let signal = vec![1.0; MIX_FRAMES];
        let mut first_objects = [Object::SILENT; MAX_OBJECTS];
        let first = scene(&signal, &mut first_objects);
        let mut mixer = SceneMixer::new(MixerConfig::default());
        mixer.push_scene(0, &first, Some(&geometry)).unwrap();
        mixer.advance(0, MIX_FRAMES as u64, &mut |_| {});

        let mut second_objects = [Object::SILENT; MAX_OBJECTS];
        second_objects[0] = Object {
            signal: &signal,
            position: Position::from_angles(30.0, 0.0),
            gain: 1.0,
            spread: 0.0,
        };
        let second = SceneBlock {
            epoch: 1,
            frames: signal.len(),
            rate: MIX_RATE,
            source_qpc: 0,
            objects: &second_objects[..1],
            lfe: None,
            new_objects: 0,
        };
        mixer.direction_updates = 0;
        mixer.push_scene(0, &second, Some(&geometry)).unwrap();

        let active = mixer.active_direction_counts[0] as u64;
        assert_eq!(mixer.direction_updates, active * FADE_FRAMES as u64);
    }

    fn fixed_seed(seed: &mut u64) -> u64 {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        *seed
    }

    fn dense_geometry(directions: usize) -> PanGeometry {
        let directions = (0..directions)
            .map(|index| {
                let azimuth = 180.0 - index as f32 * 360.0 / directions as f32;
                Direction::new(format!("D{index}"), azimuth, 0.0)
            })
            .collect();
        PanGeometry::new(&Layout::new(directions).unwrap()).unwrap()
    }

    fn run_new_object_case(mark_other_lane_new: bool) -> Vec<u32> {
        let geometry = geometry();
        let signal = vec![1.0; MIX_FRAMES];
        let mut mixer = SceneMixer::new(MixerConfig {
            arrival_slack_frames: 0,
            ..MixerConfig::default()
        });
        mixer.lane_start(0, 0);
        mixer.lane_start(1, 0);

        let mut first_zero_objects = [Object::SILENT; MAX_OBJECTS];
        first_zero_objects[0] = Object {
            signal: &signal,
            position: Position::from_angles(30.0, 0.0),
            gain: 1.0,
            spread: 0.0,
        };
        let first_zero = SceneBlock {
            epoch: 1,
            frames: MIX_FRAMES,
            rate: MIX_RATE,
            source_qpc: 0,
            objects: &first_zero_objects[..1],
            lfe: None,
            new_objects: 0,
        };
        let mut first_one_objects = [Object::SILENT; MAX_OBJECTS];
        first_one_objects[0] = Object {
            signal: &signal,
            position: Position::from_angles(-30.0, 0.0),
            gain: 0.0,
            spread: 0.0,
        };
        let first_one = SceneBlock {
            objects: &first_one_objects[..1],
            ..first_zero
        };
        mixer.push_scene(0, &first_zero, Some(&geometry)).unwrap();
        mixer.push_scene(1, &first_one, Some(&geometry)).unwrap();
        mixer.advance(0, MIX_FRAMES as u64, &mut |_| {});
        mixer.advance(1, MIX_FRAMES as u64, &mut |_| {});

        let mut second_zero_objects = [Object::SILENT; MAX_OBJECTS];
        second_zero_objects[0] = Object {
            signal: &signal,
            position: Position::from_angles(-30.0, 0.0),
            gain: 1.0,
            spread: 0.0,
        };
        let second_zero = SceneBlock {
            epoch: 2,
            frames: MIX_FRAMES,
            rate: MIX_RATE,
            source_qpc: 10_000,
            objects: &second_zero_objects[..1],
            lfe: None,
            new_objects: 0,
        };
        let mut second_one_objects = [Object::SILENT; MAX_OBJECTS];
        second_one_objects[0] = Object {
            signal: &signal,
            position: Position::from_angles(-30.0, 0.0),
            gain: 0.0,
            spread: 0.0,
        };
        let second_one = SceneBlock {
            objects: &second_one_objects[..1],
            new_objects: u64::from(mark_other_lane_new),
            ..second_zero
        };
        mixer.push_scene(1, &second_one, Some(&geometry)).unwrap();
        mixer.push_scene(0, &second_zero, Some(&geometry)).unwrap();
        let mut output = Vec::new();
        mixer.advance(0, MIX_FRAMES as u64, &mut |block| {
            collect_bits(block, &mut output)
        });
        output
    }

    #[test]
    fn new_objects_only_breaks_the_lane_local_object_ramp() {
        assert_eq!(run_new_object_case(false), run_new_object_case(true));
    }

    #[test]
    fn restart_does_not_inherit_the_previous_panning_state() {
        let geometry = geometry();
        let signal = vec![1.0; MIX_FRAMES];

        let run = |restart: bool| {
            let mut mixer = SceneMixer::new(MixerConfig {
                arrival_slack_frames: 0,
                ..MixerConfig::default()
            });
            if restart {
                let mut old_objects = [Object::SILENT; MAX_OBJECTS];
                old_objects[0] = Object {
                    signal: &signal,
                    position: Position::from_angles(30.0, 0.0),
                    gain: 0.0,
                    spread: 0.0,
                };
                let old = SceneBlock {
                    epoch: 1,
                    frames: MIX_FRAMES,
                    rate: MIX_RATE,
                    source_qpc: 0,
                    objects: &old_objects[..1],
                    lfe: None,
                    new_objects: 0,
                };
                mixer.push_scene(0, &old, Some(&geometry)).unwrap();
                mixer.advance(0, MIX_FRAMES as u64, &mut |_| {});
                mixer.lane_start(0, 0);
            }
            let mut objects = [Object::SILENT; MAX_OBJECTS];
            objects[0] = Object {
                signal: &signal,
                position: Position::from_angles(-30.0, 0.0),
                gain: 1.0,
                spread: 0.0,
            };
            let block = SceneBlock {
                epoch: 2,
                frames: MIX_FRAMES,
                rate: MIX_RATE,
                source_qpc: 10_000,
                objects: &objects[..1],
                lfe: None,
                new_objects: 0,
            };
            mixer.push_scene(0, &block, Some(&geometry)).unwrap();
            let mut output = Vec::new();
            mixer.finish(&mut |mixed| collect_bits(mixed, &mut output));
            output
        };

        assert_eq!(run(true), run(false));
    }

    #[test]
    fn objects_beyond_the_precision_limit_are_ignored_without_statistics() {
        let geometry = geometry();
        let signal = vec![1.0; MIX_FRAMES];
        let objects = (0..MAX_OBJECTS + 3)
            .map(|index| Object {
                signal: signal.as_slice(),
                position: if index % 2 == 0 {
                    Position::from_angles(30.0, 0.0)
                } else {
                    Position::from_angles(-30.0, 0.0)
                },
                gain: 1.0,
                spread: 0.0,
            })
            .collect::<Vec<_>>();
        let scene = SceneBlock {
            epoch: 1,
            frames: MIX_FRAMES,
            rate: MIX_RATE,
            source_qpc: 0,
            objects: &objects,
            lfe: None,
            new_objects: u64::MAX,
        };
        let mut mixer = SceneMixer::new(MixerConfig {
            arrival_slack_frames: 0,
            ..MixerConfig::default()
        });
        mixer.push_scene(0, &scene, Some(&geometry)).unwrap();
        let mut output = Vec::new();
        mixer.finish(&mut |mixed| output.extend_from_slice(mixed.channels));

        assert_eq!(mixer.dropped_frames(), 0);
        assert_eq!(mixer.lane_underrun_frames(), 0);
        for frame in 0..MIX_FRAMES {
            assert_eq!(output[frame], (MAX_OBJECTS / 2) as f32);
            assert_eq!(output[MIX_FRAMES + frame], (MAX_OBJECTS / 2) as f32);
        }
    }

    #[test]
    fn fixed_seed_panning_matches_dense_reference_matrix() {
        let cases = [
            (44_100, 22, 17, 64),
            (MIX_RATE, 22, 64, 11),
            (96_000, 22, 23, 64),
            (44_100, 64, 64, 9),
            (MIX_RATE, 64, 31, 64),
            (96_000, 64, 64, 27),
        ];
        for (rate, directions, first_count, second_count) in cases {
            let geometry = dense_geometry(directions);
            let frames = rate as usize / 100;
            let mut seed = 0x3c79_ac49_2ba7_b653_u64
                ^ u64::from(rate)
                ^ directions as u64
                ^ (first_count as u64) << 16
                ^ (second_count as u64) << 32;
            let first_signals = (0..first_count)
                .map(|_| {
                    (0..frames)
                        .map(|_| {
                            let bits = (fixed_seed(&mut seed) >> 40) as u32;
                            bits as f32 / 0x00ff_ffff_u32 as f32 - 0.5
                        })
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>();
            let second_signals = (0..second_count)
                .map(|_| {
                    (0..frames)
                        .map(|_| {
                            let bits = (fixed_seed(&mut seed) >> 40) as u32;
                            bits as f32 / 0x00ff_ffff_u32 as f32 - 0.5
                        })
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>();
            let mut first_objects = [Object::SILENT; MAX_OBJECTS];
            for (index, object) in first_objects[..first_count].iter_mut().enumerate() {
                *object = Object {
                    signal: &first_signals[index],
                    position: Position::from_angles(
                        (fixed_seed(&mut seed) % 360) as f32 - 180.0,
                        (fixed_seed(&mut seed) % 121) as f32 - 60.0,
                    ),
                    gain: 0.25 + (fixed_seed(&mut seed) % 750) as f32 / 1_000.0,
                    spread: [0.0, 0.15, 0.6, 1.0][index % 4],
                };
            }
            let first = SceneBlock {
                epoch: 1,
                frames,
                rate,
                source_qpc: 0,
                objects: &first_objects[..first_count],
                lfe: None,
                new_objects: 0,
            };
            let mut mixer = SceneMixer::new(MixerConfig {
                arrival_slack_frames: 0,
                ..MixerConfig::default()
            });
            mixer.push_scene(0, &first, Some(&geometry)).unwrap();
            mixer.advance(0, MIX_FRAMES as u64, &mut |_| {});

            let mut second_objects = [Object::SILENT; MAX_OBJECTS];
            for (index, object) in second_objects[..second_count].iter_mut().enumerate() {
                *object = Object {
                    signal: &second_signals[index],
                    position: Position::from_angles(
                        (fixed_seed(&mut seed) % 360) as f32 - 180.0,
                        (fixed_seed(&mut seed) % 121) as f32 - 60.0,
                    ),
                    gain: 0.25 + (fixed_seed(&mut seed) % 750) as f32 / 1_000.0,
                    spread: [1.0, 0.6, 0.15, 0.0][index % 4],
                };
            }
            let new_objects = ((second_count > 1) as u64 * 2)
                | ((second_count == MAX_OBJECTS) as u64 * (1_u64 << (MAX_OBJECTS - 1)));
            let second = SceneBlock {
                epoch: 2,
                frames,
                rate,
                source_qpc: 10_000,
                objects: &second_objects[..second_count],
                lfe: None,
                new_objects,
            };

            let lane = &mixer.lanes[0];
            assert_eq!(lane.phase_numerator, 0);
            let mut target = vec![0.0_f32; MAX_OBJECTS * MAX_DIRECTIONS];
            let mut unit = [0.0_f32; MAX_DIRECTIONS];
            for (object_index, object) in second.objects.iter().enumerate() {
                geometry.gains(object.position, object.spread, &mut unit[..directions]);
                let row = object_index * MAX_DIRECTIONS;
                for direction in 0..directions {
                    target[row + direction] = unit[direction] * object.gain;
                }
            }
            let mut expected = vec![0.0_f32; directions * MIX_FRAMES];
            let mut numerator = lane.phase_numerator;
            for output_frame in 0..MIX_FRAMES {
                let source_frame = (numerator / u64::from(MIX_RATE)) as usize;
                let fraction = numerator % u64::from(MIX_RATE);
                for object in 0..second_count {
                    let sample = sample_channel(
                        lane,
                        &mixer.kernels,
                        &second,
                        object,
                        source_frame,
                        fraction,
                    );
                    let row = object * MAX_DIRECTIONS;
                    let is_new = object >= lane.previous_object_count
                        || second.new_objects & (1_u64 << object) != 0;
                    for direction in 0..directions {
                        let target_gain = target[row + direction];
                        let gain = if output_frame < FADE_FRAMES && !is_new {
                            renderer_pan_ramp_gain(
                                lane.pan_gains[row + direction],
                                target_gain,
                                output_frame,
                                FADE_FRAMES,
                            )
                        } else {
                            target_gain
                        };
                        expected[direction * MIX_FRAMES + output_frame] += sample * gain;
                    }
                }
                numerator = numerator.saturating_add(u64::from(rate));
            }

            mixer.direction_updates = 0;
            mixer.push_scene(0, &second, Some(&geometry)).unwrap();
            let active_updates = mixer.active_direction_counts[..second_count]
                .iter()
                .sum::<usize>()
                * FADE_FRAMES;
            assert_eq!(mixer.direction_updates as usize, active_updates);
            let mut actual = Vec::new();
            mixer.advance(0, MIX_FRAMES as u64, &mut |block| {
                actual.extend_from_slice(block.channels)
            });
            assert_eq!(actual.len(), expected.len());
            for (index, (actual, expected)) in actual.iter().zip(&expected).enumerate() {
                assert_eq!(
                    actual.to_bits(),
                    expected.to_bits(),
                    "rate={rate} directions={directions} first={first_count} second={second_count} sample={index} actual={actual} expected={expected}"
                );
            }
        }
    }

    #[test]
    fn fixed_seed_schedules_conserve_every_occupied_frame() {
        let geometry = geometry();
        let mut seed = 0xd1b5_4a32_d192_ed03_u64;
        for schedule in 0..48_u64 {
            let mut mixer = SceneMixer::new(MixerConfig {
                arrival_slack_frames: schedule % 4 * 48,
                qpc_frequency: 1_000_000,
                endpoint_mask: u32::MAX,
            });
            for step in 0..32_u64 {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                let endpoint = (seed as usize) % MAX_ENDPOINTS;
                let rate = [44_100, MIX_RATE, 96_000][(seed as usize >> 2) % 3];
                if step == 0 || seed & 0x1f == 0 {
                    mixer.lane_start(endpoint as u16, 0);
                }
                if seed & 0x0f == 1 {
                    mixer.lane_discontinuity(endpoint as u16, 0);
                }
                let base = rate as usize / 100;
                let frames = base * (1 + ((seed >> 8) as usize % 5));
                let signal = vec![1.0; frames];
                let mut objects = [Object::SILENT; MAX_OBJECTS];
                let mut block = scene(&signal, &mut objects);
                block.rate = rate;
                mixer
                    .push_scene(endpoint as u16, &block, Some(&geometry))
                    .unwrap();
                let transport = MIX_FRAMES as u64 + if seed & 1 == 0 { 48 } else { 0 }
                    - if seed & 2 == 0 { 24 } else { 0 };
                mixer.advance(endpoint as u16, transport, &mut |_| {});
                if seed & 0x3f == 3 {
                    mixer.lane_stop(endpoint as u16, &mut |_| {});
                }
            }
            mixer.finish(&mut |_| {});
            for endpoint in 0..MAX_ENDPOINTS {
                assert_eq!(
                    mixer.emitted_filled_frames[endpoint]
                        + mixer.lane_dropped_frames[endpoint]
                        + mixer.lane_underruns[endpoint],
                    mixer.occupied_frames[endpoint],
                    "schedule={schedule} endpoint={endpoint} filled={} drops={} underruns={} occupied={}",
                    mixer.emitted_filled_frames[endpoint],
                    mixer.lane_dropped_frames[endpoint],
                    mixer.lane_underruns[endpoint],
                    mixer.occupied_frames[endpoint]
                );
            }
        }
    }
}
