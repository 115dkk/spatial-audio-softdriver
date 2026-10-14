#![forbid(unsafe_code)]

use ssr_sys::sal_format::{SAL_HEADER_LEN, SalHeader};

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

/// The exclusive period device tests ask for: the device's minimum unless
/// `SSR_TEST_EXCLUSIVE_PERIOD_MS` names one. CI sets 20 ms because VB-CABLE,
/// a WaveCyclic driver, signals no faster than the system timer (ADR 0026).
#[cfg(windows)]
#[allow(dead_code)] // each test binary that includes this module uses a subset
pub fn exclusive_period() -> Option<std::time::Duration> {
    std::env::var("SSR_TEST_EXCLUSIVE_PERIOD_MS")
        .ok()
        .and_then(|ms| ms.parse::<u64>().ok())
        .filter(|&ms| ms > 0)
        .map(std::time::Duration::from_millis)
}
