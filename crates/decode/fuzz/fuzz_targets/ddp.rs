//! An arbitrary E-AC-3 burst payload through the DD+ adapter (and so through
//! eac3's parser and decoder): errors are fine, panics are not.
#![no_main]

use bitstream::{Burst, DataType};
use decode::{DecodeConfig, ddp::DdpDecoder};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut decoder = DdpDecoder::new(DecodeConfig::default());
    for payload in data.split(|&b| b == 0xA5).take(4) {
        let burst = Burst {
            data_type: DataType::Eac3,
            pc: 21,
            payload,
            start: 0,
        };
        let _ = decoder.push(&burst, 0, 1, &mut |scene| assert!(scene.is_consistent()));
    }
});
