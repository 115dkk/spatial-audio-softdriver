#![forbid(unsafe_code)]

use assert_no_alloc::assert_no_alloc;
use bitstream::{Burst, DataType};
use decode::{
    DecodeConfig,
    ddp::{DdpBack, DdpDecoded, DdpDecoder, DdpFront},
    direct::{DirectLayout, DirectLayouts},
    pcm::PcmDecoder,
};
use ssr_types::{PcmFormat, SampleType, Speaker};

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

#[test]
fn ddp_place_does_not_allocate() {
    let fixture: &[u8] = include_bytes!("fixtures/ddp/tones-5.1.eac3");
    let size = eac3::parse_header(fixture).unwrap().frame_size;
    // The decode-thread half allocates; it runs outside the guard.
    let mut front = DdpFront::new();
    let mut decoded = [DdpDecoded::new(), DdpDecoded::new()];
    front.decode(&fixture[..size], &mut decoded[0]).unwrap();
    front
        .decode(&fixture[size..2 * size], &mut decoded[1])
        .unwrap();
    let mut back = DdpBack::new(DecodeConfig::default());
    let mut scenes = 0;
    let mut frames = 0;
    assert_no_alloc(|| {
        for (index, decoded) in decoded.iter().enumerate() {
            back.place(decoded, 40 + index as u64, 3, &mut |scene| {
                assert!(scene.is_consistent());
                assert_eq!((scene.epoch, scene.source_qpc), (3, 40 + index as u64));
                assert_eq!(scene.objects.len(), 5);
                assert!(scene.lfe.is_some());
                scenes += 1;
                frames += scene.frames;
            })
            .unwrap();
        }
        // An empty delivery is refused without allocating an error string.
        assert!(
            back.place(&DdpDecoded::new(), 0, 3, &mut |_| panic!(
                "empty delivery emitted"
            ))
            .is_err()
        );
        back.reset();
        back.place(&decoded[0], 50, 4, &mut |scene| frames += scene.frames)
            .unwrap();
    });
    assert_eq!((scenes, frames), (2, 3 * 1536));
}
