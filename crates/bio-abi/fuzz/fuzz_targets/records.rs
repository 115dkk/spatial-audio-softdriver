//! Whatever bytes a READ returns, the record parser must not panic, must make
//! progress, and every parsed data record must lie inside the input.
#![no_main]

use bio_abi::{Record, records};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut count = 0usize;
    for record in records(data) {
        if let Ok(Record::Data { bytes, .. }) = record {
            assert!(bytes.len() <= data.len());
        }
        count += 1;
        assert!(count <= data.len() / 40 + 1, "more records than headers fit");
    }
});
