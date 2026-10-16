#![forbid(unsafe_code)]

//! Speaker profile loading and control-thread preparation for the real-time
//! speaker output stage (ADR 0030).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use convolver::FilterBank;
use serde::Deserialize;
use ssr_types::{Direction, Layout, Speaker, SpeakerLayout};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpeakerRange {
    Full,
    Small,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SpeakerConfig {
    pub name: String,
    pub azimuth_deg: f32,
    pub elevation_deg: f32,
    pub channel: usize,
    pub distance_m: Option<f32>,
    pub delay_ms: Option<f32>,
    pub trim_db: f32,
    pub range: SpeakerRange,
    pub crossover_hz: Option<f32>,
    pub fir: Option<PathBuf>,
}

/// One subwoofer. Managed bass and LFE are shared equally among them; each
/// keeps its own alignment, trim and room FIR.
#[derive(Clone, Debug, PartialEq)]
pub struct SubwooferConfig {
    pub channel: usize,
    pub distance_m: Option<f32>,
    pub delay_ms: Option<f32>,
    pub trim_db: f32,
    pub fir: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SpeakerProfile {
    pub name: String,
    pub device_channels: usize,
    pub channel_mask: u32,
    pub gain_db: f32,
    pub limiter_ceiling_dbfs: f32,
    pub crossover_hz: f32,
    /// Gain of the LFE channel where it is reproduced: the subwoofers, or the
    /// `bass_to` speakers when there are none. +10 dB is the in-band standard.
    pub lfe_gain_db: f32,
    pub speakers: Vec<SpeakerConfig>,
    pub subwoofers: Vec<SubwooferConfig>,
    /// Full-range speakers that reproduce managed bass and LFE when there is
    /// no subwoofer; empty means L and R, or every full-range speaker.
    pub bass_to: Vec<String>,
}

/// Normalised biquad coefficients. They and the filter state are `f64`: an
/// 80 Hz pole pair at 192 kHz sits within 0.3 % of z = 1, where `f32`
/// coefficients and state lose the response and add noise.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BiquadCoefficients {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CrossoverCoefficients {
    pub low: [BiquadCoefficients; 2],
    pub high: [BiquadCoefficients; 2],
}

#[derive(Debug)]
pub struct PreparedSpeaker {
    pub name: String,
    pub device_channel: usize,
    pub small: bool,
    pub crossover: Option<CrossoverCoefficients>,
    pub delay_frames: usize,
    pub gain: f32,
    /// A one-input, one-output room-correction FIR bank.
    pub fir: Option<Arc<FilterBank>>,
}

#[derive(Debug)]
pub struct PreparedSubwoofer {
    pub device_channel: usize,
    pub delay_frames: usize,
    pub gain: f32,
    /// A one-input, one-output room-correction FIR bank.
    pub fir: Option<Arc<FilterBank>>,
}

#[derive(Debug)]
pub struct PreparedSpeakers {
    pub layout: Layout,
    pub rate: u32,
    pub partition: usize,
    pub device_channels: usize,
    pub channel_mask: u32,
    pub speakers: Vec<PreparedSpeaker>,
    pub subwoofers: Vec<PreparedSubwoofer>,
    /// Direction indices of the full-range speakers that receive managed bass
    /// and LFE when there is no subwoofer.
    pub bass_to: Vec<usize>,
    pub lfe_gain: f32,
    pub gain: f32,
    pub limiter_ceiling: f32,
}

/// The most device channels a speaker output may have (the engine's crossing
/// ring holds this many).
pub const MAX_DEVICE_CHANNELS: usize = 32;
/// Alignment and level bounds. A value outside is a typo, not a room: 50 m is
/// a large hall, 500 ms is far beyond any acoustic path, and the FIR cap keeps
/// a misplaced file from taking the machine's memory.
const MAX_DISTANCE_M: f32 = 50.0;
const MAX_DELAY_MS: f32 = 500.0;
const LEVEL_DB: std::ops::RangeInclusive<f32> = -60.0..=20.0;
const LFE_GAIN_DB: std::ops::RangeInclusive<f32> = -40.0..=20.0;
const CEILING_DBFS: std::ops::RangeInclusive<f32> = -60.0..=0.0;
const MAX_FIR_SECONDS: f64 = 4.0;

#[derive(Debug, thiserror::Error)]
pub enum SpeakerError {
    #[error("{what} is {value}, outside {range}")]
    OutOfRange {
        what: String,
        value: f32,
        range: String,
    },
    #[error("{path}: room FIR is {frames} frames, more than {seconds} s at {rate} Hz")]
    FirTooLong {
        path: PathBuf,
        frames: usize,
        seconds: f64,
        rate: u32,
    },
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("profile {path}: {detail}")]
    Profile { path: PathBuf, detail: String },
    #[error("a speaker profile needs at least one speaker")]
    NoSpeakers,
    #[error("device channel {channel} is assigned more than once")]
    DuplicateChannel { channel: usize },
    #[error("device channel {channel} is outside the {device_channels}-channel device")]
    ChannelOutOfRange {
        channel: usize,
        device_channels: usize,
    },
    #[error("subwoofer channel {channel} overlaps a speaker channel")]
    SubwooferOverlap { channel: usize },
    #[error(
        "{name} is on device channel {channel}, but the channel mask puts it on channel {expected}"
    )]
    MaskOrder {
        name: String,
        channel: usize,
        expected: usize,
    },
    #[error("the channel mask puts LFE on device channel {channel}, which is not a subwoofer")]
    LfeNotSubwoofer { channel: usize },
    #[error("speaker {name} is on device channel {channel}, the channel mask's LFE")]
    SpeakerOnLfe { name: String, channel: usize },
    #[error("bass_to applies only without a subwoofer")]
    BassToWithSubwoofer,
    #[error("speaker {0} has no nominal angles; give azimuth and elevation")]
    MissingAngles(String),
    #[error("{name} gives both distance_m and delay_ms")]
    ConflictingAlignment { name: String },
    #[error("crossover {hz} Hz is outside 20..=250 Hz")]
    BadCrossover { hz: f32 },
    #[error("bass_to names speaker {0}, which the layout lacks or is not full-range")]
    BadBassTarget(String),
    #[error("small speakers or LFE need a subwoofer or at least one full-range bass target")]
    NoBassTarget,
    #[error(
        "the channel mask has {mask_channels} channels, but device_channels is {device_channels}"
    )]
    ChannelMaskCount {
        mask_channels: usize,
        device_channels: usize,
    },
    #[error("speaker layout cannot be panned: {0}")]
    Geometry(#[from] render::RenderError),
    #[error(transparent)]
    Layout(#[from] ssr_types::LayoutError),
    #[error(transparent)]
    Convolver(#[from] convolver::ConvolverError),
    #[error(transparent)]
    Ir(#[from] brir::ir::IrError),
}

#[derive(Deserialize)]
struct ProfileToml {
    kind: String,
    name: String,
    device_channels: usize,
    #[serde(default)]
    channel_mask: MaskToml,
    #[serde(default)]
    gain_db: f32,
    limiter_ceiling_dbfs: f32,
    #[serde(default = "default_crossover")]
    crossover_hz: f32,
    #[serde(default = "default_lfe_gain")]
    lfe_gain_db: f32,
    #[serde(rename = "speaker", default)]
    speakers: Vec<SpeakerToml>,
    #[serde(rename = "subwoofer", default)]
    subwoofers: Vec<SubwooferToml>,
    #[serde(default)]
    bass_to: Vec<String>,
}

#[derive(Deserialize)]
struct SpeakerToml {
    name: String,
    azimuth: Option<f32>,
    elevation: Option<f32>,
    channel: usize,
    distance_m: Option<f32>,
    delay_ms: Option<f32>,
    #[serde(default)]
    trim_db: f32,
    #[serde(default)]
    range: RangeToml,
    crossover_hz: Option<f32>,
    fir: Option<PathBuf>,
}

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum RangeToml {
    #[default]
    Full,
    Small,
}

#[derive(Deserialize)]
struct SubwooferToml {
    channel: usize,
    distance_m: Option<f32>,
    delay_ms: Option<f32>,
    #[serde(default)]
    trim_db: f32,
    fir: Option<PathBuf>,
}

/// `channel_mask`: a layout name (`"7.1.4"`), `"direct"` (mask 0), or a number
/// (TOML `0x2d63f`, or a string `"0x2d63f"` or `"185919"`).
#[derive(Deserialize)]
#[serde(untagged)]
enum MaskToml {
    Number(u32),
    Name(String),
}

impl Default for MaskToml {
    fn default() -> Self {
        Self::Number(0)
    }
}

const fn default_crossover() -> f32 {
    80.0
}

const fn default_lfe_gain() -> f32 {
    10.0
}

/// Reads and validates a `kind = "speakers"` profile. Relative FIR paths are
/// resolved against the profile file's directory.
pub fn load_profile(path: &Path) -> Result<SpeakerProfile, SpeakerError> {
    let contents = fs::read_to_string(path).map_err(|source| SpeakerError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let parsed: ProfileToml = toml::from_str(&contents).map_err(|error| SpeakerError::Profile {
        path: path.to_path_buf(),
        detail: error.to_string(),
    })?;
    if parsed.kind != "speakers" {
        return Err(SpeakerError::Profile {
            path: path.to_path_buf(),
            detail: "kind must be \"speakers\"".into(),
        });
    }
    let channel_mask = match &parsed.channel_mask {
        MaskToml::Number(mask) => *mask,
        MaskToml::Name(name) => {
            parse_channel_mask(name).map_err(|detail| SpeakerError::Profile {
                path: path.to_path_buf(),
                detail,
            })?
        }
    };
    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    let mut speakers = Vec::with_capacity(parsed.speakers.len());
    for speaker in parsed.speakers {
        if matches!(
            Speaker::from_name(&speaker.name),
            Some(Speaker::Lfe | Speaker::Lfe2)
        ) {
            return Err(SpeakerError::Profile {
                path: path.to_path_buf(),
                detail: format!(
                    "{} is LFE, not a positional main speaker; use the subwoofer section",
                    speaker.name
                ),
            });
        }
        let nominal = nominal_speaker(&speaker.name).and_then(Speaker::angles);
        let angles = match (speaker.azimuth, speaker.elevation, nominal) {
            (Some(azimuth), Some(elevation), _) => (azimuth, elevation),
            (None, None, Some(angles)) => angles,
            (Some(azimuth), None, Some((_, elevation))) => (azimuth, elevation),
            (None, Some(elevation), Some((azimuth, _))) => (azimuth, elevation),
            _ => return Err(SpeakerError::MissingAngles(speaker.name)),
        };
        speakers.push(SpeakerConfig {
            name: speaker.name,
            azimuth_deg: angles.0,
            elevation_deg: angles.1,
            channel: speaker.channel,
            distance_m: speaker.distance_m,
            delay_ms: speaker.delay_ms,
            trim_db: speaker.trim_db,
            range: match speaker.range {
                RangeToml::Full => SpeakerRange::Full,
                RangeToml::Small => SpeakerRange::Small,
            },
            crossover_hz: speaker.crossover_hz,
            fir: speaker.fir.map(|fir| resolve_path(directory, fir)),
        });
    }
    let profile = SpeakerProfile {
        name: parsed.name,
        device_channels: parsed.device_channels,
        channel_mask,
        gain_db: parsed.gain_db,
        limiter_ceiling_dbfs: parsed.limiter_ceiling_dbfs,
        crossover_hz: parsed.crossover_hz,
        lfe_gain_db: parsed.lfe_gain_db,
        speakers,
        subwoofers: parsed
            .subwoofers
            .into_iter()
            .map(|subwoofer| SubwooferConfig {
                channel: subwoofer.channel,
                distance_m: subwoofer.distance_m,
                delay_ms: subwoofer.delay_ms,
                trim_db: subwoofer.trim_db,
                fir: subwoofer.fir.map(|fir| resolve_path(directory, fir)),
            })
            .collect(),
        bass_to: parsed.bass_to,
    };
    validate(&profile)?;
    Ok(profile)
}

/// Prepares crossover coefficients, delays, gains and optional FIR banks.
///
/// Alignment: the farthest loudspeaker (speaker or subwoofer) plays first and
/// nearer ones wait `(farthest - distance) / 343 m/s`; `delay_ms` is used as
/// given. A loudspeaker with neither is not delayed.
pub fn prepare(
    profile: &SpeakerProfile,
    rate: u32,
    partition: usize,
) -> Result<PreparedSpeakers, SpeakerError> {
    let layout = validate(profile)?;
    let maximum_distance = profile
        .speakers
        .iter()
        .filter_map(|speaker| speaker.distance_m)
        .chain(
            profile
                .subwoofers
                .iter()
                .filter_map(|subwoofer| subwoofer.distance_m),
        )
        .fold(0.0_f32, f32::max);
    let mut fir_cache = HashMap::new();
    let mut speakers = Vec::with_capacity(profile.speakers.len());
    for speaker in &profile.speakers {
        let crossover_hz = speaker.crossover_hz.unwrap_or(profile.crossover_hz);
        let crossover = (speaker.range == SpeakerRange::Small)
            .then(|| crossover_coefficients(rate, crossover_hz));
        speakers.push(PreparedSpeaker {
            name: speaker.name.clone(),
            device_channel: speaker.channel,
            small: speaker.range == SpeakerRange::Small,
            crossover,
            delay_frames: alignment_frames(
                maximum_distance,
                speaker.distance_m,
                speaker.delay_ms,
                rate,
            ),
            gain: db_to_linear(speaker.trim_db),
            fir: prepare_fir(
                speaker.fir.as_deref(),
                speaker.channel,
                rate,
                partition,
                &mut fir_cache,
            )?,
        });
    }
    let mut subwoofers = Vec::with_capacity(profile.subwoofers.len());
    for subwoofer in &profile.subwoofers {
        subwoofers.push(PreparedSubwoofer {
            device_channel: subwoofer.channel,
            delay_frames: alignment_frames(
                maximum_distance,
                subwoofer.distance_m,
                subwoofer.delay_ms,
                rate,
            ),
            gain: db_to_linear(subwoofer.trim_db),
            fir: prepare_fir(
                subwoofer.fir.as_deref(),
                subwoofer.channel,
                rate,
                partition,
                &mut fir_cache,
            )?,
        });
    }
    let bass_to = if subwoofers.is_empty() {
        resolved_bass_targets(profile)
    } else {
        Vec::new()
    };
    Ok(PreparedSpeakers {
        layout,
        rate,
        partition,
        device_channels: profile.device_channels,
        channel_mask: profile.channel_mask,
        speakers,
        subwoofers,
        bass_to,
        lfe_gain: db_to_linear(profile.lfe_gain_db),
        gain: db_to_linear(profile.gain_db),
        limiter_ceiling: db_to_linear(profile.limiter_ceiling_dbfs),
    })
}

/// Checks that `path` is a room FIR `prepare` can use for the speaker on
/// zero-based `device_channel`: a readable WAV, the track rule, and at most
/// four seconds at the file's own rate.
pub fn check_room_fir(path: &Path, device_channel: usize) -> Result<(), SpeakerError> {
    let header = brir::ir::read_wav_header(path)?;
    let track = room_fir_track(path, header.tracks, device_channel)?;
    let wav = brir::ir::read_wav_raw(path)?;
    let samples = &wav.channels[track];
    check_fir_length(path, samples.len(), wav.rate)
}

/// The speaker profile of an app layout (ADR 0034): every speaker full
/// range at its nominal angles, the LFE channel a subwoofer, no alignment,
/// and the room FIRs `room_fir` names by speaker name ("LFE" for the
/// subwoofer); names the layout lacks are ignored.
pub fn layout_profile(
    layout: SpeakerLayout,
    room_fir: &BTreeMap<String, PathBuf>,
    gain_db: f32,
) -> SpeakerProfile {
    let output = layout.output_layout();
    let mut speakers = Vec::new();
    let mut subwoofers = Vec::new();
    for (channel, &speaker) in layout.speakers().iter().enumerate() {
        let name = speaker.name();
        if speaker == Speaker::Lfe {
            subwoofers.push(SubwooferConfig {
                channel,
                distance_m: None,
                delay_ms: None,
                trim_db: 0.0,
                fir: room_fir.get("LFE").cloned(),
            });
        } else {
            let (azimuth_deg, elevation_deg) = speaker
                .angles()
                .expect("an app layout's non-LFE speakers have nominal angles");
            speakers.push(SpeakerConfig {
                name: name.into(),
                azimuth_deg,
                elevation_deg,
                channel,
                distance_m: None,
                delay_ms: None,
                trim_db: 0.0,
                range: SpeakerRange::Full,
                crossover_hz: None,
                fir: room_fir.get(name).cloned(),
            });
        }
    }
    SpeakerProfile {
        name: layout.name().into(),
        device_channels: usize::from(output.channels),
        channel_mask: output.channel_mask,
        gain_db,
        limiter_ceiling_dbfs: -1.0,
        crossover_hz: 80.0,
        lfe_gain_db: 10.0,
        speakers,
        subwoofers,
        bass_to: Vec::new(),
    }
}

/// Loads one room FIR track at `rate` into a one-output convolver bank.
fn prepare_fir(
    path: Option<&Path>,
    device_channel: usize,
    rate: u32,
    partition: usize,
    cache: &mut HashMap<(PathBuf, usize), Arc<FilterBank>>,
) -> Result<Option<Arc<FilterBank>>, SpeakerError> {
    let Some(path) = path else {
        return Ok(None);
    };
    let header = brir::ir::read_wav_header(path)?;
    let track = room_fir_track(path, header.tracks, device_channel)?;
    let key = (path.to_path_buf(), track);
    if let Some(bank) = cache.get(&key) {
        return Ok(Some(Arc::clone(bank)));
    }
    let wav = brir::ir::read_wav(path, rate)?;
    let samples = &wav.channels[track];
    check_fir_length(path, samples.len(), rate)?;
    let filters = [samples.as_slice()];
    let bank = Arc::new(FilterBank::new_mono(&filters, partition)?);
    cache.insert(key, Arc::clone(&bank));
    Ok(Some(bank))
}

fn room_fir_track(
    path: &Path,
    tracks: usize,
    device_channel: usize,
) -> Result<usize, SpeakerError> {
    if tracks == 1 {
        Ok(0)
    } else if device_channel < tracks {
        Ok(device_channel)
    } else {
        Err(brir::ir::IrError::Wav {
            path: path.to_path_buf(),
            detail: format!(
                "room FIR has {tracks} tracks, none for device channel {device_channel}"
            ),
        }
        .into())
    }
}

fn check_fir_length(path: &Path, frames: usize, rate: u32) -> Result<(), SpeakerError> {
    if frames as f64 > MAX_FIR_SECONDS * f64::from(rate) {
        Err(SpeakerError::FirTooLong {
            path: path.to_path_buf(),
            frames,
            seconds: MAX_FIR_SECONDS,
            rate,
        })
    } else {
        Ok(())
    }
}

fn validate(profile: &SpeakerProfile) -> Result<Layout, SpeakerError> {
    if profile.speakers.is_empty() {
        return Err(SpeakerError::NoSpeakers);
    }
    if !(1..=MAX_DEVICE_CHANNELS).contains(&profile.device_channels) {
        return Err(SpeakerError::OutOfRange {
            what: "device_channels".into(),
            value: profile.device_channels as f32,
            range: format!("1..={MAX_DEVICE_CHANNELS}"),
        });
    }
    validate_crossover(profile.crossover_hz)?;
    within("gain_db", profile.gain_db, &LEVEL_DB)?;
    within("lfe_gain_db", profile.lfe_gain_db, &LFE_GAIN_DB)?;
    within(
        "limiter_ceiling_dbfs",
        profile.limiter_ceiling_dbfs,
        &CEILING_DBFS,
    )?;
    for speaker in &profile.speakers {
        let name = &speaker.name;
        within(
            &format!("{name} azimuth"),
            speaker.azimuth_deg,
            &(-360.0..=360.0),
        )?;
        within(
            &format!("{name} elevation"),
            speaker.elevation_deg,
            &(-90.0..=90.0),
        )?;
        within(&format!("{name} trim_db"), speaker.trim_db, &LEVEL_DB)?;
        validate_alignment(name, speaker.distance_m, speaker.delay_ms)?;
    }
    for subwoofer in &profile.subwoofers {
        within("subwoofer trim_db", subwoofer.trim_db, &LEVEL_DB)?;
        validate_alignment("subwoofer", subwoofer.distance_m, subwoofer.delay_ms)?;
    }
    let mut channels = HashSet::new();
    for speaker in &profile.speakers {
        if speaker.channel >= profile.device_channels {
            return Err(SpeakerError::ChannelOutOfRange {
                channel: speaker.channel,
                device_channels: profile.device_channels,
            });
        }
        if !channels.insert(speaker.channel) {
            return Err(SpeakerError::DuplicateChannel {
                channel: speaker.channel,
            });
        }
        if speaker.distance_m.is_some() && speaker.delay_ms.is_some() {
            return Err(SpeakerError::ConflictingAlignment {
                name: speaker.name.clone(),
            });
        }
        if let Some(hz) = speaker.crossover_hz {
            validate_crossover(hz)?;
        }
    }
    let speaker_channels = channels.clone();
    for subwoofer in &profile.subwoofers {
        if subwoofer.distance_m.is_some() && subwoofer.delay_ms.is_some() {
            return Err(SpeakerError::ConflictingAlignment {
                name: "subwoofer".into(),
            });
        }
        if subwoofer.channel >= profile.device_channels {
            return Err(SpeakerError::ChannelOutOfRange {
                channel: subwoofer.channel,
                device_channels: profile.device_channels,
            });
        }
        if speaker_channels.contains(&subwoofer.channel) {
            return Err(SpeakerError::SubwooferOverlap {
                channel: subwoofer.channel,
            });
        }
        if !channels.insert(subwoofer.channel) {
            return Err(SpeakerError::DuplicateChannel {
                channel: subwoofer.channel,
            });
        }
    }
    if profile.channel_mask != 0
        && profile.channel_mask.count_ones() as usize != profile.device_channels
    {
        return Err(SpeakerError::ChannelMaskCount {
            mask_channels: profile.channel_mask.count_ones() as usize,
            device_channels: profile.device_channels,
        });
    }
    validate_mask_order(profile)?;
    if !profile.subwoofers.is_empty() && !profile.bass_to.is_empty() {
        return Err(SpeakerError::BassToWithSubwoofer);
    }
    for target in &profile.bass_to {
        if !profile
            .speakers
            .iter()
            .any(|speaker| speaker.name == *target && speaker.range == SpeakerRange::Full)
        {
            return Err(SpeakerError::BadBassTarget(target.clone()));
        }
    }
    if profile.subwoofers.is_empty()
        && profile
            .speakers
            .iter()
            .any(|speaker| speaker.range == SpeakerRange::Small)
        && resolved_bass_targets(profile).is_empty()
    {
        return Err(SpeakerError::NoBassTarget);
    }
    let layout = Layout::new(
        profile
            .speakers
            .iter()
            .map(|speaker| {
                Direction::new(
                    speaker.name.clone(),
                    speaker.azimuth_deg,
                    speaker.elevation_deg,
                )
            })
            .collect(),
    )?;
    render::PanGeometry::new(&layout)?;
    Ok(layout)
}

/// With a channel mask, Windows (and a receiver behind HDMI) takes device
/// channel n as the mask's n-th speaker. A speaker named after a mask speaker
/// must sit on that speaker's channel, and no speaker may sit on the mask's
/// LFE channel (a receiver feeds it to the subwoofer input); with subwoofers,
/// one of them must. Otherwise the sound comes out of the wrong loudspeaker.
///
/// Not checked: names without a Windows bit (`Ls`, `Rs`, wides, custom
/// names), and a named speaker whose bit the mask lacks. The second is
/// deliberate: a 5.1 mask has either the back pair (0x3f) or the side pair
/// (0x60f), receivers play both on their surround outputs, and a direct DAC
/// ignores the names, so side speakers on a 0x3f mask still play from the
/// speakers the profile describes.
fn validate_mask_order(profile: &SpeakerProfile) -> Result<(), SpeakerError> {
    let mask = profile.channel_mask;
    if mask == 0 {
        return Ok(());
    }
    let rank = |bit: u32| (mask & (bit - 1)).count_ones() as usize;
    for speaker in &profile.speakers {
        let Some(bit) = nominal_speaker(&speaker.name).and_then(Speaker::wave_bit) else {
            continue;
        };
        if mask & bit != 0 && rank(bit) != speaker.channel {
            return Err(SpeakerError::MaskOrder {
                name: speaker.name.clone(),
                channel: speaker.channel,
                expected: rank(bit),
            });
        }
    }
    let lfe = Speaker::Lfe.wave_bit().expect("LFE has a Windows bit");
    if mask & lfe != 0 {
        let channel = rank(lfe);
        if let Some(speaker) = profile
            .speakers
            .iter()
            .find(|speaker| speaker.channel == channel)
        {
            return Err(SpeakerError::SpeakerOnLfe {
                name: speaker.name.clone(),
                channel,
            });
        }
    }
    if mask & lfe != 0 && !profile.subwoofers.is_empty() {
        let channel = rank(lfe);
        if !profile
            .subwoofers
            .iter()
            .any(|subwoofer| subwoofer.channel == channel)
        {
            return Err(SpeakerError::LfeNotSubwoofer { channel });
        }
    }
    Ok(())
}

fn resolved_bass_targets(profile: &SpeakerProfile) -> Vec<usize> {
    if !profile.bass_to.is_empty() {
        return profile
            .bass_to
            .iter()
            .filter_map(|name| {
                profile
                    .speakers
                    .iter()
                    .position(|speaker| speaker.name == *name)
            })
            .collect();
    }
    let front: Vec<_> = profile
        .speakers
        .iter()
        .enumerate()
        .filter(|(_, speaker)| {
            speaker.range == SpeakerRange::Full && matches!(speaker.name.as_str(), "L" | "R")
        })
        .map(|(index, _)| index)
        .collect();
    if !front.is_empty() {
        front
    } else {
        profile
            .speakers
            .iter()
            .enumerate()
            .filter(|(_, speaker)| speaker.range == SpeakerRange::Full)
            .map(|(index, _)| index)
            .collect()
    }
}

fn within(
    what: &str,
    value: f32,
    range: &std::ops::RangeInclusive<f32>,
) -> Result<(), SpeakerError> {
    if value.is_finite() && range.contains(&value) {
        Ok(())
    } else {
        Err(SpeakerError::OutOfRange {
            what: what.into(),
            value,
            range: format!("{}..={}", range.start(), range.end()),
        })
    }
}

fn validate_alignment(
    name: &str,
    distance_m: Option<f32>,
    delay_ms: Option<f32>,
) -> Result<(), SpeakerError> {
    if let Some(distance) = distance_m {
        within(
            &format!("{name} distance_m"),
            distance,
            &(0.0..=MAX_DISTANCE_M),
        )?;
    }
    if let Some(delay) = delay_ms {
        within(&format!("{name} delay_ms"), delay, &(0.0..=MAX_DELAY_MS))?;
    }
    Ok(())
}

fn validate_crossover(hz: f32) -> Result<(), SpeakerError> {
    if hz.is_finite() && (20.0..=250.0).contains(&hz) {
        Ok(())
    } else {
        Err(SpeakerError::BadCrossover { hz })
    }
}

fn alignment_frames(
    maximum_distance: f32,
    distance_m: Option<f32>,
    delay_ms: Option<f32>,
    rate: u32,
) -> usize {
    let frames = if let Some(delay_ms) = delay_ms {
        f64::from(delay_ms.max(0.0)) * f64::from(rate) / 1_000.0
    } else if let Some(distance_m) = distance_m {
        f64::from((maximum_distance - distance_m).max(0.0)) / 343.0 * f64::from(rate)
    } else {
        0.0
    };
    frames.round().min(usize::MAX as f64) as usize
}

/// 4th-order Linkwitz-Riley: two cascaded 2nd-order Butterworth sections per
/// branch. The branches are in phase and sum to an all-pass.
pub fn crossover_coefficients(rate: u32, hz: f32) -> CrossoverCoefficients {
    let low = butterworth(rate, hz, false);
    let high = butterworth(rate, hz, true);
    CrossoverCoefficients {
        low: [low; 2],
        high: [high; 2],
    }
}

fn butterworth(rate: u32, hz: f32, high_pass: bool) -> BiquadCoefficients {
    let omega = 2.0 * std::f64::consts::PI * f64::from(hz) / f64::from(rate);
    let (sin, cos) = omega.sin_cos();
    let alpha = sin / std::f64::consts::SQRT_2;
    let a0 = 1.0 + alpha;
    let (b0, b1, b2) = if high_pass {
        ((1.0 + cos) * 0.5, -(1.0 + cos), (1.0 + cos) * 0.5)
    } else {
        ((1.0 - cos) * 0.5, 1.0 - cos, (1.0 - cos) * 0.5)
    };
    BiquadCoefficients {
        b0: b0 / a0,
        b1: b1 / a0,
        b2: b2 / a0,
        a1: -2.0 * cos / a0,
        a2: (1.0 - alpha) / a0,
    }
}

/// The `Speaker` a profile name refers to (case-insensitive), if any.
fn nominal_speaker(name: &str) -> Option<Speaker> {
    Speaker::from_name(name).filter(|speaker| speaker.angles().is_some())
}

fn parse_channel_mask(value: &str) -> Result<u32, String> {
    let mask = match value {
        "direct" => 0,
        preset if let Some(speakers) = ssr_types::channel_layout_preset(preset) => speakers
            .iter()
            .filter_map(|speaker| speaker.wave_bit())
            .fold(0, |mask, bit| mask | bit),
        number => match number
            .strip_prefix("0x")
            .or_else(|| number.strip_prefix("0X"))
        {
            Some(hex) => u32::from_str_radix(hex, 16),
            None => number.parse::<u32>(),
        }
        .map_err(|_| format!("unknown channel_mask {number:?}"))?,
    };
    Ok(mask)
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

#[cfg(test)]
mod tests {
    use std::f64::consts::PI;

    use super::*;

    fn response(section: BiquadCoefficients, frequency: f64, rate: f64) -> (f64, f64) {
        let omega = 2.0 * PI * frequency / rate;
        let z1 = (omega.cos(), -omega.sin());
        let z2 = ((2.0 * omega).cos(), -(2.0 * omega).sin());
        let numerator = (
            section.b0 + section.b1 * z1.0 + section.b2 * z2.0,
            section.b1 * z1.1 + section.b2 * z2.1,
        );
        let denominator = (
            1.0 + section.a1 * z1.0 + section.a2 * z2.0,
            section.a1 * z1.1 + section.a2 * z2.1,
        );
        let scale = denominator.0 * denominator.0 + denominator.1 * denominator.1;
        (
            (numerator.0 * denominator.0 + numerator.1 * denominator.1) / scale,
            (numerator.1 * denominator.0 - numerator.0 * denominator.1) / scale,
        )
    }

    fn example_path(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/profiles")
            .join(name)
    }

    #[test]
    fn example_profiles_load_and_prepare() {
        for (name, channels, speakers) in
            [("speakers-5.1.toml", 6, 5), ("speakers-7.1.4.toml", 12, 11)]
        {
            let profile = load_profile(&example_path(name)).unwrap();
            assert_eq!(profile.device_channels, channels);
            assert_eq!(profile.speakers.len(), speakers);
            assert_eq!(profile.subwoofers.len(), 1);
            assert!(
                profile
                    .speakers
                    .iter()
                    .any(|speaker| speaker.range == SpeakerRange::Small)
            );
            let prepared = prepare(&profile, 48_000, 480).unwrap();
            assert_eq!(prepared.device_channels, channels);
            assert_eq!(prepared.speakers[0].delay_frames, 0);
            assert!(
                prepared
                    .speakers
                    .iter()
                    .skip(2)
                    .any(|speaker| speaker.delay_frames > 0)
            );
            if name == "speakers-7.1.4.toml" {
                for (channel, expected) in [(4, "Lrs"), (5, "Rrs"), (6, "Lss"), (7, "Rss")] {
                    assert_eq!(
                        profile
                            .speakers
                            .iter()
                            .find(|speaker| speaker.channel == channel)
                            .unwrap()
                            .name,
                        expected
                    );
                }
            }
        }
    }

    fn simple_profile() -> SpeakerProfile {
        SpeakerProfile {
            name: "test".into(),
            device_channels: 2,
            channel_mask: 0,
            gain_db: 0.0,
            limiter_ceiling_dbfs: -1.0,
            crossover_hz: 80.0,
            lfe_gain_db: 10.0,
            speakers: vec![SpeakerConfig {
                name: "L".into(),
                azimuth_deg: 30.0,
                elevation_deg: 0.0,
                channel: 0,
                distance_m: Some(3.0),
                delay_ms: None,
                trim_db: 0.0,
                range: SpeakerRange::Full,
                crossover_hz: None,
                fir: None,
            }],
            subwoofers: Vec::new(),
            bass_to: Vec::new(),
        }
    }

    fn subwoofer(channel: usize) -> SubwooferConfig {
        SubwooferConfig {
            channel,
            distance_m: None,
            delay_ms: None,
            trim_db: 0.0,
            fir: None,
        }
    }

    #[test]
    fn validation_errors_are_specific() {
        let mut profile = simple_profile();
        profile.speakers.clear();
        assert!(matches!(validate(&profile), Err(SpeakerError::NoSpeakers)));

        let mut profile = simple_profile();
        let mut duplicate = profile.speakers[0].clone();
        duplicate.name = "R".into();
        duplicate.azimuth_deg = -30.0;
        profile.speakers.push(duplicate);
        assert!(matches!(
            validate(&profile),
            Err(SpeakerError::DuplicateChannel { channel: 0 })
        ));

        let mut profile = simple_profile();
        profile.speakers[0].channel = 2;
        assert!(matches!(
            validate(&profile),
            Err(SpeakerError::ChannelOutOfRange {
                channel: 2,
                device_channels: 2
            })
        ));

        let mut profile = simple_profile();
        profile.speakers[0].delay_ms = Some(1.0);
        assert!(matches!(
            validate(&profile),
            Err(SpeakerError::ConflictingAlignment { .. })
        ));

        let mut profile = simple_profile();
        profile.crossover_hz = 10.0;
        assert!(matches!(
            validate(&profile),
            Err(SpeakerError::BadCrossover { .. })
        ));

        let mut profile = simple_profile();
        profile.bass_to.push("missing".into());
        assert!(matches!(
            validate(&profile),
            Err(SpeakerError::BadBassTarget(_))
        ));

        let mut profile = simple_profile();
        profile.speakers[0].range = SpeakerRange::Small;
        assert!(matches!(
            validate(&profile),
            Err(SpeakerError::NoBassTarget)
        ));

        let mut profile = simple_profile();
        profile.channel_mask = 0x3f;
        assert!(matches!(
            validate(&profile),
            Err(SpeakerError::ChannelMaskCount { .. })
        ));

        let mut profile = simple_profile();
        profile.subwoofers.push(subwoofer(0));
        assert!(matches!(
            validate(&profile),
            Err(SpeakerError::SubwooferOverlap { channel: 0 })
        ));

        let mut profile = simple_profile();
        profile.subwoofers.push(subwoofer(1));
        profile.bass_to.push("L".into());
        assert!(matches!(
            validate(&profile),
            Err(SpeakerError::BassToWithSubwoofer)
        ));
    }

    #[test]
    fn mask_order_follows_windows_channel_order() {
        // 5.1 (0x3f): FL FR FC LFE BL BR. BL is Lrs, so Lrs belongs on 4.
        let mut profile = simple_profile();
        profile.device_channels = 6;
        profile.channel_mask = 0x3f;
        let mut back = profile.speakers[0].clone();
        back.name = "Lrs".into();
        back.azimuth_deg = 150.0;
        back.channel = 5;
        profile.speakers.push(back);
        assert!(matches!(
            validate(&profile),
            Err(SpeakerError::MaskOrder {
                channel: 5,
                expected: 4,
                ..
            })
        ));
        profile.speakers[1].channel = 4;
        validate(&profile).unwrap();

        // Ls (DD+ 110 degrees) has no Windows bit and may sit on either back.
        profile.speakers[1].name = "Ls".into();
        profile.speakers[1].channel = 5;
        validate(&profile).unwrap();

        // The mask's LFE is channel 3; a subwoofer elsewhere is refused.
        profile.subwoofers.push(subwoofer(4));
        assert!(matches!(
            validate(&profile),
            Err(SpeakerError::LfeNotSubwoofer { channel: 3 })
        ));
        profile.subwoofers[0].channel = 3;
        validate(&profile).unwrap();
    }

    #[test]
    fn values_outside_their_ranges_are_rejected_before_any_allocation() {
        type Change = fn(&mut SpeakerProfile);
        let cases: [(&str, Change); 8] = [
            ("delay_ms", |p| p.speakers[0].delay_ms = Some(f32::INFINITY)),
            ("delay_ms", |p| p.speakers[0].delay_ms = Some(501.0)),
            ("distance_m", |p| p.speakers[0].distance_m = Some(1.0e9)),
            ("trim_db", |p| p.speakers[0].trim_db = f32::NAN),
            ("elevation", |p| p.speakers[0].elevation_deg = 91.0),
            ("lfe_gain_db", |p| p.lfe_gain_db = 40.0),
            ("limiter_ceiling_dbfs", |p| p.limiter_ceiling_dbfs = 1.0),
            ("device_channels", |p| {
                p.device_channels = MAX_DEVICE_CHANNELS + 1
            }),
        ];
        for (what, change) in cases {
            let mut profile = simple_profile();
            profile.speakers[0].distance_m = None;
            change(&mut profile);
            assert!(
                matches!(validate(&profile), Err(SpeakerError::OutOfRange { what: ref w, .. }) if w.ends_with(what)),
                "{what}: {:?}",
                validate(&profile)
            );
        }
        // The largest allowed delay prepares without trouble.
        let mut profile = simple_profile();
        profile.speakers[0].distance_m = None;
        profile.speakers[0].delay_ms = Some(MAX_DELAY_MS);
        let prepared = prepare(&profile, 192_000, 480).unwrap();
        assert_eq!(prepared.speakers[0].delay_frames, 96_000);
    }

    #[test]
    fn no_speaker_may_sit_on_the_mask_lfe_channel() {
        let mut profile = simple_profile();
        profile.device_channels = 6;
        profile.channel_mask = 0x3f;
        let mut custom = profile.speakers[0].clone();
        custom.name = "desk".into();
        custom.azimuth_deg = 0.0;
        custom.channel = 3;
        profile.speakers.push(custom);
        assert!(matches!(
            validate(&profile),
            Err(SpeakerError::SpeakerOnLfe { channel: 3, .. })
        ));
        profile.speakers[1].channel = 2;
        validate(&profile).unwrap();
    }

    #[test]
    fn resampled_room_fir_keeps_its_response_gain() {
        // A smooth 64-tap pulse with DC gain 1, padded so the resampler's
        // edges see silence.
        let mut fir = vec![0.0_f32; 256];
        let taps = 64;
        let window: Vec<f32> = (0..taps)
            .map(|n| {
                (std::f32::consts::PI * (n as f32 + 0.5) / taps as f32)
                    .sin()
                    .powi(2)
            })
            .collect();
        let sum: f32 = window.iter().sum();
        for (index, value) in window.iter().enumerate() {
            fir[96 + index] = value / sum;
        }
        for (from, to) in [(24_000, 48_000), (96_000, 48_000), (44_100, 48_000)] {
            let resampled = brir::ir::resample_channels(&[fir.clone()], from, to).unwrap();
            let dc: f32 = resampled[0].iter().sum();
            assert!((dc - 1.0).abs() < 1e-3, "{from} -> {to}: DC gain {dc}");
        }
    }

    #[test]
    fn a_room_fir_longer_than_four_seconds_is_refused() {
        let directory =
            std::env::temp_dir().join(format!("speakers-fir-long-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("long.wav");
        let mut writer = hound::WavWriter::create(
            &path,
            hound::WavSpec {
                channels: 1,
                sample_rate: 8_000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        for _ in 0..(8_000 * 4 + 1) {
            writer.write_sample(0_i16).unwrap();
        }
        writer.finalize().unwrap();
        let result = prepare_fir(Some(&path), 8_000, 480, &mut HashMap::new());
        let _ = std::fs::remove_dir_all(directory);
        assert!(matches!(result, Err(SpeakerError::FirTooLong { .. })));
    }

    #[test]
    fn channel_masks_accept_names_numbers_and_hex() {
        assert_eq!(parse_channel_mask("7.1.4"), Ok(0x2d63f));
        assert_eq!(parse_channel_mask("direct"), Ok(0));
        assert_eq!(parse_channel_mask("0x2D63F"), Ok(0x2d63f));
        assert_eq!(parse_channel_mask("63"), Ok(0x3f));
        assert!(parse_channel_mask("7.1.6").is_err());
        let parsed: ProfileToml = toml::from_str(
            "kind = \"speakers\"\nname = \"n\"\ndevice_channels = 6\nlimiter_ceiling_dbfs = 0.0\nchannel_mask = 0x3f\n",
        )
        .unwrap();
        assert!(matches!(parsed.channel_mask, MaskToml::Number(0x3f)));
    }

    #[test]
    fn subwoofers_align_trim_and_default_to_no_delay() {
        let mut profile = simple_profile();
        profile.device_channels = 4;
        profile.speakers[0].range = SpeakerRange::Small;
        profile.subwoofers = vec![
            SubwooferConfig {
                distance_m: Some(2.0),
                trim_db: -6.0,
                ..subwoofer(1)
            },
            subwoofer(2),
            SubwooferConfig {
                delay_ms: Some(1.0),
                ..subwoofer(3)
            },
        ];
        let prepared = prepare(&profile, 48_000, 480).unwrap();
        // L at 3 m is the farthest; the 2 m subwoofer waits 1 m / 343 m/s.
        assert_eq!(prepared.speakers[0].delay_frames, 0);
        assert_eq!(prepared.subwoofers[0].delay_frames, 140);
        assert!((prepared.subwoofers[0].gain - 0.501_187).abs() < 1e-6);
        assert_eq!(prepared.subwoofers[1].delay_frames, 0);
        assert_eq!(prepared.subwoofers[2].delay_frames, 48);
        assert!(prepared.bass_to.is_empty());
        assert!((prepared.lfe_gain - 10.0_f32.sqrt()).abs() < 1e-6);
    }

    #[test]
    fn loader_accepts_ffmpeg_speaker_aliases() {
        let directory =
            std::env::temp_dir().join(format!("speakers-profile-alias-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let profile = directory.join("profile.toml");
        std::fs::write(
            &profile,
            r#"kind = "speakers"
name = "alias"
device_channels = 1
limiter_ceiling_dbfs = 0.0

[[speaker]]
name = "FL"
channel = 0
"#,
        )
        .unwrap();
        let loaded = load_profile(&profile).unwrap();
        assert_eq!(loaded.speakers[0].name, "FL");
        assert_eq!(loaded.speakers[0].azimuth_deg, 30.0);
        assert_eq!(loaded.speakers[0].elevation_deg, 0.0);
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn loader_requires_angles_for_unknown_speaker_names() {
        let directory =
            std::env::temp_dir().join(format!("speakers-profile-angles-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let profile = directory.join("profile.toml");
        std::fs::write(
            &profile,
            r#"kind = "speakers"
name = "angles"
device_channels = 1
limiter_ceiling_dbfs = 0.0

[[speaker]]
name = "custom"
channel = 0
"#,
        )
        .unwrap();
        assert!(
            matches!(load_profile(&profile), Err(SpeakerError::MissingAngles(name)) if name == "custom")
        );
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn relative_mono_fir_is_loaded_and_resampled() {
        let directory =
            std::env::temp_dir().join(format!("speakers-profile-fir-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let fir = directory.join("room.wav");
        let mut writer = hound::WavWriter::create(
            &fir,
            hound::WavSpec {
                channels: 1,
                sample_rate: 24_000,
                bits_per_sample: 32,
                sample_format: hound::SampleFormat::Float,
            },
        )
        .unwrap();
        writer.write_sample(1.0_f32).unwrap();
        writer.write_sample(0.25_f32).unwrap();
        writer.finalize().unwrap();
        let profile = directory.join("profile.toml");
        std::fs::write(
            &profile,
            r#"kind = "speakers"
name = "fir"
device_channels = 2
limiter_ceiling_dbfs = 0.0

[[speaker]]
name = "L"
channel = 0
fir = "room.wav"

[[speaker]]
name = "R"
channel = 1
fir = "room.wav"
"#,
        )
        .unwrap();
        let loaded = load_profile(&profile).unwrap();
        assert_eq!(loaded.speakers[0].fir.as_deref(), Some(fir.as_path()));
        let prepared = prepare(&loaded, 48_000, 480).unwrap();
        let first = prepared.speakers[0].fir.as_ref().unwrap();
        let second = prepared.speakers[1].fir.as_ref().unwrap();
        assert!(Arc::ptr_eq(first, second));
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn explicit_delay_and_distance_alignment_round_to_frames() {
        assert_eq!(alignment_frames(3.0, None, Some(1.5), 48_000), 72);
        assert_eq!(alignment_frames(3.0, Some(2.0), None, 48_000), 140);
    }

    #[test]
    fn lr4_sum_is_flat_and_crosses_at_minus_six_db() {
        let crossover = crossover_coefficients(48_000, 80.0);
        let mut maximum_error_db = 0.0_f64;
        for step in 0..=2_000 {
            let frequency = 20.0_f64 * (1_000.0_f64).powf(step as f64 / 2_000.0);
            let low = response(crossover.low[0], frequency, 48_000.0);
            let high = response(crossover.high[0], frequency, 48_000.0);
            let low2 = (low.0 * low.0 - low.1 * low.1, 2.0 * low.0 * low.1);
            let high2 = (high.0 * high.0 - high.1 * high.1, 2.0 * high.0 * high.1);
            let sum = (low2.0 + high2.0).hypot(low2.1 + high2.1);
            maximum_error_db = maximum_error_db.max(20.0 * sum.log10().abs());
        }
        let at_crossover = response(crossover.low[0], 80.0, 48_000.0);
        let low_db = 20.0 * (at_crossover.0.hypot(at_crossover.1).powi(2)).log10();
        eprintln!("LR4 maximum sum error {maximum_error_db:.6} dB; crossover {low_db:.6} dB");
        assert!(maximum_error_db <= 0.02);
        assert!((low_db + 6.020_599_913).abs() <= 0.05);
    }

    #[test]
    fn lr4_stays_exact_at_the_extreme_crossover_and_rate() {
        // 20 Hz at 192 kHz puts the poles closest to z = 1.
        for (rate, hz) in [(192_000_u32, 20.0_f32), (48_000, 250.0)] {
            let crossover = crossover_coefficients(rate, hz);
            let at = |frequency: f64| {
                let low = response(crossover.low[0], frequency, f64::from(rate));
                let high = response(crossover.high[0], frequency, f64::from(rate));
                let low2 = (low.0 * low.0 - low.1 * low.1, 2.0 * low.0 * low.1);
                let high2 = (high.0 * high.0 - high.1 * high.1, 2.0 * high.0 * high.1);
                (low2, high2)
            };
            for step in 0..=1_000 {
                let frequency = 10.0_f64 * (2_000.0_f64).powf(step as f64 / 1_000.0);
                let (low2, high2) = at(frequency);
                let sum = (low2.0 + high2.0).hypot(low2.1 + high2.1);
                assert!(
                    20.0 * sum.log10().abs() <= 1e-6,
                    "{rate} Hz, {hz} Hz at {frequency}"
                );
            }
            let (low2, _) = at(f64::from(hz));
            let low_db = 20.0 * low2.0.hypot(low2.1).log10();
            assert!(
                (low_db + 6.020_599_913).abs() <= 1e-6,
                "{rate} Hz, {hz} Hz: {low_db}"
            );
        }
    }
}
