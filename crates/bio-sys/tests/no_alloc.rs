#![forbid(unsafe_code)]

mod common;
use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use bio_abi::RecordWriter;
use bio_sys::sal_format::SalTranslator;

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

#[test]
fn sal_translation_does_not_allocate_in_steady_state() {
    let mut translator = SalTranslator::new();
    let mut header = common::header();
    let input = common::read(header, &[0; 512]);
    let mut output = [0; 1024];
    translator
        .translate(&input, 1, &mut RecordWriter::new(&mut output))
        .unwrap();
    header.dropped_bytes = 20;
    let overrun = common::read(header, &[1; 512]);
    assert_no_alloc(|| {
        for qpc in 2..100 {
            translator
                .translate(&input, qpc, &mut RecordWriter::new(&mut output))
                .unwrap();
        }
        translator
            .translate(&overrun, 100, &mut RecordWriter::new(&mut output))
            .unwrap();
    });
}

#[cfg(windows)]
#[test]
#[ignore = "requires an active default render endpoint and plays 200 ms of silence"]
fn wasapi_shared_periods_do_not_allocate() {
    use bio_types::{OutputOpener, OutputSink};
    let mut sink = bio_sys::output::WasapiOpener {
        endpoint: None,
        rate: 48_000,
        exclusive: false,
        period: None,
    }
    .open()
    .unwrap();
    let count = (sink.format().rate as usize / 5).div_ceil(sink.format().period_frames);
    sink.next_period(&mut |samples| samples.fill(0.0)).unwrap();
    assert_no_alloc(|| {
        for _ in 0..count {
            sink.next_period(&mut |samples| samples.fill(0.0)).unwrap();
        }
    });
}
