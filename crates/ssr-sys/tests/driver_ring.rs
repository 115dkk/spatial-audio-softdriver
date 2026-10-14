#![forbid(unsafe_code)]
#![cfg(windows)]

use std::time::Duration;

use ssr_sys::driver_ring::DriverRing;
use ssr_types::CaptureSource;

#[test]
#[ignore = "requires the test-signed Spatial SoftReceiver driver installed"]
fn installed_driver_opens_reads_and_accepts_trim() {
    let mut ring = DriverRing::open()
        .expect("Spatial SoftReceiver control device must be installed for this test");
    assert_eq!(ring.info().endpoint_mask, 0x7);
    assert!(ring.info().qpc_frequency > 0);

    for ppm in [0.0, -100.0, 100.0] {
        ring.set_rate_trim(ppm)
            .unwrap_or_else(|_| panic!("driver rejected {ppm:+} ppm trim"));
    }

    let mut output = vec![0; ssr_abi::READ_MAX_BYTES as usize];
    let returned = ring
        .read(&mut output, Duration::from_millis(10))
        .expect("short driver READ");
    assert!(returned <= output.len());
}
