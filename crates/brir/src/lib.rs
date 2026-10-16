#![forbid(unsafe_code)]

//! Profile to [`PreparedBrir`]: the deep module that turns files on disk into
//! what the real-time thread uses directly.
//!
//! `prepare` reads the BRIR set (Impulcifer `hesuvi.wav` or `hrir.wav`, or one
//! stereo WAV per direction for custom sets such as the 21-direction D49 set),
//! names each direction and gives it its angles, resamples to the scene rate,
//! and computes
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
use serde::Deserialize;
use ssr_types::{Direction, DirectionId, Layout, Speaker};

pub mod ir;

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

/// A BRIR set, gain and LFE policy (CONTEXT.md: 프로필). The output device
/// selector joins in M4.
#[derive(Clone, Debug, PartialEq)]
pub struct Profile {
    pub name: String,
    pub brir: BrirSource,
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
    #[error(transparent)]
    Ir(#[from] ir::IrError),
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
    if let Some(speaker) = Speaker::from_name(name) {
        return match speaker {
            Speaker::L => Some("FL"),
            Speaker::R => Some("FR"),
            Speaker::C => Some("FC"),
            Speaker::Ls | Speaker::Lss => Some("SL"),
            Speaker::Rs | Speaker::Rss => Some("SR"),
            Speaker::Lrs => Some("BL"),
            Speaker::Rrs => Some("BR"),
            Speaker::Lw => Some("WL"),
            Speaker::Rw => Some("WR"),
            Speaker::Tfl => Some("TFL"),
            Speaker::Tfr => Some("TFR"),
            Speaker::Tsl => Some("TSL"),
            Speaker::Tsr => Some("TSR"),
            Speaker::Trl => Some("TBL"),
            Speaker::Trr => Some("TBR"),
            Speaker::Tc => Some("TC"),
            Speaker::Cs
            | Speaker::Lc
            | Speaker::Rc
            | Speaker::Tfc
            | Speaker::Trc
            | Speaker::Lfe
            | Speaker::Lfe2 => None,
        };
    }
    match name.to_ascii_uppercase().as_str() {
        "LB" => Some("BL"),
        "RB" => Some("BR"),
        "FWL" => Some("WL"),
        "FWR" => Some("WR"),
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
    /// Removed in ADR 0034. Retained only to reject old profile files clearly.
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
    if parsed.brir.headphone_fir.is_some() {
        return Err(BrirError::Profile {
            path: path.to_path_buf(),
            detail: "headphone_fir is no longer supported: headphone correction belongs in the BRIR set (Impulcifer); for room correction use a speaker profile's per-speaker fir (ADR 0034)".into(),
        });
    }
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

/// The feed layout [`prepare`] would build for `profile`, read from WAV
/// headers, the folder listing and the sidecar only: no sample is decoded,
/// so it is cheap enough for the control thread to call when a BRIR set is
/// imported.
///
/// It equals [`prepare`]'s layout (order, names, angles) for every set
/// [`prepare`] accepts, because [`prepare`] keeps every declared direction,
/// silent ones included (ADR 0016, 2026-10-15 amendment). Every check
/// [`prepare`] makes on the headers is made here too (track counts, sample
/// format, one sample rate per set, sidecar rows, duplicate directions,
/// unknown angles, an empty set); what only the samples can tell (a data
/// chunk shorter than its header says) is left to [`prepare`].
pub fn layout(profile: &Profile) -> Result<Layout, BrirError> {
    let directions = match &profile.brir {
        BrirSource::Hesuvi(path) => {
            ordered_layout(path, &HESUVI_TRACK_ORDER, 14, "HeSuVi", profile)?
        }
        BrirSource::Hrir(path) => ordered_layout(path, &HRIR_TRACK_ORDER, 16, "hrir", profile)?,
        BrirSource::Directions(files) => {
            let mut rate = SetRate::default();
            let mut directions = Vec::with_capacity(files.len());
            for file in files {
                let header = direction_header(&file.path)?;
                rate.check(&file.path, header.rate)?;
                directions.push(Direction::new(
                    file.name.clone(),
                    file.azimuth_deg,
                    file.elevation_deg,
                ));
            }
            directions
        }
        BrirSource::Mapped { wav, map } => mapped_layout(wav, map, profile)?,
        BrirSource::Folder(path) => folder_layout(path, profile)?,
    };
    Ok(Layout::new(directions)?)
}

/// The first sample rate of a set of direction files, which the rest must
/// share (as in [`load_direction_files`] and [`load_folder`]).
#[derive(Default)]
struct SetRate(Option<u32>);

impl SetRate {
    fn check(&mut self, path: &Path, rate: u32) -> Result<(), BrirError> {
        match self.0 {
            Some(expected) if expected != rate => Err(BrirError::Wav {
                path: path.to_path_buf(),
                detail: format!("sample rate {rate} differs from the first direction's {expected}"),
            }),
            Some(_) => Ok(()),
            None => {
                self.0 = Some(rate);
                Ok(())
            }
        }
    }
}

/// A direction file's header, which must have two tracks (left ear, right ear).
fn direction_header(path: &Path) -> Result<ir::WavHeader, BrirError> {
    let header = ir::read_wav_header(path)?;
    if header.tracks != 2 {
        return Err(BrirError::Wav {
            path: path.to_path_buf(),
            detail: format!("a direction WAV needs two tracks, got {}", header.tracks),
        });
    }
    Ok(header)
}

fn ordered_layout(
    path: &Path,
    order: &[&str],
    minimum_tracks: usize,
    format: &'static str,
    profile: &Profile,
) -> Result<Vec<Direction>, BrirError> {
    let tracks = ir::read_wav_header(path)?.tracks;
    if tracks < minimum_tracks || tracks > order.len() || !tracks.is_multiple_of(2) {
        return Err(BrirError::TrackCount {
            path: path.to_path_buf(),
            tracks,
            format,
        });
    }
    let mut names: Vec<&str> = Vec::new();
    for track in &order[..tracks] {
        let (speaker, _) = track
            .rsplit_once('-')
            .expect("constant track names contain an ear suffix");
        if speaker != "LFE" && !names.contains(&speaker) {
            names.push(speaker);
        }
    }
    names
        .into_iter()
        .map(|name| {
            let (azimuth, elevation) = angles_for(profile, name)?;
            Ok(Direction::new(name, azimuth, elevation))
        })
        .collect()
}

fn folder_layout(path: &Path, profile: &Profile) -> Result<Vec<Direction>, BrirError> {
    let mut rate = SetRate::default();
    folder_files(path)?
        .into_iter()
        .map(|(name, file_path)| {
            let header = direction_header(&file_path)?;
            rate.check(&file_path, header.rate)?;
            let (azimuth, elevation) = angles_for(profile, &name)?;
            Ok(Direction::new(name, azimuth, elevation))
        })
        .collect()
}

fn mapped_layout(wav: &Path, map: &Path, profile: &Profile) -> Result<Vec<Direction>, BrirError> {
    let tracks = ir::read_wav_header(wav)?.tracks;
    Ok(read_sidecar(map, tracks, profile)?
        .into_iter()
        .map(|row| row.direction)
        .collect())
}

/// Prepares `profile` for scenes at `rate` and blocks that are multiples of
/// `partition`.
pub fn prepare(profile: &Profile, rate: u32, partition: usize) -> Result<PreparedBrir, BrirError> {
    validate_truncation(profile.truncate)?;
    let mut loaded = load_brir_source(&profile.brir, profile)?;

    if loaded.rate != rate {
        let mut flat = Vec::with_capacity(loaded.irs.len() * 2);
        for ears in loaded.irs {
            flat.push(ears[0].clone());
            flat.push(ears[1].clone());
        }
        let resampled = ir::resample_channels(&flat, loaded.rate, rate)?;
        loaded.irs = resampled
            .as_chunks::<2>()
            .0
            .iter()
            .map(|ears| [ears[0].clone(), ears[1].clone()])
            .collect();
        loaded.rate = rate;
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
}

fn load_brir_source(source: &BrirSource, profile: &Profile) -> Result<LoadedBrir, BrirError> {
    // Every declared direction is kept, silent ones included, so the layout
    // is what the headers say (`layout`, ADR 0016's 2026-10-15 amendment).
    let loaded = match source {
        BrirSource::Hesuvi(path) => {
            load_ordered_wav(path, &HESUVI_TRACK_ORDER, 14, "HeSuVi", profile)
        }
        BrirSource::Hrir(path) => load_ordered_wav(path, &HRIR_TRACK_ORDER, 16, "hrir", profile),
        BrirSource::Directions(files) => load_direction_files(files),
        BrirSource::Mapped { wav, map } => load_mapped_wav(wav, map, profile),
        BrirSource::Folder(path) => load_folder(path, profile),
    }?;
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
    let wav = ir::read_wav_raw(path)?;
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
        });
    }
    let mut rate = None;
    let mut directions = Vec::with_capacity(files.len());
    let mut irs = Vec::with_capacity(files.len());
    for direction in files {
        let wav = ir::read_wav_raw(&direction.path)?;
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
    })
}

fn load_mapped_wav(
    wav_path: &Path,
    map_path: &Path,
    profile: &Profile,
) -> Result<LoadedBrir, BrirError> {
    let wav = ir::read_wav_raw(wav_path)?;
    let rows = read_sidecar(map_path, wav.channels.len(), profile)?;
    let mut directions = Vec::with_capacity(rows.len());
    let mut irs = Vec::with_capacity(rows.len());
    for row in rows {
        irs.push([
            wav.channels[row.left].clone(),
            wav.channels[row.right].clone(),
        ]);
        directions.push(row.direction);
    }
    Ok(LoadedBrir {
        rate: wav.rate,
        directions,
        irs,
    })
}

/// One sidecar row: the direction and its zero-based left and right tracks.
struct SidecarRow {
    direction: Direction,
    left: usize,
    right: usize,
}

/// Parses the sidecar of a `tracks`-track WAV in file order, skipping blank
/// lines and repeat measurements.
fn read_sidecar(
    map_path: &Path,
    tracks: usize,
    profile: &Profile,
) -> Result<Vec<SidecarRow>, BrirError> {
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

    let mut rows = Vec::new();
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
        let left = parse_track(map_path, line_number, fields[left_field], tracks)?;
        let right = parse_track(map_path, line_number, fields[right_field], tracks)?;
        let (azimuth, elevation) = angles_for(profile, name)?;
        rows.push(SidecarRow {
            direction: Direction::new(name, azimuth, elevation),
            left,
            right,
        });
    }
    Ok(rows)
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
    let files = folder_files(path)?;
    let mut rate = None;
    let mut directions = Vec::with_capacity(files.len());
    let mut irs = Vec::with_capacity(files.len());
    for (name, file_path) in files {
        let wav = ir::read_wav_raw(&file_path)?;
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
    })
}

/// The folder's direction WAVs as (canonical name, path) in feed order:
/// canonical directions first in [`CANONICAL_ORDER`], then other names by
/// lowercase file stem. Repeat measurements and non-WAV entries are skipped.
fn folder_files(path: &Path) -> Result<Vec<(String, PathBuf)>, BrirError> {
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
    Ok(files)
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

fn trim_common_leading_delay(irs: &mut [[Vec<f32>; 2]], rate: u32) -> usize {
    let earliest = irs
        .iter()
        .flat_map(|ears| ears.iter())
        .filter_map(|samples| {
            let peak = samples
                .iter()
                .map(|sample| sample.abs())
                .fold(0.0, f32::max);
            let threshold = peak * 10.0_f32.powf(-60.0 / 20.0);
            // A silent track has no onset and does not hold the trim back.
            samples.iter().position(|sample| sample.abs() > threshold)
        })
        .min()
        .unwrap_or(0);
    let margin = (u64::from(rate) / 1_000) as usize;
    let trim = earliest.saturating_sub(margin);
    if trim != 0 {
        for samples in irs.iter_mut().flat_map(|ears| ears.iter_mut()) {
            // An audible track starts after `trim`; a silent one shorter than
            // it keeps one (silent) sample, so no filter is left empty.
            samples.drain(..trim.min(samples.len().saturating_sub(1)));
        }
    }
    trim
}
