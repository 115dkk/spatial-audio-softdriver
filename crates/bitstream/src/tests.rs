#![forbid(unsafe_code)]

use proptest::prelude::*;

use super::*;

#[derive(Debug, PartialEq, Eq)]
struct OwnedBurst {
    data_type: DataType,
    pc: u16,
    payload: Vec<u8>,
    start: u64,
}

fn collect(burst: Burst<'_>, out: &mut Vec<OwnedBurst>) {
    out.push(OwnedBurst {
        data_type: burst.data_type,
        pc: burst.pc,
        payload: burst.payload.to_vec(),
        start: burst.start,
    });
}

fn decode(stream: &[u8], chunks: &[usize]) -> (Vec<OwnedBurst>, DeframerStats) {
    let mut deframer = Deframer::new();
    let mut bursts = Vec::new();
    let mut position = 0;
    for &size in chunks.iter().cycle() {
        if position == stream.len() {
            break;
        }
        let end = (position + size).min(stream.len());
        deframer.push(&stream[position..end], &mut |burst| {
            collect(burst, &mut bursts)
        });
        position = end;
    }
    (bursts, deframer.stats())
}

fn payload(length: usize) -> Vec<u8> {
    (0..length).map(|i| (i % 251) as u8).collect()
}

#[test]
fn ac3_eac3_mat_round_trip_even_and_odd_payloads() {
    let mut stream = Vec::new();
    let mut expected = Vec::new();
    for (data_type, length) in [
        (DataType::Ac3, 1536),
        (DataType::Ac3, 1537),
        (DataType::Eac3, 12_000),
        (DataType::Eac3, 12_001),
        (DataType::Mat, 61_424),
        (DataType::Mat, 61_423),
    ] {
        let bytes = payload(length);
        let start = stream.len() as u64;
        frame_burst(
            data_type,
            &bytes,
            repetition_period(data_type).unwrap(),
            &mut stream,
        );
        expected.push((data_type, bytes, start));
    }
    let (bursts, stats) = decode(&stream, &[1, 3, 4096, 7, 8191]);
    for (burst, (data_type, bytes, start)) in bursts.iter().zip(&expected) {
        assert_eq!(burst.data_type, *data_type);
        assert_eq!(burst.payload, *bytes);
        assert_eq!(burst.start, *start);
    }
    assert_eq!(bursts.len(), expected.len());
    assert_eq!(
        stats,
        DeframerStats {
            bursts: 6,
            resyncs: 0,
            skipped_bytes: 0
        }
    );
}

#[test]
fn framing_matches_wire_bytes_and_pd_units() {
    for (data_type, pc, pd, period) in [
        (DataType::Ac3, 1_u16, 24_u16, 16),
        (DataType::Eac3, 21, 3, 16),
        (DataType::Mat, 22, 3, 16),
        (DataType::Dts, 11, 24, 2048),
        (DataType::Dts, 12, 24, 4096),
        (DataType::Dts, 13, 24, 8192),
        (DataType::DtsHd, 17, 3, 2048),
        (DataType::DtsHd, 0x0511, 3, 65_536),
        (DataType::Other(21), 21, 3, 16),
        (DataType::Other(31), 31, 24, 16),
    ] {
        let mut stream = vec![0xab];
        frame_burst(data_type, &[0x0b, 0x77, 0x19], period, &mut stream);
        assert_eq!(&stream[1..5], &[0x72, 0xf8, 0x1f, 0x4e]);
        assert_eq!(&stream[5..7], &pc.to_le_bytes());
        assert_eq!(&stream[7..9], &pd.to_le_bytes());
        assert_eq!(&stream[9..13], &[0x77, 0x0b, 0, 0x19]);
        assert!(stream[13..].iter().all(|&byte| byte == 0));
        assert_eq!(stream.len(), period + 1);
        let (bursts, _) = decode(&stream[1..], &[1]);
        assert_eq!(bursts[0].pc, pc);
        assert_eq!(bursts[0].payload, [0x0b, 0x77, 0x19]);
    }
}

#[test]
fn literal_wire_bursts_preserve_pc_and_round_up_bit_lengths() {
    // Pd = 17 bits occupies three codec bytes in two carrier words. Pc's
    // stream/type information bits must survive without affecting its type.
    let stream = [
        0x72, 0xf8, 0x1f, 0x4e, 0x81, 0xab, 17, 0, 0x77, 0x0b, 0, 0x80,
    ];
    for split in 0..=stream.len() {
        let mut deframer = Deframer::new();
        let mut bursts = Vec::new();
        deframer.push(&stream[..split], &mut |burst| collect(burst, &mut bursts));
        deframer.push(&[], &mut |_| panic!("empty input must not deliver"));
        deframer.push(&stream[split..], &mut |burst| collect(burst, &mut bursts));
        assert_eq!(
            bursts,
            vec![OwnedBurst {
                data_type: DataType::Ac3,
                pc: 0xab81,
                payload: vec![0x0b, 0x77, 0x80],
                start: 0,
            }]
        );
    }
}

#[test]
fn garbage_padding_and_overlapping_sync_candidates_recover() {
    let mut stream = vec![0, 0, 0x72, 0x72, 0xf8, 0, 0x1f, 0x4e, 0xff];
    let leading = stream.len();
    frame_burst(DataType::Ac3, &[0x72, 0xf8, 0x1f, 0x4e], 6144, &mut stream);
    // The second burst is three bytes late. Padding in the first period must
    // not count as discarded bytes, but these extra zeros do.
    stream.extend_from_slice(&[0, 0, 0]);
    let second = stream.len();
    frame_burst(DataType::Ac3, &[8, 9, 10], 6144, &mut stream);
    let (bursts, stats) = decode(&stream, &[1, 2, 5]);
    assert_eq!(bursts.len(), 2);
    assert_eq!(bursts[0].start, leading as u64);
    assert_eq!(bursts[1].start, second as u64);
    assert_eq!(
        stats,
        DeframerStats {
            bursts: 2,
            resyncs: 2,
            skipped_bytes: leading as u64 + 3,
        }
    );
    assert_eq!(decode(&stream, &[stream.len()]), (bursts, stats));

    let mut overlap = vec![0x72];
    frame_burst(DataType::Ac3, &[1, 2], 6144, &mut overlap);
    let (bursts, stats) = decode(&overlap, &[1]);
    assert_eq!(bursts[0].start, 1);
    assert_eq!(stats.resyncs, 1);
    assert_eq!(stats.skipped_bytes, 1);
}

#[test]
fn early_bursts_and_nonzero_padding_are_not_hidden_by_periods() {
    let mut stream = Vec::new();
    frame_burst(DataType::Ac3, &[1, 2], 16, &mut stream);
    stream[12] = 0xff;
    frame_burst(DataType::Mat, &[3, 4], 61_440, &mut stream);
    let (bursts, stats) = decode(&stream, &[7]);
    assert_eq!(bursts[1].start, 16);
    assert_eq!(stats.resyncs, 1);
    assert_eq!(stats.skipped_bytes, 1);
}

#[test]
fn dts_subtypes_determine_expected_positions() {
    for (data_type, period) in [
        (DataType::Dts, 2048),
        (DataType::Dts, 4096),
        (DataType::Dts, 8192),
        (DataType::DtsHd, 2048),
        (DataType::DtsHd, 4096),
        (DataType::DtsHd, 8192),
        (DataType::DtsHd, 16_384),
        (DataType::DtsHd, 32_768),
        (DataType::DtsHd, 65_536),
    ] {
        let mut stream = Vec::new();
        frame_burst(data_type, &[1, 2, 3], period, &mut stream);
        frame_burst(data_type, &[4, 5, 6], period, &mut stream);
        let (bursts, stats) = decode(&stream, &[3, 4093]);
        assert_eq!(bursts[1].start, period as u64);
        assert_eq!(
            stats,
            DeframerStats {
                bursts: 2,
                resyncs: 0,
                skipped_bytes: 0
            }
        );
    }
}

#[test]
fn zero_pd_null_pause_and_codec_bursts_are_delivered() {
    let mut stream = Vec::new();
    for (data_type, period) in [
        (DataType::Null, 16),
        (DataType::Pause, 32),
        (DataType::Ac3, 6144),
    ] {
        frame_burst(data_type, &[], period, &mut stream);
    }
    frame_burst(DataType::Eac3, &[1], 24_576, &mut stream);
    let (bursts, stats) = decode(&stream, &[1]);
    assert_eq!(bursts.len(), 4);
    assert_eq!(bursts[0].data_type, DataType::Null);
    assert_eq!(bursts[1].data_type, DataType::Pause);
    assert!(bursts[..3].iter().all(|burst| burst.payload.is_empty()));
    assert_eq!(
        bursts.iter().map(|burst| burst.start).collect::<Vec<_>>(),
        [0, 16, 48, 6192]
    );
    assert_eq!(
        stats,
        DeframerStats {
            bursts: 4,
            resyncs: 0,
            skipped_bytes: 0
        }
    );
}

#[test]
fn partial_mat_exposes_only_the_received_codec_order_prefix() {
    let bytes = payload(61_423);
    let mut stream = Vec::new();
    frame_burst(DataType::Mat, &bytes, 61_440, &mut stream);
    let mut deframer = Deframer::new();
    let mut count = 0;
    for (i, byte) in stream.iter().enumerate() {
        deframer.push(std::slice::from_ref(byte), &mut |burst| {
            count += 1;
            assert_eq!(burst.payload, bytes);
        });
        let received = i + 1;
        if (8..8 + bytes.len().next_multiple_of(2)).contains(&received) {
            assert_eq!(
                deframer.partial(),
                Some((DataType::Mat, &bytes[..(received - 8) & !1]))
            );
        } else {
            assert_eq!(deframer.partial(), None);
        }
    }
    assert_eq!(count, 1);
}

#[test]
fn reset_mid_burst_forgets_payload_sync_offsets_and_stats() {
    let mut stream = Vec::new();
    frame_burst(DataType::Mat, &[1, 2, 3, 4, 5], 61_440, &mut stream);
    let mut deframer = Deframer::new();
    for cut in 0..14 {
        deframer.push(&[0xff], &mut |_| {});
        deframer.push(&stream[..cut], &mut |_| {});
        deframer.reset();
        assert_eq!(deframer.partial(), None);
        assert_eq!(deframer.stats(), DeframerStats::default());
        let mut count = 0;
        deframer.push(&stream, &mut |burst| {
            assert_eq!(burst.start, 0);
            assert_eq!(burst.payload, [1, 2, 3, 4, 5]);
            count += 1;
        });
        assert_eq!(count, 1);
        assert_eq!(
            deframer.stats(),
            DeframerStats {
                bursts: 1,
                resyncs: 0,
                skipped_bytes: 0
            }
        );
        deframer.reset();
    }
}

#[test]
fn maximum_wire_pd_is_bounded_and_preserves_following_burst() {
    for (pc, length) in [
        (1_u16, 8192_usize),
        (21, 65_535),
        (22, 65_535),
        (17, 65_535),
    ] {
        let mut stream = SYNC.to_vec();
        stream.extend_from_slice(&pc.to_le_bytes());
        stream.extend_from_slice(&u16::MAX.to_le_bytes());
        stream.extend(std::iter::repeat_n(0x55, length.next_multiple_of(2)));
        let next_start = stream.len() as u64;
        frame_burst(DataType::Null, &[], 8, &mut stream);
        let (bursts, _) = decode(&stream, &[3, 1024]);
        assert_eq!(bursts.len(), 2);
        assert_eq!(bursts[0].payload, vec![0x55; length]);
        assert_eq!(bursts[1].start, next_start);
    }
}

#[test]
fn over_limit_length_is_rejected_before_payload_assembly() {
    // No 16-bit Pd can announce more than MAX_PAYLOAD. Exercise the internal
    // bound directly, rather than pretending such a wire header exists.
    let mut deframer = Deframer::new();
    deframer.offset = 8;
    deframer.begin_payload(MAX_PAYLOAD + 1, &mut |_| {
        panic!("oversized burst delivered")
    });
    assert_eq!(deframer.partial(), None);
    let mut stream = Vec::new();
    frame_burst(DataType::Null, &[], 8, &mut stream);
    deframer.push(&stream, &mut |burst| assert_eq!(burst.start, 8));
    assert_eq!(
        deframer.stats(),
        DeframerStats {
            bursts: 1,
            resyncs: 1,
            skipped_bytes: 8
        }
    );
}

#[test]
fn framing_rejects_unrepresentable_lengths_and_invalid_periods_before_appending() {
    for (data_type, length, period) in [
        (DataType::Ac3, 8192, 10_000),
        (DataType::Mat, 65_536, 70_000),
        (DataType::Ac3, 3, 10),
        (DataType::Ac3, 0, 9),
        (DataType::Ac3, 0, 6),
        (DataType::Dts, 0, 16),
        (DataType::DtsHd, 0, 3072),
        (DataType::Other(128), 0, 16),
    ] {
        let mut out = vec![0xaa];
        let bytes = vec![0; length];
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            frame_burst(data_type, &bytes, period, &mut out);
        }));
        assert!(result.is_err());
        assert_eq!(out, [0xaa]);
    }
}

#[test]
fn repetition_periods_do_not_guess_missing_subtypes() {
    assert_eq!(repetition_period(DataType::Ac3), Some(6144));
    assert_eq!(repetition_period(DataType::Eac3), Some(24_576));
    assert_eq!(repetition_period(DataType::Mat), Some(61_440));
    for data_type in [
        DataType::Null,
        DataType::Pause,
        DataType::Dts,
        DataType::DtsHd,
        DataType::Other(31),
    ] {
        assert_eq!(repetition_period(data_type), None);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn random_chunk_sizes_preserve_bursts_starts_and_stats(
        chunks in proptest::collection::vec(1_usize..8192, 1..64),
        ac3 in proptest::collection::vec(any::<u8>(), 0..2048),
        eac3 in proptest::collection::vec(any::<u8>(), 0..24_000),
        mat in proptest::collection::vec(any::<u8>(), 0..61_424),
    ) {
        let mut stream = vec![0, 0x72, 0x72, 0xf8, 0, 0x99];
        frame_burst(DataType::Ac3, &ac3, 6144, &mut stream);
        stream.extend_from_slice(&[0, 0, 0x72]);
        frame_burst(DataType::Eac3, &eac3, 24_576, &mut stream);
        frame_burst(DataType::Pause, &[], 16, &mut stream);
        frame_burst(DataType::Mat, &mat, 61_440, &mut stream);
        prop_assert_eq!(decode(&stream, &chunks), decode(&stream, &[stream.len()]));
    }

    #[test]
    fn arbitrary_input_is_chunk_invariant(
        stream in proptest::collection::vec(any::<u8>(), 0..4096),
        chunks in proptest::collection::vec(1_usize..64, 1..32),
    ) {
        prop_assert_eq!(decode(&stream, &chunks), decode(&stream, &[stream.len().max(1)]));
    }
}
