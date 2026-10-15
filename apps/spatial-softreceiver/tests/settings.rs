#![forbid(unsafe_code)]

//! The settings file and the paths, on any OS.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use spatial_softreceiver::control::SettingsStore;
use spatial_softreceiver::paths::{self, Args};
use spatial_softreceiver::settings::{self, JsonSettingsFile};
use ssr_types::DeviceId;
use ssr_types::app::OutputChoice;
use supervisor::Settings;

/// A fresh directory under the temporary directory, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "spatial-softreceiver-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn chosen() -> Settings {
    Settings {
        output: Some(OutputChoice {
            id: DeviceId("{0.0.0.00000000}.{a1b2}".to_owned()),
            name: "스피커 (USB DAC)".to_owned(),
        }),
    }
}

#[test]
fn settings_round_trip() {
    let scratch = Scratch::new("round-trip");
    // The directory does not exist yet: the first save makes it.
    let file = JsonSettingsFile::in_dir(&scratch.path().join("state"));
    assert_eq!(file.load(), Settings::default());

    file.save(&chosen());
    assert_eq!(file.load(), chosen());
    let written: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&file.path).expect("the file was written"))
            .expect("the file is JSON");
    assert_eq!(written["version"], 1);
    assert_eq!(
        written["settings"]["output"]["id"],
        "{0.0.0.00000000}.{a1b2}"
    );
    assert_eq!(written["settings"]["output"]["name"], "스피커 (USB DAC)");

    file.save(&Settings::default());
    assert_eq!(file.load(), Settings::default());
    let leftovers: Vec<_> = fs::read_dir(file.path.parent().expect("a directory"))
        .expect("the directory lists")
        .map(|entry| entry.expect("an entry").file_name())
        .collect();
    assert_eq!(leftovers, [OsString::from(settings::FILE)]);
}

#[test]
fn a_broken_or_foreign_settings_file_starts_fresh() {
    let scratch = Scratch::new("broken");
    let file = JsonSettingsFile::in_dir(scratch.path());
    fs::create_dir_all(scratch.path()).expect("the directory");
    for text in [
        "",
        "{ not json",
        "[]",
        r#"{ "settings": { "output": null } }"#,
        r#"{ "version": 2, "settings": { "output": null } }"#,
        r#"{ "version": "1", "settings": { "output": null } }"#,
        r#"{ "version": 1 }"#,
        r#"{ "version": 1, "settings": { "output": 7 } }"#,
    ] {
        fs::write(&file.path, text).expect("the file is written");
        assert!(file.read().is_err(), "{text:?} was read");
        assert_eq!(file.load(), Settings::default(), "{text:?}");
    }
    // A foreign file is replaced by the next save.
    file.save(&chosen());
    assert_eq!(file.load(), chosen());
}

#[test]
fn state_dir_falls_back_to_the_temp_dir_without_local_app_data() {
    let local = Path::new("C:")
        .join("Users")
        .join("me")
        .join("AppData")
        .join("Local");
    assert_eq!(
        paths::state_dir(Some(local.clone().into_os_string())),
        local.join("Spatial SoftReceiver")
    );
    let fallback = std::env::temp_dir().join("Spatial SoftReceiver");
    assert_eq!(paths::state_dir(None), fallback);
    assert_eq!(paths::state_dir(Some(OsString::new())), fallback);
    assert_eq!(
        paths::setup_exe(Path::new("app")),
        Path::new("app").join("ssr-setup.exe")
    );
    assert_eq!(
        paths::guard_exe(Path::new("app")),
        Path::new("app").join("ssr-guard.exe")
    );
}

#[test]
fn args_parse_smoke_and_minimized() {
    let parse = |args: &[&str]| Args::parse(args.iter().copied());
    assert_eq!(parse(&[]), Ok(Args::default()));
    assert_eq!(
        parse(&["--minimized"]),
        Ok(Args {
            smoke: None,
            minimized: true,
            guarded: false,
            resume: false,
        })
    );
    assert_eq!(
        parse(&["--smoke"]),
        Ok(Args {
            smoke: Some(Duration::from_secs(30)),
            minimized: false,
            guarded: false,
            resume: false,
        })
    );
    assert_eq!(
        parse(&["--smoke", "90"]),
        Ok(Args {
            smoke: Some(Duration::from_secs(90)),
            minimized: false,
            guarded: false,
            resume: false,
        })
    );
    assert_eq!(
        parse(&["--smoke", "--minimized"]),
        Ok(Args {
            smoke: Some(Duration::from_secs(30)),
            minimized: true,
            guarded: false,
            resume: false,
        })
    );
    assert_eq!(
        parse(&["--minimized", "--smoke", "5"]),
        Ok(Args {
            smoke: Some(Duration::from_secs(5)),
            minimized: true,
            guarded: false,
            resume: false,
        })
    );
    assert_eq!(
        parse(&["--guarded", "--resume", "--minimized"]),
        Ok(Args {
            smoke: None,
            minimized: true,
            guarded: true,
            resume: true,
        })
    );
    for wrong in [
        &["--smoke", "0"][..],
        &["--smoke", "soon"],
        &["--smoke", "1.5"],
        &["--hidden"],
        &["90"],
    ] {
        assert!(parse(wrong).is_err(), "{wrong:?} parsed");
    }
}
