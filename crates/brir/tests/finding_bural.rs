#![forbid(unsafe_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use brir::{
    AnglePreset, BrirError, BrirSource, LfePolicy, Profile, Truncation, canonical_name,
    load_profile, prepare, preset_angles,
};
use convolver::Binauralizer;
use hound::{SampleFormat, WavSpec, WavWriter};
use ssr_types::Feeds;

const CANONICAL: [&str; 21] = [
    "FL", "FR", "FC", "SL", "SR", "BL", "BR", "WL", "WR", "HFL", "HFR", "HFC", "HBL", "HBR", "TFL",
    "TFR", "TSL", "TSR", "TBL", "TBR", "TC",
];

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(name: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "ssr-brir-finding-bural-{name}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn write_wav(path: &Path, rate: u32, channels: &[Vec<f32>]) {
    let frames = channels.iter().map(Vec::len).max().unwrap();
    let mut writer = WavWriter::create(
        path,
        WavSpec {
            channels: channels.len() as u16,
            sample_rate: rate,
            bits_per_sample: 32,
            sample_format: SampleFormat::Float,
        },
    )
    .unwrap();
    for frame in 0..frames {
        for channel in channels {
            writer
                .write_sample(*channel.get(frame).unwrap_or(&0.0))
                .unwrap();
        }
    }
    writer.finalize().unwrap();
}

fn impulse_tracks(tracks: usize, frames: usize) -> Vec<Vec<f32>> {
    (0..tracks)
        .map(|track| {
            let mut samples = vec![0.0; frames];
            samples[track + 1] = 0.1 + track as f32 / 100.0;
            samples
        })
        .collect()
}

fn profile(source: BrirSource) -> Profile {
    Profile {
        name: "finding-bural".into(),
        brir: source,
        headphone_fir: None,
        gain_db: 0.0,
        limiter_ceiling_dbfs: -1.0,
        lfe: LfePolicy::Mute,
        angles: AnglePreset::Standard,
        angle_overrides: Vec::new(),
        truncate: None,
        trim_leading_delay: false,
    }
}

fn render_direction(
    prepared: &brir::PreparedBrir,
    direction: usize,
    frames: usize,
) -> [Vec<f32>; 2] {
    let mut feeds = Feeds::new(prepared.layout.len(), frames);
    feeds.channel_mut(direction)[0] = 1.0;
    let mut binauralizer = Binauralizer::new(Arc::clone(&prepared.bank), frames);
    let mut left = vec![0.0; frames];
    let mut right = vec![0.0; frames];
    binauralizer.process(&feeds, &mut left, &mut right);
    [left, right]
}

fn strongest_at(signal: &[f32]) -> (usize, f32) {
    signal
        .iter()
        .copied()
        .enumerate()
        .max_by(|(_, left), (_, right)| left.abs().total_cmp(&right.abs()))
        .unwrap()
}

fn channels_map(names: &[&str]) -> String {
    let mut csv = String::from("speaker,BRIR_left_1based,BRIR_right_1based\n");
    for (index, name) in names.iter().enumerate() {
        csv.push_str(&format!("{name},{},{}\n", index * 2 + 1, index * 2 + 2));
    }
    csv
}

#[test]
fn canonical_name_covers_current_aliases_legacy_names_and_repeats() {
    let cases = [
        ("C", "FC"),
        ("FWL", "WL"),
        ("FWR", "WR"),
        ("TOP", "TC"),
        ("TML", "TSL"),
        ("TMR", "TSR"),
        ("HSL", "HBL"),
        ("HSR", "HBR"),
        ("LFH", "HFL"),
        ("RFH", "HFR"),
        ("CFH", "HFC"),
        ("LBH", "HBL"),
        ("RBH", "HBR"),
        ("LFT", "TFL"),
        ("RFT", "TFR"),
        ("LMT", "TSL"),
        ("RMT", "TSR"),
        ("LS", "SL"),
        ("RS", "SR"),
        ("LB", "BL"),
        ("RB", "BR"),
    ];
    for (alias, canonical) in cases {
        assert_eq!(canonical_name(alias), Some(canonical), "{alias}");
    }
    for name in CANONICAL {
        assert_eq!(canonical_name(name), Some(name));
    }
    assert_eq!(canonical_name("LFE"), None);
    assert_eq!(canonical_name("unknown"), None);
    assert_eq!(canonical_name("FL_repeat"), None);
    assert_eq!(canonical_name("TML_REPEAT"), None);
}

#[test]
fn angle_presets_cover_every_canonical_direction() {
    for preset in [
        AnglePreset::Standard,
        AnglePreset::Template21,
        AnglePreset::Room21,
    ] {
        for name in CANONICAL {
            assert!(preset_angles(preset, name).is_some(), "{preset:?} {name}");
        }
    }
    assert_eq!(
        preset_angles(AnglePreset::Template21, "SL"),
        Some((90.0, 0.0))
    );
    assert_eq!(preset_angles(AnglePreset::Room21, "SL"), Some((120.0, 0.0)));
    assert_eq!(
        preset_angles(AnglePreset::Template21, "HFL"),
        Some((30.0, 35.0))
    );
    assert_eq!(
        preset_angles(AnglePreset::Room21, "HFL"),
        Some((30.0, 30.0))
    );
    assert_eq!(
        preset_angles(AnglePreset::Template21, "TFL"),
        Some((45.0, 35.0))
    );
    assert_eq!(
        preset_angles(AnglePreset::Room21, "TFL"),
        Some((45.0, 45.0))
    );
    assert_eq!(
        preset_angles(AnglePreset::Standard, "HBL"),
        Some((110.0, 30.0))
    );
    assert_eq!(
        preset_angles(AnglePreset::Standard, "TC"),
        Some((0.0, 90.0))
    );
    assert_eq!(preset_angles(AnglePreset::Standard, "LFE"), None);
}

#[test]
fn mapped_channels_sidecar_preserves_every_track_delay_and_level() {
    let directory = TestDirectory::new("mapped-channels");
    let wav = directory.join("set.wav");
    let map = directory.join("channels.csv");
    write_wav(&wav, 48_000, &impulse_tracks(42, 64));
    fs::write(&map, channels_map(&CANONICAL)).unwrap();

    let prepared = prepare(&profile(BrirSource::Mapped { wav, map }), 48_000, 64).unwrap();
    let names: Vec<&str> = prepared
        .layout
        .directions()
        .iter()
        .map(|direction| direction.name.as_str())
        .collect();
    assert_eq!(names, CANONICAL);
    for (direction, name) in CANONICAL.iter().enumerate() {
        let output = render_direction(&prepared, direction, 128);
        for (ear, samples) in output.iter().enumerate() {
            let track = direction * 2 + ear;
            let (delay, level) = strongest_at(samples);
            assert_eq!(delay, track + 1, "{name} ear {ear}");
            assert!((level - (0.1 + track as f32 / 100.0)).abs() < 2.0e-6);
        }
    }
}

#[test]
fn mapped_wav_drops_and_reports_silent_directions() {
    let directory = TestDirectory::new("mapped-silent");
    let wav = directory.join("set.wav");
    let map = directory.join("channels.csv");
    write_wav(&wav, 48_000, &[vec![1.0], vec![0.5], vec![0.0], vec![0.0]]);
    fs::write(
        &map,
        "speaker,BRIR_left_1based,BRIR_right_1based\nFL,1,2\nTC,3,4\n",
    )
    .unwrap();
    let prepared = prepare(&profile(BrirSource::Mapped { wav, map }), 48_000, 64).unwrap();
    assert_eq!(prepared.layout.len(), 1);
    assert_eq!(prepared.omitted_silent_directions, ["TC"]);
}

#[test]
fn mapped_reaverb_sidecar_uses_its_track_columns() {
    let directory = TestDirectory::new("mapped-reaverb");
    let wav = directory.join("set.wav");
    let map = directory.join("ReaVerb_채널표.csv");
    write_wav(&wav, 48_000, &impulse_tracks(42, 64));
    let mut csv = String::from("\u{feff}speaker,file,left_track,right_track,main_track\n");
    for (index, name) in CANONICAL.iter().enumerate() {
        csv.push_str(&format!(
            "{name},{name}.wav,{},{},{}\n",
            index * 2 + 1,
            index * 2 + 2,
            index + 1
        ));
    }
    fs::write(&map, csv).unwrap();

    let prepared = prepare(&profile(BrirSource::Mapped { wav, map }), 48_000, 64).unwrap();
    let last = render_direction(&prepared, 20, 128);
    assert_eq!(strongest_at(&last[0]).0, 41);
    assert_eq!(strongest_at(&last[1]).0, 42);
}

#[test]
fn mapped_44_track_repeat_row_is_skipped() {
    let directory = TestDirectory::new("mapped-repeat");
    let wav = directory.join("set.wav");
    let map = directory.join("channels.csv");
    write_wav(&wav, 48_000, &impulse_tracks(44, 80));
    let mut csv = channels_map(&CANONICAL);
    csv.push_str("FL_repeat,43,44\n");
    fs::write(&map, csv).unwrap();

    let prepared = prepare(&profile(BrirSource::Mapped { wav, map }), 48_000, 64).unwrap();
    assert_eq!(prepared.layout.len(), 21);
    assert_eq!(strongest_at(&render_direction(&prepared, 0, 128)[0]).0, 1);
}

#[test]
fn mapped_missing_track_reports_sidecar_line() {
    let directory = TestDirectory::new("mapped-missing-track");
    let wav = directory.join("set.wav");
    let map = directory.join("channels.csv");
    write_wav(&wav, 48_000, &impulse_tracks(42, 64));
    fs::write(
        &map,
        "speaker,BRIR_left_1based,BRIR_right_1based\nFL,1,2\nFR,3,43\n",
    )
    .unwrap();

    assert!(matches!(
        prepare(&profile(BrirSource::Mapped { wav, map }), 48_000, 64),
        Err(BrirError::Sidecar { line: 3, .. })
    ));
}

#[test]
fn mapped_alias_duplicate_is_rejected() {
    let directory = TestDirectory::new("mapped-duplicate");
    let wav = directory.join("set.wav");
    let map = directory.join("channels.csv");
    write_wav(&wav, 48_000, &impulse_tracks(4, 16));
    fs::write(
        &map,
        "speaker,BRIR_left_1based,BRIR_right_1based\nFC,1,2\nC,3,4\n",
    )
    .unwrap();

    assert!(matches!(
        prepare(&profile(BrirSource::Mapped { wav, map }), 48_000, 64),
        Err(BrirError::DuplicateDirection { name, line: 3, .. }) if name == "FC"
    ));
}

#[test]
fn folder_aliases_are_canonical_and_follow_documented_order() {
    let directory = TestDirectory::new("folder-order");
    let folder = directory.join("reaverb");
    fs::create_dir(&folder).unwrap();
    for (name, value) in [
        ("TOP", 0.21),
        ("HSL", 0.13),
        ("TML", 0.17),
        ("FWL", 0.08),
        ("C", 0.03),
        ("FL", 0.01),
    ] {
        write_wav(
            &folder.join(format!("{name}.WaV")),
            48_000,
            &[vec![value], vec![-value]],
        );
    }

    let prepared = prepare(&profile(BrirSource::Folder(folder)), 48_000, 64).unwrap();
    let names: Vec<&str> = prepared
        .layout
        .directions()
        .iter()
        .map(|direction| direction.name.as_str())
        .collect();
    assert_eq!(names, ["FL", "FC", "WL", "HBL", "TSL", "TC"]);
}

#[test]
fn folder_drops_and_reports_silent_directions() {
    let directory = TestDirectory::new("folder-silent");
    write_wav(&directory.join("FL.wav"), 48_000, &[vec![1.0], vec![0.5]]);
    write_wav(
        &directory.join("TC.wav"),
        48_000,
        &[vec![0.0; 4], vec![0.0; 4]],
    );
    let prepared = prepare(
        &profile(BrirSource::Folder(directory.0.clone())),
        48_000,
        64,
    )
    .unwrap();
    assert_eq!(prepared.layout.len(), 1);
    assert_eq!(prepared.omitted_silent_directions, ["TC"]);
}

#[test]
fn folder_rejects_mono_direction_file() {
    let directory = TestDirectory::new("folder-mono");
    write_wav(&directory.join("FL.wav"), 48_000, &[vec![1.0]]);
    assert!(matches!(
        prepare(&profile(BrirSource::Folder(directory.0.clone())), 48_000, 64),
        Err(BrirError::Wav { detail, .. }) if detail.contains("two tracks")
    ));
}

#[test]
fn truncation_cuts_all_tracks_fades_and_preserves_the_start() {
    let directory = TestDirectory::new("truncate");
    let wav = directory.join("set.wav");
    let map = directory.join("channels.csv");
    write_wav(
        &wav,
        1_000,
        &[vec![1.0; 20], vec![0.5; 20], vec![-1.0; 20], vec![-0.5; 20]],
    );
    fs::write(
        &map,
        "speaker,BRIR_left_1based,BRIR_right_1based\nFL,1,2\nFR,3,4\n",
    )
    .unwrap();
    let mut input = profile(BrirSource::Mapped { wav, map });
    input.truncate = Some(Truncation {
        length_ms: 10.0,
        fade_ms: 4.0,
    });
    let prepared = prepare(&input, 1_000, 16).unwrap();
    assert_eq!(prepared.ir_frames, 10);
    for direction in 0..2 {
        let output = render_direction(&prepared, direction, 16);
        for (ear, samples) in output.iter().enumerate() {
            let base = if direction == 0 { 1.0 } else { -1.0 } * if ear == 0 { 1.0 } else { 0.5 };
            for actual in &samples[..6] {
                assert!((*actual - base).abs() < 2.0e-6);
            }
            for (actual, gain) in samples[6..10].iter().zip([1.0, 0.75, 0.25, 0.0]) {
                assert!(
                    (*actual - base * gain).abs() < 2.0e-6,
                    "{actual} vs {}",
                    base * gain
                );
            }
            assert!(samples[10].abs() < 2.0e-6);
        }
    }
}

#[test]
fn invalid_truncation_parameters_are_rejected() {
    let directory = TestDirectory::new("truncate-invalid");
    let wav = directory.join("FL.wav");
    write_wav(&wav, 48_000, &[vec![1.0], vec![1.0]]);
    for truncation in [
        Truncation {
            length_ms: 0.0,
            fade_ms: 1.0,
        },
        Truncation {
            length_ms: 1.0,
            fade_ms: 0.0,
        },
        Truncation {
            length_ms: 1.0,
            fade_ms: 2.0,
        },
    ] {
        let mut input = profile(BrirSource::Folder(directory.0.clone()));
        input.truncate = Some(truncation);
        assert!(matches!(
            prepare(&input, 48_000, 64),
            Err(BrirError::Truncation { .. })
        ));
    }
}

#[test]
fn angle_override_beats_preset_and_unknown_without_override_errors() {
    let directory = TestDirectory::new("angles");
    let wav = directory.join("set.wav");
    let map = directory.join("channels.csv");
    write_wav(&wav, 48_000, &[vec![1.0], vec![1.0]]);
    fs::write(
        &map,
        "speaker,BRIR_left_1based,BRIR_right_1based\nHFL,1,2\n",
    )
    .unwrap();
    let mut input = profile(BrirSource::Mapped { wav, map });
    input.angles = AnglePreset::Template21;
    input.angle_overrides.push(("LFH".into(), 31.0, 32.0));
    let prepared = prepare(&input, 48_000, 64).unwrap();
    assert_eq!(prepared.layout.directions()[0].azimuth_deg, 31.0);
    assert_eq!(prepared.layout.directions()[0].elevation_deg, 32.0);

    let folder = directory.join("unknown");
    fs::create_dir(&folder).unwrap();
    write_wav(&folder.join("Mystery.wav"), 48_000, &[vec![1.0], vec![1.0]]);
    assert!(matches!(
        prepare(&profile(BrirSource::Folder(folder.clone())), 48_000, 64),
        Err(BrirError::UnknownDirection(name)) if name == "Mystery"
    ));
    let mut overridden = profile(BrirSource::Folder(folder));
    overridden
        .angle_overrides
        .push(("Mystery".into(), 12.0, -3.0));
    let prepared = prepare(&overridden, 48_000, 64).unwrap();
    assert_eq!(prepared.layout.directions()[0].name, "Mystery");
    assert_eq!(prepared.layout.directions()[0].azimuth_deg, 12.0);
    assert_eq!(prepared.layout.directions()[0].elevation_deg, -3.0);
}

#[test]
fn raw_measurement_named_as_headphone_file_is_rejected() {
    let directory = TestDirectory::new("measurement-fir");
    let brir = directory.join("FL.wav");
    let measurement = directory.join("headphone_measurement_LR.wav");
    write_wav(&brir, 48_000, &[vec![1.0], vec![1.0]]);
    write_wav(&measurement, 48_000, &[vec![1.0], vec![1.0]]);
    let mut input = profile(BrirSource::Folder(directory.0.clone()));
    input.headphone_fir = Some(measurement.clone());
    assert!(matches!(
        prepare(&input, 48_000, 64),
        Err(BrirError::MeasurementFir { path }) if path == measurement
    ));
}

#[test]
fn profile_toml_loads_mapped_folder_angles_overrides_and_truncation() {
    let directory = TestDirectory::new("toml-new");
    let path = directory.join("profile.toml");
    fs::write(
        &path,
        r#"
name = "mapped"
limiter_ceiling_dbfs = -1.0
angles = "room21"
truncate_ms = 1000

[angle_overrides]
HFL = [30.0, 30.0]

[brir]
wav = "RBRIR_X_42ch.wav"
map = "channels.csv"
headphone_fir = "hp.wav"
"#,
    )
    .unwrap();
    let loaded = load_profile(&path).unwrap();
    assert_eq!(loaded.angles, AnglePreset::Room21);
    assert_eq!(loaded.angle_overrides, vec![("HFL".into(), 30.0, 30.0)]);
    assert_eq!(
        loaded.truncate,
        Some(Truncation {
            length_ms: 1000.0,
            fade_ms: 50.0,
        })
    );
    assert_eq!(
        loaded.brir,
        BrirSource::Mapped {
            wav: directory.join("RBRIR_X_42ch.wav"),
            map: directory.join("channels.csv"),
        }
    );

    fs::write(
        &path,
        r#"
name = "folder"
limiter_ceiling_dbfs = -1.0
truncate_ms = 900
fade_ms = 25
[brir]
folder = "reaverb"
"#,
    )
    .unwrap();
    let loaded = load_profile(&path).unwrap();
    assert_eq!(loaded.angles, AnglePreset::Standard);
    assert_eq!(loaded.brir, BrirSource::Folder(directory.join("reaverb")));
    assert_eq!(loaded.truncate.unwrap().fade_ms, 25.0);
}

#[test]
fn profile_toml_enforces_exactly_one_source_and_mapped_pair() {
    let directory = TestDirectory::new("toml-source-rule");
    let path = directory.join("profile.toml");
    for brir in [
        "hesuvi = \"a.wav\"\nfolder = \"set\"",
        "wav = \"a.wav\"",
        "map = \"channels.csv\"",
    ] {
        fs::write(
            &path,
            format!("name = \"bad\"\nlimiter_ceiling_dbfs = -1.0\n[brir]\n{brir}\n"),
        )
        .unwrap();
        assert!(matches!(
            load_profile(&path),
            Err(BrirError::Profile { .. })
        ));
    }
}
