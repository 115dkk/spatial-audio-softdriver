#![forbid(unsafe_code)]

use bitstream::{Burst, DataType};
use decode::{DecodeConfig, DecodeError, Decoders};
use ssr_types::{BitstreamCodec, BitstreamFormat, PcmFormat, SampleType, SourceFormat};

fn mat_format(codec: BitstreamCodec) -> SourceFormat {
    SourceFormat::Bitstream(BitstreamFormat {
        codec,
        container_rate: 192_000,
        container_channels: 8,
        encoded_rate: 48_000,
        encoded_channels: 0,
    })
}

#[test]
fn unavailable_mat_selection_is_unsupported_and_leaves_no_active_adapter() {
    for codec in [BitstreamCodec::Mat20, BitstreamCodec::Mat21] {
        let mut decoders = Decoders::new(DecodeConfig::default());
        decoders
            .select(
                &SourceFormat::Pcm(PcmFormat {
                    channels: 2,
                    sample_rate: 48_000,
                    bits_per_sample: 32,
                    valid_bits: 32,
                    sample_type: SampleType::Float,
                    channel_mask: 3,
                }),
                6,
            )
            .unwrap();
        let error = decoders.select(&mat_format(codec), 7).unwrap_err();
        assert_eq!(
            error,
            DecodeError::Unsupported {
                codec: "MAT 2.1",
                detail: "the decoder is built from ring dumps, none recorded yet",
            }
        );
        let burst = Burst {
            data_type: DataType::Mat,
            pc: 22,
            payload: &[],
            start: 0,
        };
        assert_eq!(
            decoders.push_burst(&burst, 0, &mut |_| panic!("no scene")),
            Err(DecodeError::NoFormat)
        );
    }
}
