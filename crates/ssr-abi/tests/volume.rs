#![forbid(unsafe_code)]

use ssr_abi::*;
use zerocopy::{FromBytes, IntoBytes};

fn payload() -> VolumePayload {
    VolumePayload {
        channel_count: 2,
        mute_mask: 0,
        level_q16: [0; MAX_VOLUME_CHANNELS],
    }
}

#[test]
fn volume_gain_is_power_average_not_channel_zero() {
    let mut p = payload();
    assert_eq!(volume_gain(&p), 1.0);
    p.level_q16[..2].fill(-20 * 65536);
    assert!((volume_gain(&p) - 0.1).abs() < 1e-7);
    p.level_q16[1] = 0;
    assert!((volume_gain(&p) - (0.505_f32).sqrt()).abs() < 1e-7);
    p.mute_mask = 1;
    assert!((volume_gain(&p) - 0.5_f32.sqrt()).abs() < 1e-7);
    p.mute_mask = 3;
    assert_eq!(volume_gain(&p), 0.0);
    p.channel_count = 0;
    assert_eq!(volume_gain(&p), 0.0);
    p.channel_count = 33;
    assert_eq!(volume_gain(&p), 0.0);
}

#[test]
fn volume_records_round_trip_and_old_reader_skips_them() {
    let mut bytes = [0; 512];
    let mut writer = RecordWriter::new(&mut bytes);
    writer.set_endpoint(2);
    let mut p = payload();
    p.level_q16[1] = VOLUME_MIN_Q16;
    p.mute_mask = 1;
    assert!(writer.stream_start(7, 10));
    assert!(writer.volume(11, false, &p));
    assert!(writer.volume(12, true, &p));
    assert!(writer.stream_stop(7, 13));
    let n = writer.len();
    let parsed = records(&bytes[..n]).collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(
        parsed[1],
        Record::Volume {
            endpoint: 2,
            repeat: false,
            qpc: 11,
            channel_count: 2,
            mute_mask: 1,
            levels: p.level_q16
        }
    );
    assert!(matches!(parsed[2], Record::Volume { repeat: true, .. }));
    // The pre-volume reader's known-kind dispatch: unknown payloads are skipped
    // using the unchanged length and alignment, not mistaken for data.
    let mut at = 0;
    let mut old_kinds = Vec::new();
    while at < n {
        let (header, _) = RecordHeader::read_from_prefix(&bytes[at..n]).unwrap();
        if (RECORD_KIND_DATA..=RECORD_KIND_OVERRUN).contains(&header.kind) {
            old_kinds.push(header.kind);
        }
        at += record_size(header.len as usize);
    }
    assert_eq!(at, n);
    assert_eq!(
        old_kinds,
        [RECORD_KIND_STREAM_START, RECORD_KIND_STREAM_STOP]
    );
}

#[test]
fn legacy_16_channel_volume_parses_and_other_lengths_are_rejected() {
    let header = RecordHeader {
        kind: RECORD_KIND_VOLUME,
        flags: 0,
        epoch: 0,
        byte_pos: 0,
        qpc: 19,
        qpc_written: 0,
        len: 72,
        endpoint: 1,
        reserved: 0,
    };
    let mut payload = Vec::with_capacity(72);
    payload.extend_from_slice(&2_u32.to_le_bytes());
    payload.extend_from_slice(&1_u32.to_le_bytes());
    for level in [-20_i32 * 65536, 0].into_iter().chain([0; 14]) {
        payload.extend_from_slice(&level.to_le_bytes());
    }
    let mut bytes = vec![0; record_size(payload.len())];
    let mut writer = RecordWriter::new(&mut bytes);
    assert!(writer.push(header, &payload));
    assert!(matches!(
        records(&bytes).next(),
        Some(Ok(Record::Volume {
            endpoint: 1,
            qpc: 19,
            channel_count: 2,
            mute_mask: 1,
            levels,
            ..
        })) if levels[0] == -20 * 65536 && levels[1..].iter().all(|level| *level == 0)
    ));

    for (count, mute_mask, accepted) in [(16_u32, u16::MAX as u32, true), (17, 0, false)] {
        let mut boundary = Vec::with_capacity(72);
        boundary.extend_from_slice(&count.to_le_bytes());
        boundary.extend_from_slice(&mute_mask.to_le_bytes());
        boundary.extend_from_slice(&[0; 64]);
        let mut bytes = vec![0; record_size(boundary.len())];
        let mut writer = RecordWriter::new(&mut bytes);
        assert!(writer.push(header, &boundary));
        assert_eq!(records(&bytes).next().unwrap().is_ok(), accepted);
    }

    for len in [71_u32, 73, 135, 137] {
        let mut malformed = vec![0; record_size(len as usize)];
        let mut writer = RecordWriter::new(&mut malformed);
        assert!(writer.push(RecordHeader { len, ..header }, &vec![0; len as usize]));
        assert!(matches!(
            records(&malformed).next(),
            Some(Err(AbiError::BadLength { len: actual, .. })) if actual == len
        ));
    }
}

#[test]
fn invalid_volume_rejects_but_iteration_continues() {
    for invalid in 0..5 {
        let mut p = payload();
        match invalid {
            0 => p.channel_count = 0,
            1 => p.channel_count = 33,
            2 => p.mute_mask = 4,
            3 => p.level_q16[0] = 1,
            _ => p.level_q16[2] = -1,
        }
        let mut bytes = [0; 256];
        let mut writer = RecordWriter::new(&mut bytes);
        assert!(!writer.volume(0, false, &p));
        let header = RecordHeader {
            kind: RECORD_KIND_VOLUME,
            flags: 0,
            epoch: 0,
            byte_pos: 0,
            qpc: 0,
            qpc_written: 0,
            len: 0,
            endpoint: 1,
            reserved: 0,
        };
        assert!(writer.push(header, p.as_bytes()));
        assert!(writer.stream_stop(0, 1));
        let n = writer.len();
        let result: Vec<_> = records(&bytes[..n]).collect();
        assert!(matches!(
            result[0],
            Err(AbiError::BadVolume { endpoint: 1, .. })
        ));
        assert!(matches!(result[1], Ok(Record::StreamStop { .. })));
    }
}
