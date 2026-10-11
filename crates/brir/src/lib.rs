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

use std::path::{Path, PathBuf};
use std::sync::Arc;

use bio_types::{DirectionId, Layout};
use convolver::FilterBank;

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
    Layout(#[from] bio_types::LayoutError),
    #[error(transparent)]
    Convolver(#[from] convolver::ConvolverError),
    #[error("resampling failed: {0}")]
    Resample(String),
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
}

/// Standard angles (azimuth, elevation in degrees; azimuth positive to the
/// left) of a speaker name: FL 30, FR -30, FC 0, BL 150, BR -150, SL 90,
/// SR -90, WL 60, WR -60, TFL (45, 45), TFR (-45, 45), TSL (90, 45),
/// TSR (-90, 45), TBL (135, 45), TBR (-135, 45). `None` for LFE and unknown
/// names.
pub fn standard_angles(name: &str) -> Option<(f32, f32)> {
    let _ = name;
    todo!("M1: brir worker")
}

/// Reads a profile TOML. Relative paths resolve against the file's directory.
///
/// ```toml
/// name = "demo"
/// gain_db = 0.0
/// limiter_ceiling_dbfs = -1.0
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
    let _ = path;
    todo!("M1: brir worker")
}

/// Prepares `profile` for scenes at `rate` and blocks that are multiples of
/// `partition`.
pub fn prepare(profile: &Profile, rate: u32, partition: usize) -> Result<PreparedBrir, BrirError> {
    let _ = (profile, rate, partition);
    todo!("M1: brir worker")
}
