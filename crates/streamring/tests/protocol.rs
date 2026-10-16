#![forbid(unsafe_code)]

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use streamring::{
    Consumer, Fault, Geometry, HeaderError, InProcessEvent, Pipeline, Producer, Publish, Region,
    RegionBuf, RegionMemory, State, Waited,
};

fn geometry(lanes: u32) -> Geometry {
    Geometry {
        lanes,
        input_channels: 2,
        output_channels: 6,
        max_frames: 64,
        sample_rate: 48_000,
        latency_bound_frames: 256,
    }
}

#[test]
fn header_layout_is_pinned() {
    assert_eq!(streamring::MAGIC.to_le_bytes(), *b"SSRR");
    assert_eq!(streamring::HEADER_BYTES, 512);
    assert_eq!(geometry(1).total_bytes().unwrap(), 3_712);
    assert_eq!(geometry(2).total_bytes().unwrap(), 6_784);
}

#[test]
fn readiness_in_order_late_drop_fault_and_close() {
    let storage = RegionBuf::new(geometry(1)).unwrap();
    let work = InProcessEvent::new();
    let ready = InProcessEvent::new();
    let done = InProcessEvent::new();
    let mut producer = Producer::new(storage.region(), [Some(&work), None], &ready);
    producer.announce(geometry(1), 11).unwrap();
    assert_eq!(storage.region().state(), Some(State::Announced));
    assert_eq!(producer.wait_ready(Duration::ZERO), Waited::TimedOut);

    let order = Arc::new(Mutex::new(Vec::new()));
    thread::scope(|scope| {
        let order = Arc::clone(&order);
        let storage_ref = &storage;
        let ready_ref = &ready;
        let done_ref = &done;
        let work_ref = &work;
        scope.spawn(move || {
            let mut consumer =
                Consumer::new(storage_ref.region(), ready_ref, [Some(done_ref), None]);
            consumer.validate_and_ready(22).unwrap();
            let mut buffer = vec![0.0; geometry(1).samples_per_slot()];
            for _ in 0..3 {
                loop {
                    if let Some(sequence) = consumer.next_published(0) {
                        order.lock().unwrap().push(sequence);
                        assert!(consumer.slot_mut(0, sequence, &mut buffer, |samples| {
                            for sample in samples {
                                *sample *= 0.5;
                            }
                        }));
                        if sequence == 1 {
                            thread::sleep(Duration::from_millis(10));
                        }
                        consumer.complete(0, sequence);
                        break;
                    }
                    if storage_ref.region().state() == Some(State::Closing) {
                        return;
                    }
                    let _ = streamring::Wait::wait(work_ref, Duration::from_millis(20));
                }
            }
        });

        assert_eq!(
            producer.wait_ready(Duration::from_secs(1)),
            Waited::Signalled
        );
        assert_eq!(
            producer.try_publish(0, 1, |slot| assert!(slot.copy_from(&vec![2.0; 384]))),
            Publish::Published
        );
        assert_eq!(
            producer.try_publish(0, 2, |slot| assert!(slot.copy_from(&vec![4.0; 384]))),
            Publish::Published
        );
        assert_eq!(producer.try_publish(0, 3, |_| {}), Publish::Dropped);
        thread::sleep(Duration::from_millis(30));
        assert_eq!(
            producer.try_publish(0, 3, |slot| assert!(slot.copy_from(&vec![6.0; 384]))),
            Publish::Published
        );
        while producer.completed(0) < 3 {
            let _ = streamring::Wait::wait(&done, Duration::from_millis(20));
        }
        let mut first = 0.0;
        assert!(producer.try_read(0, 3, |slot| first = slot.read(0, 0)));
        assert_eq!(first, 3.0);
        producer.close();
    });
    assert_eq!(*order.lock().unwrap(), vec![1, 2, 3]);
    assert_eq!(storage.region().state(), Some(State::Closing));
}

#[test]
fn publisher_and_owned_region_cross_threads_after_announcement() {
    let storage = Arc::new(RegionBuf::new(geometry(1)).unwrap());
    let ready = Arc::new(InProcessEvent::new());
    let work = Arc::new(InProcessEvent::new());
    // The producer only announces; its real-time half outlives it.
    let publisher = {
        let mut producer = Producer::new(storage.region(), [None, None], ready.as_ref());
        producer.announce(geometry(1), 1).unwrap();
        producer.publisher().unwrap()
    };
    let consumer_storage = Arc::clone(&storage);
    let consumer_ready = Arc::clone(&ready);
    let consumer = thread::spawn(move || {
        let mut consumer = Consumer::new(
            consumer_storage.region(),
            consumer_ready.as_ref(),
            [None, None],
        );
        consumer.validate_and_ready(2).unwrap();
        let mut buffer = vec![0.0; geometry(1).samples_per_slot()];
        for sequence in 1..=3 {
            while consumer.next_published(0) != Some(sequence) {
                thread::yield_now();
            }
            assert!(consumer.slot_mut(0, sequence, &mut buffer, |samples| samples[0] *= 2.0));
            consumer.complete(0, sequence);
        }
    });
    // The publishing thread holds nothing but the publisher and owned memory.
    let memory: Box<dyn RegionMemory> = Box::new(Arc::clone(&storage));
    let publishing = thread::spawn(move || {
        while publisher.state(memory.region()) != Some(State::Ready) {
            thread::yield_now();
        }
        let mut results = Vec::new();
        for sequence in 1..=3_u32 {
            assert_eq!(
                publisher.try_publish(memory.region(), 0, sequence, Some(work.as_ref()), |slot| {
                    slot.write(0, 0, sequence as f32 * 0.25);
                }),
                Publish::Published
            );
            while publisher.completed(memory.region(), 0) != sequence {
                thread::yield_now();
            }
            assert!(publisher.try_read(memory.region(), 0, sequence, |slot| {
                results.push(slot.read(0, 0));
            }));
        }
        results
    });
    consumer.join().unwrap();
    assert_eq!(publishing.join().unwrap(), [0.5, 1.0, 1.5]);
}

#[test]
fn late_blocks_are_processed_without_skipping() {
    let storage = RegionBuf::new(geometry(1)).unwrap();
    let event = InProcessEvent::new();
    let mut producer = Producer::new(storage.region(), [Some(&event), None], &event);
    producer.announce(geometry(1), 1).unwrap();
    let mut consumer = Consumer::new(storage.region(), &event, [Some(&event), None]);
    consumer.validate_and_ready(2).unwrap();
    assert_eq!(producer.try_publish(0, 1, |_| {}), Publish::Published);
    assert_eq!(producer.try_publish(0, 2, |_| {}), Publish::Published);
    assert_eq!(consumer.next_published(0), Some(1));
    consumer.complete(0, 1);
    assert_eq!(consumer.next_published(0), Some(2));
    consumer.complete(0, 2);
}

#[test]
fn fault_code_and_peer_gone_are_observable() {
    let storage = RegionBuf::new(geometry(1)).unwrap();
    let event = InProcessEvent::new();
    let mut producer = Producer::new(storage.region(), [Some(&event), None], &event);
    producer.announce(geometry(1), 1).unwrap();
    let consumer = Consumer::new(storage.region(), &event, [Some(&event), None]);
    consumer.fault(Fault::EngineFailed as u32);
    assert_eq!(
        producer.wait_ready(Duration::from_secs(1)),
        Waited::PeerGone
    );
    assert_eq!(storage.region().fault_code(), Fault::EngineFailed as u32);
    event.peer_gone();
    assert_eq!(
        streamring::Wait::wait(&event, Duration::from_secs(1)),
        Waited::PeerGone
    );
}

#[test]
fn hostile_headers_are_rejected() {
    let cases: &[(usize, u32, HeaderError)] = &[
        (0, 0, HeaderError::Magic),
        (1, 99, HeaderError::Version),
        (2, 0, HeaderError::TotalBytes),
        (
            3,
            3,
            HeaderError::Geometry(streamring::GeometryError::LaneCount),
        ),
        (
            4,
            65,
            HeaderError::Geometry(streamring::GeometryError::Channels),
        ),
        (
            6,
            65_537,
            HeaderError::Geometry(streamring::GeometryError::Frames),
        ),
        (
            7,
            999,
            HeaderError::Geometry(streamring::GeometryError::SampleRate),
        ),
        (16, 0, HeaderError::LaneGeometry),
        (17, 512, HeaderError::LaneGeometry),
        (18, 0, HeaderError::LaneGeometry),
    ];
    for &(index, value, expected) in cases {
        let prefix: Vec<_> = (0..geometry(1).total_bytes().unwrap() / 4 - 32)
            .map(|_| std::sync::atomic::AtomicU32::new(0))
            .collect();
        let counters: Vec<_> = (0..16)
            .map(|_| std::sync::atomic::AtomicU64::new(0))
            .collect();
        let event = InProcessEvent::new();
        let region = Region::new(&prefix, &counters);
        let mut producer = Producer::new(region, [Some(&event), None], &event);
        producer.announce(geometry(1), 1).unwrap();
        prefix[index].store(value, std::sync::atomic::Ordering::Relaxed);
        let mut consumer = Consumer::new(region, &event, [Some(&event), None]);
        assert_eq!(consumer.validate_and_ready(2), Err(expected));
    }
}

#[test]
fn cached_geometry_ignores_later_header_mutation() {
    let prefix: Vec<_> = (0..geometry(1).total_bytes().unwrap() / 4 - 32)
        .map(|_| std::sync::atomic::AtomicU32::new(0))
        .collect();
    let counters: Vec<_> = (0..16)
        .map(|_| std::sync::atomic::AtomicU64::new(0))
        .collect();
    let event = InProcessEvent::new();
    let region = Region::new(&prefix, &counters);
    let mut producer = Producer::new(region, [Some(&event), None], &event);
    producer.announce(geometry(1), 1).unwrap();
    let mut consumer = Consumer::new(region, &event, [Some(&event), None]);
    consumer.validate_and_ready(2).unwrap();

    prefix[6].store(1, std::sync::atomic::Ordering::Relaxed);
    prefix[16].store(0, std::sync::atomic::Ordering::Relaxed);
    let samples = geometry(1).samples_per_slot();
    assert_eq!(
        producer.try_publish(0, 2, |slot| assert!(slot.copy_from(&vec![7.0; samples]))),
        Publish::Published
    );
    assert_eq!(consumer.next_published(0), Some(1));
    consumer.complete(0, 1);
    assert_eq!(consumer.next_published(0), Some(2));
    let mut buffer = vec![0.0; samples];
    assert!(consumer.slot_mut(0, 2, &mut buffer, |_| {}));
    assert_eq!(buffer[0], 7.0);
}

#[test]
fn both_lanes_are_independent() {
    let storage = RegionBuf::new(geometry(2)).unwrap();
    let event = InProcessEvent::new();
    let mut producer = Producer::new(storage.region(), [Some(&event), Some(&event)], &event);
    producer.announce(geometry(2), 1).unwrap();
    let mut consumer = Consumer::new(storage.region(), &event, [Some(&event), Some(&event)]);
    consumer.validate_and_ready(2).unwrap();

    assert_eq!(producer.try_publish(1, 1, |_| {}), Publish::Published);
    assert_eq!(consumer.next_published(0), None);
    assert_eq!(consumer.next_published(1), Some(1));
    consumer.complete(1, 1);
    assert_eq!(producer.completed(0), 0);
    assert_eq!(producer.completed(1), 1);
}

#[test]
fn corrupt_and_truncated_regions_are_rejected_at_boundary() {
    let event = InProcessEvent::new();
    let prefix: Vec<_> = (0..128)
        .map(|_| std::sync::atomic::AtomicU32::new(0))
        .collect();
    let counters: Vec<_> = (0..16)
        .map(|_| std::sync::atomic::AtomicU64::new(0))
        .collect();
    let region = Region::new(&prefix, &counters);
    let mut consumer = Consumer::new(region, &event, [None, None]);
    assert_eq!(consumer.validate_and_ready(1), Err(HeaderError::Magic));

    let tiny_prefix = [std::sync::atomic::AtomicU32::new(0)];
    let tiny = Region::new(&tiny_prefix, &[]);
    let mut consumer = Consumer::new(tiny, &event, [None, None]);
    assert_eq!(consumer.validate_and_ready(1), Err(HeaderError::TooShort));
}

#[test]
fn pipeline_rule_covers_first_misses_generation_and_late_results() {
    let mut pipeline = Pipeline::new();
    assert_eq!(pipeline.output_sequence(0, 1, 0), None);
    pipeline.published(1, 0, 1);
    assert_eq!(pipeline.output_sequence(1, 1, 1), Some(1));
    assert_eq!(pipeline.output_sequence(2, 1, 1), None);
    pipeline.published(2, 2, 1);
    assert_eq!(pipeline.output_sequence(3, 1, 1), None);
    assert_eq!(pipeline.output_sequence(4, 1, 2), None);
    pipeline.published(3, 4, 1);
    assert_eq!(pipeline.output_sequence(5, 2, 3), None);
}
