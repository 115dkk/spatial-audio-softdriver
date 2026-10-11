#![forbid(unsafe_code)]

mod common;
use bio_abi::{Record, RecordWriter, records};
use bio_sys::sal_format::{SalError, SalTranslator, parse_header};
use bio_types::{BitstreamCodec, SampleType, SourceFormat};
use common::{header, read};

#[test]
fn sal_idle_before_first_format_waits_without_committing_an_epoch() {
    let mut idle = header();
    idle.format_changes = 0;
    idle.total_bytes = 0;
    idle.rate = 0;
    idle.block_align = 0;
    let mut translator = SalTranslator::new();
    let mut bytes = [0; 1024];
    let mut writer = RecordWriter::new(&mut bytes);
    for qpc in 1..4 {
        assert_eq!(
            translator.translate(&read(idle, &[]), qpc, &mut writer),
            Ok(0)
        );
        assert!(writer.is_empty());
    }
    // A payload cannot be silently discarded just because the format is idle.
    assert!(
        translator
            .translate(&read(idle, &[1]), 4, &mut writer)
            .is_err()
    );
    assert!(writer.is_empty());
    assert_eq!(
        translator.translate(&read(header(), &[7]), 5, &mut writer),
        Ok(3)
    );
    let len = writer.len();
    assert_eq!(
        records(&bytes[..len]).next().unwrap().unwrap(),
        Record::StreamStart { epoch: 1, qpc: 5 }
    );
    // A reset-looking invalid header during an active stream is still an error.
    assert!(
        translator
            .translate(&read(idle, &[]), 6, &mut RecordWriter::new(&mut bytes))
            .is_err()
    );
}

#[test]
fn sal_header_round_trip() {
    let mut h = header();
    h.payload_bytes = 4;
    h.header_bytes = 88;
    assert_eq!(parse_header(&read(h, &[1, 2, 3, 4])), Ok(h));
}

#[test]
fn sal_rejects_truncated_and_inconsistent_lengths() {
    let bytes = read(header(), &[1; 4]);
    for len in 0..80 {
        assert!(matches!(
            parse_header(&bytes[..len]),
            Err(SalError::Truncated { .. })
        ));
    }
    assert_eq!(parse_header(&bytes[..83]), Err(SalError::BadLengths));
    for (offset, value) in [(4, 79u32), (4, u32::MAX), (8, u32::MAX)] {
        let mut bad = bytes.clone();
        bad[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        assert_eq!(parse_header(&bad), Err(SalError::BadLengths));
    }
}

#[test]
fn sal_ddplus_is_eac3() {
    let h = header();
    let SourceFormat::Bitstream(format) = h.source_format().unwrap() else {
        panic!("expected bitstream")
    };
    assert_eq!(format.codec, BitstreamCodec::Eac3);
    assert_eq!(
        (
            format.container_rate,
            format.container_channels,
            format.encoded_rate,
            format.encoded_channels
        ),
        (192_000, 2, 0, 0)
    );
    let wave = h.wave_format();
    assert_eq!(wave.len(), 52);
    assert_eq!(&wave[8..12], &768_000u32.to_le_bytes());
    assert_eq!(&wave[16..18], &22u16.to_le_bytes());
    assert_eq!(&wave[40..], &[0; 12]);
}

#[test]
fn sal_float_is_pcm() {
    let mut h = header();
    h.subformat[0] = 3;
    h.subformat[4..6].fill(0);
    h.rate = 48_000;
    h.bits = 32;
    h.valid_bits = 32;
    h.block_align = 8;
    let SourceFormat::Pcm(format) = h.source_format().unwrap() else {
        panic!("expected PCM")
    };
    assert_eq!(format.sample_type, SampleType::Float);
    assert_eq!(
        (
            format.sample_rate,
            format.channels,
            format.bits_per_sample,
            format.valid_bits,
            format.channel_mask
        ),
        (48_000, 2, 32, 32, 3)
    );
    assert_eq!(h.wave_format().len(), 40);
}

#[test]
fn sal_records_parse_back_with_positions_qpc_and_drops() {
    let mut translator = SalTranslator::new();
    let mut bytes = [0; 1024];
    let mut writer = RecordWriter::new(&mut bytes);
    let mut h = header();
    assert_eq!(
        translator.translate(&read(h, &[1, 2]), 20, &mut writer),
        Ok(3)
    );
    h.dropped_bytes = 7;
    assert_eq!(
        translator.translate(&read(h, &[3, 4]), 30, &mut writer),
        Ok(2)
    );
    h.dropped_bytes = 11;
    assert_eq!(translator.translate(&read(h, &[]), 40, &mut writer), Ok(1));
    assert_eq!(translator.translate(&read(h, &[5]), 50, &mut writer), Ok(1));
    let len = writer.len();
    let parsed: Vec<_> = records(&bytes[..len]).collect::<Result<_, _>>().unwrap();
    assert_eq!(
        parsed,
        vec![
            Record::StreamStart { epoch: 1, qpc: 20 },
            Record::FormatChange {
                epoch: 1,
                qpc: 20,
                format: h.source_format().unwrap()
            },
            Record::Data {
                epoch: 1,
                flags: 0,
                pos: 0,
                qpc: 20,
                qpc_written: 0,
                bytes: &[1, 2]
            },
            Record::Overrun {
                pos: 2,
                qpc: 30,
                lost_bytes: 7
            },
            Record::Data {
                epoch: 1,
                flags: 0,
                pos: 9,
                qpc: 30,
                qpc_written: 0,
                bytes: &[3, 4]
            },
            Record::Overrun {
                pos: 11,
                qpc: 40,
                lost_bytes: 4
            },
            Record::Data {
                epoch: 1,
                flags: 0,
                pos: 15,
                qpc: 50,
                qpc_written: 0,
                bytes: &[5]
            },
        ]
    );
}

#[test]
fn sal_format_change_bumps_epoch_for_each_format_field() {
    for field in 0..8 {
        let mut h = header();
        let mut t = SalTranslator::new();
        let mut bytes = [0; 1024];
        t.translate(&read(h, &[]), 1, &mut RecordWriter::new(&mut bytes))
            .unwrap();
        match field {
            0 => h.subformat[0] = 11,
            1 => h.rate = 48_000,
            2 => h.channels = 8,
            3 => h.bits = 32,
            4 => h.channel_mask = 0x63f,
            5 => h.format_changes += 1,
            6 => h.valid_bits = 8,
            _ => h.block_align = 8,
        }
        let mut writer = RecordWriter::new(&mut bytes);
        assert_eq!(t.translate(&read(h, &[7]), 2, &mut writer), Ok(3));
        let len = writer.len();
        assert_eq!(
            records(&bytes[..len]).next().unwrap().unwrap(),
            Record::StreamStart { epoch: 2, qpc: 2 }
        );
        assert_eq!(
            t.translate(&read(h, &[]), 3, &mut RecordWriter::new(&mut bytes)),
            Ok(0)
        );
    }
}

#[test]
fn sal_buffer_full_retry_is_transactional() {
    let mut t = SalTranslator::new();
    let h = header();
    let input = read(h, &[42; 100]);
    let mut small = [0; 200];
    let mut writer = RecordWriter::new(&mut small);
    assert!(writer.stream_stop(99, 0));
    let before = writer.len();
    assert_eq!(
        t.translate(&input, 5, &mut writer),
        Err(SalError::BufferFull)
    );
    assert_eq!(writer.len(), before);
    let mut enough = [0; 1024];
    let mut writer = RecordWriter::new(&mut enough);
    assert_eq!(t.translate(&input, 5, &mut writer), Ok(3));
    let len = writer.len();
    assert_eq!(
        records(&enough[..len]).next().unwrap().unwrap(),
        Record::StreamStart { epoch: 1, qpc: 5 }
    );
}

#[test]
fn sal_invalid_format_does_not_commit_state() {
    let mut h = header();
    h.rate = 0;
    let mut t = SalTranslator::new();
    let mut bytes = [0; 512];
    let mut writer = RecordWriter::new(&mut bytes);
    assert!(matches!(
        t.translate(&read(h, &[]), 1, &mut writer),
        Err(SalError::Format(_))
    ));
    assert!(writer.is_empty());
    assert_eq!(t.translate(&read(header(), &[]), 2, &mut writer), Ok(2));
    h.rate = u32::MAX;
    assert!(h.source_format().is_err());
}
