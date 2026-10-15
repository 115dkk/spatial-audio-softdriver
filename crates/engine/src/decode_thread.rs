//! The decode thread `ssr-decode` (ADR 0031).
//!
//! Upstream codec decoders that allocate per frame (E-AC-3 today) do not run
//! on the capture thread. The capture thread copies each complete burst into a
//! preallocated job and queues it; the decode thread runs the codec's
//! [`FrontDecoders`] half and returns the job with the upstream-owned frames
//! in it; the capture thread places them as scenes at its next step.
//!
//! ```text
//! capture thread                         decode thread
//! DecodeQueue ── jobs ring (Box ptrs) ──▶ DecodeWorker::run_one
//!      ▲                                          │
//!      └───────── done ring (Box ptrs) ◀──────────┘
//! ```
//!
//! Both rings carry `Box<DecodeJob>` pointers and have as many slots as there
//! are jobs, so neither side ever waits for room and the capture side never
//! allocates, frees or locks: it fills a job's payload within its preallocated
//! capacity, moves pointers, and keeps finished jobs (with their frames) on its
//! free list until the decode thread clears them on reuse. A reset job runs
//! after the bursts queued before it, so a burst is always decoded with the
//! decoder state of the moment it was queued.

use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::Thread;
use std::time::Duration;

use decode::{DecodeError, Decoded, DeferredCodec, FrontDecoders};
use rtrb::{Consumer, Producer, PushError, RingBuffer};
use ssr_abi::MAX_ENDPOINTS;

/// Jobs in flight at most: 16 E-AC-3 bursts are 512 ms of one lane.
pub const DECODE_SLOTS: usize = 16;
/// Payload capacity of each job; a burst can carry no more.
pub const MAX_JOB_PAYLOAD: usize = bitstream::MAX_PAYLOAD;

/// How long the idle decode thread sleeps before it looks for a stop.
const PARK_TIMEOUT: Duration = Duration::from_millis(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum JobKind {
    Reset,
    Burst(DeferredCodec),
}

/// One unit of decode-thread work, allocated once and reused.
#[derive(Debug)]
pub struct DecodeJob {
    endpoint: u16,
    epoch: u32,
    qpc: u64,
    kind: JobKind,
    /// Capacity `MAX_JOB_PAYLOAD`, filled with `clear` + `extend_from_slice`
    /// (never grows).
    payload: Vec<u8>,
    /// `Ok` for a reset.
    result: Result<(), DecodeError>,
    /// Written and cleared on the decode thread only.
    decoded: Decoded,
}

impl DecodeJob {
    fn new() -> Self {
        Self {
            endpoint: 0,
            epoch: 0,
            qpc: 0,
            kind: JobKind::Reset,
            payload: Vec::with_capacity(MAX_JOB_PAYLOAD),
            result: Ok(()),
            decoded: Decoded::new(),
        }
    }

    pub fn endpoint(&self) -> u16 {
        self.endpoint
    }

    /// The front's epoch when the burst was queued (0 for a reset).
    pub fn epoch(&self) -> u32 {
        self.epoch
    }

    /// The source-clock time of the burst's first carrier byte.
    pub fn qpc(&self) -> u64 {
        self.qpc
    }

    pub fn is_reset(&self) -> bool {
        self.kind == JobKind::Reset
    }

    /// The decode result; `Ok` for a reset.
    pub fn result(&self) -> Result<(), DecodeError> {
        self.result
    }

    pub fn decoded(&self) -> &Decoded {
        &self.decoded
    }
}

fn endpoint_bit(endpoint: u16) -> u32 {
    1_u32.checked_shl(u32::from(endpoint)).unwrap_or(0)
}

/// Capture-thread end. Never allocates after construction.
#[derive(Debug)]
pub struct DecodeQueue {
    // Jobs cross the rings as `Box` pointers; the free list keeps them boxed
    // so taking one and queueing it moves a pointer, never the job.
    #[allow(clippy::vec_box)]
    free: Vec<Box<DecodeJob>>,
    jobs: Producer<Box<DecodeJob>>,
    done: Consumer<Box<DecodeJob>>,
    in_flight: usize,
    /// One bit per endpoint whose reset found no free job.
    pending_resets: u32,
    wake: Option<Thread>,
    refused: u64,
}

impl DecodeQueue {
    /// Copies the payload into a free job and queues it.
    /// `Err(DecoderBehind)` when no job is free (or the ring refuses), or
    /// while a reset for this endpoint is still pending (see
    /// [`Self::submit_reset`]).
    pub fn submit_burst(
        &mut self,
        endpoint: u16,
        epoch: u32,
        qpc: u64,
        codec: DeferredCodec,
        payload: &[u8],
    ) -> Result<(), DecodeError> {
        if usize::from(endpoint) >= MAX_ENDPOINTS {
            return Err(DecodeError::WrongRoute(
                "the decode thread has no lane for this endpoint",
            ));
        }
        if payload.len() > MAX_JOB_PAYLOAD {
            return Err(DecodeError::Corrupt {
                codec: "IEC 61937",
                detail: "a burst payload exceeds the largest burst",
            });
        }
        if self.pending_resets & endpoint_bit(endpoint) != 0 {
            return Err(self.refuse());
        }
        let Some(mut job) = self.free.pop() else {
            return Err(self.refuse());
        };
        job.endpoint = endpoint;
        job.epoch = epoch;
        job.qpc = qpc;
        job.kind = JobKind::Burst(codec);
        job.result = Ok(());
        job.payload.clear();
        job.payload.extend_from_slice(payload);
        match self.jobs.push(job) {
            Ok(()) => {
                self.in_flight += 1;
                self.wake();
                Ok(())
            }
            Err(PushError::Full(job)) => {
                self.free.push(job);
                Err(self.refuse())
            }
        }
    }

    /// Queues a reset for the lane's decode-thread half, after the bursts
    /// queued before it. With no free job it is remembered and queued first
    /// thing when a job comes back; until then [`Self::submit_burst`] refuses
    /// that endpoint, because a burst must not decode on a stale front.
    pub fn submit_reset(&mut self, endpoint: u16) {
        if usize::from(endpoint) >= MAX_ENDPOINTS {
            return;
        }
        if self.pending_resets & endpoint_bit(endpoint) == 0 && self.queue_reset(endpoint) {
            self.wake();
        } else {
            self.pending_resets |= endpoint_bit(endpoint);
        }
    }

    fn queue_reset(&mut self, endpoint: u16) -> bool {
        let Some(mut job) = self.free.pop() else {
            return false;
        };
        job.endpoint = endpoint;
        job.epoch = 0;
        job.qpc = 0;
        job.kind = JobKind::Reset;
        job.result = Ok(());
        job.payload.clear();
        match self.jobs.push(job) {
            Ok(()) => {
                self.in_flight += 1;
                true
            }
            Err(PushError::Full(job)) => {
                self.free.push(job);
                false
            }
        }
    }

    /// Pops finished jobs in order and hands each to `handle`, then returns it
    /// to the free list (its frames stay in it until the decode thread reuses
    /// it), and queues pending resets. Wakes the worker if it queued anything.
    pub fn collect(&mut self, handle: &mut dyn FnMut(&DecodeJob)) {
        let mut returned = false;
        while let Ok(job) = self.done.pop() {
            self.in_flight = self.in_flight.saturating_sub(1);
            handle(&job);
            // `free` has room for every job, so this never reallocates.
            self.free.push(job);
            returned = true;
        }
        if !returned || self.pending_resets == 0 {
            return;
        }
        let mut queued = false;
        for endpoint in 0..MAX_ENDPOINTS as u16 {
            let bit = endpoint_bit(endpoint);
            if self.pending_resets & bit != 0 && self.queue_reset(endpoint) {
                self.pending_resets &= !bit;
                queued = true;
            }
        }
        if queued {
            self.wake();
        }
    }

    /// Jobs queued and not yet collected, resets included.
    pub fn in_flight(&self) -> usize {
        self.in_flight
    }

    /// Bursts refused with [`DecodeError::DecoderBehind`].
    pub fn refused(&self) -> u64 {
        self.refused
    }

    /// The thread to unpark after queueing a job (the decode thread).
    pub fn set_wake(&mut self, thread: Thread) {
        self.wake = Some(thread);
    }

    fn refuse(&mut self) -> DecodeError {
        self.refused = self.refused.saturating_add(1);
        DecodeError::DecoderBehind
    }

    fn wake(&self) {
        // Wait-free and allocation-free: an atomic swap and a futex wake.
        if let Some(thread) = &self.wake {
            thread.unpark();
        }
    }
}

/// Decode-thread end. May allocate.
#[derive(Debug)]
pub struct DecodeWorker {
    jobs: Consumer<Box<DecodeJob>>,
    done: Producer<Box<DecodeJob>>,
    /// One per endpoint. On the heap: a lane's upstream decoders take about
    /// 60 KB, too much to move around inside a `Chain` on a thread's stack.
    fronts: Box<[FrontDecoders]>,
}

impl DecodeWorker {
    /// Takes one job, runs it (a reset resets the lane's front; a burst
    /// clears the job's frames, then decodes into it and records the result)
    /// and returns it. Returns false when there was no job.
    pub fn run_one(&mut self) -> bool {
        let Ok(mut job) = self.jobs.pop() else {
            return false;
        };
        let job_ref = &mut *job;
        job_ref.decoded.clear();
        job_ref.result = match (
            job_ref.kind,
            self.fronts.get_mut(usize::from(job_ref.endpoint)),
        ) {
            (_, None) => Err(DecodeError::WrongRoute(
                "the decode thread has no lane for this endpoint",
            )),
            (JobKind::Reset, Some(front)) => {
                front.reset();
                Ok(())
            }
            (JobKind::Burst(codec), Some(front)) => {
                front.decode(codec, &job_ref.payload, &mut job_ref.decoded)
            }
        };
        // `done` has a slot for every job, so it always has room.
        let returned = self.done.push(job).is_ok();
        debug_assert!(returned, "the done ring holds every job");
        true
    }

    /// Thread main: runs jobs until `stop`, sleeping between wakes, then runs
    /// whatever is still queued so no job is lost, and returns.
    pub fn run_until(mut self, stop: &AtomicBool) {
        while !stop.load(Ordering::Acquire) {
            while self.run_one() {}
            std::thread::park_timeout(PARK_TIMEOUT);
        }
        while self.run_one() {}
    }
}

/// Builds a queue and its worker with `slots` preallocated jobs (at least one).
pub fn channel(slots: usize) -> (DecodeQueue, DecodeWorker) {
    let slots = slots.max(1);
    let (jobs, job_consumer) = RingBuffer::new(slots);
    let (done_producer, done) = RingBuffer::new(slots);
    let mut free = Vec::with_capacity(slots);
    free.extend((0..slots).map(|_| Box::new(DecodeJob::new())));
    (
        DecodeQueue {
            free,
            jobs,
            done,
            in_flight: 0,
            pending_resets: 0,
            wake: None,
            refused: 0,
        },
        DecodeWorker {
            jobs: job_consumer,
            done: done_producer,
            fronts: (0..MAX_ENDPOINTS).map(|_| FrontDecoders::new()).collect(),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reset_follows_the_bursts_queued_before_it_and_pending_resets_block_bursts() {
        let (mut queue, mut worker) = channel(2);
        queue
            .submit_burst(1, 7, 99, DeferredCodec::Ddp, &[0; 7])
            .unwrap();
        queue.submit_reset(1);
        assert_eq!(queue.in_flight(), 2);
        // No free job: the reset is remembered and bursts for that lane wait.
        queue.submit_reset(2);
        assert_eq!(
            queue.submit_burst(2, 7, 0, DeferredCodec::Ddp, &[0; 7]),
            Err(DecodeError::DecoderBehind)
        );
        assert_eq!(
            queue.submit_burst(0, 7, 0, DeferredCodec::Ddp, &[0; 7]),
            Err(DecodeError::DecoderBehind)
        );
        assert_eq!(queue.refused(), 2);
        while worker.run_one() {}
        let mut seen = Vec::new();
        queue.collect(&mut |job| {
            seen.push((
                job.endpoint(),
                job.is_reset(),
                job.qpc(),
                job.result().is_err(),
            ))
        });
        assert_eq!(seen, [(1, false, 99, true), (1, true, 0, false)]);
        // The pending reset went out first thing.
        assert_eq!(queue.in_flight(), 1);
        queue
            .submit_burst(2, 7, 5, DeferredCodec::Ddp, &[0; 7])
            .unwrap();
        while worker.run_one() {}
        seen.clear();
        queue.collect(&mut |job| seen.push((job.endpoint(), job.is_reset(), job.qpc(), false)));
        assert_eq!(seen, [(2, true, 0, false), (2, false, 5, false)]);
        assert_eq!(queue.in_flight(), 0);
    }

    #[test]
    fn run_until_drains_the_queue_after_a_stop() {
        let (mut queue, worker) = channel(DECODE_SLOTS);
        for endpoint in 0..MAX_ENDPOINTS as u16 {
            queue.submit_reset(endpoint);
        }
        let stop = AtomicBool::new(true);
        worker.run_until(&stop);
        let mut resets = 0;
        queue.collect(&mut |job| resets += usize::from(job.is_reset()));
        assert_eq!(resets, MAX_ENDPOINTS);
        assert_eq!(queue.in_flight(), 0);
    }
}
