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
#[ignore = "requires a default render endpoint with an exclusive multichannel format; plays 200 ms of silence"]
fn wasapi_multichannel_periods_do_not_allocate() {
    use ssr_types::{OutputLayout, OutputOpener, OutputSink};
    // The first exclusive layout wider than stereo that the default endpoint
    // offers: VB-CABLE on the runners takes 7.1 but not 5.1, a desktop DAC may
    // take only 5.1. None at all is a failure, not a skip, because the job
    // that runs this installs a multichannel cable.
    let default = ssr_sys::devices::render_endpoint_formats()
        .unwrap()
        .into_iter()
        .find(|formats| formats.endpoint.is_default)
        .expect("a default render endpoint");
    let layout = default
        .layouts
        .iter()
        .find(|layout| layout.exclusive && layout.channels > 2)
        .map(|layout| OutputLayout {
            channels: layout.channels,
            channel_mask: layout.channel_mask,
        })
        .unwrap_or_else(|| {
            panic!(
                "{} offers no exclusive multichannel layout",
                default.endpoint.name
            )
        });
    let mut sink = ssr_sys::output::WasapiOpener {
        endpoint: Some(default.endpoint.id.clone()),
        rate: 48_000,
        shared_fallback: false,
        exclusive: true,
        period: common::exclusive_period(),
    }
    .open(layout)
    .unwrap();
    let format = sink.format();
    assert_eq!(
        (format.channels, format.channel_mask),
        (layout.channels, layout.channel_mask)
    );
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
        period: common::exclusive_period(),
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
