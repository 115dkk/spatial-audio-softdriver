#![forbid(unsafe_code)]

use bio_sys::sal_format::{SAL_HEADER_LEN, SalHeader};

pub fn header() -> SalHeader {
    SalHeader {
        version: 3,
        header_bytes: 80,
        payload_bytes: 0,
        sequence: 17,
        total_bytes: 1024,
        dropped_bytes: 0,
        subformat: [
            10, 0, 0, 0, 0xea, 0x0c, 0x10, 0, 0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71,
        ],
        format_changes: 4,
        rate: 192_000,
        channel_mask: 3,
        channels: 2,
        bits: 16,
        valid_bits: 16,
        block_align: 4,
    }
}

pub fn read(h: SalHeader, payload: &[u8]) -> Vec<u8> {
    let mut b = Vec::new();
    for n in [h.version, h.header_bytes, payload.len() as u32, 0] {
        b.extend_from_slice(&n.to_le_bytes());
    }
    for n in [h.sequence, h.total_bytes, h.dropped_bytes] {
        b.extend_from_slice(&n.to_le_bytes());
    }
    b.extend_from_slice(&h.subformat);
    b.extend_from_slice(&h.format_changes.to_le_bytes());
    b.extend_from_slice(&h.rate.to_le_bytes());
    b.extend_from_slice(&h.channel_mask.to_le_bytes());
    for n in [h.channels, h.bits, h.valid_bits, h.block_align] {
        b.extend_from_slice(&n.to_le_bytes());
    }
    assert_eq!(b.len(), SAL_HEADER_LEN);
    b.resize(h.header_bytes as usize, 0);
    b.extend_from_slice(payload);
    b
}
