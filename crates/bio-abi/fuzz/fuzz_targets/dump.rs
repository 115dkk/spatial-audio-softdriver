//! A dump file from disk (bio-replay's input) must never panic the reader.
#![no_main]

use bio_abi::{records, split_dump};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok((_, stream)) = split_dump(data) {
        for record in records(stream) {
            let _ = record;
        }
    }
});
