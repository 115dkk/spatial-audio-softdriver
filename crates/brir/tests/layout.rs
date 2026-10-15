#![forbid(unsafe_code)]

//! `brir::layout` reads headers only and equals `prepare`'s layout, silent
//! directions included (ADR 0016, 2026-10-15 amendment);
//! `brir::check_headphone_fir` accepts exactly what `prepare` can fold.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use brir::{
    AnglePreset, BrirError, BrirSource, DirectionFile, LfePolicy, Profile, check_headphone_fir,
    layout, prepare,
};
use hound::{SampleFormat, WavSpec, WavWriter};
use ssr_types::{Direction, LayoutError};

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(name: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "ssr-brir-layout-{name}-{}-{nonce}",
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

fn write_wav_as(
    path: &Path,
    rate: u32,
    channels: &[Vec<f32>],
    bits_per_sample: u16,
    sample_format: SampleFormat,
) {
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
                        .write_sample((sample * (scale - 1.0)).round() as i32)
                        .unwrap();
                }
            }
        }
    }
    writer.finalize().unwrap();
}

fn write_wav(path: &Path, rate: u32, channels: &[Vec<f32>]) {
    write_wav_as(path, rate, channels, 32, SampleFormat::Float);
}

fn impulse_tracks(tracks: usize, frames: usize) -> Vec<Vec<f32>> {
    (0..tracks)
        .map(|track| {
            let mut samples = vec![0.0; frames];
            samples[track % frames] = 0.1 + track as f32 / 100.0;
            samples
        })
        .collect()
}

fn stereo_impulse() -> Vec<Vec<f32>> {
    vec![vec![0.5, 0.0, 0.0], vec![0.0, 0.25, 0.0]]
}

fn profile(source: BrirSource) -> Profile {
    Profile {
        name: "layout".into(),
        brir: source,
        headphone_fir: None,
        gain_db: 0.0,
        limiter_ceiling_dbfs: -1.0,
        lfe: LfePolicy::Mute,
        angles: AnglePreset::Standard,
        angle_overrides: Vec::new(),
        truncate: None,
        trim_leading_delay: true,
    }
}

/// The header-only layout, checked against the prepared one direction by
/// direction (name, azimuth, elevation, in order).
fn assert_matches_prepare(profile: &Profile) -> Vec<Direction> {
    let from_headers = layout(profile).unwrap();
    let prepared = prepare(profile, 48_000, 64).unwrap();
    assert_eq!(from_headers.directions(), prepared.layout.directions());
    from_headers.directions().to_vec()
}

fn names(directions: &[Direction]) -> Vec<&str> {
    directions
        .iter()
        .map(|direction| direction.name.as_str())
        .collect()
}

fn channels_map(names: &[&str]) -> String {
    let mut csv = String::from("speaker,BRIR_left_1based,BRIR_right_1based\n");
    for (index, name) in names.iter().enumerate() {
        csv.push_str(&format!("{name},{},{}\n", index * 2 + 1, index * 2 + 2));
    }
    csv
}

#[test]
fn hesuvi_layout_equals_prepare_for_every_track_count() {
    let directory = TestDirectory::new("hesuvi");
    let mut seen = Vec::new();
    for tracks in (14..=30).step_by(2) {
        let path = directory.join(&format!("hesuvi-{tracks}.wav"));
        write_wav(&path, 48_000, &impulse_tracks(tracks, 48));
        let directions = assert_matches_prepare(&profile(BrirSource::Hesuvi(path)));
        seen.push((tracks, directions.len()));
        if tracks == 14 {
            assert_eq!(
                names(&directions),
                ["FL", "SL", "BL", "FC", "FR", "SR", "BR"]
            );
        }
        if tracks == 30 {
            assert_eq!(
                &names(&directions)[7..],
                ["WL", "WR", "TFL", "TFR", "TSL", "TSR", "TBL", "TBR"]
            );
            assert_eq!(directions[9].azimuth_deg, 45.0);
            assert_eq!(directions[9].elevation_deg, 45.0);
        }
    }
    assert_eq!(seen.first(), Some(&(14, 7)));
    assert_eq!(seen.last(), Some(&(30, 15)));
}

#[test]
fn hrir_layout_equals_prepare_and_skips_lfe() {
    let directory = TestDirectory::new("hrir");
    for tracks in [16, 20, 32] {
        let path = directory.join(&format!("hrir-{tracks}.wav"));
        write_wav(&path, 44_100, &impulse_tracks(tracks, 48));
        let directions = assert_matches_prepare(&profile(BrirSource::Hrir(path)));
        assert!(!names(&directions).contains(&"LFE"));
        assert_eq!(
            &names(&directions)[..7],
            ["FL", "FR", "FC", "BL", "BR", "SL", "SR"]
        );
        assert_eq!(directions.len(), tracks / 2 - 1);
    }
}

#[test]
fn direction_files_layout_equals_prepare_with_explicit_angles() {
    let directory = TestDirectory::new("directions");
    let first = directory.join("one.wav");
    let second = directory.join("two.wav");
    write_wav(&first, 96_000, &stereo_impulse());
    write_wav_as(&second, 96_000, &stereo_impulse(), 24, SampleFormat::Int);
    let directions = assert_matches_prepare(&profile(BrirSource::Directions(vec![
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
    ])));
    assert_eq!(
        directions,
        [
            Direction::new("Odd", 12.5, -7.0),
            Direction::new("Elsewhere", -75.0, 22.0)
        ]
    );
}

#[test]
fn mapped_layouts_equal_prepare_for_both_sidecar_shapes() {
    let directory = TestDirectory::new("mapped");
    let wav = directory.join("set.wav");
    write_wav(&wav, 48_000, &impulse_tracks(10, 32));

    let channels = directory.join("channels.csv");
    fs::write(
        &channels,
        format!(
            "\u{feff}{}\nFL_repeat,1,2\n\n",
            channels_map(&["L", "R", "C", "TOP", "LFH"]).trim_end()
        ),
    )
    .unwrap();
    let directions = assert_matches_prepare(&profile(BrirSource::Mapped {
        wav: wav.clone(),
        map: channels,
    }));
    assert_eq!(names(&directions), ["FL", "FR", "FC", "TC", "HFL"]);

    let reaverb = directory.join("reaverb.csv");
    fs::write(
        &reaverb,
        "speaker,file,left_track,right_track,main_track\n\
         SR,set.wav,10,9,1\n\
         FWL,set.wav,3,4,1\n",
    )
    .unwrap();
    let mut room = profile(BrirSource::Mapped { wav, map: reaverb });
    room.angles = AnglePreset::Room21;
    let directions = assert_matches_prepare(&room);
    assert_eq!(
        directions,
        [
            Direction::new("SR", -120.0, 0.0),
            Direction::new("WL", 60.0, 0.0)
        ]
    );
}

#[test]
fn folder_layout_equals_prepare_with_aliases_overrides_and_order() {
    let directory = TestDirectory::new("folder");
    let folder = directory.join("set");
    fs::create_dir(&folder).unwrap();
    for name in [
        "Custom.wav",
        "TOP.WAV",
        "FR.wav",
        "L.wav",
        "FL_repeat.wav",
        "aardvark.wav",
    ] {
        write_wav(&folder.join(name), 44_100, &stereo_impulse());
    }
    fs::write(folder.join("notes.txt"), "not a direction").unwrap();
    fs::create_dir(folder.join("nested.wav")).unwrap();
    let mut input = profile(BrirSource::Folder(folder));
    input.angle_overrides = vec![
        ("custom".into(), 10.0, 5.0),
        ("Aardvark".into(), -10.0, -5.0),
        ("L".into(), 33.0, 1.0),
    ];
    let directions = assert_matches_prepare(&input);
    assert_eq!(names(&directions), ["FL", "FR", "TC", "aardvark", "Custom"]);
    assert_eq!(directions[0], Direction::new("FL", 33.0, 1.0));
    assert_eq!(directions[2], Direction::new("TC", 0.0, 90.0));
    assert_eq!(directions[3], Direction::new("aardvark", -10.0, -5.0));
}

#[test]
fn silent_directions_are_kept_by_headers_and_prepare_alike() {
    let directory = TestDirectory::new("silent");
    let path = directory.join("hesuvi.wav");
    let mut tracks = impulse_tracks(14, 32);
    tracks[0].fill(0.0);
    tracks[1].fill(0.0);
    write_wav(&path, 48_000, &tracks);
    let input = profile(BrirSource::Hesuvi(path));
    let from_headers = layout(&input).unwrap();
    let prepared = prepare(&input, 48_000, 64).unwrap();
    assert_eq!(from_headers.len(), 7);
    assert_eq!(from_headers.directions(), prepared.layout.directions());
    assert_eq!(prepared.layout.directions()[0].name, "FL");

    // A set whose every direction is silent still has its declared layout;
    // only a set that declares none is empty.
    let all_silent = directory.join("all-silent");
    fs::create_dir(&all_silent).unwrap();
    write_wav(
        &all_silent.join("FL.wav"),
        48_000,
        &[vec![0.0; 4], vec![0.0; 4]],
    );
    let input = profile(BrirSource::Folder(all_silent));
    assert_eq!(layout(&input).unwrap().len(), 1);
    assert_eq!(prepare(&input, 48_000, 64).unwrap().layout.len(), 1);
}

#[test]
fn layout_reads_headers_only() {
    let directory = TestDirectory::new("headers-only");
    let path = directory.join("hesuvi.wav");
    write_wav(&path, 48_000, &impulse_tracks(14, 4_096));
    // Cut the data chunk short: the header still promises every frame.
    let bytes = fs::read(&path).unwrap();
    fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();
    let input = profile(BrirSource::Hesuvi(path));
    assert_eq!(layout(&input).unwrap().len(), 7);
    assert!(prepare(&input, 48_000, 64).is_err());
}

#[test]
fn layout_rejects_what_prepare_rejects_from_headers() {
    let directory = TestDirectory::new("errors");
    let both = |input: &Profile| -> (BrirError, BrirError) {
        (
            layout(input).unwrap_err(),
            prepare(input, 48_000, 64).unwrap_err(),
        )
    };

    let thirteen = directory.join("hesuvi-13.wav");
    write_wav(&thirteen, 48_000, &impulse_tracks(13, 32));
    let fifteen = directory.join("hrir-15.wav");
    write_wav(&fifteen, 48_000, &impulse_tracks(15, 32));
    for input in [
        profile(BrirSource::Hesuvi(thirteen)),
        profile(BrirSource::Hrir(fifteen.clone())),
        profile(BrirSource::Hesuvi(fifteen)),
    ] {
        let (from_headers, prepared) = both(&input);
        assert!(
            matches!(from_headers, BrirError::TrackCount { .. }),
            "{from_headers}"
        );
        assert!(
            matches!(prepared, BrirError::TrackCount { .. }),
            "{prepared}"
        );
    }

    let mono = directory.join("mono.wav");
    write_wav(&mono, 48_000, &[vec![1.0]]);
    let (from_headers, prepared) = both(&profile(BrirSource::Directions(vec![DirectionFile {
        name: "Mono".into(),
        azimuth_deg: 0.0,
        elevation_deg: 0.0,
        path: mono,
    }])));
    assert_eq!(from_headers.to_string(), prepared.to_string());

    let folder = directory.join("rates");
    fs::create_dir(&folder).unwrap();
    write_wav(&folder.join("FL.wav"), 48_000, &stereo_impulse());
    write_wav(&folder.join("FR.wav"), 44_100, &stereo_impulse());
    let (from_headers, prepared) = both(&profile(BrirSource::Folder(folder)));
    assert_eq!(from_headers.to_string(), prepared.to_string());
    assert!(from_headers.to_string().contains("sample rate 44100"));

    let unknown = directory.join("unknown");
    fs::create_dir(&unknown).unwrap();
    write_wav(&unknown.join("Mystery.wav"), 48_000, &stereo_impulse());
    let (from_headers, prepared) = both(&profile(BrirSource::Folder(unknown)));
    assert!(matches!(from_headers, BrirError::UnknownDirection(ref name) if name == "Mystery"));
    assert_eq!(from_headers.to_string(), prepared.to_string());

    let duplicate = directory.join("duplicate");
    fs::create_dir(&duplicate).unwrap();
    write_wav(&duplicate.join("L.wav"), 48_000, &stereo_impulse());
    write_wav(&duplicate.join("FL.wav"), 48_000, &stereo_impulse());
    let (from_headers, prepared) = both(&profile(BrirSource::Folder(duplicate)));
    assert!(matches!(from_headers, BrirError::DuplicateDirection { .. }));
    assert!(matches!(prepared, BrirError::DuplicateDirection { .. }));

    let wav = directory.join("mapped.wav");
    write_wav(&wav, 48_000, &impulse_tracks(4, 16));
    let map = directory.join("missing-track.csv");
    fs::write(&map, channels_map(&["FL", "FR", "FC"])).unwrap();
    let (from_headers, prepared) = both(&profile(BrirSource::Mapped { wav, map }));
    assert_eq!(from_headers.to_string(), prepared.to_string());
    assert!(matches!(from_headers, BrirError::Sidecar { line: 4, .. }));

    let eight_bit = directory.join("hesuvi-8.wav");
    write_wav_as(
        &eight_bit,
        48_000,
        &impulse_tracks(14, 32),
        8,
        SampleFormat::Int,
    );
    let (from_headers, prepared) = both(&profile(BrirSource::Hesuvi(eight_bit)));
    assert_eq!(from_headers.to_string(), prepared.to_string());
    assert!(from_headers.to_string().contains("unsupported 8-bit"));

    let text = directory.join("hrir.wav");
    fs::write(&text, "not a wav").unwrap();
    let (from_headers, prepared) = both(&profile(BrirSource::Hrir(text)));
    assert_eq!(from_headers.to_string(), prepared.to_string());

    let missing = profile(BrirSource::Hesuvi(directory.join("absent.wav")));
    let (from_headers, prepared) = both(&missing);
    assert_eq!(from_headers.to_string(), prepared.to_string());

    let empty_folder = directory.join("empty");
    fs::create_dir(&empty_folder).unwrap();
    let (from_headers, _) = both(&profile(BrirSource::Folder(empty_folder)));
    assert!(matches!(
        from_headers,
        BrirError::Layout(LayoutError::Empty)
    ));
}

#[test]
fn headphone_fir_check_accepts_one_or_two_readable_tracks() {
    let directory = TestDirectory::new("fir");
    let mono = directory.join("mono.wav");
    write_wav(&mono, 44_100, &[vec![1.0, 0.5]]);
    let stereo = directory.join("stereo.wav");
    write_wav_as(&stereo, 48_000, &stereo_impulse(), 16, SampleFormat::Int);
    check_headphone_fir(&mono).unwrap();
    check_headphone_fir(&stereo).unwrap();

    // What passes the check is what prepare folds.
    let set = directory.join("FL.wav");
    write_wav(&set, 48_000, &stereo_impulse());
    for fir in [mono, stereo] {
        let mut input = profile(BrirSource::Directions(vec![DirectionFile {
            name: "FL".into(),
            azimuth_deg: 30.0,
            elevation_deg: 0.0,
            path: set.clone(),
        }]));
        input.headphone_fir = Some(fir);
        prepare(&input, 48_000, 64).unwrap();
    }
}

#[test]
fn headphone_fir_check_rejects_measurements_and_unreadable_files() {
    let directory = TestDirectory::new("fir-bad");

    let measurement = directory.join("Headphones_measurement_LR.wav");
    write_wav(&measurement, 48_000, &stereo_impulse());
    assert!(matches!(
        check_headphone_fir(&measurement),
        Err(BrirError::MeasurementFir { .. })
    ));

    let three = directory.join("three.wav");
    write_wav(&three, 48_000, &impulse_tracks(3, 8));
    let error = check_headphone_fir(&three).unwrap_err();
    assert!(
        error.to_string().contains("one or two tracks, got 3"),
        "{error}"
    );

    let text = directory.join("text.wav");
    fs::write(&text, "RIFF but not really").unwrap();
    assert!(check_headphone_fir(&text).is_err());

    assert!(check_headphone_fir(directory.join("absent.wav")).is_err());

    let eight_bit = directory.join("eight.wav");
    write_wav_as(&eight_bit, 48_000, &[vec![0.5, 0.25]], 8, SampleFormat::Int);
    assert!(
        check_headphone_fir(&eight_bit)
            .unwrap_err()
            .to_string()
            .contains("unsupported 8-bit")
    );

    let truncated = directory.join("truncated.wav");
    write_wav(&truncated, 48_000, &[vec![0.5; 4_096], vec![0.25; 4_096]]);
    let bytes = fs::read(&truncated).unwrap();
    fs::write(&truncated, &bytes[..bytes.len() / 2]).unwrap();
    assert!(check_headphone_fir(&truncated).is_err());

    let empty = directory.join("empty.wav");
    write_wav_as(&empty, 48_000, &[Vec::new()], 32, SampleFormat::Float);
    assert!(
        check_headphone_fir(&empty)
            .unwrap_err()
            .to_string()
            .contains("no samples")
    );
}
