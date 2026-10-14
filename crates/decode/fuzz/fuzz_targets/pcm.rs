//! Any PCM format the first bytes describe, then any bytes: configure either
//! refuses or accepts, push never panics, and every scene is consistent.
#![no_main]

use ssr_types::{PcmFormat, SampleType};
use decode::{DecodeConfig, pcm::PcmDecoder};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() < 8 {
        return;
    }
    let format = PcmFormat {
        channels: u16::from(data[0] % 10),
        sample_rate: [8_000, 44_100, 48_000, 96_000, 192_000][usize::from(data[1] % 5)],
        bits_per_sample: [16, 24, 32, 8][usize::from(data[2] % 4)],
        valid_bits: [16, 20, 24, 32][usize::from(data[3] % 4)],
        sample_type: if data[4] & 1 == 0 { SampleType::Int } else { SampleType::Float },
        channel_mask: u32::from_le_bytes([data[5], data[6], data[7], 0]) & 0x3ffff,
    };
    let mut decoder = PcmDecoder::new(DecodeConfig::default());
    if decoder.configure(format).is_err() {
        return;
    }
    for chunk in data[8..].chunks(97) {
        let _ = decoder.push(chunk, 0, 1, &mut |scene| assert!(scene.is_consistent()));
    }
});
