#![forbid(unsafe_code)]

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use bitstream::{DataType, Deframer, DeframerStats, frame_burst};

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

#[test]
fn steady_state_push_partial_and_reset_do_not_allocate() {
    let payload = vec![0x5a; 61_423];
    let mut stream = vec![0, 0x72, 0x72, 0xf8, 0x11];
    frame_burst(DataType::Mat, &payload, 61_440, &mut stream);
    frame_burst(DataType::Pause, &[], 16, &mut stream);
    frame_burst(DataType::Ac3, &[1, 2, 3], 6144, &mut stream);
    let mut deframer = Deframer::new();
    let mut count = 0;
    let mut payload_bytes = 0;
    let mut partial_seen = false;

    // Construction, fixture creation and destruction are outside the guard.
    // Every parse path, including the first push and post-reset reuse, is in it.
    assert_no_alloc(|| {
        for chunk_size in [1, 3, 4096, stream.len()] {
            for chunk in stream.chunks(chunk_size) {
                deframer.push(chunk, &mut |burst| {
                    count += 1;
                    payload_bytes += burst.payload.len();
                    if burst.data_type == DataType::Mat {
                        assert_eq!(burst.payload, payload);
                        assert_eq!(burst.start, 5);
                    }
                });
                if let Some((DataType::Mat, bytes)) = deframer.partial() {
                    assert_eq!(bytes, &payload[..bytes.len()]);
                    partial_seen |= !bytes.is_empty();
                }
            }
            assert_eq!(deframer.stats().bursts, 3);
            deframer.reset();
            assert_eq!(deframer.stats(), DeframerStats::default());
        }
    });

    assert_eq!(count, 12);
    assert_eq!(payload_bytes, 4 * (payload.len() + 3));
    assert!(partial_seen);
}
