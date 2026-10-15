use std::collections::VecDeque;

use rtrb::{Consumer, Producer, RingBuffer};
use ssr_types::OutputLayout;

use super::{EventWriter, Shared, TIMING_QUEUE_CAPACITY};
use std::sync::atomic::Ordering;

#[derive(Clone, Copy, Debug)]
pub(super) struct TimingMarker {
    pub(super) stream_frame: u64,
    pub(super) frames: u32,
    pub(super) source_qpc: u64,
    pub(super) layout: OutputLayout,
}

pub(super) struct CrossingProducer {
    audio: Producer<f32>,
    timing: Producer<TimingMarker>,
    pub(super) stream_frame: u64,
}

pub(super) struct CrossingConsumer {
    pub(super) audio: Consumer<f32>,
    timing: Consumer<TimingMarker>,
    pub(super) stream_frame: u64,
    pub(super) pending_marker: Option<TimingMarker>,
    marker_queue: VecDeque<TimingMarker>,
    pub(super) capacity_frames: usize,
}

pub(super) fn channel(
    capacity_frames: usize,
    channels_max: usize,
) -> (CrossingProducer, CrossingConsumer) {
    let (audio_producer, audio_consumer) =
        RingBuffer::new(capacity_frames.saturating_mul(channels_max));
    let (timing_producer, timing_consumer) = RingBuffer::new(TIMING_QUEUE_CAPACITY);
    (
        CrossingProducer {
            audio: audio_producer,
            timing: timing_producer,
            stream_frame: 0,
        },
        CrossingConsumer {
            audio: audio_consumer,
            timing: timing_consumer,
            stream_frame: 0,
            pending_marker: None,
            marker_queue: VecDeque::with_capacity(TIMING_QUEUE_CAPACITY),
            capacity_frames,
        },
    )
}

impl CrossingProducer {
    pub(super) fn audio_slots(&self) -> usize {
        self.audio.slots()
    }
    pub(super) fn timing_slots(&self) -> usize {
        self.timing.slots()
    }
    pub(super) fn stream_frame(&self) -> u64 {
        self.stream_frame
    }
    pub(super) fn push(
        &mut self,
        block: crate::chain::BlockOut<'_>,
        events: &mut EventWriter,
        shared: &Shared,
    ) {
        let frames = block.frames;
        if frames == 0 {
            return;
        }
        debug_assert_eq!(block.rate, crate::MIX_RATE);
        let samples = frames.saturating_mul(block.channels);
        if self.audio.slots() < samples || self.timing.slots() == 0 {
            if !(block.idle && block.silent) {
                shared
                    .output_overflow_frames
                    .fetch_add(frames as u64, Ordering::Relaxed);
                events.overflow(frames as u64);
            }
            return;
        }
        events.overflow_ended();
        let _ = self.timing.push(TimingMarker {
            stream_frame: self.stream_frame,
            frames: frames.min(u32::MAX as usize) as u32,
            source_qpc: block.source_qpc,
            layout: OutputLayout {
                channels: block.channels as u16,
                channel_mask: block.channel_mask,
            },
        });
        for frame in 0..frames {
            for channel in 0..block.channels {
                let _ = self.audio.push(block.channel(channel)[frame]);
            }
        }
        self.stream_frame = self.stream_frame.saturating_add(frames as u64);
        let previous_rate = shared.scene_rate.swap(crate::MIX_RATE, Ordering::Release);
        debug_assert!(previous_rate == 0 || previous_rate == crate::MIX_RATE);
        if block.idle {
            shared.idle_blocks.fetch_add(1, Ordering::Relaxed);
        } else {
            shared.blocks.fetch_add(1, Ordering::Relaxed);
        }
        shared.note_block();
    }
}

/// Where one output period plays: the ring frames it took (from stream frame
/// `start`, `frames` of them), the QPC its first frame plays at, and how ring
/// frames become device frames and QPC ticks.
#[derive(Clone, Copy, Debug)]
pub(super) struct PeriodPlayback {
    pub(super) start: u64,
    pub(super) frames: usize,
    pub(super) qpc: u64,
    pub(super) mode: super::ClockMode,
    pub(super) device_rate: u32,
    pub(super) qpc_frequency: u64,
}

impl CrossingConsumer {
    #[cfg(test)]
    pub(super) fn from_parts(
        audio: Consumer<f32>,
        timing: Consumer<TimingMarker>,
        capacity_frames: usize,
    ) -> Self {
        Self {
            audio,
            timing,
            stream_frame: 0,
            pending_marker: None,
            marker_queue: VecDeque::with_capacity(TIMING_QUEUE_CAPACITY),
            capacity_frames,
        }
    }
    pub(super) fn refresh_markers(&mut self) {
        while let Ok(marker) = self.timing.pop() {
            debug_assert!(self.marker_queue.len() < TIMING_QUEUE_CAPACITY);
            self.marker_queue.push_back(marker);
        }
    }
    pub(super) fn next_marker(&mut self) -> Option<TimingMarker> {
        self.marker_queue
            .pop_front()
            .or_else(|| self.timing.pop().ok())
    }
    fn unconsumed_frames(&self, marker: TimingMarker) -> usize {
        marker
            .stream_frame
            .saturating_add(u64::from(marker.frames))
            .saturating_sub(marker.stream_frame.max(self.stream_frame)) as usize
    }
    pub(super) fn queued_frames(&self, channels: usize) -> usize {
        if self.pending_marker.is_none() && self.marker_queue.is_empty() {
            return self.audio.slots() / channels;
        }
        self.pending_marker
            .iter()
            .chain(self.marker_queue.iter())
            .map(|m| self.unconsumed_frames(*m))
            .sum()
    }
    pub(super) fn current_layout_frames(&self, layout: OutputLayout, channels: usize) -> usize {
        if self.pending_marker.is_none() && self.marker_queue.is_empty() {
            return self.audio.slots() / channels;
        }
        let pending = self.pending_marker.map_or(0, |m| {
            if m.layout == layout {
                self.unconsumed_frames(m)
            } else {
                0
            }
        });
        pending.saturating_add(
            self.marker_queue
                .iter()
                .take_while(|m| m.layout == layout)
                .map(|m| self.unconsumed_frames(*m))
                .sum::<usize>(),
        )
    }
    pub(super) fn layout_at_boundary(&mut self) -> Option<OutputLayout> {
        self.refresh_markers();
        self.discard_stale_markers();
        self.pending_marker
            .filter(|m| m.stream_frame <= self.stream_frame)
            .map(|m| m.layout)
    }
    pub(super) fn pending_layout_change(&mut self, layout: OutputLayout) -> Option<OutputLayout> {
        self.layout_at_boundary().filter(|l| *l != layout)
    }
    pub(super) fn discard_buffered_audio(
        &mut self,
        mut accept: impl FnMut(TimingMarker) -> bool,
    ) -> u64 {
        let mut sample_budget = self.audio.slots();
        self.refresh_markers();
        if self.pending_marker.is_none() {
            self.pending_marker = self.marker_queue.pop_front();
        }
        let mut frames = 0;
        while let Some(m) = self.pending_marker {
            let end = m.stream_frame.saturating_add(u64::from(m.frames));
            if end <= self.stream_frame {
                self.pending_marker = self.marker_queue.pop_front();
                continue;
            }
            if m.stream_frame > self.stream_frame || !accept(m) {
                break;
            }
            let channels = usize::from(m.layout.channels);
            if channels == 0 {
                break;
            }
            let discard = end
                .saturating_sub(self.stream_frame)
                .min((sample_budget / channels) as u64) as usize;
            if discard == 0 {
                break;
            }
            for _ in 0..discard * channels {
                let _ = self.audio.pop();
            }
            sample_budget -= discard * channels;
            self.stream_frame = self.stream_frame.saturating_add(discard as u64);
            frames += discard as u64;
            if self.stream_frame != end {
                break;
            }
            self.pending_marker = self.marker_queue.pop_front();
        }
        frames
    }
    pub(super) fn discard_stale_markers(&mut self) {
        if self.pending_marker.is_none() {
            self.pending_marker = self.next_marker();
        }
        while self.pending_marker.is_some_and(|m| {
            m.stream_frame.saturating_add(u64::from(m.frames)) <= self.stream_frame
        }) {
            self.pending_marker = self.next_marker();
        }
    }
    pub(super) fn discard(&mut self, frames: usize, channels: usize) -> usize {
        let inspect = frames.min(self.queued_frames(channels));
        if inspect == 0 {
            return 0;
        }
        let Ok(chunk) = self.audio.read_chunk(inspect * channels) else {
            return 0;
        };
        let (a, b) = chunk.as_slices();
        let silent = a.iter().chain(b).take_while(|s| **s == 0.0).count() / channels;
        chunk.commit(silent * channels);
        self.stream_frame = self.stream_frame.saturating_add(silent as u64);
        self.discard_stale_markers();
        silent
    }
    /// Reports the latency of every marker that starts inside `period` and
    /// moves past the markers it finished.
    pub(super) fn consume_markers(&mut self, period: PeriodPlayback, mut latency: impl FnMut(f32)) {
        if self.pending_marker.is_none() {
            self.pending_marker = self.next_marker();
        }
        let end = period.start.saturating_add(period.frames as u64);
        while let Some(m) = self.pending_marker {
            if m.stream_frame >= end {
                break;
            }
            if m.stream_frame.saturating_add(u64::from(m.frames)) <= period.start {
                self.pending_marker = self.next_marker();
                continue;
            }
            let offset = m.stream_frame.saturating_sub(period.start) as f64;
            let device_offset = match period.mode {
                super::ClockMode::Trim => offset,
                super::ClockMode::Asrc => {
                    offset * f64::from(period.device_rate) / f64::from(crate::MIX_RATE)
                }
            };
            let qpc = period.qpc.saturating_add(
                (device_offset * period.qpc_frequency as f64 / f64::from(period.device_rate))
                    .round() as u64,
            );
            if m.source_qpc != 0 && qpc >= m.source_qpc {
                latency(
                    ((qpc - m.source_qpc) as f64 * 1000.0 / period.qpc_frequency as f64) as f32,
                );
            }
            if m.stream_frame.saturating_add(u64::from(m.frames)) <= end {
                self.pending_marker = self.next_marker();
            } else {
                break;
            }
        }
    }
    pub(super) fn drained(&self) -> bool {
        self.audio.slots() == 0
    }
}
