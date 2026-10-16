#![forbid(unsafe_code)]

//! The control thread against a fake host and a quiet engine (ADR 0001): the
//! first snapshot, intents and their snapshots, the 30 Hz meters, refusals,
//! and both ways the thread ends.

mod common;

use std::time::Duration;

use common::{
    DEV_A, DEV_A_NAME, FakeHost, FakeLauncher, MemoryStore, PanickingLauncher, Recorder,
    ReprepareLauncher, id, wait_until,
};
use spatial_softreceiver::control::{self, Bridge, ControlConfig};
use ssr_types::app::{Intent, NextAction, OutputChoice, Phase, SetupStep};

/// Long enough for any one step on a loaded runner.
const PATIENCE: Duration = Duration::from_secs(5);

fn start(
    host: &FakeHost,
    store: &MemoryStore,
    recorder: &Recorder,
) -> (Bridge, std::thread::JoinHandle<()>) {
    control::spawn(
        host.clone(),
        FakeLauncher,
        common::config(),
        ControlConfig::default(),
        store.clone(),
        recorder.clone(),
        Vec::new(),
    )
}

#[test]
fn language_changes_repaint_marker_and_preserve_note_lifetime() {
    use ssr_guard::marker::{self, Marker, MarkerReason};
    use ssr_types::app::Language::{En, Ko};
    let (host, store, recorder) = (FakeHost::new(), MemoryStore::default(), Recorder::default());
    let marker = Marker::new(
        MarkerReason::AppExited,
        1,
        vec!["diagnostic detail".to_owned()],
    );
    let (bridge, thread) = control::spawn_with_marker(
        host,
        FakeLauncher,
        supervisor::machine::Config {
            default_language: En,
            ..common::config()
        },
        ControlConfig::default(),
        store.clone(),
        recorder.clone(),
        (Vec::new(), Some(marker.clone())),
    );
    let first = bridge.snapshot();
    assert_eq!(first.language, En);
    assert!(
        first
            .notes
            .contains(&marker::sentence(&marker, En).to_owned())
    );
    bridge
        .dispatch(Intent::SetLanguage(Some(Ko)))
        .expect("Korean selected");
    let korean = bridge.snapshot();
    assert_eq!(korean.language, Ko);
    assert!(
        korean
            .notes
            .contains(&marker::sentence(&marker, Ko).to_owned())
    );
    assert!(
        !korean
            .notes
            .contains(&marker::sentence(&marker, En).to_owned())
    );
    assert!(korean.notes.contains(&"diagnostic detail".to_owned()));
    assert!(recorder.wait_for_snapshots(PATIENCE, |snapshots| {
        snapshots.last().is_some_and(|s| s.language == Ko)
    }));
    assert!(
        recorder
            .snapshots()
            .iter()
            .flat_map(|s| &s.notes)
            .all(|note| !note.contains('\0'))
    );
    assert_eq!(store.settings().language, Some(Ko));
    bridge
        .dispatch(Intent::SetLanguage(None))
        .expect("system language selected");
    assert_eq!(bridge.snapshot().language, En);
    assert_eq!(store.settings().language, None);
    bridge.dispatch(Intent::BeginSetup).expect("wizard begins");
    assert!(
        !bridge
            .snapshot()
            .notes
            .contains(&marker::sentence(&marker, En).to_owned())
    );
    bridge.dispatch(Intent::Quit).expect("quit succeeds");
    thread.join().expect("control ends");
    assert_eq!(bridge.snapshot().language, En);
    assert_eq!(
        bridge
            .dispatch(Intent::Enable)
            .expect_err("bridge is closed")
            .message,
        "The app is shutting down."
    );
}

#[test]
fn startup_restore_notices_follow_language_without_losing_external_details() {
    use control::Note;
    use ssr_types::Role;
    use ssr_types::app::Language::{En, Ko};
    use supervisor::text::Message;

    let (host, store, recorder) = (FakeHost::new(), MemoryStore::default(), Recorder::default());
    let detail = "C:\\사용자\\restore.json: OS error 5 / 사용자 detail";
    // Even token-shaped external text is opaque, not a second placeholder.
    let external = "\0ssr-shell-startup-notes";
    let diagnostic = Message::RestoreRoleFailed {
        role: Role::Console,
        detail: detail.to_owned(),
    };
    let (bridge, thread) = control::spawn_with_startup(
        host.clone(),
        FakeLauncher,
        common::config(),
        ControlConfig::default(),
        store,
        recorder.clone(),
        (
            vec![
                Note::RestoreReadFailed {
                    detail: detail.to_owned(),
                },
                Note::RestoreFailed,
                Note::Supervisor(diagnostic.clone()),
                Note::External(external.to_owned()),
            ],
            None,
        ),
    );
    let initial = bridge.snapshot();
    assert_eq!(initial.language, Ko);
    let korean_notes = vec![
        format!("복원점을 읽지 못했습니다: {detail}"),
        "지난 실행의 기본 장치를 되돌리지 못했습니다.".to_owned(),
        diagnostic.render(Ko),
        external.to_owned(),
    ];
    assert_eq!(initial.notes, korean_notes);
    let calls = host.world().calls.clone();

    bridge.dispatch(Intent::SetLanguage(Some(En))).unwrap();
    let english_notes = vec![
        format!("Could not read the restore point: {detail}"),
        "Could not restore the default audio devices from the previous run.".to_owned(),
        diagnostic.render(En),
        external.to_owned(),
    ];
    assert_eq!(bridge.snapshot().notes, english_notes);
    assert!(recorder.wait_for_snapshots(PATIENCE, |snapshots| {
        snapshots.last().is_some_and(|s| s.notes == english_notes)
    }));
    bridge.dispatch(Intent::SetLanguage(Some(Ko))).unwrap();
    assert_eq!(bridge.snapshot().notes, korean_notes);
    assert_eq!(
        host.world().calls,
        calls,
        "translation must not restore again"
    );

    bridge.dispatch(Intent::BeginSetup).unwrap();
    assert!(bridge.snapshot().notes.is_empty());
    bridge.dispatch(Intent::SetLanguage(Some(En))).unwrap();
    assert!(
        bridge.snapshot().notes.is_empty(),
        "cleared notes must stay cleared"
    );
    bridge.dispatch(Intent::Quit).unwrap();
    thread.join().unwrap();
    assert_eq!(
        bridge.snapshot().profile,
        supervisor::machine::default_profile_view_for(En)
    );
}

#[test]
fn the_first_snapshot_is_emitted_before_any_intent() {
    let (host, store, recorder) = (FakeHost::new(), MemoryStore::default(), Recorder::default());
    let (_bridge, _thread) = start(&host, &store, &recorder);

    assert!(
        recorder.wait_for_snapshots(Duration::from_millis(100), |snapshots| !snapshots
            .is_empty()),
        "no snapshot within 100 ms"
    );
    let snapshots = recorder.snapshots();
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].phase, Phase::Setup(SetupStep::Welcome));
}

#[test]
fn intents_walk_the_wizard_and_persist_the_output() {
    let (host, store, recorder) = (FakeHost::new(), MemoryStore::default(), Recorder::default());
    let (bridge, _thread) = start(&host, &store, &recorder);
    assert!(recorder.wait_for_snapshots(PATIENCE, |snapshots| snapshots.len() == 1));

    bridge
        .dispatch(Intent::BeginSetup)
        .expect("the wizard starts");
    assert!(recorder.wait_for_snapshots(PATIENCE, |snapshots| snapshots.len() == 2));
    assert_eq!(
        recorder.snapshots()[1].phase,
        Phase::Setup(SetupStep::ChooseOutput)
    );

    bridge
        .dispatch(Intent::ChooseOutput(id(DEV_A)))
        .expect("the output is chosen");
    assert!(recorder.wait_for_snapshots(PATIENCE, |snapshots| snapshots.len() == 3));
    let ready = &recorder.snapshots()[2];
    assert_eq!(ready.phase, Phase::Ready);
    let chosen = OutputChoice {
        id: id(DEV_A),
        name: DEV_A_NAME.to_owned(),
    };
    assert_eq!(ready.chosen_output, Some(chosen.clone()));
    assert_eq!(store.settings().output, Some(chosen));
    assert_eq!(bridge.snapshot().phase, Phase::Ready);
}

#[test]
fn running_emits_meters_at_the_tick_rate() {
    let host = FakeHost::new();
    let store = MemoryStore::with_output(DEV_A, DEV_A_NAME);
    let recorder = Recorder::default();
    let (bridge, _thread) = start(&host, &store, &recorder);

    bridge.dispatch(Intent::Enable).expect("the engine starts");
    assert_eq!(bridge.snapshot().phase, Phase::Running);
    let before = recorder.meters();
    std::thread::sleep(Duration::from_millis(300));
    let meters = recorder.meters() - before;
    assert!(meters >= 5, "{meters} meters in 300 ms");

    bridge.dispatch(Intent::Disable).expect("the engine stops");
    assert!(recorder.wait_for_snapshots(PATIENCE, |snapshots| {
        snapshots
            .last()
            .is_some_and(|last| last.phase == Phase::Ready)
    }));
    let after_disable = recorder.meters();
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(recorder.meters(), after_disable, "meters after Disable");
}

#[test]
fn a_guarded_control_thread_writes_a_heartbeat_each_refresh() {
    let host = FakeHost::new();
    let store = MemoryStore::with_output(DEV_A, DEV_A_NAME);
    let recorder = Recorder::default();
    let (bridge, _thread) = control::spawn(
        host,
        FakeLauncher,
        common::config(),
        ControlConfig {
            tick: Duration::from_millis(5),
            refresh: Duration::from_millis(25),
        },
        store,
        recorder.clone(),
        Vec::new(),
    );
    assert!(wait_until(PATIENCE, || !recorder.heartbeats().is_empty()));
    bridge.dispatch(Intent::Enable).expect("the engine starts");
    assert!(wait_until(PATIENCE, || {
        recorder
            .heartbeats()
            .iter()
            .filter(|heartbeat| heartbeat.running)
            .count()
            >= 3
    }));
    let heartbeats = recorder.heartbeats();
    // The thread's first line, before Enable, says not Running; from the
    // Enable on, every line says Running.
    assert!(!heartbeats[0].running);
    assert!(
        heartbeats
            .iter()
            .skip_while(|heartbeat| !heartbeat.running)
            .all(|heartbeat| heartbeat.running)
    );

    // Disable announces itself with a not-Running line before the engine
    // stops (ADR 0033), so the guard never takes the stop for a stall.
    let before = recorder.heartbeats().len();
    bridge.dispatch(Intent::Disable).expect("the engine stops");
    let heartbeats = recorder.heartbeats();
    assert!(heartbeats.len() > before);
    assert!(!heartbeats[before].running);
}

#[test]
fn a_running_profile_import_is_bracketed_by_heartbeats_for_the_guard() {
    let host = FakeHost::new();
    let store = MemoryStore::with_output(DEV_A, DEV_A_NAME);
    let recorder = Recorder::default();
    let (bridge, _thread) = control::spawn(
        host,
        FakeLauncher,
        common::config(),
        ControlConfig {
            tick: Duration::from_millis(5),
            // Long enough that no refresh line lands inside the intent.
            refresh: Duration::from_secs(60),
        },
        store,
        recorder.clone(),
        Vec::new(),
    );
    bridge.dispatch(Intent::Enable).expect("the engine starts");
    assert_eq!(bridge.snapshot().phase, Phase::Running);

    // A profile change may prepare for longer than the guard's stall limit: it
    // is announced as not Running, and Running is said again right after,
    // whether it succeeds or (as with the invalid BRIR path) fails.
    for (intent, succeeds) in [
        (Intent::ImportBrir("not-a-set.wav".into()), false),
        (
            Intent::SetSpeakerLayout(ssr_types::app::SpeakerLayout::Surround51),
            true,
        ),
    ] {
        let before = recorder.heartbeats().len();
        assert_eq!(bridge.dispatch(intent).is_ok(), succeeds);
        let heartbeats = recorder.heartbeats();
        assert_eq!(heartbeats.len(), before + 2, "{heartbeats:?}");
        assert!(!heartbeats[before].running);
        assert!(heartbeats[before + 1].running);
    }

    // Outside Running there is no engine to prepare and no extra line.
    bridge.dispatch(Intent::Disable).expect("the engine stops");
    let before = recorder.heartbeats().len();
    assert!(
        bridge
            .dispatch(Intent::ImportBrir("not-a-set.wav".into()))
            .is_err()
    );
    assert_eq!(recorder.heartbeats().len(), before);
}

#[test]
fn a_reprepare_after_needs_prepared_is_bracketed_by_heartbeats_for_the_guard() {
    let launcher = ReprepareLauncher::default();
    let recorder = Recorder::default();
    let (bridge, _thread) = control::spawn(
        FakeHost::new(),
        launcher.clone(),
        common::config(),
        ControlConfig {
            tick: Duration::from_millis(5),
            // Long enough that no refresh line lands among these.
            refresh: Duration::from_secs(60),
        },
        MemoryStore::with_output(DEV_A, DEV_A_NAME),
        recorder.clone(),
        Vec::new(),
    );
    // Answered after the thread's first heartbeat.
    assert_eq!(bridge.snapshot().phase, Phase::Ready);
    let before = recorder.heartbeats().len();
    bridge.dispatch(Intent::Enable).expect("the engine starts");

    // The engine's first poll asks for its output again: the tick prepares
    // the running profile (the default stereo pair) between a not-Running
    // line and a Running one, so a long prepare is not taken for a stall.
    assert!(wait_until(Duration::from_secs(5), || {
        recorder.heartbeats().len() >= before + 2
    }));
    let heartbeats = recorder.heartbeats();
    assert_eq!(heartbeats.len(), before + 2, "{heartbeats:?}");
    assert!(!heartbeats[before].running);
    assert!(heartbeats[before + 1].running);
    assert_eq!(launcher.prepared(), [2]);
    assert_eq!(bridge.snapshot().phase, Phase::Running);
}

#[test]
fn reprepare_apply_retries_do_not_emit_idle_heartbeats() {
    let launcher = ReprepareLauncher::rejecting(3);
    let recorder = Recorder::default();
    let (bridge, _thread) = control::spawn(
        FakeHost::new(),
        launcher.clone(),
        common::config(),
        ControlConfig {
            tick: Duration::from_millis(20),
            refresh: Duration::from_secs(60),
        },
        MemoryStore::with_output(DEV_A, DEV_A_NAME),
        recorder.clone(),
        Vec::new(),
    );
    assert_eq!(bridge.snapshot().phase, Phase::Ready);
    let before = recorder.heartbeats().len();
    bridge.dispatch(Intent::Enable).expect("the engine starts");
    assert!(wait_until(PATIENCE, || launcher.attempts() >= 4));

    let heartbeats = recorder.heartbeats();
    assert_eq!(heartbeats.len(), before + 2, "{heartbeats:?}");
    assert!(!heartbeats[before].running);
    assert!(heartbeats[before + 1].running);
    assert_eq!(launcher.prepared(), [2]);
}

#[test]
fn a_refused_intent_returns_its_error_and_keeps_the_phase() {
    let host = FakeHost::new();
    let store = MemoryStore::with_output(DEV_A, DEV_A_NAME);
    let recorder = Recorder::default();
    let (bridge, _thread) = start(&host, &store, &recorder);
    assert_eq!(bridge.snapshot().phase, Phase::Ready);

    let error = bridge
        .dispatch(Intent::InstallDriver)
        .expect_err("installing is not possible when Ready");
    assert!(!error.message.is_empty());
    assert_eq!(bridge.snapshot().phase, Phase::Ready);
    assert!(
        !host
            .world()
            .calls
            .iter()
            .any(|call| call.starts_with("setup")),
        "the refused intent ran ssr-setup"
    );
}

#[test]
fn quit_restores_the_roles_and_ends_the_thread() {
    let host = FakeHost::new();
    let store = MemoryStore::with_output(DEV_A, DEV_A_NAME);
    let recorder = Recorder::default();
    let (bridge, thread) = start(&host, &store, &recorder);

    bridge.dispatch(Intent::Enable).expect("the engine starts");
    assert!(bridge.snapshot().roles_taken);
    host.world().calls.clear();

    bridge.dispatch(Intent::Quit).expect("the supervisor quits");
    thread
        .join()
        .expect("the control thread ends without a panic");

    assert_eq!(recorder.quits(), 1);
    assert_eq!(
        recorder.snapshots().last().map(|last| last.phase.clone()),
        Some(Phase::Quit)
    );
    let calls = host.world().calls.clone();
    for role in ["Console", "Multimedia", "Communications"] {
        assert!(
            calls.contains(&format!("set {role} {DEV_A}")),
            "{role} was not given back: {calls:?}"
        );
    }
    assert!(
        calls.contains(&"remove restore point".to_owned()),
        "{calls:?}"
    );
    assert_eq!(host.world().restore_point, None);
}

#[test]
fn a_dropped_bridge_quits_the_supervisor() {
    let host = FakeHost::new();
    let store = MemoryStore::with_output(DEV_A, DEV_A_NAME);
    let recorder = Recorder::default();
    let (bridge, thread) = start(&host, &store, &recorder);
    bridge.dispatch(Intent::Enable).expect("the engine starts");
    host.world().calls.clear();

    let copy = bridge.clone();
    drop(bridge);
    assert!(
        !wait_until(Duration::from_millis(100), || thread.is_finished()),
        "the thread ended while a clone of the bridge was alive"
    );
    drop(copy);
    assert!(wait_until(PATIENCE, || thread.is_finished()));
    thread
        .join()
        .expect("the control thread ends without a panic");

    assert_eq!(recorder.quits(), 1);
    let calls = host.world().calls.clone();
    assert!(calls.contains(&format!("set Console {DEV_A}")), "{calls:?}");
    assert!(
        calls.contains(&"remove restore point".to_owned()),
        "{calls:?}"
    );
}

#[test]
fn dispatch_after_the_thread_ended_is_an_error() {
    let (host, store, recorder) = (FakeHost::new(), MemoryStore::default(), Recorder::default());
    let (bridge, thread) = start(&host, &store, &recorder);
    bridge.dispatch(Intent::Quit).expect("the supervisor quits");
    thread
        .join()
        .expect("the control thread ends without a panic");

    let error = bridge
        .dispatch(Intent::BeginSetup)
        .expect_err("nobody is left to handle it");
    assert_eq!(error.message, "앱이 종료되는 중입니다.");
    assert_eq!(error.next, NextAction::None);
    assert_eq!(bridge.snapshot().phase, Phase::Quit);
}

#[test]
fn a_panicking_launcher_still_quits_the_supervisor() {
    let host = FakeHost::new();
    let store = MemoryStore::with_output(DEV_A, DEV_A_NAME);
    let recorder = Recorder::default();
    let (bridge, thread) = control::spawn(
        host.clone(),
        PanickingLauncher,
        common::config(),
        ControlConfig::default(),
        store,
        recorder.clone(),
        Vec::new(),
    );
    assert_eq!(bridge.snapshot().phase, Phase::Ready);

    // The panic drops the reply: the caller hears that the app is quitting.
    let error = bridge
        .dispatch(Intent::Enable)
        .expect_err("the launcher panicked");
    assert_eq!(error.message, "앱이 종료되는 중입니다.");
    assert!(wait_until(PATIENCE, || thread.is_finished()));
    thread
        .join()
        .expect("the panic is caught on the control thread");

    assert_eq!(recorder.quits(), 1);
    let calls = host.world().calls.clone();
    for role in ["Console", "Multimedia", "Communications"] {
        assert!(
            calls.contains(&format!("set {role} ours-0")),
            "{role} was not taken: {calls:?}"
        );
        assert!(
            calls.contains(&format!("set {role} {DEV_A}")),
            "{role} was not given back: {calls:?}"
        );
    }
    assert!(
        calls.contains(&"remove restore point".to_owned()),
        "{calls:?}"
    );
    assert_eq!(host.world().restore_point, None);
    assert_eq!(bridge.snapshot().phase, Phase::Quit);
}
