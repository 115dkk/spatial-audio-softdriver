#![forbid(unsafe_code)]
#![cfg(windows)]

use plughost::{ControlEvent, PluginFormat, PluginProcess, Started};
use plughost_proto::{Action, HostWatch, Message, Received, StateHistory};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use streamring::{Publish, SlotReader, SlotWriter, Wait};

/// Publishes `sequence` through the host's ring as the engine does: the
/// publisher on the region the host owns, waking it with its work event.
fn try_publish(
    started: &Started,
    sequence: u32,
    write: impl FnOnce(&mut SlotWriter<'_>),
) -> Publish {
    started.publisher.try_publish(
        started.region.region(),
        0,
        sequence,
        Some(&*started.work),
        write,
    )
}

fn completed(started: &Started) -> u32 {
    started.publisher.completed(started.region.region(), 0)
}

fn try_read(started: &Started, sequence: u32, read: impl FnOnce(SlotReader<'_>)) -> bool {
    started
        .publisher
        .try_read(started.region.region(), 0, sequence, read)
}

macro_rules! format_tests {
    ($module:ident, $format:expr, $environment:literal) => {
        mod $module {
            use super::*;
            fn state(mode: u8) -> Vec<u8> {
                if $format == PluginFormat::Vst3 {
                    plughost::state::encode(&[1, mode], &[1, mode]).unwrap()
                } else {
                    vec![1, mode]
                }
            }
            fn paths() -> (PathBuf, PathBuf) {
                (
                    std::env::var_os("SSR_PLUGHOST_EXE")
                        .expect("set SSR_PLUGHOST_EXE to ssr-plughost.exe")
                        .into(),
                    std::env::var_os($environment)
                        .expect(concat!("set ", $environment, " to the test plugin"))
                        .into(),
                )
            }
            fn start(mode: u8) -> Started {
                let (exe, plugin) = paths();
                PluginProcess::start(&exe, &plugin, $format, None, 48000, Some(&state(mode)))
                    .expect("start test plugin")
            }
            fn publish(started: &Started, sequence: u32) {
                assert_eq!(
                    try_publish(started, sequence, |slot| {
                        for frame in 0..slot.frames() {
                            slot.write(0, frame, (frame as f32 * 0.17).sin() * 0.25);
                            slot.write(1, frame, (frame as f32 * 0.23).cos() * 0.25);
                        }
                    }),
                    Publish::Published
                );
            }
            fn complete(started: &Started, sequence: u32) {
                let deadline = Instant::now() + Duration::from_secs(5);
                while completed(started) < sequence {
                    assert!(
                        Instant::now() < deadline,
                        "host failed to complete sequence {sequence}"
                    );
                    started.done.wait(Duration::from_millis(10));
                }
            }
            fn response(started: &mut Started, request: u32) -> Message {
                let deadline = Instant::now() + Duration::from_secs(5);
                loop {
                    assert!(Instant::now() < deadline, "control response timed out");
                    for event in started.control.poll(started.control.elapsed_ms()) {
                        if let ControlEvent::Message(frame) = event
                            && frame.request == request
                        {
                            return frame.message;
                        }
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
            }

            #[test]
            #[ignore = "needs ssr-plughost and the test plugin; run by the plughost CI job"]
            fn start_and_process_matrix_with_latency() {
                let started = start(0);
                assert_eq!(started.latency_frames, 64);
                assert_eq!(started.output_wave_bits, [1, 2, 4, 8, 16, 32]);
                for sequence in 1..=3 {
                    publish(&started, sequence);
                    complete(&started, sequence);
                    assert!(try_read(&started, sequence, |slot| {
                        for frame in 0..slot.frames() {
                            let previous = if frame >= 64 {
                                Some(frame - 64)
                            } else if sequence > 1 {
                                Some(slot.frames() + frame - 64)
                            } else {
                                None
                            };
                            let (left, right) = previous.map_or((0.0, 0.0), |index| {
                                (
                                    (index as f32 * 0.17).sin() * 0.25,
                                    (index as f32 * 0.23).cos() * 0.25,
                                )
                            });
                            let expected = [
                                left,
                                right,
                                0.5 * (left + right),
                                0.5 * (left + right),
                                0.5 * left,
                                0.5 * right,
                            ];
                            for (channel, expected) in expected.into_iter().enumerate() {
                                assert!((slot.read(channel, frame) - expected).abs() < 1e-6);
                            }
                        }
                    }));
                }
            }
            #[test]
            #[ignore = "needs ssr-plughost and the test plugin; run by the plughost CI job"]
            fn state_round_trip() {
                let mut started = start(0);
                let set = started.control.set_state(&state(3)).unwrap();
                assert_eq!(response(&mut started, set), Message::Ack);
                let get = started.control.get_state().unwrap();
                assert_eq!(
                    response(&mut started, get),
                    Message::State { blob: state(3) }
                );
            }
            #[test]
            #[ignore = "needs ssr-plughost and the test plugin; run by the plughost CI job"]
            fn editor_round_trip() {
                let mut started = start(0);
                let request = started.control.open_editor().unwrap();
                assert_eq!(response(&mut started, request), Message::Ack);
                if $format == PluginFormat::Vst3 {
                    // The fixture's attach gesture edits Width to 0.5. Only the
                    // host parameter queue can deliver that edit to its processor.
                    publish(&started, 1);
                    complete(&started, 1);
                    assert!(try_read(&started, 1, |slot| {
                        let left = (36.0_f32 * 0.17).sin() * 0.25;
                        assert!((slot.read(4, 100) - 0.25 * left).abs() < 1e-6);
                    }));
                }
                started.control.close_editor_for_test().unwrap();
                let deadline = Instant::now() + Duration::from_secs(5);
                loop {
                    assert!(Instant::now() < deadline, "EditorClosed not received");
                    if started
                        .control
                        .poll(started.control.elapsed_ms())
                        .into_iter()
                        .any(|event| {
                            matches!(
                                event,
                                ControlEvent::Message(plughost_proto::Frame {
                                    message: Message::EditorClosed,
                                    ..
                                })
                            )
                        })
                    {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            #[test]
            #[ignore = "needs ssr-plughost and the test plugin; run by the plughost CI job"]
            fn crash_respawn_and_disable() {
                let mut watch = HostWatch::new(0);
                for (attempt, time) in [0, 501, 2502].into_iter().enumerate() {
                    let mut started = start(1);
                    watch.received(time, Received::Hello);
                    watch.received(time, Received::Loaded);
                    watch.received(time, Received::Configured);
                    publish(&started, 1);
                    let deadline = Instant::now() + Duration::from_secs(5);
                    let code = loop {
                        assert!(Instant::now() < deadline, "crash process did not exit");
                        if let Some(code) = started.control.exit_code().unwrap() {
                            break code;
                        }
                        std::thread::sleep(Duration::from_millis(5));
                    };
                    assert_ne!(code, 0);
                    assert!(
                        started
                            .control
                            .poll(started.control.elapsed_ms())
                            .iter()
                            .any(|event| matches!(event, ControlEvent::Exited(_)))
                    );
                    let action = watch.process_exited(time + 1, code as i32);
                    if attempt == 2 {
                        assert!(matches!(action, Action::Disable { .. }));
                    } else {
                        assert_eq!(action, Action::EnterFallback);
                        assert_eq!(
                            watch.tick(if attempt == 0 { 501 } else { 2502 }),
                            Action::Spawn
                        );
                    }
                }
            }
            #[test]
            #[ignore = "needs ssr-plughost and the test plugin; run by the plughost CI job"]
            fn hung_process_is_killed() {
                let mut started = start(2);
                publish(&started, 1);
                let deadline = Instant::now() + Duration::from_secs(5);
                loop {
                    assert!(Instant::now() < deadline, "hang was not killed");
                    if started
                        .control
                        .poll(started.control.elapsed_ms())
                        .into_iter()
                        .any(|event| matches!(event, ControlEvent::Action(Action::Kill { .. })))
                    {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            #[test]
            #[ignore = "needs ssr-plughost and the test plugin; run by the plughost CI job"]
            fn nan_is_preserved_for_app_sanitization() {
                let started = start(3);
                publish(&started, 1);
                complete(&started, 1);
                assert!(try_read(&started, 1, |slot| {
                    let sample = slot.read(2, 100);
                    assert!(sample.is_nan());
                    let sanitized = if sample.is_finite() { sample } else { 0.0 };
                    assert_eq!(sanitized, 0.0);
                }));
                assert!(started.region.region().last_error() > 0);
            }
            #[test]
            #[ignore = "needs ssr-plughost and the test plugin; run by the plughost CI job"]
            fn failed_restore_uses_previous_state() {
                let (exe, plugin) = paths();
                let mut history = StateHistory::new();
                history.save(state(0));
                history.save(state(4));
                assert!(
                    PluginProcess::start(
                        &exe,
                        &plugin,
                        $format,
                        None,
                        48000,
                        history.begin_restore()
                    )
                    .is_err()
                );
                let mut previous = PluginProcess::start(
                    &exe,
                    &plugin,
                    $format,
                    None,
                    48000,
                    history.crash_after_restore(),
                )
                .unwrap();
                let request = previous.control.get_state().unwrap();
                assert_eq!(
                    response(&mut previous, request),
                    Message::State { blob: state(0) }
                );
            }
            #[test]
            #[ignore = "needs ssr-plughost and the test plugin; run by the plughost CI job"]
            fn scan_returns_name_and_vendor() {
                let (exe, plugin) = paths();
                let classes = PluginProcess::scan(&exe, &plugin, $format).unwrap();
                assert_eq!(classes.len(), 1);
                assert_eq!(classes[0].name, "Test matrix");
                assert_eq!(classes[0].vendor, "Test fixtures");
            }
            #[test]
            #[ignore = "needs ssr-plughost and the test plugin; run by the plughost CI job"]
            fn shutdown_exits_zero_without_respawn() {
                let mut started = start(0);
                started.control.shutdown().unwrap();
                let deadline = Instant::now() + Duration::from_secs(5);
                loop {
                    assert!(Instant::now() < deadline, "shutdown timed out");
                    let events = started.control.poll(started.control.elapsed_ms());
                    assert!(
                        !events
                            .iter()
                            .any(|event| matches!(event, ControlEvent::NeedsRespawn))
                    );
                    if let Some(code) = started.control.exit_code().unwrap() {
                        assert_eq!(code, 0);
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }
    };
}
format_tests!(vst2, PluginFormat::Vst2, "SSR_TEST_PLUGIN_VST2");
format_tests!(vst3, PluginFormat::Vst3, "SSR_TEST_PLUGIN_VST3");

fn vst3_paths() -> (PathBuf, PathBuf) {
    (
        std::env::var_os("SSR_PLUGHOST_EXE")
            .expect("set SSR_PLUGHOST_EXE")
            .into(),
        std::env::var_os("SSR_TEST_PLUGIN_VST3")
            .expect("set SSR_TEST_PLUGIN_VST3")
            .into(),
    )
}
#[test]
#[ignore = "needs ssr-plughost and the test plugin; run by the plughost CI job"]
fn scan_vst3_lists_one_audio_effect_class() {
    let (exe, plugin) = vst3_paths();
    let classes = PluginProcess::scan(&exe, &plugin, PluginFormat::Vst3).unwrap();
    assert_eq!(classes.len(), 1);
    assert_eq!(classes[0].category, "Fx|Spatial");
    assert_ne!(classes[0].id, [0; 16]);
    let started = PluginProcess::start(
        &exe,
        &plugin,
        PluginFormat::Vst3,
        Some(classes[0].id),
        48000,
        None,
    )
    .unwrap();
    assert_eq!(started.input_wave_bits, [1, 2]);
    assert!(
        PluginProcess::start(
            &exe,
            &plugin,
            PluginFormat::Vst3,
            Some([0; 16]),
            48000,
            None
        )
        .is_err()
    );
}
#[test]
#[ignore = "needs ssr-plughost and the test plugin; run by the plughost CI job"]
fn vst3_bus_arrangements_end_at_stereo_in_surround_out() {
    let (exe, plugin) = vst3_paths();
    let started =
        PluginProcess::start(&exe, &plugin, PluginFormat::Vst3, None, 48000, None).unwrap();
    assert_eq!(started.geometry.input_channels, 2);
    assert_eq!(started.geometry.output_channels, 6);
    assert_eq!(started.input_wave_bits, [1, 2]);
    assert_eq!(started.output_wave_bits, [1, 2, 4, 8, 16, 32]);
}

/// A third-party VST 3 upmixer the CI job decrypts from the repository
/// (`tools/ci/private/`, ADR 0035). It is not open source: the job never
/// uploads or logs the file, and without the secret the variable is unset and
/// this test passes without doing anything.
#[test]
#[ignore = "needs ssr-plughost and the decrypted third-party plugin; run by the plughost CI job"]
fn third_party_vst3_scans_starts_and_processes() {
    let Some(plugin) = std::env::var_os("SSR_THIRD_PARTY_VST3") else {
        eprintln!("SSR_THIRD_PARTY_VST3 is not set; skipped");
        return;
    };
    let exe: PathBuf = std::env::var_os("SSR_PLUGHOST_EXE")
        .expect("set SSR_PLUGHOST_EXE")
        .into();
    let plugin = PathBuf::from(plugin);
    let classes = PluginProcess::scan(&exe, &plugin, PluginFormat::Vst3).expect("scan");
    assert!(!classes.is_empty(), "no audio effect class");
    eprintln!(
        "classes: {:?}",
        classes
            .iter()
            .map(|class| (&class.name, &class.vendor, &class.category))
            .collect::<Vec<_>>()
    );
    let started = PluginProcess::start(
        &exe,
        &plugin,
        PluginFormat::Vst3,
        Some(classes[0].id),
        48000,
        None,
    )
    .expect("start");
    eprintln!(
        "inputs {:?}, outputs {:?}, latency {} frames",
        started.input_wave_bits, started.output_wave_bits, started.latency_frames
    );
    assert!(!started.input_wave_bits.is_empty());
    assert!(
        started.output_wave_bits.len() > started.input_wave_bits.len(),
        "an upmixer has more outputs than inputs"
    );
    assert!(started.latency_frames <= 2 * 48000);
    let inputs = started.input_wave_bits.len();
    for sequence in 1..=20 {
        // A failed publish means the host is two blocks behind: wait for it.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let published = try_publish(&started, sequence, |slot| {
                for frame in 0..slot.frames() {
                    let t = (sequence as usize * slot.frames() + frame) as f32;
                    for channel in 0..inputs {
                        let phase = 0.05 + 0.03 * channel as f32;
                        slot.write(channel, frame, (t * phase).sin() * 0.25);
                    }
                }
            });
            if published == Publish::Published {
                break;
            }
            assert!(Instant::now() < deadline, "host stuck before {sequence}");
            started.done.wait(Duration::from_millis(10));
        }
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while completed(&started) < 20 {
        assert!(Instant::now() < deadline, "host did not finish 20 blocks");
        started.done.wait(Duration::from_millis(10));
    }
    let outputs = started.output_wave_bits.len();
    assert!(try_read(&started, 20, |slot| {
        for channel in 0..outputs {
            for frame in 0..slot.frames() {
                assert!(slot.read(channel, frame).is_finite());
            }
        }
    }));
}
