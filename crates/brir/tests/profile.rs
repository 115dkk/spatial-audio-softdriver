#![forbid(unsafe_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use bio_types::Feeds;
use brir::{
    AnglePreset, BrirError, BrirSource, DirectionFile, LfePolicy, Profile, load_profile, prepare,
    standard_angles,
};
use convolver::Binauralizer;
use hound::{SampleFormat, WavSpec, WavWriter};

const HESUVI_TRACKS: [&str; 30] = [
    "FL-left",
    "FL-right",
    "SL-left",
    "SL-right",
    "BL-left",
    "BL-right",
    "FC-left",
    "FR-right",
    "FR-left",
    "SR-right",
    "SR-left",
    "BR-right",
    "BR-left",
    "FC-right",
    "WL-left",
    "WL-right",
    "WR-left",
    "WR-right",
    "TFL-left",
    "TFL-right",
    "TFR-left",
    "TFR-right",
    "TSL-left",
    "TSL-right",
    "TSR-left",
    "TSR-right",
    "TBL-left",
    "TBL-right",
    "TBR-left",
    "TBR-right",
];

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(name: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("bio-brir-{name}-{}-{nonce}", std::process::id()));
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

fn write_wav_with_format(
    path: &Path,
    rate: u32,
    channels: &[Vec<f32>],
    bits_per_sample: u16,
    sample_format: SampleFormat,
) {
    assert!(!channels.is_empty());
    let frames = channels.iter().map(Vec::len).max().unwrap();
    let spec = WavSpec {
        channels: channels.len() as u16,
        sample_rate: rate,
        bits_per_sample,
        sample_format,
    };
    let mut writer = WavWriter::create(path, spec).unwrap();
    for frame in 0..frames {
        for channel in channels {
            let sample = *channel.get(frame).unwrap_or(&0.0);
            match sample_format {
                SampleFormat::Float => writer.write_sample(sample).unwrap(),
                SampleFormat::Int => {
                    let scale = 2.0_f32.powi(i32::from(bits_per_sample) - 1);
                    writer
                        .write_sample((sample * scale).round() as i32)
                        .unwrap();
                }
            }
        }
    }
    writer.finalize().unwrap();
}

fn write_wav(path: &Path, rate: u32, channels: &[Vec<f32>]) {
    write_wav_with_format(path, rate, channels, 32, SampleFormat::Float);
}

fn impulse_tracks(tracks: usize, frames: usize) -> Vec<Vec<f32>> {
    (0..tracks)
        .map(|track| {
            let mut channel = vec![0.0; frames];
            channel[track + 1] = 0.25 + track as f32 / 100.0;
            channel
        })
        .collect()
}

fn profile(source: BrirSource) -> Profile {
    Profile {
        name: "test".into(),
        brir: source,
        headphone_fir: None,
        gain_db: 0.0,
        limiter_ceiling_dbfs: -1.0,
        lfe: LfePolicy::Mute,
        angles: AnglePreset::Standard,
        angle_overrides: Vec::new(),
        truncate: None,
    }
}

fn render_impulses(prepared: &brir::PreparedBrir, frames: usize) -> [Vec<f32>; 2] {
    let mut feeds = Feeds::new(prepared.layout.len(), frames);
    for direction in 0..prepared.layout.len() {
        feeds.channel_mut(direction)[direction] = 1.0;
    }
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

#[test]
fn integer_wav_formats_are_decoded_to_float() {
    let directory = TestDirectory::new("integer-formats");
    for bits in [16, 24, 32] {
        let path = directory.join(&format!("int-{bits}.wav"));
        write_wav_with_format(
            &path,
            48_000,
            &[vec![0.5, -0.25], vec![-0.5, 0.25]],
            bits,
            SampleFormat::Int,
        );
        let source = BrirSource::Directions(vec![DirectionFile {
            name: format!("int-{bits}"),
            azimuth_deg: 0.0,
            elevation_deg: 0.0,
            path,
        }]);
        let prepared = prepare(&profile(source), 48_000, 64).unwrap();
        let output = render_impulses(&prepared, 64);
        assert!((output[0][0] - 0.5).abs() < 2.0e-5, "{bits}-bit left");
        assert!((output[1][0] + 0.5).abs() < 2.0e-5, "{bits}-bit right");
    }
}

#[test]
fn standard_speaker_angles_match_contract() {
    assert_eq!(standard_angles("FL"), Some((30.0, 0.0)));
    assert_eq!(standard_angles("BR"), Some((-150.0, 0.0)));
    assert_eq!(standard_angles("TBL"), Some((135.0, 45.0)));
    assert_eq!(standard_angles("LFE"), None);
    assert_eq!(standard_angles("UNKNOWN"), None);
}

#[test]
fn synthetic_hesuvi_maps_every_direction_and_ear() {
    let directory = TestDirectory::new("hesuvi-map");
    let path = directory.join("hesuvi.wav");
    write_wav(&path, 48_000, &impulse_tracks(14, 32));
    let prepared = prepare(&profile(BrirSource::Hesuvi(path)), 48_000, 64).unwrap();
    let names: Vec<&str> = prepared
        .layout
        .directions()
        .iter()
        .map(|direction| direction.name.as_str())
        .collect();
    assert_eq!(names, ["FL", "SL", "BL", "FC", "FR", "SR", "BR"]);

    for (direction_index, direction) in prepared.layout.directions().iter().enumerate() {
        let mut feeds = Feeds::new(prepared.layout.len(), 64);
        feeds.channel_mut(direction_index)[0] = 1.0;
        let mut binauralizer = Binauralizer::new(Arc::clone(&prepared.bank), 64);
        let mut left = vec![0.0; 64];
        let mut right = vec![0.0; 64];
        binauralizer.process(&feeds, &mut left, &mut right);
        for (ear_name, output) in [("left", left), ("right", right)] {
            let track = HESUVI_TRACKS[..14]
                .iter()
                .position(|track| *track == format!("{}-{ear_name}", direction.name))
                .unwrap();
            let (delay, gain) = strongest_at(&output);
            assert_eq!(delay, track + 1, "{}-{ear_name}", direction.name);
            assert!((gain - (0.25 + track as f32 / 100.0)).abs() < 2.0e-6);
        }
    }
}

#[test]
fn sixteen_track_hrir_drops_silent_lfe_placeholders() {
    let directory = TestDirectory::new("hrir-map");
    let path = directory.join("hrir.wav");
    let mut tracks = impulse_tracks(16, 32);
    tracks[6].fill(0.0);
    tracks[7].fill(0.0);
    write_wav(&path, 48_000, &tracks);
    let prepared = prepare(&profile(BrirSource::Hrir(path)), 48_000, 64).unwrap();
    let names: Vec<&str> = prepared
        .layout
        .directions()
        .iter()
        .map(|direction| direction.name.as_str())
        .collect();
    assert_eq!(names, ["FL", "FR", "FC", "BL", "BR", "SL", "SR"]);
    assert!(prepared.layout.find("LFE").is_none());
}

#[test]
fn extended_hesuvi_drops_unmeasured_silent_speakers() {
    let directory = TestDirectory::new("hesuvi-silent");
    let path = directory.join("hesuvi.wav");
    let mut tracks = impulse_tracks(18, 40);
    for track in &mut tracks[2..=6] {
        track.fill(0.0);
    }
    for track in &mut tracks[9..=13] {
        track.fill(0.0);
    }
    write_wav(&path, 48_000, &tracks);
    let prepared = prepare(&profile(BrirSource::Hesuvi(path)), 48_000, 64).unwrap();
    let names: Vec<&str> = prepared
        .layout
        .directions()
        .iter()
        .map(|direction| direction.name.as_str())
        .collect();
    assert_eq!(names, ["FL", "FR", "WL", "WR"]);
}

#[test]
fn directions_profile_keeps_file_order_and_angles() {
    let directory = TestDirectory::new("directions");
    let first = directory.join("one.wav");
    let second = directory.join("two.wav");
    write_wav(&first, 48_000, &[vec![1.0], vec![0.5]]);
    write_wav(&second, 48_000, &[vec![0.25], vec![-0.25]]);
    let profile = profile(BrirSource::Directions(vec![
        DirectionFile {
            name: "Odd".into(),
            azimuth_deg: 12.5,
            elevation_deg: -7.0,
            path: first,
        },
        DirectionFile {
            name: "Elsewhere".into(),
            azimuth_deg: -75.0,
            elevation_deg: 22.0,
            path: second,
        },
    ]));
    let prepared = prepare(&profile, 48_000, 64).unwrap();
    assert_eq!(prepared.layout.directions()[0].name, "Odd");
    assert_eq!(prepared.layout.directions()[0].azimuth_deg, 12.5);
    assert_eq!(prepared.layout.directions()[1].name, "Elsewhere");
    assert_eq!(prepared.layout.directions()[1].elevation_deg, 22.0);
}

#[test]
fn headphone_fir_folding_equals_filtering_the_binaural_output() {
    let directory = TestDirectory::new("fir-fold");
    let brir_path = directory.join("direction.wav");
    let fir_path = directory.join("headphone.wav");
    let left_ir = vec![0.75, -0.25, 0.125];
    let right_ir = vec![-0.5, 0.2, 0.1, -0.05];
    let left_fir = vec![0.8, 0.1, -0.05];
    let right_fir = vec![0.7, -0.2];
    write_wav(&brir_path, 48_000, &[left_ir.clone(), right_ir.clone()]);
    write_wav(&fir_path, 48_000, &[left_fir.clone(), right_fir.clone()]);
    let source = BrirSource::Directions(vec![DirectionFile {
        name: "X".into(),
        azimuth_deg: 0.0,
        elevation_deg: 0.0,
        path: brir_path,
    }]);
    let without = prepare(&profile(source.clone()), 48_000, 64).unwrap();
    let mut with_profile = profile(source);
    with_profile.headphone_fir = Some(fir_path);
    let with = prepare(&with_profile, 48_000, 64).unwrap();
    let frames = 64;
    let without_output = render_impulses(&without, frames);
    let with_output = render_impulses(&with, frames);
    for (ear, fir) in [left_fir, right_fir].iter().enumerate() {
        let mut filtered = vec![0.0; frames];
        for frame in 0..frames {
            for tap in 0..=frame.min(fir.len() - 1) {
                filtered[frame] += without_output[ear][frame - tap] * fir[tap];
            }
        }
        for (actual, expected) in with_output[ear].iter().zip(filtered) {
            assert!((actual - expected).abs() < 2.0e-6);
        }
    }
}

#[test]
fn mono_headphone_fir_applies_to_both_ears() {
    let directory = TestDirectory::new("mono-fir");
    let brir_path = directory.join("direction.wav");
    let fir_path = directory.join("headphone.wav");
    let fir = vec![0.75, -0.25, 0.125];
    write_wav(&brir_path, 48_000, &[vec![1.0], vec![0.5]]);
    write_wav(&fir_path, 48_000, std::slice::from_ref(&fir));
    let mut input = profile(BrirSource::Directions(vec![DirectionFile {
        name: "X".into(),
        azimuth_deg: 0.0,
        elevation_deg: 0.0,
        path: brir_path,
    }]));
    input.headphone_fir = Some(fir_path);
    let prepared = prepare(&input, 48_000, 64).unwrap();
    let output = render_impulses(&prepared, 64);
    for (frame, sample) in fir.iter().enumerate() {
        assert!((output[0][frame] - sample).abs() < 2.0e-6);
        assert!((output[1][frame] - sample * 0.5).abs() < 2.0e-6);
    }
}

#[test]
fn resampling_44100_to_48000_preserves_impulse_time_and_gain() {
    let directory = TestDirectory::new("resample");
    let path = directory.join("direction.wav");
    let mut impulse = vec![0.0; 512];
    let input_delay = 100;
    impulse[input_delay] = 0.5;
    write_wav(&path, 44_100, &[impulse.clone(), impulse]);
    let source = BrirSource::Directions(vec![DirectionFile {
        name: "X".into(),
        azimuth_deg: 0.0,
        elevation_deg: 0.0,
        path,
    }]);
    let prepared = prepare(&profile(source), 48_000, 64).unwrap();
    let output = render_impulses(&prepared, 1024);
    let expected_delay = (input_delay as f64 * 48_000.0 / 44_100.0).round() as usize;
    let (delay, gain) = strongest_at(&output[0]);
    assert!(
        delay.abs_diff(expected_delay) <= 1,
        "{delay} vs {expected_delay}"
    );
    let gain_error_db = 20.0 * (gain.abs() / 0.5).log10().abs();
    assert!(gain_error_db < 0.5, "gain error was {gain_error_db} dB");
}

#[test]
fn profile_toml_resolves_paths_defaults_lfe_and_parses_route() {
    let directory = TestDirectory::new("toml");
    let profile_path = directory.join("profile.toml");
    fs::write(
        &profile_path,
        r#"
name = "relative"
gain_db = 6.0
limiter_ceiling_dbfs = -2.0

[brir]
hesuvi = "sets/hesuvi.wav"
headphone_fir = "filters/headphone.wav"
"#,
    )
    .unwrap();
    let loaded = load_profile(&profile_path).unwrap();
    assert_eq!(
        loaded.brir,
        BrirSource::Hesuvi(directory.join("sets/hesuvi.wav"))
    );
    assert_eq!(
        loaded.headphone_fir,
        Some(directory.join("filters/headphone.wav"))
    );
    assert_eq!(loaded.lfe, LfePolicy::Mute);

    fs::write(
        &profile_path,
        r#"
name = "directions"
gain_db = 0.0
limiter_ceiling_dbfs = -1.0

[brir]
directions = [{ name = "Custom", azimuth_deg = 17.5, elevation_deg = -3.0, path = "custom.wav" }]

[lfe]
direction = "Custom"
gain_db = -6.0
"#,
    )
    .unwrap();
    let loaded = load_profile(&profile_path).unwrap();
    let BrirSource::Directions(directions) = loaded.brir else {
        panic!("directions source expected");
    };
    assert_eq!(directions[0].path, directory.join("custom.wav"));
    assert_eq!(
        loaded.lfe,
        LfePolicy::Direction {
            name: "Custom".into(),
            gain_db: -6.0
        }
    );
}

#[test]
fn bad_track_count_and_unknown_lfe_direction_are_errors() {
    let directory = TestDirectory::new("errors");
    let bad = directory.join("bad.wav");
    write_wav(&bad, 48_000, &impulse_tracks(12, 20));
    assert!(matches!(
        prepare(&profile(BrirSource::Hesuvi(bad)), 48_000, 64),
        Err(BrirError::TrackCount { tracks: 12, .. })
    ));

    let direction_path = directory.join("direction.wav");
    write_wav(&direction_path, 48_000, &[vec![1.0], vec![1.0]]);
    let mut bad_lfe = profile(BrirSource::Directions(vec![DirectionFile {
        name: "Known".into(),
        azimuth_deg: 0.0,
        elevation_deg: 0.0,
        path: direction_path,
    }]));
    bad_lfe.lfe = LfePolicy::Direction {
        name: "Missing".into(),
        gain_db: 0.0,
    };
    assert!(matches!(
        prepare(&bad_lfe, 48_000, 64),
        Err(BrirError::BadLfeDirection(name)) if name == "Missing"
    ));
}

#[test]
fn gains_and_lfe_route_become_linear() {
    let directory = TestDirectory::new("linear");
    let direction_path = directory.join("direction.wav");
    write_wav(&direction_path, 48_000, &[vec![1.0], vec![1.0]]);
    let mut input = profile(BrirSource::Directions(vec![DirectionFile {
        name: "Center".into(),
        azimuth_deg: 0.0,
        elevation_deg: 0.0,
        path: direction_path,
    }]));
    input.gain_db = 6.0;
    input.limiter_ceiling_dbfs = -1.0;
    input.lfe = LfePolicy::Direction {
        name: "Center".into(),
        gain_db: -6.0,
    };
    let prepared = prepare(&input, 48_000, 64).unwrap();
    assert!((prepared.gain - 10.0_f32.powf(6.0 / 20.0)).abs() < 1.0e-6);
    assert!((prepared.limiter_ceiling - 10.0_f32.powf(-1.0 / 20.0)).abs() < 1.0e-6);
    let (_, lfe_gain) = prepared.lfe.unwrap();
    assert!((lfe_gain - 10.0_f32.powf(-6.0 / 20.0)).abs() < 1.0e-6);
}

#[test]
#[ignore = "requires BIO_DEMO_HESUVI"]
fn prepares_demo_hesuvi_from_environment() {
    let path = std::env::var_os("BIO_DEMO_HESUVI").expect("BIO_DEMO_HESUVI is not set");
    let prepared = prepare(&profile(BrirSource::Hesuvi(path.into())), 48_000, 480).unwrap();
    let names: Vec<&str> = prepared
        .layout
        .directions()
        .iter()
        .map(|direction| direction.name.as_str())
        .collect();
    assert_eq!(&names[..7], ["FL", "SL", "BL", "FC", "FR", "SR", "BR"]);
}
