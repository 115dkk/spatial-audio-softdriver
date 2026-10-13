// SPDX-License-Identifier: MIT
// Cross-checks driver/core dump records with the Rust ABI parser.
#![forbid(unsafe_code)]

use std::fs;
use std::path::PathBuf;

use bio_abi::{RECORD_ALIGN, Record, records, split_dump};

fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("u32 field"))
}

fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().expect("u64 field"))
}

#[test]
fn driver_core_dump_matches_expectations() {
    let Some(path) = std::env::var_os("BIO_DRIVER_CORE_DUMP") else {
        return;
    };
    let path = PathBuf::from(path);
    let dump = fs::read(&path).expect("read driver-core dump");
    let (_, stream) = split_dump(&dump).expect("parse dump header");
    let parsed: Vec<Record<'_>> = records(stream)
        .collect::<Result<_, _>>()
        .expect("parse driver-core records");

    let mut actual = Vec::new();
    let mut offset = 0;
    while offset < stream.len() {
        let kind = u16_at(stream, offset);
        let flags = u16_at(stream, offset + 2);
        let epoch = u32_at(stream, offset + 4);
        let pos = u64_at(stream, offset + 8);
        let len = u32_at(stream, offset + 32);
        let endpoint = u16_at(stream, offset + 36);
        let payload = &stream[offset + 40..offset + 40 + len as usize];
        let sum = if kind == bio_abi::RECORD_KIND_DATA {
            payload.iter().map(|&byte| u64::from(byte)).sum()
        } else {
            0
        };
        actual.push(format!(
            "{endpoint} {kind} {epoch} {flags} {pos} {len} {sum}"
        ));
        let raw = 40 + len as usize;
        let align = RECORD_ALIGN as usize;
        offset += raw.div_ceil(align) * align;
    }
    assert_eq!(parsed.len(), actual.len());

    let expected: Vec<String> = fs::read_to_string(path.with_extension("biodump.expect"))
        .expect("read driver-core expectations")
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(actual, expected);
}
