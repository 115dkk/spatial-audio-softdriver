//! A dump file from disk (bio-replay's input) must never panic the reader.
#![no_main]

use bio_abi::{reads, records, split_dump};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok((header, stream)) = split_dump(data) {
        for chunk in reads(&header, stream) {
            for record in records(chunk.bytes) {
                let _ = record;
            }
        }
    }
});
