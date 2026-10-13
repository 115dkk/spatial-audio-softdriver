#![forbid(unsafe_code)]

use assert_no_alloc::assert_no_alloc;
use bio_types::{PcmFormat, SampleType, Speaker};
use bitstream::{Burst, DataType};
use decode::{
    DecodeConfig,
    ddp::DdpDecoder,
    direct::{DirectLayout, DirectLayouts},
    pcm::PcmDecoder,
};

#[cfg(debug_assertions)]
#[global_allocator]
static ALLOCATOR: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

#[test]
fn pcm_configure_push_reset_do_not_allocate() {
    let mut decoder = PcmDecoder::new(DecodeConfig::default());
    let bytes = [0_u8; 480 * 12 * 4 + 3];
    let mut count = 0;
    assert_no_alloc(|| {
        for (bits, valid, kind) in [
            (16, 16, SampleType::Int),
            (24, 24, SampleType::Int),
            (32, 32, SampleType::Int),
            (32, 24, SampleType::Int),
            (32, 32, SampleType::Float),
        ] {
            decoder
                .configure(PcmFormat {
                    channels: 12,
                    sample_rate: 48_000,
                    bits_per_sample: bits,
                    valid_bits: valid,
                    sample_type: kind,
                    channel_mask: 0x2d63f,
                })
                .unwrap();
            for _ in 0..8 {
                for chunk in bytes.chunks(137) {
                    decoder
                        .push(chunk, 77, 2, &mut |scene| {
                            assert!(scene.is_consistent());
                            assert_eq!(scene.frames, 480);
                            count += 1;
                        })
                        .unwrap();
                }
            }
            decoder
                .flush(&mut |scene| {
                    assert!(scene.is_consistent());
                    assert!(scene.frames < 480);
                    count += 1;
                })
                .unwrap();
            decoder.reset();
        }
    });
    assert!(count >= 40);
}

#[test]
fn pcm_emit_buffered_does_not_allocate() {
    let mut decoder = PcmDecoder::new(DecodeConfig::default());
    decoder
        .configure(PcmFormat {
            channels: 8,
            sample_rate: 48_000,
            bits_per_sample: 32,
            valid_bits: 32,
            sample_type: SampleType::Float,
            channel_mask: 0x63f,
        })
        .unwrap();
    let bytes = [0_u8; 48 * 8 * 4 + 5];
    let mut frames = 0;
    assert_no_alloc(|| {
        for _ in 0..30 {
            decoder
                .push(&bytes, 77, 2, &mut |scene| frames += scene.frames)
                .unwrap();
            decoder.emit_buffered(&mut |scene| {
                assert!(scene.is_consistent());
                frames += scene.frames;
            });
        }
    });
    assert_eq!(frames, 30 * (48 * 8 * 4 + 5) / (8 * 4));
}

#[test]
fn thirty_two_channel_direct_configure_push_and_reset_do_not_allocate() {
    let speakers = [
        Speaker::L,
        Speaker::R,
        Speaker::C,
        Speaker::Lfe,
        Speaker::Lrs,
        Speaker::Rrs,
        Speaker::Lc,
        Speaker::Rc,
        Speaker::Cs,
        Speaker::Lss,
        Speaker::Rss,
        Speaker::Tc,
        Speaker::Tfl,
        Speaker::Tfc,
        Speaker::Tfr,
        Speaker::Trl,
        Speaker::Trc,
        Speaker::Trr,
        Speaker::Ls,
        Speaker::Rs,
        Speaker::Lw,
        Speaker::Rw,
        Speaker::Tsl,
        Speaker::Tsr,
        Speaker::L,
        Speaker::R,
        Speaker::C,
        Speaker::Lrs,
        Speaker::Rrs,
        Speaker::Lss,
        Speaker::Rss,
        Speaker::Tc,
    ];
    let mut direct = DirectLayouts::empty();
    direct
        .set(32, DirectLayout::new(&speakers).unwrap())
        .unwrap();
    let mut decoder = PcmDecoder::new(DecodeConfig {
        direct,
        ..DecodeConfig::default()
    });
    let bytes = [0_u8; 480 * 32 * 4 + 3];
    let mut count = 0;
    assert_no_alloc(|| {
        decoder
            .configure(PcmFormat {
                channels: 32,
                sample_rate: 48_000,
                bits_per_sample: 32,
                valid_bits: 32,
                sample_type: SampleType::Int,
                channel_mask: 0,
            })
            .unwrap();
        for chunk in bytes.chunks(137) {
            decoder
                .push(chunk, 77, 2, &mut |scene| {
                    assert!(scene.is_consistent());
                    count += 1;
                })
                .unwrap();
        }
        decoder.reset();
    });
    assert_eq!(count, 1);
}

#[test]
fn rejected_pushes_do_not_allocate_error_strings() {
    let mut pcm = PcmDecoder::new(DecodeConfig::default());
    let mut ddp = DdpDecoder::new(DecodeConfig::default());
    let burst = Burst {
        data_type: DataType::Eac3,
        pc: 21,
        payload: &[0; 7],
        start: 0,
    };
    assert_no_alloc(|| {
        assert!(
            pcm.push(&[0], 0, 0, &mut |_| panic!("unexpected scene"))
                .is_err()
        );
        assert!(
            ddp.push(&burst, 0, 0, &mut |_| panic!("unexpected scene"))
                .is_err()
        );
    });
}
