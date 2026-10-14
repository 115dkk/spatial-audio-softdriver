#![forbid(unsafe_code)]

mod common;
use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use ssr_abi::RecordWriter;
use ssr_sys::sal_format::SalTranslator;

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
    use ssr_types::{OutputOpener, OutputSink};
    let mut sink = ssr_sys::output::WasapiOpener {
        endpoint: None,
        rate: 48_000,
        shared_fallback: true,
        exclusive: false,
        period: None,
    }
    .open(ssr_types::OutputLayout::STEREO)
    .unwrap();
    let count = (sink.format().rate as usize / 5).div_ceil(sink.format().period_frames);
    sink.next_period(&mut |samples| samples.fill(0.0)).unwrap();
    assert_no_alloc(|| {
        for _ in 0..count {
            sink.next_period(&mut |samples| samples.fill(0.0)).unwrap();
        }
    });
}

#[cfg(windows)]
#[test]
#[ignore = "requires a multichannel default render endpoint and plays 200 ms of silence"]
fn wasapi_multichannel_periods_do_not_allocate() {
    use ssr_types::{OutputLayout, OutputOpener, OutputSink};
    let layout = OutputLayout {
        channels: 6,
        channel_mask: 0x3f,
    };
    let mut sink = ssr_sys::output::WasapiOpener {
        endpoint: None,
        rate: 48_000,
        shared_fallback: true,
        exclusive: true,
        period: None,
    }
    .open(layout)
    .unwrap();
    let format = sink.format();
    assert_eq!((format.channels, format.channel_mask), (6, 0x3f));
    let count = (format.rate as usize / 5).div_ceil(format.period_frames);
    sink.next_period(&mut |samples| samples.fill(0.0)).unwrap();
    assert_no_alloc(|| {
        for _ in 0..count {
            sink.next_period(&mut |samples| samples.fill(0.0)).unwrap();
        }
    });
}

#[cfg(windows)]
#[test]
#[ignore = "requires a render endpoint; requests exclusive mode and plays 1 s of silence"]
fn wasapi_exclusive_periods_and_a_restart_do_not_allocate() {
    use ssr_types::{OutputOpener, OutputSink};
    let mut sink = ssr_sys::output::WasapiOpener {
        endpoint: None,
        rate: 48_000,
        shared_fallback: true,
        exclusive: true,
        period: None,
    }
    .open(ssr_types::OutputLayout::STEREO)
    .unwrap();
    let count = (sink.format().rate as usize / 2).div_ceil(sink.format().period_frames);
    sink.next_period(&mut |samples| samples.fill(0.0)).unwrap();
    assert_no_alloc(|| {
        for index in 0..count {
            if index == count / 4 {
                // Miss a deadline so the stall path and a restart run too.
                std::thread::sleep(std::time::Duration::from_millis(30));
            }
            sink.next_period(&mut |samples| samples.fill(0.0)).unwrap();
        }
    });
    println!("{} restarts", sink.restarts());
}
