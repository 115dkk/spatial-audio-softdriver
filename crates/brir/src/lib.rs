#![forbid(unsafe_code)]

//! Profile to [`PreparedBrir`]: the deep module that turns files on disk into
//! what the real-time thread uses directly.
//!
//! `prepare` reads the BRIR set (Impulcifer `hesuvi.wav` or `hrir.wav`, or one
//! stereo WAV per direction for custom sets such as the 21-direction D49 set),
//! names each direction and gives it its angles, folds the headphone FIR into
//! every BRIR (both are linear and time invariant, so the result is the same as
//! filtering afterwards; ADR 0016), resamples to the scene rate, and computes
//! the partitioned spectra. It runs on the control thread; hundreds of
//! milliseconds do not interrupt sound. One adapter, so no trait.
//!
//! The direction order of the resulting layout is the feed order every other
//! crate uses (ADR 0009).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use convolver::FilterBank;
use realfft::RealFftPlanner;
use rubato::audioadapter::Adapter;
use rubato::audioadapter_buffers::owned::InterleavedOwned;
use rubato::{Fft, FixedSync, Resampler, WindowFunction};
use serde::Deserialize;
use ssr_types::{Direction, DirectionId, Layout};

const HESUVI_TRACK_ORDER: [&str; 30] = [
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

const HRIR_TRACK_ORDER: [&str; 32] = [
    "FL-left",
    "FL-right",
    "FR-left",
    "FR-right",
    "FC-left",
    "FC-right",
    "LFE-left",
    "LFE-right",
    "BL-left",
    "BL-right",
    "BR-left",
    "BR-right",
    "SL-left",
    "SL-right",
    "SR-left",
    "SR-right",
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

/// Where a profile's BRIRs come from.
#[derive(Clone, Debug, PartialEq)]
pub enum BrirSource {
    /// Impulcifer `hesuvi.wav`: HeSuVi track order, 14 tracks or more.
    Hesuvi(PathBuf),
    /// Impulcifer `hrir.wav`: hexadecagonal order (FL FR FC LFE BL BR SL SR,
    /// then the extended speakers), left ear first for every speaker.
    Hrir(PathBuf),
    /// One stereo WAV (left ear, right ear) per direction, angles explicit.
    Directions(Vec<DirectionFile>),
    /// One multichannel WAV plus a sidecar CSV naming each direction's left and right tracks.
    Mapped { wav: PathBuf, map: PathBuf },
    /// A folder of stereo WAVs named `<DIRECTION>.wav`, left ear first.
    Folder(PathBuf),
}

/// Nominal angles for sources that carry names but no angles.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum AnglePreset {
    /// `standard_angles` (ITU-style 7.1.4 with SL/SR at ±90), extended with
    /// HFL/HFR 30/30, HFC 0/30, HBL/HBR ±110/30, TC 0/90.
    #[default]
    Standard,
    /// The community's 21-direction template (heights at 35°).
    Template21,
    /// The 21-direction room as its users describe it (SL/SR ±120, tops at 45°).
    Room21,
}

/// A common cut applied identically to every track and both ears after
/// resampling and folding.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Truncation {
    pub length_ms: f32,
    pub fade_ms: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DirectionFile {
    pub name: String,
    pub azimuth_deg: f32,
    pub elevation_deg: f32,
    pub path: PathBuf,
}

#[derive(Clone, Debug, PartialEq)]
pub enum LfePolicy {
    Mute,
    /// Feed LFE into this direction's BRIR at this gain (the build logs used
    /// FC at 0 dB).
    Direction {
        name: String,
        gain_db: f32,
    },
}

/// A BRIR set, headphone FIR, gain and LFE policy (CONTEXT.md: 프로필). The
/// output device selector joins in M4.
#[derive(Clone, Debug, PartialEq)]
pub struct Profile {
    pub name: String,
    pub brir: BrirSource,
    /// Mono (both ears) or stereo (left, right) FIR WAV.
    pub headphone_fir: Option<PathBuf>,
    /// Applied by the engine after the convolver, before the limiter.
    pub gain_db: f32,
    pub limiter_ceiling_dbfs: f32,
    pub lfe: LfePolicy,
    pub angles: AnglePreset,
    /// Per-direction (canonical name, azimuth_deg, elevation_deg) overriding the preset.
    pub angle_overrides: Vec<(String, f32, f32)>,
    pub truncate: Option<Truncation>,
    /// Remove delay common to every direction and ear, retaining 1 ms of margin.
    /// Profile TOML defaults this to `true`; set `trim_leading_delay = false` for
    /// the previous bit-identical prepared IRs.
    pub trim_leading_delay: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum BrirError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{path}: not a readable WAV: {detail}")]
    Wav { path: PathBuf, detail: String },
    #[error("profile {path}: {detail}")]
    Profile { path: PathBuf, detail: String },
    #[error("{path} has {tracks} tracks, which matches no {format} layout")]
    TrackCount {
        path: PathBuf,
        tracks: usize,
        format: &'static str,
    },
    #[error("direction {0} has no standard angles; give them in the profile")]
    UnknownDirection(String),
    #[error("the LFE policy names {0}, which the layout lacks")]
    BadLfeDirection(String),
    #[error(transparent)]
    Layout(#[from] ssr_types::LayoutError),
    #[error(transparent)]
    Convolver(#[from] convolver::ConvolverError),
    #[error("resampling failed: {0}")]
    Resample(String),
    #[error("sidecar {path}, line {line}: {detail}")]
    Sidecar {
        path: PathBuf,
        line: usize,
        detail: String,
    },
    #[error("direction {name} appears more than once ({path}, line {line})")]
    DuplicateDirection {
        name: String,
        path: PathBuf,
        line: usize,
    },
    #[error("{path} is a raw measurement, not a headphone FIR")]
    MeasurementFir { path: PathBuf },
    #[error("invalid truncation: {detail}")]
    Truncation { detail: String },
}

/// Everything the real-time chain needs from a profile, at one scene rate and
/// partition size.
#[derive(Debug)]
pub struct PreparedBrir {
    pub layout: Layout,
    pub bank: Arc<FilterBank>,
    pub rate: u32,
    pub partition: usize,
    /// LFE route as (direction, linear gain).
    pub lfe: Option<(DirectionId, f32)>,
    /// Linear output gain (from `gain_db`).
    pub gain: f32,
    /// Linear limiter ceiling (from `limiter_ceiling_dbfs`).
    pub limiter_ceiling: f32,
    /// Length in frames of the longest filter after folding and resampling.
    pub ir_frames: usize,
    /// Direction names omitted because both ears were exactly silent.
    pub omitted_silent_directions: Vec<String>,
    /// Frames removed equally from the front of every direction and ear.
    pub trimmed_leading_frames: usize,
}

const CANONICAL_ORDER: [&str; 21] = [
    "FL", "FR", "FC", "SL", "SR", "BL", "BR", "WL", "WR", "HFL", "HFR", "HFC", "HBL", "HBR", "TFL",
    "TFR", "TSL", "TSR", "TBL", "TBR", "TC",
];

/// Canonical spelling of a direction name, or `None` for LFE, repeat
/// measurements, and names this crate does not know.
pub fn canonical_name(name: &str) -> Option<&'static str> {
    let name = name.trim();
    if is_repeat_name(name) {
        return None;
    }
    match name.to_ascii_uppercase().as_str() {
        "FL" => Some("FL"),
        "FR" => Some("FR"),
        "FC" | "C" => Some("FC"),
        "SL" | "LS" => Some("SL"),
        "SR" | "RS" => Some("SR"),
        "BL" | "LB" => Some("BL"),
        "BR" | "RB" => Some("BR"),
        "WL" | "FWL" => Some("WL"),
        "WR" | "FWR" => Some("WR"),
        "HFL" | "LFH" => Some("HFL"),
        "HFR" | "RFH" => Some("HFR"),
        "HFC" | "CFH" => Some("HFC"),
        "HBL" | "HSL" | "LBH" => Some("HBL"),
        "HBR" | "HSR" | "RBH" => Some("HBR"),
        "TFL" | "LFT" => Some("TFL"),
        "TFR" | "RFT" => Some("TFR"),
        "TSL" | "TML" | "LMT" => Some("TSL"),
        "TSR" | "TMR" | "RMT" => Some("TSR"),
        "TBL" => Some("TBL"),
        "TBR" => Some("TBR"),
        "TC" | "TOP" => Some("TC"),
        _ => None,
    }
}

/// Angles of a canonical name under a nominal preset.
pub fn preset_angles(preset: AnglePreset, canonical: &str) -> Option<(f32, f32)> {
    let horizontal = match canonical {
        "FL" => Some((30.0, 0.0)),
        "FR" => Some((-30.0, 0.0)),
        "FC" => Some((0.0, 0.0)),
        "SL" => Some((
            if preset == AnglePreset::Room21 {
                120.0
            } else {
                90.0
            },
            0.0,
        )),
        "SR" => Some((
            if preset == AnglePreset::Room21 {
                -120.0
            } else {
                -90.0
            },
            0.0,
        )),
        "BL" => Some((150.0, 0.0)),
        "BR" => Some((-150.0, 0.0)),
        "WL" => Some((60.0, 0.0)),
        "WR" => Some((-60.0, 0.0)),
        _ => None,
    };
    if horizontal.is_some() {
        return horizontal;
    }

    let (height, top_height) = match preset {
        AnglePreset::Standard => (30.0, 45.0),
        AnglePreset::Template21 => (35.0, 35.0),
        AnglePreset::Room21 => (30.0, 45.0),
    };
    match canonical {
        "HFL" => Some((30.0, height)),
        "HFR" => Some((-30.0, height)),
        "HFC" => Some((0.0, height)),
        "HBL" => Some((110.0, height)),
        "HBR" => Some((-110.0, height)),
        "TFL" => Some((45.0, top_height)),
        "TFR" => Some((-45.0, top_height)),
        "TSL" => Some((90.0, top_height)),
        "TSR" => Some((-90.0, top_height)),
        "TBL" => Some((135.0, top_height)),
        "TBR" => Some((-135.0, top_height)),
        "TC" => Some((0.0, 90.0)),
        _ => None,
    }
}

/// Standard angles (azimuth, elevation in degrees; azimuth positive to the
/// left) of a speaker name: FL 30, FR -30, FC 0, BL 150, BR -150, SL 90,
/// SR -90, WL 60, WR -60, TFL (45, 45), TFR (-45, 45), TSL (90, 45),
/// TSR (-90, 45), TBL (135, 45), TBR (-135, 45). `None` for LFE and unknown
/// names.
pub fn standard_angles(name: &str) -> Option<(f32, f32)> {
    match name {
        "FL" | "FR" | "FC" | "BL" | "BR" | "SL" | "SR" | "WL" | "WR" | "TFL" | "TFR" | "TSL"
        | "TSR" | "TBL" | "TBR" => preset_angles(AnglePreset::Standard, name),
        _ => None,
    }
}

#[derive(Deserialize)]
struct ProfileToml {
    name: String,
    #[serde(default)]
    gain_db: f32,
    limiter_ceiling_dbfs: f32,
    #[serde(default)]
    angles: AnglePresetToml,
    truncate_ms: Option<f32>,
    fade_ms: Option<f32>,
    #[serde(default = "default_true")]
    trim_leading_delay: bool,
    #[serde(default)]
    angle_overrides: BTreeMap<String, [f32; 2]>,
    brir: BrirToml,
    lfe: Option<LfeToml>,
}

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum AnglePresetToml {
    #[default]
    Standard,
    Template21,
    Room21,
}

impl From<AnglePresetToml> for AnglePreset {
    fn from(value: AnglePresetToml) -> Self {
        match value {
            AnglePresetToml::Standard => Self::Standard,
            AnglePresetToml::Template21 => Self::Template21,
            AnglePresetToml::Room21 => Self::Room21,
        }
    }
}

const fn default_true() -> bool {
    true
}

#[derive(Deserialize)]
struct BrirToml {
    hesuvi: Option<PathBuf>,
    hrir: Option<PathBuf>,
    directions: Option<Vec<DirectionToml>>,
    wav: Option<PathBuf>,
    map: Option<PathBuf>,
    folder: Option<PathBuf>,
    headphone_fir: Option<PathBuf>,
}

#[derive(Deserialize)]
struct DirectionToml {
    name: String,
    #[serde(alias = "azimuth_deg")]
    azimuth: f32,
    #[serde(alias = "elevation_deg")]
    elevation: f32,
    #[serde(alias = "path")]
    file: PathBuf,
}

#[derive(Deserialize)]
struct LfeToml {
    direction: String,
    #[serde(default)]
    gain_db: f32,
}

/// Reads a profile TOML. Relative paths resolve against the file's directory.
///
/// ```toml
/// name = "demo"
/// gain_db = 0.0
/// limiter_ceiling_dbfs = -1.0
/// trim_leading_delay = true       # default; retain 1 ms before the earliest onset
///
/// [brir]
/// hesuvi = "hesuvi.wav"          # or: hrir = "hrir.wav"
/// # or: directions = [{ name = "FL", azimuth = 30, elevation = 0, file = "FL.wav" }, ...]
/// headphone_fir = "headphones.wav" # optional
///
/// [lfe]
/// direction = "FC"                 # omit the table to mute LFE
/// gain_db = 0.0
/// ```
pub fn load_profile(path: &Path) -> Result<Profile, BrirError> {
    let contents = fs::read_to_string(path).map_err(|source| BrirError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let parsed: ProfileToml = toml::from_str(&contents).map_err(|error| BrirError::Profile {
        path: path.to_path_buf(),
        detail: error.to_string(),
    })?;
    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    let mapped = parsed.brir.wav.is_some() || parsed.brir.map.is_some();
    if parsed.brir.wav.is_some() != parsed.brir.map.is_some() {
        return Err(BrirError::Profile {
            path: path.to_path_buf(),
            detail: "[brir] wav and map must be given together".into(),
        });
    }
    let sources = usize::from(parsed.brir.hesuvi.is_some())
        + usize::from(parsed.brir.hrir.is_some())
        + usize::from(parsed.brir.directions.is_some())
        + usize::from(mapped)
        + usize::from(parsed.brir.folder.is_some());
    if sources != 1 {
        return Err(BrirError::Profile {
            path: path.to_path_buf(),
            detail:
                "[brir] must contain exactly one of hesuvi, hrir, directions, wav+map, or folder"
                    .into(),
        });
    }

    let brir = if let Some(source) = parsed.brir.hesuvi {
        BrirSource::Hesuvi(resolve_path(directory, source))
    } else if let Some(source) = parsed.brir.hrir {
        BrirSource::Hrir(resolve_path(directory, source))
    } else if let Some(directions) = parsed.brir.directions {
        BrirSource::Directions(
            directions
                .into_iter()
                .map(|direction| DirectionFile {
                    name: direction.name,
                    azimuth_deg: direction.azimuth,
                    elevation_deg: direction.elevation,
                    path: resolve_path(directory, direction.file),
                })
                .collect(),
        )
    } else if let (Some(wav), Some(map)) = (parsed.brir.wav, parsed.brir.map) {
        BrirSource::Mapped {
            wav: resolve_path(directory, wav),
            map: resolve_path(directory, map),
        }
    } else {
        BrirSource::Folder(resolve_path(
            directory,
            parsed
                .brir
                .folder
                .expect("exactly one BRIR source was checked"),
        ))
    };

    let truncate = match (parsed.truncate_ms, parsed.fade_ms) {
        (Some(length_ms), fade_ms) => Some(Truncation {
            length_ms,
            fade_ms: fade_ms.unwrap_or(50.0),
        }),
        (None, Some(_)) => {
            return Err(BrirError::Profile {
                path: path.to_path_buf(),
                detail: "fade_ms requires truncate_ms".into(),
            });
        }
        (None, None) => None,
    };
    let angle_overrides = parsed
        .angle_overrides
        .into_iter()
        .map(|(name, [azimuth, elevation])| (name, azimuth, elevation))
        .collect();

    Ok(Profile {
        name: parsed.name,
        brir,
        headphone_fir: parsed
            .brir
            .headphone_fir
            .map(|path| resolve_path(directory, path)),
        gain_db: parsed.gain_db,
        limiter_ceiling_dbfs: parsed.limiter_ceiling_dbfs,
        lfe: parsed
            .lfe
            .map_or(LfePolicy::Mute, |lfe| LfePolicy::Direction {
                name: lfe.direction,
                gain_db: lfe.gain_db,
            }),
        angles: parsed.angles.into(),
        angle_overrides,
        truncate,
        trim_leading_delay: parsed.trim_leading_delay,
    })
}

/// Prepares `profile` for scenes at `rate` and blocks that are multiples of
/// `partition`.
pub fn prepare(profile: &Profile, rate: u32, partition: usize) -> Result<PreparedBrir, BrirError> {
    validate_truncation(profile.truncate)?;
    if let Some(path) = &profile.headphone_fir
        && is_measurement_file(path)
    {
        return Err(BrirError::MeasurementFir { path: path.clone() });
    }
    let mut loaded = load_brir_source(&profile.brir, profile)?;

    if loaded.rate != rate {
        let mut flat = Vec::with_capacity(loaded.irs.len() * 2);
        for ears in loaded.irs {
            flat.push(ears[0].clone());
            flat.push(ears[1].clone());
        }
        let resampled = resample_channels(&flat, loaded.rate, rate)?;
        loaded.irs = resampled
            .as_chunks::<2>()
            .0
            .iter()
            .map(|ears| [ears[0].clone(), ears[1].clone()])
            .collect();
        loaded.rate = rate;
    }

    if let Some(path) = &profile.headphone_fir {
        let headphone = read_wav(path)?;
        if headphone.channels.len() != 1 && headphone.channels.len() != 2 {
            return Err(BrirError::Wav {
                path: path.clone(),
                detail: format!(
                    "headphone FIR needs one or two tracks, got {}",
                    headphone.channels.len()
                ),
            });
        }
        let headphone_channels = if headphone.rate == rate {
            headphone.channels
        } else {
            resample_channels(&headphone.channels, headphone.rate, rate)?
        };
        for ears in &mut loaded.irs {
            ears[0] = convolve(&ears[0], &headphone_channels[0]);
            let right_fir = if headphone_channels.len() == 1 {
                &headphone_channels[0]
            } else {
                &headphone_channels[1]
            };
            ears[1] = convolve(&ears[1], right_fir);
        }
    }

    let trimmed_leading_frames = if profile.trim_leading_delay {
        trim_common_leading_delay(&mut loaded.irs, rate)
    } else {
        0
    };

    if let Some(truncation) = profile.truncate {
        truncate_irs(&mut loaded.irs, rate, truncation);
    }

    let layout = Layout::new(loaded.directions)?;
    let lfe = match &profile.lfe {
        LfePolicy::Mute => None,
        LfePolicy::Direction { name, gain_db } => Some((
            layout
                .find(name)
                .ok_or_else(|| BrirError::BadLfeDirection(name.clone()))?,
            db_to_linear(*gain_db),
        )),
    };
    let ir_frames = loaded
        .irs
        .iter()
        .flat_map(|ears| ears.iter())
        .map(Vec::len)
        .max()
        .expect("a valid layout has at least one IR");
    let borrowed: Vec<[&[f32]; 2]> = loaded
        .irs
        .iter()
        .map(|ears| [ears[0].as_slice(), ears[1].as_slice()])
        .collect();
    let bank = Arc::new(FilterBank::new(&borrowed, partition)?);

    Ok(PreparedBrir {
        layout,
        bank,
        rate,
        partition,
        lfe,
        gain: db_to_linear(profile.gain_db),
        limiter_ceiling: db_to_linear(profile.limiter_ceiling_dbfs),
        ir_frames,
        omitted_silent_directions: loaded.omitted_silent_directions,
        trimmed_leading_frames,
    })
}

fn resolve_path(directory: &Path, path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        directory.join(path)
    }
}

fn db_to_linear(db: f32) -> f32 {
    10.0_f32.powf(db / 20.0)
}

struct LoadedBrir {
    rate: u32,
    directions: Vec<Direction>,
    irs: Vec<[Vec<f32>; 2]>,
    omitted_silent_directions: Vec<String>,
}

impl LoadedBrir {
    fn omit_silent_directions(&mut self) {
        let mut kept_directions = Vec::with_capacity(self.directions.len());
        let mut kept_irs = Vec::with_capacity(self.irs.len());
        for (direction, ears) in self.directions.drain(..).zip(self.irs.drain(..)) {
            if is_silent(&ears[0]) && is_silent(&ears[1]) {
                self.omitted_silent_directions.push(direction.name);
            } else {
                kept_directions.push(direction);
                kept_irs.push(ears);
            }
        }
        self.directions = kept_directions;
        self.irs = kept_irs;
    }
}

fn load_brir_source(source: &BrirSource, profile: &Profile) -> Result<LoadedBrir, BrirError> {
    let mut loaded = match source {
        BrirSource::Hesuvi(path) => {
            load_ordered_wav(path, &HESUVI_TRACK_ORDER, 14, "HeSuVi", profile)
        }
        BrirSource::Hrir(path) => load_ordered_wav(path, &HRIR_TRACK_ORDER, 16, "hrir", profile),
        BrirSource::Directions(files) => load_direction_files(files),
        BrirSource::Mapped { wav, map } => load_mapped_wav(wav, map, profile),
        BrirSource::Folder(path) => load_folder(path, profile),
    }?;
    loaded.omit_silent_directions();
    if loaded.directions.is_empty() {
        return Err(BrirError::Layout(ssr_types::LayoutError::Empty));
    }
    Ok(loaded)
}

fn load_ordered_wav(
    path: &Path,
    order: &[&str],
    minimum_tracks: usize,
    format: &'static str,
    profile: &Profile,
) -> Result<LoadedBrir, BrirError> {
    let wav = read_wav(path)?;
    let tracks = wav.channels.len();
    if tracks < minimum_tracks || tracks > order.len() || !tracks.is_multiple_of(2) {
        return Err(BrirError::TrackCount {
            path: path.to_path_buf(),
            tracks,
            format,
        });
    }

    let mut by_name: HashMap<&str, [Option<Vec<f32>>; 2]> = HashMap::new();
    let mut direction_order = Vec::new();
    for (track_name, samples) in order[..tracks].iter().zip(wav.channels) {
        let (speaker, ear) = track_name
            .rsplit_once('-')
            .expect("constant track names contain an ear suffix");
        if speaker == "LFE" {
            continue;
        }
        let ear = match ear {
            "left" => 0,
            "right" => 1,
            _ => unreachable!("constant track names use left or right"),
        };
        if !by_name.contains_key(speaker) {
            direction_order.push(speaker);
            by_name.insert(speaker, [None, None]);
        }
        by_name.get_mut(speaker).expect("speaker was inserted")[ear] = Some(samples);
    }

    let mut directions = Vec::new();
    let mut irs = Vec::new();
    for name in direction_order {
        let ears = by_name
            .remove(name)
            .expect("speaker order came from the map");
        let [Some(left), Some(right)] = ears else {
            return Err(BrirError::TrackCount {
                path: path.to_path_buf(),
                tracks,
                format,
            });
        };
        let (azimuth, elevation) = angles_for(profile, name)?;
        directions.push(Direction::new(name, azimuth, elevation));
        irs.push([left, right]);
    }

    if directions.is_empty() {
        return Err(BrirError::TrackCount {
            path: path.to_path_buf(),
            tracks,
            format,
        });
    }
    Ok(LoadedBrir {
        rate: wav.rate,
        directions,
        irs,
        omitted_silent_directions: Vec::new(),
    })
}

fn angles_for(profile: &Profile, name: &str) -> Result<(f32, f32), BrirError> {
    let requested_canonical = canonical_name(name);
    if let Some((_, azimuth, elevation)) =
        profile
            .angle_overrides
            .iter()
            .find(|(override_name, _, _)| {
                override_name.eq_ignore_ascii_case(name)
                    || requested_canonical.is_some()
                        && canonical_name(override_name) == requested_canonical
            })
    {
        return Ok((*azimuth, *elevation));
    }
    preset_angles(profile.angles, name).ok_or_else(|| BrirError::UnknownDirection(name.to_owned()))
}

fn load_direction_files(files: &[DirectionFile]) -> Result<LoadedBrir, BrirError> {
    if files.is_empty() {
        return Ok(LoadedBrir {
            rate: 0,
            directions: Vec::new(),
            irs: Vec::new(),
            omitted_silent_directions: Vec::new(),
        });
    }
    let mut rate = None;
    let mut directions = Vec::with_capacity(files.len());
    let mut irs = Vec::with_capacity(files.len());
    for direction in files {
        let wav = read_wav(&direction.path)?;
        if wav.channels.len() != 2 {
            return Err(BrirError::Wav {
                path: direction.path.clone(),
                detail: format!(
                    "a direction WAV needs two tracks, got {}",
                    wav.channels.len()
                ),
            });
        }
        match rate {
            Some(expected) if expected != wav.rate => {
                return Err(BrirError::Wav {
                    path: direction.path.clone(),
                    detail: format!(
                        "sample rate {} differs from the first direction's {expected}",
                        wav.rate
                    ),
                });
            }
            None => rate = Some(wav.rate),
            _ => {}
        }
        directions.push(Direction::new(
            direction.name.clone(),
            direction.azimuth_deg,
            direction.elevation_deg,
        ));
        let mut channels = wav.channels;
        let right = channels.pop().expect("two channels were checked");
        let left = channels.pop().expect("two channels were checked");
        irs.push([left, right]);
    }
    Ok(LoadedBrir {
        rate: rate.expect("nonempty files set a rate"),
        directions,
        irs,
        omitted_silent_directions: Vec::new(),
    })
}

fn load_mapped_wav(
    wav_path: &Path,
    map_path: &Path,
    profile: &Profile,
) -> Result<LoadedBrir, BrirError> {
    let wav = read_wav(wav_path)?;
    let contents = fs::read_to_string(map_path).map_err(|source| BrirError::Io {
        path: map_path.to_path_buf(),
        source,
    })?;
    let mut lines = contents.lines();
    let header = lines
        .next()
        .ok_or_else(|| sidecar_error(map_path, 1, "missing header"))?;
    let columns: Vec<&str> = header
        .trim_start_matches('\u{feff}')
        .split(',')
        .map(str::trim)
        .collect();
    let shape = if columns == ["speaker", "BRIR_left_1based", "BRIR_right_1based"] {
        SidecarShape::Channels
    } else if columns == ["speaker", "file", "left_track", "right_track", "main_track"] {
        SidecarShape::ReaVerb
    } else {
        return Err(sidecar_error(map_path, 1, "unsupported header"));
    };

    let mut directions = Vec::new();
    let mut irs = Vec::new();
    let mut seen = HashSet::new();
    for (index, line) in lines.enumerate() {
        let line_number = index + 2;
        if line.trim().is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split(',').map(str::trim).collect();
        let expected_fields = match shape {
            SidecarShape::Channels => 3,
            SidecarShape::ReaVerb => 5,
        };
        if fields.len() != expected_fields {
            return Err(sidecar_error(
                map_path,
                line_number,
                &format!("expected {expected_fields} columns, got {}", fields.len()),
            ));
        }
        let raw_name = fields[0];
        if is_repeat_name(raw_name) {
            continue;
        }
        let name = canonical_name(raw_name).unwrap_or(raw_name);
        if !seen.insert(name.to_owned()) {
            return Err(BrirError::DuplicateDirection {
                name: name.to_owned(),
                path: map_path.to_path_buf(),
                line: line_number,
            });
        }
        let (left_field, right_field) = match shape {
            SidecarShape::Channels => (1, 2),
            SidecarShape::ReaVerb => (2, 3),
        };
        let left = parse_track(
            map_path,
            line_number,
            fields[left_field],
            wav.channels.len(),
        )?;
        let right = parse_track(
            map_path,
            line_number,
            fields[right_field],
            wav.channels.len(),
        )?;
        let (azimuth, elevation) = angles_for(profile, name)?;
        directions.push(Direction::new(name, azimuth, elevation));
        irs.push([wav.channels[left].clone(), wav.channels[right].clone()]);
    }
    Ok(LoadedBrir {
        rate: wav.rate,
        directions,
        irs,
        omitted_silent_directions: Vec::new(),
    })
}

#[derive(Clone, Copy)]
enum SidecarShape {
    Channels,
    ReaVerb,
}

fn sidecar_error(path: &Path, line: usize, detail: &str) -> BrirError {
    BrirError::Sidecar {
        path: path.to_path_buf(),
        line,
        detail: detail.to_owned(),
    }
}

fn parse_track(path: &Path, line: usize, value: &str, tracks: usize) -> Result<usize, BrirError> {
    let one_based: usize = value.parse().map_err(|_| {
        sidecar_error(
            path,
            line,
            &format!("track number {value:?} is not an integer"),
        )
    })?;
    if one_based == 0 || one_based > tracks {
        return Err(sidecar_error(
            path,
            line,
            &format!("track {one_based} does not exist in a {tracks}-track WAV"),
        ));
    }
    Ok(one_based - 1)
}

fn load_folder(path: &Path, profile: &Profile) -> Result<LoadedBrir, BrirError> {
    let entries = fs::read_dir(path).map_err(|source| BrirError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut files = Vec::new();
    let mut seen = HashSet::new();
    for entry in entries {
        let entry = entry.map_err(|source| BrirError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let file_path = entry.path();
        if !file_path.is_file()
            || !file_path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("wav"))
        {
            continue;
        }
        let raw_name = file_path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| BrirError::Wav {
                path: file_path.clone(),
                detail: "file name is not valid UTF-8".into(),
            })?;
        if is_repeat_name(raw_name) {
            continue;
        }
        let name = canonical_name(raw_name).unwrap_or(raw_name).to_owned();
        if !seen.insert(name.clone()) {
            return Err(BrirError::DuplicateDirection {
                name,
                path: file_path.clone(),
                line: 0,
            });
        }
        files.push((name, file_path));
    }
    files.sort_by(|(left_name, left_path), (right_name, right_path)| {
        folder_sort_key(left_name, left_path).cmp(&folder_sort_key(right_name, right_path))
    });

    let mut rate = None;
    let mut directions = Vec::with_capacity(files.len());
    let mut irs = Vec::with_capacity(files.len());
    for (name, file_path) in files {
        let wav = read_wav(&file_path)?;
        if wav.channels.len() != 2 {
            return Err(BrirError::Wav {
                path: file_path,
                detail: format!(
                    "a direction WAV needs two tracks, got {}",
                    wav.channels.len()
                ),
            });
        }
        match rate {
            Some(expected) if expected != wav.rate => {
                return Err(BrirError::Wav {
                    path: file_path,
                    detail: format!(
                        "sample rate {} differs from the first direction's {expected}",
                        wav.rate
                    ),
                });
            }
            None => rate = Some(wav.rate),
            _ => {}
        }
        let (azimuth, elevation) = angles_for(profile, &name)?;
        directions.push(Direction::new(name, azimuth, elevation));
        let mut channels = wav.channels;
        let right = channels.pop().expect("two channels were checked");
        let left = channels.pop().expect("two channels were checked");
        irs.push([left, right]);
    }
    Ok(LoadedBrir {
        rate: rate.unwrap_or(0),
        directions,
        irs,
        omitted_silent_directions: Vec::new(),
    })
}

fn folder_sort_key(name: &str, path: &Path) -> (usize, String) {
    let rank = CANONICAL_ORDER
        .iter()
        .position(|candidate| *candidate == name)
        .unwrap_or(CANONICAL_ORDER.len());
    let fallback = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    (rank, fallback)
}

fn is_repeat_name(name: &str) -> bool {
    name.trim().to_ascii_lowercase().ends_with("_repeat")
}

fn validate_truncation(truncation: Option<Truncation>) -> Result<(), BrirError> {
    let Some(truncation) = truncation else {
        return Ok(());
    };
    if !truncation.length_ms.is_finite()
        || !truncation.fade_ms.is_finite()
        || truncation.length_ms <= 0.0
        || truncation.fade_ms <= 0.0
        || truncation.fade_ms > truncation.length_ms
    {
        return Err(BrirError::Truncation {
            detail: "length_ms and fade_ms must be finite and positive, and fade_ms must not exceed length_ms".into(),
        });
    }
    Ok(())
}

fn truncate_irs(irs: &mut [[Vec<f32>; 2]], rate: u32, truncation: Truncation) {
    let keep =
        ((f64::from(rate) * f64::from(truncation.length_ms) / 1000.0).round() as usize).max(1);
    let fade = ((f64::from(rate) * f64::from(truncation.fade_ms) / 1000.0).round() as usize)
        .clamp(1, keep);
    let fade_start = keep - fade;
    for ears in irs {
        for samples in ears {
            samples.truncate(keep);
            for (frame, sample) in samples.iter_mut().enumerate().skip(fade_start) {
                let fade_index = frame - fade_start;
                let phase = if fade == 1 {
                    1.0
                } else {
                    fade_index as f32 / (fade - 1) as f32
                };
                let gain = 0.5 * (1.0 + (std::f32::consts::PI * phase).cos());
                *sample *= gain;
            }
        }
    }
}

fn is_measurement_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.to_ascii_lowercase().ends_with("_measurement_lr.wav"))
}

fn is_silent(samples: &[f32]) -> bool {
    samples.iter().all(|sample| *sample == 0.0)
}

struct WavData {
    rate: u32,
    channels: Vec<Vec<f32>>,
}

fn read_wav(path: &Path) -> Result<WavData, BrirError> {
    let mut reader = hound::WavReader::open(path).map_err(|error| map_wav_error(path, error))?;
    let spec = reader.spec();
    if spec.channels == 0 {
        return Err(BrirError::Wav {
            path: path.to_path_buf(),
            detail: "the file has no tracks".into(),
        });
    }
    let channels = usize::from(spec.channels);
    let frames = reader.duration() as usize;
    let mut deinterleaved = vec![Vec::with_capacity(frames); channels];
    match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Int, 16 | 24 | 32) => {
            let scale = 2.0_f32.powi(i32::from(spec.bits_per_sample) - 1);
            for (index, sample) in reader.samples::<i32>().enumerate() {
                let sample = sample.map_err(|error| map_wav_error(path, error))? as f32 / scale;
                deinterleaved[index % channels].push(sample);
            }
        }
        (hound::SampleFormat::Float, 32) => {
            for (index, sample) in reader.samples::<f32>().enumerate() {
                deinterleaved[index % channels]
                    .push(sample.map_err(|error| map_wav_error(path, error))?);
            }
        }
        (format, bits) => {
            return Err(BrirError::Wav {
                path: path.to_path_buf(),
                detail: format!("unsupported {bits}-bit {format:?} samples"),
            });
        }
    }
    Ok(WavData {
        rate: spec.sample_rate,
        channels: deinterleaved,
    })
}

fn map_wav_error(path: &Path, error: hound::Error) -> BrirError {
    match error {
        hound::Error::IoError(source) => BrirError::Io {
            path: path.to_path_buf(),
            source,
        },
        other => BrirError::Wav {
            path: path.to_path_buf(),
            detail: other.to_string(),
        },
    }
}

fn convolve(signal: &[f32], filter: &[f32]) -> Vec<f32> {
    debug_assert!(!signal.is_empty() && !filter.is_empty());
    let output_len = signal.len() + filter.len() - 1;
    let fft_len = output_len.next_power_of_two();
    let mut planner = RealFftPlanner::<f32>::new();
    let forward = planner.plan_fft_forward(fft_len);
    let inverse = planner.plan_fft_inverse(fft_len);
    let mut signal_time = forward.make_input_vec();
    let mut filter_time = forward.make_input_vec();
    signal_time[..signal.len()].copy_from_slice(signal);
    filter_time[..filter.len()].copy_from_slice(filter);
    let mut signal_spectrum = forward.make_output_vec();
    let mut filter_spectrum = forward.make_output_vec();
    forward
        .process(&mut signal_time, &mut signal_spectrum)
        .expect("planner-sized signal buffers are valid");
    forward
        .process(&mut filter_time, &mut filter_spectrum)
        .expect("planner-sized filter buffers are valid");
    for (signal_bin, filter_bin) in signal_spectrum.iter_mut().zip(filter_spectrum) {
        *signal_bin *= filter_bin;
    }
    let mut output = inverse.make_output_vec();
    inverse
        .process(&mut signal_spectrum, &mut output)
        .expect("planner-sized inverse buffers are valid");
    let scale = 1.0 / fft_len as f32;
    output.truncate(output_len);
    for sample in &mut output {
        *sample *= scale;
    }
    output
}

fn trim_common_leading_delay(irs: &mut [[Vec<f32>; 2]], rate: u32) -> usize {
    let earliest = irs
        .iter()
        .flat_map(|ears| ears.iter())
        .map(|samples| {
            let peak = samples
                .iter()
                .map(|sample| sample.abs())
                .fold(0.0, f32::max);
            let threshold = peak * 10.0_f32.powf(-60.0 / 20.0);
            samples
                .iter()
                .position(|sample| sample.abs() > threshold)
                .unwrap_or(0)
        })
        .min()
        .unwrap_or(0);
    let margin = (u64::from(rate) / 1_000) as usize;
    let trim = earliest.saturating_sub(margin);
    if trim != 0 {
        for samples in irs.iter_mut().flat_map(|ears| ears.iter_mut()) {
            samples.drain(..trim.min(samples.len()));
        }
    }
    trim
}

fn resample_channels(
    channels: &[Vec<f32>],
    input_rate: u32,
    output_rate: u32,
) -> Result<Vec<Vec<f32>>, BrirError> {
    if input_rate == output_rate {
        return Ok(channels.to_vec());
    }
    if channels.is_empty() || channels.iter().any(Vec::is_empty) {
        return Err(BrirError::Resample(
            "cannot resample an empty channel set or signal".into(),
        ));
    }
    let input_frames = channels.iter().map(Vec::len).max().unwrap_or(0);
    let channel_count = channels.len();
    let mut interleaved = vec![0.0; input_frames * channel_count];
    for (channel, samples) in channels.iter().enumerate() {
        for (frame, sample) in samples.iter().enumerate() {
            interleaved[frame * channel_count + channel] = *sample;
        }
    }
    let input = InterleavedOwned::new_from(interleaved, channel_count, input_frames)
        .map_err(|error| BrirError::Resample(error.to_string()))?;
    let chunk_size = input_frames.clamp(1, 1024);
    let mut resampler = Fft::<f32>::new_custom(
        input_rate as usize,
        output_rate as usize,
        chunk_size,
        1,
        channel_count,
        WindowFunction::BlackmanHarris2,
        FixedSync::Both,
    )
    .map_err(|error| BrirError::Resample(error.to_string()))?;
    // Fft is rubato's high-quality band-limited sinc resampler. process_all
    // repeatedly pumps it and removes output_delay() from the front, preserving
    // each impulse's time origin.
    let output = resampler
        .process_all(&input, input_frames, None)
        .map_err(|error| BrirError::Resample(error.to_string()))?;
    // The resampler keeps sample amplitude, but an impulse response sampled
    // at a higher rate has proportionally more taps: scaling by the rate ratio
    // keeps its frequency response (DC gain included) instead of its peak, so
    // a set measured at 96 kHz plays as loud as the same set at 48 kHz.
    let scale = input_rate as f32 / output_rate as f32;
    let mut deinterleaved = vec![Vec::with_capacity(output.frames()); channel_count];
    for frame in 0..output.frames() {
        for (channel, destination) in deinterleaved.iter_mut().enumerate() {
            destination.push(
                output
                    .read_sample(channel, frame)
                    .expect("channel and frame are within the output adapter")
                    * scale,
            );
        }
    }
    Ok(deinterleaved)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn direct_convolve(signal: &[f32], filter: &[f32]) -> Vec<f32> {
        let mut output = vec![0.0; signal.len() + filter.len() - 1];
        for (signal_index, signal_sample) in signal.iter().enumerate() {
            for (filter_index, filter_sample) in filter.iter().enumerate() {
                output[signal_index + filter_index] += signal_sample * filter_sample;
            }
        }
        output
    }

    #[test]
    fn fft_convolution_matches_direct_reference() {
        let cases = [(1, 1), (3, 7), (31, 257), (70_000, 3), (3, 70_000)];
        let mut state = 0x243f_6a88_u32;
        let mut maximum_ratio = 0.0_f32;
        for (signal_len, filter_len) in cases {
            let mut next = || {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (state as f32 / u32::MAX as f32 - 0.5) * 0.25
            };
            let signal: Vec<f32> = (0..signal_len).map(|_| next()).collect();
            let filter: Vec<f32> = (0..filter_len).map(|_| next()).collect();
            let actual = convolve(&signal, &filter);
            let expected = direct_convolve(&signal, &filter);
            let scale = signal.iter().map(|sample| sample.abs()).sum::<f32>()
                * filter.iter().map(|sample| sample.abs()).fold(0.0, f32::max);
            let tolerance = 1.0e-6 * scale.max(f32::MIN_POSITIVE);
            let maximum_error = actual
                .iter()
                .zip(expected)
                .map(|(actual, expected)| (actual - expected).abs())
                .fold(0.0, f32::max);
            maximum_ratio = maximum_ratio.max(maximum_error / scale.max(f32::MIN_POSITIVE));
            assert!(
                maximum_error <= tolerance,
                "{signal_len}x{filter_len}: error {maximum_error}, tolerance {tolerance}"
            );
        }
        eprintln!("maximum FFT convolution normalized error: {maximum_ratio:e}");
    }
}
