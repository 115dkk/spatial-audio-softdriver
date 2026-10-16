#![forbid(unsafe_code)]

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use streamring::{Geometry, Producer, Publish, RegionBuf, Signal, Wait, Waited};

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

struct Noop;

impl Signal for Noop {
    fn signal(&self) {}
}

impl Wait for Noop {
    fn wait(&self, _timeout: Duration) -> Waited {
        Waited::TimedOut
    }
}

#[test]
fn producer_per_block_path_does_not_allocate() {
    let storage = RegionBuf::new(Geometry {
        lanes: 1,
        input_channels: 2,
        output_channels: 6,
        max_frames: 480,
        sample_rate: 48_000,
        latency_bound_frames: 1_024,
    })
    .unwrap();
    let noop = Noop;
    let mut producer = Producer::new(storage.region(), [Some(&noop), None], &noop);
    producer
        .announce(
            Geometry {
                lanes: 1,
                input_channels: 2,
                output_channels: 6,
                max_frames: 480,
                sample_rate: 48_000,
                latency_bound_frames: 1_024,
            },
            1,
        )
        .unwrap();
    let mut consumer = streamring::Consumer::new(storage.region(), &noop, [None, None]);
    consumer.validate_and_ready(2).unwrap();
    let publisher = producer.publisher().unwrap();
    let touched = AtomicU32::new(0);

    assert_no_alloc(|| {
        assert_eq!(
            publisher.try_publish(storage.region(), 0, 1, Some(&noop), |slot| {
                slot.write(0, 0, 0.5);
                touched.fetch_add(1, Ordering::Relaxed);
            }),
            Publish::Published
        );
        let _ = publisher.completed(storage.region(), 0);
        let _ = publisher.try_read(storage.region(), 0, 1, |_| {});
        // The producer's own methods delegate to the same real-time half.
        assert_eq!(
            producer.try_publish(0, 2, |slot| {
                slot.write(1, 0, 0.25);
                touched.fetch_add(1, Ordering::Relaxed);
            }),
            Publish::Published
        );
        let _ = producer.completed(0);
        let _ = producer.try_read(0, 2, |_| {});
        // Sequence 3 reuses sequence 1's slot, which is not completed: dropped.
        assert_eq!(
            publisher.try_publish(storage.region(), 0, 3, Some(&noop), |_| {
                touched.fetch_add(1, Ordering::Relaxed);
            }),
            Publish::Dropped
        );
    });
    assert_eq!(touched.load(Ordering::Relaxed), 2);
}
