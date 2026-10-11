#![forbid(unsafe_code)]

use std::sync::Arc;

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use bio_abi::{RecordWriter, wave_format_bytes};
use bio_types::{Direction, Layout, PcmFormat, SampleType, SourceFormat};
use brir::PreparedBrir;
use convolver::FilterBank;
use engine::{Chain, ChainConfig, ChainEvent, Prepared};

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

#[test]
fn steady_state_pcm_push_records_does_not_allocate() {
    let format = SourceFormat::Pcm(PcmFormat {
        channels: 2,
        sample_rate: 48_000,
        bits_per_sample: 32,
        valid_bits: 32,
        sample_type: SampleType::Float,
        channel_mask: 3,
    });
    let mut format_record = vec![0; 256];
    let mut writer = RecordWriter::new(&mut format_record);
    assert!(writer.format_change(1, 0, &wave_format_bytes(&format)));
    let format_len = writer.len();
    format_record.truncate(format_len);

    let mut pcm = vec![0_u8; 480 * 2 * 4];
    for frame in pcm.as_chunks_mut::<8>().0 {
        frame[..4].copy_from_slice(&0.1_f32.to_le_bytes());
        frame[4..].copy_from_slice(&(-0.1_f32).to_le_bytes());
    }
    let mut data_record = vec![0; bio_abi::record_size(pcm.len())];
    let mut writer = RecordWriter::new(&mut data_record);
    assert!(writer.data(1, 0, 1, &pcm));
    let data_len = writer.len();
    data_record.truncate(data_len);

    let ir = [1.0_f32];
    let filters = [[ir.as_slice(), ir.as_slice()]];
    let brir = Arc::new(PreparedBrir {
        layout: Layout::new(vec![Direction::new("front", 0.0, 0.0)]).unwrap(),
        bank: Arc::new(FilterBank::new(&filters, 480).unwrap()),
        rate: 48_000,
        partition: 480,
        lfe: None,
        gain: 1.0,
        limiter_ceiling: 1.0,
        ir_frames: 1,
    });
    let mut chain = Chain::new(ChainConfig::default());
    chain.set_prepared(Box::new(Prepared::new(brir, 480).unwrap()));
    chain.push_records(&format_record, &mut |_| {});
    chain.push_records(&data_record, &mut |_| {});
    assert_no_alloc(|| {
        chain.push_records(&data_record, &mut |event| {
            assert!(matches!(event, ChainEvent::Block(_)));
        });
    });
}
