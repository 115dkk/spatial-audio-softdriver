//! Bytes from the virtual device, cut into chunks the input itself chooses:
//! the deframer must not panic, every burst must fit MAX_PAYLOAD, and the
//! bursts must not depend on where the stream was cut.
#![no_main]

use bitstream::{Deframer, MAX_PAYLOAD};
use libfuzzer_sys::fuzz_target;

fn bursts(stream: &[u8], cuts: &[usize]) -> Vec<(u16, Vec<u8>, u64)> {
    let mut d = Deframer::new();
    let mut out = Vec::new();
    let mut rest = stream;
    for &cut in cuts {
        let n = cut.min(rest.len());
        let (head, tail) = rest.split_at(n);
        d.push(head, &mut |b| {
            assert!(b.payload.len() <= MAX_PAYLOAD);
            out.push((b.pc, b.payload.to_vec(), b.start));
        });
        let _ = d.partial();
        rest = tail;
    }
    d.push(rest, &mut |b| out.push((b.pc, b.payload.to_vec(), b.start)));
    out
}

fuzz_target!(|data: &[u8]| {
    if data.is_empty() {
        return;
    }
    let cut_count = usize::from(data[0] % 8);
    let cuts: Vec<usize> = data[1..].iter().take(cut_count).map(|&c| usize::from(c) * 37 + 1).collect();
    let stream = &data[(1 + cut_count).min(data.len())..];
    assert_eq!(bursts(stream, &cuts), bursts(stream, &[]));
});
