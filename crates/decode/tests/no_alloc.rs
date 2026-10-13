#![forbid(unsafe_code)]

use assert_no_alloc::assert_no_alloc;
use bio_types::{PcmFormat, SampleType};
use bitstream::{Burst, DataType};
use decode::{DecodeConfig, ddp::DdpDecoder, pcm::PcmDecoder};

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
            decoder.reset();
        }
    });
    assert!(count >= 40);
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
