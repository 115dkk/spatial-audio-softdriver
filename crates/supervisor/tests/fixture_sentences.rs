#![forbid(unsafe_code)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use ssr_types::app::{Language, NextAction, Snapshot};
use ssr_types::{OutputError, SecureBoot};
use supervisor::machine::Config;
use supervisor::text::Message;

const NAMES: &[&str] = &[
    "setup-welcome",
    "setup-unsupported",
    "setup-not-test-signing",
    "setup-driver-not-installed",
    "setup-driver-problem",
    "setup-driver-declined",
    "setup-reboot-required",
    "setup-choose-output",
    "setup-output-missing",
    "setup-choose-output-empty",
    "ready",
    "ready-output-renamed",
    "running",
    "running-reconnecting",
    "faulted",
    "ready-headphones",
    "ready-profile-file",
    "ready-brir-unreadable",
    "ready-start-failed",
    "quit",
];

struct Expected {
    error: Option<(Message, NextAction)>,
    notes: Vec<Message>,
    default_profile: bool,
}

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apps/spatial-softreceiver/ui/fixtures")
}

fn expected(name: &str, output_name: &str) -> Expected {
    let none = || Expected {
        error: None,
        notes: Vec::new(),
        default_profile: true,
    };
    match name {
        "setup-welcome"
        | "setup-driver-not-installed"
        | "setup-choose-output"
        | "setup-choose-output-empty"
        | "ready"
        | "quit" => none(),
        "setup-unsupported" => Expected {
            error: Some((
                Message::OsRequirement {
                    min_build: Config::MIN_OS_BUILD,
                },
                NextAction::None,
            )),
            ..none()
        },
        "setup-not-test-signing" => Expected {
            error: Some((
                Message::TestSigningDisabled {
                    secure_boot: SecureBoot::Enabled,
                },
                NextAction::Reboot,
            )),
            ..none()
        },
        "setup-driver-problem" => Expected {
            error: Some((Message::DriverSignatureRejected, NextAction::Retry)),
            ..none()
        },
        "setup-driver-declined" => Expected {
            error: Some((Message::AdminApprovalCancelled, NextAction::Retry)),
            ..none()
        },
        "setup-reboot-required" => Expected {
            error: Some((Message::RebootRequired, NextAction::Reboot)),
            ..none()
        },
        "setup-output-missing" => Expected {
            error: Some((
                Message::StoredOutputMissing {
                    name: output_name.to_owned(),
                },
                NextAction::OpenSettings,
            )),
            ..none()
        },
        "ready-output-renamed" => Expected {
            notes: vec![Message::OutputIdChanged {
                name: output_name.to_owned(),
            }],
            ..none()
        },
        "running" => Expected {
            error: None,
            notes: vec![Message::Reconnected],
            default_profile: false,
        },
        "running-reconnecting" => Expected {
            error: Some((
                Message::ReconnectingOutput {
                    detail: OutputError::Removed.to_string(),
                },
                NextAction::None,
            )),
            notes: Vec::new(),
            default_profile: false,
        },
        "faulted" => Expected {
            error: Some((
                Message::ReconnectTimedOutOutput {
                    seconds: Config::FAULT_GRACE.as_secs(),
                    detail: OutputError::Removed.to_string(),
                },
                NextAction::Retry,
            )),
            ..none()
        },
        "ready-headphones" | "ready-profile-file" => Expected {
            default_profile: false,
            ..none()
        },
        "ready-brir-unreadable" => Expected {
            error: Some((
                Message::BrirReadFailed {
                    detail: r"C:\Users\me\BRIR\studio\hesuvi.wav has 2 tracks, which matches no HeSuVi layout"
                        .to_owned(),
                },
                NextAction::None,
            )),
            ..none()
        },
        "ready-start-failed" => Expected {
            error: Some((
                // What enable() puts there: the launcher's SessionError.
                Message::EngineStartFailed {
                    detail: supervisor::SessionError::Start(engine::StartError::Output(
                        OutputError::Busy,
                    ))
                    .to_string(),
                },
                NextAction::Retry,
            )),
            ..none()
        },
        _ => panic!("unknown fixture in index: {name}"),
    }
}

#[test]
fn every_fixture_sentence_is_the_supervisors() {
    let root = fixtures_dir();
    let names: Vec<String> = serde_json::from_str(
        &std::fs::read_to_string(root.join("index.json")).expect("read fixture index"),
    )
    .expect("parse fixture index");
    let unique: BTreeSet<_> = names.iter().map(String::as_str).collect();
    assert_eq!(
        unique.len(),
        names.len(),
        "fixture index contains a duplicate"
    );
    assert_eq!(unique, NAMES.iter().copied().collect());

    for (language, directory) in [
        (Language::Ko, root.clone()),
        (Language::En, root.join("en")),
    ] {
        for name in &names {
            let snapshot: Snapshot = serde_json::from_str(
                &std::fs::read_to_string(directory.join(format!("{name}.json")))
                    .unwrap_or_else(|error| panic!("read {name} fixture: {error}")),
            )
            .unwrap_or_else(|error| panic!("parse {name} fixture: {error}"));
            assert_eq!(snapshot.language, language, "{name}");
            assert_eq!(snapshot.language_choice, None, "{name}");

            let output_name = snapshot
                .chosen_output
                .as_ref()
                .map(|output| output.name.as_str())
                .or_else(|| snapshot.outputs.get(1).map(|output| output.name.as_str()))
                .unwrap_or("DENON AVR (NVIDIA High Definition Audio)");
            let expected = expected(name, output_name);
            match (snapshot.error, expected.error) {
                (None, None) => {}
                (Some(actual), Some((message, next))) => {
                    assert_eq!(actual.message, message.render(language), "{name}");
                    assert_eq!(actual.next, next, "{name}");
                }
                (actual, expected) => panic!(
                    "{name} error presence differs: actual={}, expected={}",
                    actual.is_some(),
                    expected.is_some()
                ),
            }
            let expected_notes: Vec<_> = expected
                .notes
                .into_iter()
                .map(|message| message.render(language))
                .collect();
            assert_eq!(snapshot.notes, expected_notes, "{name}");
            if expected.default_profile {
                assert_eq!(
                    snapshot.profile.name,
                    Message::DefaultProfileName.render(language),
                    "{name}"
                );
            }
        }
    }
}
