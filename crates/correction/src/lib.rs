#![forbid(unsafe_code)]

//! SECS 2 / SECS 2R speaker correction: reads the CamillaDSP v4 configuration
//! SECS exports and runs its signal chain as the speaker output stage (ADR 0034).

use std::collections::BTreeMap;
use std::f64::consts::PI;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use convolver::{FilterBank, MonoConvolver};
use ssr_types::{Direction, Feeds, Layout};
use yaml_rust2::parser::{Event, EventReceiver, Parser};
use yaml_rust2::{Yaml, YamlLoader};

pub const MAX_YAML_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_FILTERS: usize = 4096;
pub const MAX_MIXERS: usize = 64;
pub const MAX_PIPELINE_STEPS: usize = 4096;
pub const MAX_EXPANDED_FILTER_USES: usize = 16_384;
pub const MAX_MIXER_SOURCE_EDGES: usize = 65_536;
pub const MAX_BIQUAD_SECTIONS: usize = 16_384;
pub const MAX_CONVOLVER_USES: usize = 512;
pub const MAX_FIR_SAMPLES: usize = 32 * 4 * 48_000;
pub const MAX_DELAY_SECONDS: usize = 2;
pub const MAX_DELAY_SAMPLES: usize = 32 * MAX_DELAY_SECONDS * 48_000;
pub const MAX_TAIL_SECONDS: usize = 6;

const MAX_CHANNELS: usize = 64;
const MAX_FIR_SECONDS: f64 = 4.0;
const TAIL_EPSILON: f64 = 1.0e-30;
const MIN_GAIN_DB: f64 = -150.0;
const MAX_GAIN_DB: f64 = 40.0;
const MAX_LINEAR_GAIN: f64 = 100.0;
const MIN_Q: f64 = 0.001;
const MAX_Q: f64 = 1000.0;
const MIN_PEAKING_GAIN_DB: f64 = -40.0;
const MAX_PEAKING_GAIN_DB: f64 = 40.0;

#[derive(Debug)]
pub struct Correction {
    pub path: PathBuf,
    pub title: String,
    pub samplerate: u32,
    /// SECS inputs in order (the channels after `route_in`).
    pub inputs: Vec<Input>,
    /// One per device (playback) channel, in device order.
    pub outputs: Vec<Output>,
    pub device_channels: usize,
    /// ADR 0034's channel-mask rule; 0 is direct out.
    pub channel_mask: u32,
    filters: BTreeMap<String, Filter>,
    mixers: BTreeMap<String, Mixer>,
    pipeline: Vec<Step>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Input {
    pub name: String,
    /// Nominal (azimuth, elevation) in degrees, left positive (ADR 0034's
    /// role table); `None` for "LFE".
    pub angles: Option<(f32, f32)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Output {
    /// The SECS output routed to this device channel, if one is.
    pub name: Option<String>,
    /// The name starts with "SW".
    pub subwoofer: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum CorrectionError {
    #[error("{path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid CamillaDSP YAML: {0}")]
    Yaml(String),
    #[error("unsupported or invalid {item}: {detail}")]
    Invalid { item: String, detail: String },
    #[error("{path}: FIR track {channel} does not exist (the WAV has {tracks} tracks)")]
    FirTrack {
        path: PathBuf,
        channel: usize,
        tracks: usize,
    },
    #[error("{path}: FIR is {seconds:.3} seconds; the limit is {MAX_FIR_SECONDS} seconds")]
    FirTooLong { path: PathBuf, seconds: f64 },
    #[error(transparent)]
    Ir(#[from] brir::ir::IrError),
    #[error(transparent)]
    Convolver(#[from] convolver::ConvolverError),
    #[error(transparent)]
    Layout(#[from] ssr_types::LayoutError),
}

#[derive(Clone, Debug)]
enum Filter {
    Biquad(BiquadSpec),
    Combo(ComboSpec),
    Conv { path: PathBuf, channel: usize },
    Gain(f32),
    Delay(DelaySpec),
}

#[derive(Clone, Debug)]
enum BiquadSpec {
    Peaking { freq: f64, q: f64, gain_db: f64 },
    Allpass { freq: f64, q: f64 },
}

#[derive(Clone, Copy, Debug)]
enum ComboKind {
    ButterworthHighpass,
    ButterworthLowpass,
    LinkwitzRileyHighpass,
    LinkwitzRileyLowpass,
}

#[derive(Clone, Debug)]
struct ComboSpec {
    kind: ComboKind,
    freq: f64,
    order: usize,
}

#[derive(Clone, Debug)]
enum DelaySpec {
    Samples(f64),
    Milliseconds(f64),
}

#[derive(Clone, Debug)]
struct Mixer {
    inputs: usize,
    outputs: usize,
    mapping: Vec<MixDestination>,
}

#[derive(Clone, Debug)]
struct MixDestination {
    destination: usize,
    mute: bool,
    sources: Vec<MixSource>,
}

#[derive(Clone, Debug)]
struct MixSource {
    channel: usize,
    gain: f32,
    identity: bool,
}

#[derive(Clone, Debug)]
enum Step {
    Mixer(String),
    Filter {
        channels: Option<Vec<usize>>,
        names: Vec<String>,
    },
}

/// Parses and validates without touching the file system; FIR paths resolve
/// against `base` but are not opened.
pub fn parse(text: &str, base: &Path) -> Result<Correction, CorrectionError> {
    if text.len() > MAX_YAML_BYTES {
        return limit("YAML document", MAX_YAML_BYTES);
    }
    reject_yaml_aliases(text)?;
    let mut documents = YamlLoader::load_from_str(text)
        .map_err(|error| CorrectionError::Yaml(error.to_string()))?;
    if documents.len() != 1 {
        return invalid("document", "exactly one YAML document is required");
    }
    let root = documents.remove(0);
    let root = mapping(&root, "document")?;
    let title = optional_string(field(root, "title"), "title")?
        .unwrap_or("SECS 2")
        .to_owned();
    let description = optional_string(field(root, "description"), "description")?;
    let (input_names, output_names) = names(text, description)?;

    let devices = required_mapping(root, "devices")?;
    let samplerate = required_u32(devices, "samplerate")?;
    if samplerate == 0 {
        return invalid("devices.samplerate", "must be positive");
    }
    let capture = required_mapping(devices, "capture")?;
    let playback = required_mapping(devices, "playback")?;
    let capture_channels = required_usize(capture, "channels")?;
    let device_channels = required_usize(playback, "channels")?;
    check_channel_count("devices.capture.channels", capture_channels)?;
    check_channel_count("devices.playback.channels", device_channels)?;
    let has_backs =
        input_names.iter().any(|name| name == "Lb") && input_names.iter().any(|name| name == "Rb");
    let inputs = input_names
        .iter()
        .map(|name| {
            Ok(Input {
                name: name.clone(),
                angles: role_angles(name, has_backs)?,
            })
        })
        .collect::<Result<Vec<_>, CorrectionError>>()?;

    let filters = parse_filters(field(root, "filters"), base)?;
    let mixers = parse_mixers(field(root, "mixers"))?;
    let mut pipeline = parse_pipeline(field(root, "pipeline"), &filters, &mixers)?;
    if matches!(pipeline.first(), Some(Step::Mixer(name)) if name == "route_in") {
        let route_in = mixers
            .get("route_in")
            .expect("validated mixer reference exists");
        if route_in.outputs != inputs.len() {
            return invalid(
                "mixers.route_in",
                "output count does not match the SECS inputs",
            );
        }
        pipeline.remove(0);
    } else if capture_channels != inputs.len() {
        return invalid(
            "devices.capture.channels",
            "must match the SECS inputs when route_in is absent",
        );
    }

    validate_widths(
        inputs.len(),
        device_channels,
        &mut pipeline,
        &filters,
        &mixers,
    )?;
    let routed_outputs = route_output_names(&output_names, device_channels, &pipeline, &mixers)?;
    let outputs = routed_outputs
        .into_iter()
        .map(|name| Output {
            subwoofer: name.as_deref().is_some_and(|name| name.starts_with("SW")),
            name,
        })
        .collect::<Vec<_>>();
    let channel_mask = channel_mask(&outputs);

    Ok(Correction {
        path: PathBuf::new(),
        title,
        samplerate,
        inputs,
        outputs,
        device_channels,
        channel_mask,
        filters,
        mixers,
        pipeline,
    })
}

/// Reads `path`, parses it, and checks that every referenced WAV exists and
/// has the track `channel` names (header only, no decoding).
pub fn load(path: &Path) -> Result<Correction, CorrectionError> {
    let metadata = std::fs::metadata(path).map_err(|source| CorrectionError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.len() > MAX_YAML_BYTES as u64 {
        return limit("YAML document", MAX_YAML_BYTES);
    }
    let text = std::fs::read_to_string(path).map_err(|source| CorrectionError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let base = path.parent().unwrap_or_else(|| Path::new("."));
    let mut correction = parse(&text, base)?;
    correction.path = path.to_path_buf();
    for filter in correction.filters.values() {
        if let Filter::Conv { path, channel } = filter {
            let header = brir::ir::read_wav_header(path)?;
            if *channel >= header.tracks {
                return Err(CorrectionError::FirTrack {
                    path: path.clone(),
                    channel: *channel,
                    tracks: header.tracks,
                });
            }
            let seconds = header.frames as f64 / f64::from(header.rate);
            if seconds > MAX_FIR_SECONDS {
                return Err(CorrectionError::FirTooLong {
                    path: path.clone(),
                    seconds,
                });
            }
        }
    }
    Ok(correction)
}

impl Correction {
    /// The render directions: the non-LFE inputs in input order.
    pub fn layout(&self) -> Result<Layout, CorrectionError> {
        Layout::new(
            self.inputs
                .iter()
                .filter_map(|input| {
                    input.angles.map(|(azimuth, elevation)| {
                        Direction::new(input.name.clone(), azimuth, elevation)
                    })
                })
                .collect(),
        )
        .map_err(Into::into)
    }

    /// Index of the "LFE" input, if any.
    pub fn lfe_input(&self) -> Option<usize> {
        self.inputs.iter().position(|input| input.name == "LFE")
    }

    /// Decodes FIRs and compiles the graph for `rate`.
    pub fn prepare(
        &self,
        rate: u32,
        partition: usize,
    ) -> Result<PreparedCorrection, CorrectionError> {
        if rate == 0 {
            return invalid("rate", "must be positive");
        }
        if partition < 16 || !partition.is_multiple_of(2) {
            return invalid("partition", "must be even and at least 16");
        }
        let tail_frames = self.validate_prepare_budget(rate)?;
        let mut banks: BTreeMap<(PathBuf, usize), Arc<FilterBank>> = BTreeMap::new();
        let mut steps = Vec::with_capacity(self.pipeline.len());
        for step in &self.pipeline {
            match step {
                Step::Mixer(name) => {
                    let mixer = self.mixers.get(name).expect("validated mixer exists");
                    steps.push(PreparedStep::Mixer(mixer.clone()));
                }
                Step::Filter { channels, names } => {
                    let channels = channels
                        .as_ref()
                        .expect("filter channels are expanded during validation");
                    let capacity = channels
                        .len()
                        .checked_mul(names.len())
                        .expect("expanded filter-use budget checked during parsing");
                    let mut uses = Vec::with_capacity(capacity);
                    for &channel in channels {
                        for name in names {
                            let filter = self.filters.get(name).expect("validated filter exists");
                            uses.push(FilterUse {
                                channel,
                                filter: prepare_filter(
                                    filter,
                                    self.samplerate,
                                    rate,
                                    partition,
                                    &mut banks,
                                )?,
                            });
                        }
                    }
                    steps.push(PreparedStep::Filter(uses));
                }
            }
        }
        Ok(PreparedCorrection {
            rate,
            partition,
            input_channels: self.inputs.len(),
            device_channels: self.device_channels,
            tail_frames,
            steps,
        })
    }

    fn validate_prepare_budget(&self, rate: u32) -> Result<usize, CorrectionError> {
        let mut biquad_sections = 0_usize;
        let mut convolver_uses = 0_usize;
        let mut fir_samples = 0_usize;
        let mut delay_samples = 0_usize;
        let mut tails = vec![0_usize; self.inputs.len()];
        for step in &self.pipeline {
            match step {
                Step::Mixer(name) => {
                    let mixer = &self.mixers[name];
                    let mut next = vec![0_usize; mixer.outputs];
                    for destination in &mixer.mapping {
                        next[destination.destination] = next[destination.destination].max(
                            destination
                                .sources
                                .iter()
                                .map(|source| tails[source.channel])
                                .max()
                                .unwrap_or(0),
                        );
                    }
                    tails = next;
                }
                Step::Filter { channels, names } => {
                    let channels = channels
                        .as_ref()
                        .expect("filter channels are expanded during validation");
                    for &channel in channels {
                        for name in names {
                            match &self.filters[name] {
                                Filter::Biquad(_) => {
                                    biquad_sections = checked_add_limit(
                                        "biquad sections",
                                        biquad_sections,
                                        1,
                                        MAX_BIQUAD_SECTIONS,
                                    )?;
                                    tails[channel] = checked_add_limit(
                                        "correction tail frames",
                                        tails[channel],
                                        usize::try_from(rate / 2).unwrap_or(usize::MAX),
                                        usize::MAX,
                                    )?;
                                }
                                Filter::Combo(spec) => {
                                    let sections = combo_section_count(spec);
                                    biquad_sections = checked_add_limit(
                                        "biquad sections",
                                        biquad_sections,
                                        sections,
                                        MAX_BIQUAD_SECTIONS,
                                    )?;
                                    tails[channel] = checked_add_limit(
                                        "correction tail frames",
                                        tails[channel],
                                        usize::try_from(rate / 2).unwrap_or(usize::MAX),
                                        usize::MAX,
                                    )?;
                                }
                                Filter::Conv {
                                    path,
                                    channel: track,
                                } => {
                                    convolver_uses = checked_add_limit(
                                        "convolver uses",
                                        convolver_uses,
                                        1,
                                        MAX_CONVOLVER_USES,
                                    )?;
                                    let header = brir::ir::read_wav_header(path)?;
                                    if *track >= header.tracks {
                                        return Err(CorrectionError::FirTrack {
                                            path: path.clone(),
                                            channel: *track,
                                            tracks: header.tracks,
                                        });
                                    }
                                    let source_samples = header
                                        .frames
                                        .checked_mul(header.tracks)
                                        .ok_or_else(|| {
                                        CorrectionError::Invalid {
                                            item: format!("FIR {} decoded samples", path.display()),
                                            detail: format!("exceeds limit {MAX_FIR_SAMPLES}"),
                                        }
                                    })?;
                                    if source_samples > MAX_FIR_SAMPLES {
                                        return limit(
                                            format!("FIR {} decoded samples", path.display()),
                                            MAX_FIR_SAMPLES,
                                        );
                                    }
                                    let samples =
                                        converted_frames(header.frames, header.rate, rate)?;
                                    let converted_samples = samples
                                        .checked_mul(header.tracks)
                                        .ok_or_else(|| CorrectionError::Invalid {
                                            item: format!("FIR {} decoded samples", path.display()),
                                            detail: format!("exceeds limit {MAX_FIR_SAMPLES}"),
                                        })?;
                                    if converted_samples > MAX_FIR_SAMPLES {
                                        return limit(
                                            format!("FIR {} decoded samples", path.display()),
                                            MAX_FIR_SAMPLES,
                                        );
                                    }
                                    if samples > MAX_FIR_SECONDS as usize * rate as usize {
                                        return Err(CorrectionError::FirTooLong {
                                            path: path.clone(),
                                            seconds: samples as f64 / f64::from(rate),
                                        });
                                    }
                                    fir_samples = checked_add_limit(
                                        "FIR samples summed over uses",
                                        fir_samples,
                                        samples,
                                        MAX_FIR_SAMPLES,
                                    )?;
                                    tails[channel] = checked_add_limit(
                                        "correction tail frames",
                                        tails[channel],
                                        samples.saturating_sub(1),
                                        usize::MAX,
                                    )?;
                                }
                                Filter::Delay(delay) => {
                                    let samples = delay_frames(delay, self.samplerate, rate)?;
                                    let per_delay_limit = MAX_DELAY_SECONDS * rate as usize;
                                    if samples > per_delay_limit {
                                        return limit("each delay samples", per_delay_limit);
                                    }
                                    delay_samples = checked_add_limit(
                                        "delay samples summed over uses",
                                        delay_samples,
                                        samples,
                                        MAX_DELAY_SAMPLES,
                                    )?;
                                    tails[channel] = checked_add_limit(
                                        "correction tail frames",
                                        tails[channel],
                                        samples,
                                        usize::MAX,
                                    )?;
                                }
                                Filter::Gain(_) => {}
                            }
                        }
                    }
                }
            }
        }
        let tail = tails.into_iter().max().unwrap_or(0);
        let tail_limit = MAX_TAIL_SECONDS * rate as usize;
        if tail > tail_limit {
            return limit("correction tail frames", tail_limit);
        }
        Ok(tail)
    }
}

#[derive(Debug)]
pub struct PreparedCorrection {
    rate: u32,
    partition: usize,
    input_channels: usize,
    device_channels: usize,
    tail_frames: usize,
    steps: Vec<PreparedStep>,
}

impl PreparedCorrection {
    pub fn rate(&self) -> u32 {
        self.rate
    }

    pub fn partition(&self) -> usize {
        self.partition
    }

    pub fn inputs(&self) -> usize {
        self.input_channels
    }

    pub fn outputs(&self) -> usize {
        self.device_channels
    }

    pub fn tail_frames(&self) -> usize {
        self.tail_frames
    }
}

#[derive(Debug)]
enum PreparedStep {
    Mixer(Mixer),
    Filter(Vec<FilterUse>),
}

#[derive(Debug)]
struct FilterUse {
    channel: usize,
    filter: PreparedFilter,
}

#[derive(Debug)]
enum PreparedFilter {
    Biquads(Vec<Coefficients>),
    Conv(Arc<FilterBank>),
    Gain(f32),
    Delay(usize),
}

#[derive(Clone, Copy, Debug)]
struct Coefficients {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

impl Coefficients {
    fn finite(self) -> bool {
        [self.b0, self.b1, self.b2, self.a1, self.a2]
            .into_iter()
            .all(f64::is_finite)
    }
}

#[derive(Debug)]
pub struct CorrectionState {
    max_block: usize,
    channels: usize,
    current: usize,
    buffers: [Vec<f32>; 2],
    states: Vec<Vec<FilterState>>,
    nonfinite_resets: u64,
}

#[derive(Debug)]
enum FilterState {
    Biquads(Vec<BiquadState>),
    Conv {
        convolver: Box<MonoConvolver>,
        feed: Feeds,
    },
    Gain,
    Delay(DelayLine),
}

#[derive(Clone, Copy, Debug, Default)]
struct BiquadState {
    x1: f64,
    x2: f64,
    y1: f64,
    y2: f64,
}

#[derive(Debug)]
struct DelayLine {
    samples: Vec<f32>,
    cursor: usize,
}

impl CorrectionState {
    pub fn new(prepared: &PreparedCorrection, max_block: usize) -> Self {
        assert!(max_block > 0, "maximum block must be positive");
        assert!(max_block.is_multiple_of(prepared.partition));
        let channels = prepared
            .steps
            .iter()
            .map(|step| match step {
                PreparedStep::Mixer(mixer) => mixer.inputs.max(mixer.outputs),
                PreparedStep::Filter(uses) => uses
                    .iter()
                    .map(|filter| filter.channel + 1)
                    .max()
                    .unwrap_or(0),
            })
            .max()
            .unwrap_or(prepared.input_channels)
            .max(prepared.input_channels)
            .max(prepared.device_channels);
        let states = prepared
            .steps
            .iter()
            .map(|step| match step {
                PreparedStep::Mixer(_) => Vec::new(),
                PreparedStep::Filter(uses) => uses
                    .iter()
                    .map(|filter| match &filter.filter {
                        PreparedFilter::Biquads(coefficients) => {
                            FilterState::Biquads(vec![BiquadState::default(); coefficients.len()])
                        }
                        PreparedFilter::Conv(bank) => FilterState::Conv {
                            convolver: Box::new(MonoConvolver::new(Arc::clone(bank), max_block)),
                            feed: Feeds::new(1, max_block),
                        },
                        PreparedFilter::Gain(_) => FilterState::Gain,
                        PreparedFilter::Delay(samples) => {
                            FilterState::Delay(DelayLine::new(*samples))
                        }
                    })
                    .collect(),
            })
            .collect();
        Self {
            max_block,
            channels,
            current: 0,
            buffers: [
                vec![0.0; channels * max_block],
                vec![0.0; channels * max_block],
            ],
            states,
            nonfinite_resets: 0,
        }
    }

    /// The buffer of SECS input `index` for the next `frames` frames.
    pub fn input(&mut self, index: usize, frames: usize) -> &mut [f32] {
        assert!(index < self.channels);
        assert!(frames <= self.max_block);
        let start = index * self.max_block;
        &mut self.buffers[self.current][start..start + frames]
    }

    /// Runs the pipeline and writes device channels channel-major into `output`.
    pub fn run(&mut self, prepared: &PreparedCorrection, frames: usize, output: &mut [f32]) {
        assert!(frames <= self.max_block);
        assert!(frames.is_multiple_of(prepared.partition));
        assert!(output.len() >= prepared.device_channels * frames);
        for (step_index, step) in prepared.steps.iter().enumerate() {
            match step {
                PreparedStep::Mixer(mixer) => {
                    let source_index = self.current;
                    let destination_index = 1 - source_index;
                    let (source, destination) =
                        two_buffers(&mut self.buffers, source_index, destination_index);
                    destination.fill(0.0);
                    for mapping in &mixer.mapping {
                        if mapping.mute {
                            continue;
                        }
                        let out_start = mapping.destination * self.max_block;
                        for source_spec in &mapping.sources {
                            let in_start = source_spec.channel * self.max_block;
                            for frame in 0..frames {
                                destination[out_start + frame] +=
                                    source[in_start + frame] * source_spec.gain;
                            }
                        }
                    }
                    self.current = destination_index;
                }
                PreparedStep::Filter(uses) => {
                    for (use_index, filter_use) in uses.iter().enumerate() {
                        let start = filter_use.channel * self.max_block;
                        let samples = &mut self.buffers[self.current][start..start + frames];
                        process_filter(
                            &filter_use.filter,
                            &mut self.states[step_index][use_index],
                            samples,
                            frames,
                        );
                    }
                }
            }
        }
        output[..prepared.device_channels * frames].fill(0.0);
        for channel in 0..prepared.device_channels {
            let source = channel * self.max_block;
            let destination = channel * frames;
            output[destination..destination + frames]
                .copy_from_slice(&self.buffers[self.current][source..source + frames]);
        }
        let active = prepared.device_channels * frames;
        if output[..active].iter().any(|sample| !sample.is_finite()) {
            self.reset();
            self.nonfinite_resets = self.nonfinite_resets.saturating_add(1);
            output[..active].fill(0.0);
            return;
        }
        for channel in 0..prepared.input_channels {
            let start = channel * self.max_block;
            self.buffers[self.current][start..start + frames].fill(0.0);
        }
    }

    pub fn nonfinite_resets(&self) -> u64 {
        self.nonfinite_resets
    }

    pub fn reset(&mut self) {
        self.buffers[0].fill(0.0);
        self.buffers[1].fill(0.0);
        self.current = 0;
        for states in &mut self.states {
            for state in states {
                match state {
                    FilterState::Biquads(sections) => sections.fill(BiquadState::default()),
                    FilterState::Conv { convolver, feed } => {
                        convolver.reset();
                        feed.clear();
                    }
                    FilterState::Gain => {}
                    FilterState::Delay(delay) => delay.reset(),
                }
            }
        }
    }

    /// Every filter state, delay line and convolver tail is silent.
    pub fn is_quiet(&self) -> bool {
        self.states.iter().flatten().all(|state| match state {
            FilterState::Biquads(sections) => sections.iter().all(BiquadState::is_quiet),
            FilterState::Conv { convolver, .. } => convolver.is_quiet(),
            FilterState::Gain => true,
            FilterState::Delay(delay) => delay.is_quiet(),
        })
    }
}

fn two_buffers(
    buffers: &mut [Vec<f32>; 2],
    source: usize,
    destination: usize,
) -> (&[f32], &mut [f32]) {
    debug_assert_ne!(source, destination);
    let (first, second) = buffers.split_at_mut(1);
    if source == 0 {
        (&first[0], &mut second[0])
    } else {
        (&second[0], &mut first[0])
    }
}

fn process_filter(
    filter: &PreparedFilter,
    state: &mut FilterState,
    samples: &mut [f32],
    frames: usize,
) {
    match (filter, state) {
        (PreparedFilter::Biquads(coefficients), FilterState::Biquads(states)) => {
            for (coefficients, state) in coefficients.iter().zip(states) {
                state.process(*coefficients, samples);
            }
        }
        (PreparedFilter::Conv(_), FilterState::Conv { convolver, feed }) => {
            feed.set_frames(frames);
            feed.channel_mut(0).copy_from_slice(samples);
            let silent = samples.iter().all(|sample| *sample == 0.0);
            if !silent {
                convolver.note_active_input();
            }
            convolver.process(feed, samples);
            if silent {
                convolver.note_silent_input(frames);
            }
        }
        (PreparedFilter::Gain(gain), FilterState::Gain) => {
            for sample in samples {
                *sample *= *gain;
            }
        }
        (PreparedFilter::Delay(_), FilterState::Delay(delay)) => delay.process(samples),
        _ => unreachable!("prepared filters and state are built together"),
    }
}

impl BiquadState {
    fn process(&mut self, coefficients: Coefficients, samples: &mut [f32]) {
        let silent = samples.iter().all(|sample| *sample == 0.0);
        for sample in samples {
            let x = f64::from(*sample);
            let y = coefficients.b0 * x + coefficients.b1 * self.x1 + coefficients.b2 * self.x2
                - coefficients.a1 * self.y1
                - coefficients.a2 * self.y2;
            self.x2 = self.x1;
            self.x1 = x;
            self.y2 = self.y1;
            self.y1 = y;
            *sample = y as f32;
        }
        if silent {
            self.flush_tail();
        }
    }

    fn flush_tail(&mut self) {
        if self.x1.abs() < TAIL_EPSILON {
            self.x1 = 0.0;
        }
        if self.x2.abs() < TAIL_EPSILON {
            self.x2 = 0.0;
        }
        if self.y1.abs() < TAIL_EPSILON {
            self.y1 = 0.0;
        }
        if self.y2.abs() < TAIL_EPSILON {
            self.y2 = 0.0;
        }
    }

    fn is_quiet(&self) -> bool {
        self.x1 == 0.0 && self.x2 == 0.0 && self.y1 == 0.0 && self.y2 == 0.0
    }
}

impl DelayLine {
    fn new(delay: usize) -> Self {
        Self {
            samples: vec![0.0; delay],
            cursor: 0,
        }
    }

    fn process(&mut self, samples: &mut [f32]) {
        if self.samples.is_empty() {
            return;
        }
        for sample in samples {
            std::mem::swap(sample, &mut self.samples[self.cursor]);
            self.cursor += 1;
            if self.cursor == self.samples.len() {
                self.cursor = 0;
            }
        }
    }

    fn reset(&mut self) {
        self.samples.fill(0.0);
        self.cursor = 0;
    }

    fn is_quiet(&self) -> bool {
        self.samples.iter().all(|sample| *sample == 0.0)
    }
}

fn prepare_filter(
    filter: &Filter,
    source_rate: u32,
    target_rate: u32,
    partition: usize,
    banks: &mut BTreeMap<(PathBuf, usize), Arc<FilterBank>>,
) -> Result<PreparedFilter, CorrectionError> {
    Ok(match filter {
        Filter::Biquad(spec) => PreparedFilter::Biquads(vec![biquad(spec, target_rate)?]),
        Filter::Combo(spec) => PreparedFilter::Biquads(combo(spec, target_rate)?),
        Filter::Conv { path, channel } => {
            let key = (path.clone(), *channel);
            let bank = if let Some(bank) = banks.get(&key) {
                Arc::clone(bank)
            } else {
                let wav = brir::ir::read_wav(path, target_rate)?;
                if *channel >= wav.channels.len() {
                    return Err(CorrectionError::FirTrack {
                        path: path.clone(),
                        channel: *channel,
                        tracks: wav.channels.len(),
                    });
                }
                let samples = &wav.channels[*channel];
                if samples.iter().any(|sample| !sample.is_finite()) {
                    return invalid(
                        format!("FIR {} track {channel}", path.display()),
                        "sample is not finite",
                    );
                }
                let seconds = samples.len() as f64 / f64::from(target_rate);
                if seconds > MAX_FIR_SECONDS {
                    return Err(CorrectionError::FirTooLong {
                        path: path.clone(),
                        seconds,
                    });
                }
                let bank = Arc::new(FilterBank::new_mono(&[samples.as_slice()], partition)?);
                banks.insert(key, Arc::clone(&bank));
                bank
            };
            PreparedFilter::Conv(bank)
        }
        Filter::Gain(gain) => PreparedFilter::Gain(*gain),
        Filter::Delay(delay) => {
            PreparedFilter::Delay(delay_frames(delay, source_rate, target_rate)?)
        }
    })
}

fn biquad(spec: &BiquadSpec, rate: u32) -> Result<Coefficients, CorrectionError> {
    let (freq, q) = match *spec {
        BiquadSpec::Peaking { freq, q, .. } | BiquadSpec::Allpass { freq, q } => (freq, q),
    };
    check_frequency(freq, rate)?;
    if !q.is_finite() || !(MIN_Q..=MAX_Q).contains(&q) {
        return invalid("biquad q", format!("must be {MIN_Q} through {MAX_Q}"));
    }
    let omega = 2.0 * PI * freq / f64::from(rate);
    let alpha = omega.sin() / (2.0 * q);
    let cosine = omega.cos();
    let coefficients = match *spec {
        BiquadSpec::Peaking { gain_db, .. } => {
            if !gain_db.is_finite()
                || !(MIN_PEAKING_GAIN_DB..=MAX_PEAKING_GAIN_DB).contains(&gain_db)
            {
                return invalid(
                    "biquad gain",
                    format!("must be {MIN_PEAKING_GAIN_DB} through {MAX_PEAKING_GAIN_DB} dB"),
                );
            }
            let a = 10.0_f64.powf(gain_db / 40.0);
            let a0 = 1.0 + alpha / a;
            Coefficients {
                b0: (1.0 + alpha * a) / a0,
                b1: -2.0 * cosine / a0,
                b2: (1.0 - alpha * a) / a0,
                a1: -2.0 * cosine / a0,
                a2: (1.0 - alpha / a) / a0,
            }
        }
        BiquadSpec::Allpass { .. } => {
            let a0 = 1.0 + alpha;
            Coefficients {
                b0: (1.0 - alpha) / a0,
                b1: -2.0 * cosine / a0,
                b2: 1.0,
                a1: -2.0 * cosine / a0,
                a2: (1.0 - alpha) / a0,
            }
        }
    };
    if !coefficients.finite() {
        return invalid("biquad coefficients", "derived coefficient is not finite");
    }
    Ok(coefficients)
}

fn combo(spec: &ComboSpec, rate: u32) -> Result<Vec<Coefficients>, CorrectionError> {
    check_frequency(spec.freq, rate)?;
    let (butterworth_order, repeats) = match spec.kind {
        ComboKind::ButterworthHighpass | ComboKind::ButterworthLowpass => (spec.order, 1),
        ComboKind::LinkwitzRileyHighpass | ComboKind::LinkwitzRileyLowpass => (spec.order / 2, 2),
    };
    let highpass = matches!(
        spec.kind,
        ComboKind::ButterworthHighpass | ComboKind::LinkwitzRileyHighpass
    );
    let mut one = Vec::with_capacity(butterworth_order.div_ceil(2));
    if butterworth_order % 2 == 1 {
        one.push(first_order(spec.freq, rate, highpass));
    }
    for index in 0..(butterworth_order / 2) {
        let q = 1.0 / (2.0 * (PI * (2 * index + 1) as f64 / (2 * butterworth_order) as f64).sin());
        one.push(second_order(spec.freq, rate, q, highpass));
    }
    let mut sections = Vec::with_capacity(one.len() * repeats);
    for _ in 0..repeats {
        sections.extend_from_slice(&one);
    }
    if sections.iter().copied().any(|section| !section.finite()) {
        return invalid("biquad coefficients", "derived coefficient is not finite");
    }
    Ok(sections)
}

fn first_order(freq: f64, rate: u32, highpass: bool) -> Coefficients {
    let k = (PI * freq / f64::from(rate)).tan();
    let denominator = 1.0 + k;
    if highpass {
        Coefficients {
            b0: 1.0 / denominator,
            b1: -1.0 / denominator,
            b2: 0.0,
            a1: (k - 1.0) / denominator,
            a2: 0.0,
        }
    } else {
        Coefficients {
            b0: k / denominator,
            b1: k / denominator,
            b2: 0.0,
            a1: (k - 1.0) / denominator,
            a2: 0.0,
        }
    }
}

fn second_order(freq: f64, rate: u32, q: f64, highpass: bool) -> Coefficients {
    let omega = 2.0 * PI * freq / f64::from(rate);
    let alpha = omega.sin() / (2.0 * q);
    let cosine = omega.cos();
    let a0 = 1.0 + alpha;
    let numerator = if highpass { 1.0 + cosine } else { 1.0 - cosine };
    let middle_sign = if highpass { -1.0 } else { 1.0 };
    Coefficients {
        b0: numerator / (2.0 * a0),
        b1: middle_sign * numerator / a0,
        b2: numerator / (2.0 * a0),
        a1: -2.0 * cosine / a0,
        a2: (1.0 - alpha) / a0,
    }
}

fn gain_multiplier(
    gain: f64,
    scale: &str,
    mute: bool,
    inverted: bool,
    item: &str,
) -> Result<f32, CorrectionError> {
    if !gain.is_finite() {
        return invalid(format!("{item}.gain"), "must be finite");
    }
    let linear = match scale {
        "dB" if (MIN_GAIN_DB..=MAX_GAIN_DB).contains(&gain) => 10.0_f64.powf(gain / 20.0),
        "dB" => {
            return invalid(
                format!("{item}.gain"),
                format!("dB gain must be {MIN_GAIN_DB} through {MAX_GAIN_DB}"),
            );
        }
        "linear" if gain.abs() <= MAX_LINEAR_GAIN => gain,
        "linear" => {
            return invalid(
                format!("{item}.gain"),
                format!("linear gain magnitude must not exceed {MAX_LINEAR_GAIN}"),
            );
        }
        other => return invalid(format!("{item}.scale"), other),
    };
    let signed = if mute {
        0.0
    } else if inverted {
        -linear
    } else {
        linear
    };
    let result = signed as f32;
    if !result.is_finite() {
        return invalid(format!("{item}.gain"), "converted gain must be finite");
    }
    Ok(result)
}

fn combo_section_count(spec: &ComboSpec) -> usize {
    let (order, repeats) = match spec.kind {
        ComboKind::ButterworthHighpass | ComboKind::ButterworthLowpass => (spec.order, 1),
        ComboKind::LinkwitzRileyHighpass | ComboKind::LinkwitzRileyLowpass => (spec.order / 2, 2),
    };
    order.div_ceil(2) * repeats
}

fn converted_frames(
    frames: usize,
    source_rate: u32,
    target_rate: u32,
) -> Result<usize, CorrectionError> {
    let numerator = (frames as u128)
        .checked_mul(u128::from(target_rate))
        .ok_or_else(|| CorrectionError::Invalid {
            item: "FIR sample conversion".into(),
            detail: "arithmetic overflow".into(),
        })?;
    usize::try_from(numerator.div_ceil(u128::from(source_rate))).map_err(|_| {
        CorrectionError::Invalid {
            item: "FIR sample conversion".into(),
            detail: "arithmetic overflow".into(),
        }
    })
}

fn delay_frames(
    delay: &DelaySpec,
    source_rate: u32,
    target_rate: u32,
) -> Result<usize, CorrectionError> {
    let value = match delay {
        DelaySpec::Samples(samples) => *samples * f64::from(target_rate) / f64::from(source_rate),
        DelaySpec::Milliseconds(milliseconds) => *milliseconds * f64::from(target_rate) / 1000.0,
    };
    if !value.is_finite() || value < 0.0 || value > usize::MAX as f64 {
        return invalid("delay samples", "conversion is outside the supported range");
    }
    Ok(value.round() as usize)
}

fn checked_add_limit(
    item: &str,
    current: usize,
    add: usize,
    maximum: usize,
) -> Result<usize, CorrectionError> {
    let total = current
        .checked_add(add)
        .ok_or_else(|| CorrectionError::Invalid {
            item: item.into(),
            detail: format!("exceeds limit {maximum}"),
        })?;
    if total > maximum {
        return limit(item, maximum);
    }
    Ok(total)
}

fn check_frequency(freq: f64, rate: u32) -> Result<(), CorrectionError> {
    if !freq.is_finite() || freq <= 0.0 || freq >= f64::from(rate) / 2.0 {
        return invalid(
            "filter frequency",
            format!("{freq} must be between zero and the {rate} Hz Nyquist frequency"),
        );
    }
    Ok(())
}

fn parse_filters(yaml: &Yaml, base: &Path) -> Result<BTreeMap<String, Filter>, CorrectionError> {
    let definitions = mapping(yaml, "filters")?;
    if definitions.len() > MAX_FILTERS {
        return limit("filters", MAX_FILTERS);
    }
    let mut filters = BTreeMap::new();
    for (name, value) in definitions {
        let name = string(name, "filter name")?.to_owned();
        let definition = mapping(value, &format!("filters.{name}"))?;
        let filter_type = required_string(definition, "type")?;
        let parameters = required_mapping(definition, "parameters")?;
        let filter = match filter_type {
            "Biquad" => match required_string(parameters, "type")? {
                "Peaking" => Filter::Biquad(BiquadSpec::Peaking {
                    freq: required_f64(parameters, "freq")?,
                    q: required_f64(parameters, "q")?,
                    gain_db: required_f64(parameters, "gain")?,
                }),
                "Allpass" => Filter::Biquad(BiquadSpec::Allpass {
                    freq: required_f64(parameters, "freq")?,
                    q: required_f64(parameters, "q")?,
                }),
                other => return invalid(format!("filters.{name}"), other),
            },
            "BiquadCombo" => {
                let kind = match required_string(parameters, "type")? {
                    "ButterworthHighpass" => ComboKind::ButterworthHighpass,
                    "ButterworthLowpass" => ComboKind::ButterworthLowpass,
                    "LinkwitzRileyHighpass" => ComboKind::LinkwitzRileyHighpass,
                    "LinkwitzRileyLowpass" => ComboKind::LinkwitzRileyLowpass,
                    other => return invalid(format!("filters.{name}"), other),
                };
                let order = required_usize(parameters, "order")?;
                match kind {
                    ComboKind::ButterworthHighpass | ComboKind::ButterworthLowpass
                        if !(1..=8).contains(&order) =>
                    {
                        return invalid(format!("filters.{name}.order"), "must be 1 through 8");
                    }
                    ComboKind::LinkwitzRileyHighpass | ComboKind::LinkwitzRileyLowpass
                        if !matches!(order, 2 | 4 | 8) =>
                    {
                        return invalid(format!("filters.{name}.order"), "must be 2, 4, or 8");
                    }
                    _ => {}
                }
                Filter::Combo(ComboSpec {
                    kind,
                    freq: required_f64(parameters, "freq")?,
                    order,
                })
            }
            "Conv" => {
                if required_string(parameters, "type")? != "Wav" {
                    return invalid(
                        format!("filters.{name}"),
                        "only Wav convolution is supported",
                    );
                }
                let filename = required_string(parameters, "filename")?;
                let path = base.join(filename);
                let channel = optional_usize(field(parameters, "channel"), "channel")?.unwrap_or(0);
                Filter::Conv { path, channel }
            }
            "Gain" => {
                let scale = optional_string(field(parameters, "scale"), "scale")?.unwrap_or("dB");
                let mute = optional_bool(field(parameters, "mute"), "mute")?.unwrap_or(false);
                let inverted =
                    optional_bool(field(parameters, "inverted"), "inverted")?.unwrap_or(false);
                let gain = gain_multiplier(
                    required_f64(parameters, "gain")?,
                    scale,
                    mute,
                    inverted,
                    &format!("filters.{name}"),
                )?;
                Filter::Gain(gain)
            }
            "Delay" => {
                if optional_bool(field(parameters, "subsample"), "subsample")?.unwrap_or(false) {
                    return invalid(format!("filters.{name}.subsample"), "must be false");
                }
                let unit = optional_string(field(parameters, "unit"), "unit")?.unwrap_or("ms");
                let delay = required_f64(parameters, "delay")?;
                if !delay.is_finite() || delay < 0.0 {
                    return invalid(
                        format!("filters.{name}.delay"),
                        "must be finite and non-negative",
                    );
                }
                match unit {
                    "samples" => Filter::Delay(DelaySpec::Samples(delay)),
                    "ms" => Filter::Delay(DelaySpec::Milliseconds(delay)),
                    other => return invalid(format!("filters.{name}.unit"), other),
                }
            }
            other => return invalid(format!("filters.{name}.type"), other),
        };
        filters.insert(name, filter);
    }
    Ok(filters)
}

fn parse_mixers(yaml: &Yaml) -> Result<BTreeMap<String, Mixer>, CorrectionError> {
    let definitions = mapping(yaml, "mixers")?;
    if definitions.len() > MAX_MIXERS {
        return limit("mixers", MAX_MIXERS);
    }
    let mut mixers = BTreeMap::new();
    let mut source_edges = 0_usize;
    for (name, value) in definitions {
        let name = string(name, "mixer name")?.to_owned();
        let definition = mapping(value, &format!("mixers.{name}"))?;
        let channels = required_mapping(definition, "channels")?;
        let inputs = required_usize(channels, "in")?;
        let outputs = required_usize(channels, "out")?;
        check_channel_count(&format!("mixers.{name}.channels.in"), inputs)?;
        check_channel_count(&format!("mixers.{name}.channels.out"), outputs)?;
        let mut destinations = Vec::new();
        for value in array(
            field(definition, "mapping"),
            &format!("mixers.{name}.mapping"),
        )? {
            let destination_mapping = mapping(value, &format!("mixers.{name}.mapping"))?;
            let destination = required_usize(destination_mapping, "dest")?;
            if destination >= outputs {
                return invalid(format!("mixers.{name}.dest"), destination.to_string());
            }
            let mute = optional_bool(field(destination_mapping, "mute"), "mute")?.unwrap_or(false);
            let mut sources = Vec::new();
            let source_values = array(field(destination_mapping, "sources"), "sources")?;
            source_edges = source_edges
                .checked_add(source_values.len())
                .ok_or_else(|| CorrectionError::Invalid {
                    item: "mixer source edges".into(),
                    detail: format!("exceeds limit {MAX_MIXER_SOURCE_EDGES}"),
                })?;
            if source_edges > MAX_MIXER_SOURCE_EDGES {
                return limit("mixer source edges", MAX_MIXER_SOURCE_EDGES);
            }
            for value in source_values {
                let source = mapping(value, "mixer source")?;
                let channel = required_usize(source, "channel")?;
                if channel >= inputs {
                    return invalid(format!("mixers.{name}.source"), channel.to_string());
                }
                let scale = optional_string(field(source, "scale"), "scale")?.unwrap_or("dB");
                let source_mute = optional_bool(field(source, "mute"), "mute")?.unwrap_or(false);
                let inverted =
                    optional_bool(field(source, "inverted"), "inverted")?.unwrap_or(false);
                let default_gain = if scale == "linear" { 1.0 } else { 0.0 };
                let gain_value =
                    optional_f64(field(source, "gain"), "gain")?.unwrap_or(default_gain);
                let identity = !source_mute
                    && !inverted
                    && match scale {
                        "dB" => gain_value == 0.0,
                        "linear" => gain_value == 1.0,
                        _ => false,
                    };
                let gain = gain_multiplier(
                    gain_value,
                    scale,
                    source_mute,
                    inverted,
                    &format!("mixers.{name}"),
                )?;
                sources.push(MixSource {
                    channel,
                    gain,
                    identity,
                });
            }
            destinations.push(MixDestination {
                destination,
                mute,
                sources,
            });
        }
        mixers.insert(
            name,
            Mixer {
                inputs,
                outputs,
                mapping: destinations,
            },
        );
    }
    Ok(mixers)
}

fn parse_pipeline(
    yaml: &Yaml,
    filters: &BTreeMap<String, Filter>,
    mixers: &BTreeMap<String, Mixer>,
) -> Result<Vec<Step>, CorrectionError> {
    let values = array(yaml, "pipeline")?;
    if values.len() > MAX_PIPELINE_STEPS {
        return limit("pipeline steps", MAX_PIPELINE_STEPS);
    }
    let mut steps = Vec::new();
    for value in values {
        let step = mapping(value, "pipeline step")?;
        if optional_bool(field(step, "bypassed"), "bypassed")?.unwrap_or(false) {
            continue;
        }
        match required_string(step, "type")? {
            "Mixer" => {
                let name = required_string(step, "name")?.to_owned();
                if !mixers.contains_key(&name) {
                    return invalid("pipeline mixer", name);
                }
                steps.push(Step::Mixer(name));
            }
            "Filter" => {
                let channels = match field(step, "channels") {
                    Yaml::BadValue | Yaml::Null => None,
                    value => Some(
                        array(value, "filter channels")?
                            .iter()
                            .map(|channel| usize_value(channel, "filter channel"))
                            .collect::<Result<Vec<_>, _>>()?,
                    ),
                };
                let names = array(field(step, "names"), "filter names")?
                    .iter()
                    .map(|name| Ok(string(name, "filter name")?.to_owned()))
                    .collect::<Result<Vec<_>, CorrectionError>>()?;
                for name in &names {
                    if !filters.contains_key(name) {
                        return invalid("pipeline filter", name);
                    }
                }
                steps.push(Step::Filter { channels, names });
            }
            other => return invalid("pipeline step type", other),
        }
    }
    Ok(steps)
}

fn validate_widths(
    mut width: usize,
    device_channels: usize,
    pipeline: &mut [Step],
    _filters: &BTreeMap<String, Filter>,
    mixers: &BTreeMap<String, Mixer>,
) -> Result<(), CorrectionError> {
    let mut expanded_filter_uses = 0_usize;
    let mut expanded_mixer_edges = 0_usize;
    for step in pipeline {
        match step {
            Step::Mixer(name) => {
                let mixer = &mixers[name];
                if mixer.inputs != width {
                    return invalid(
                        format!("pipeline mixer {name}"),
                        format!("expects {} channels but has {width}", mixer.inputs),
                    );
                }
                let edges = mixer
                    .mapping
                    .iter()
                    .map(|destination| destination.sources.len())
                    .sum::<usize>();
                expanded_mixer_edges =
                    expanded_mixer_edges.checked_add(edges).ok_or_else(|| {
                        CorrectionError::Invalid {
                            item: "mixer source edges".into(),
                            detail: format!("exceeds limit {MAX_MIXER_SOURCE_EDGES}"),
                        }
                    })?;
                if expanded_mixer_edges > MAX_MIXER_SOURCE_EDGES {
                    return limit("mixer source edges", MAX_MIXER_SOURCE_EDGES);
                }
                width = mixer.outputs;
            }
            Step::Filter { channels, names } => {
                let channels = channels.get_or_insert_with(|| (0..width).collect());
                if let Some(channel) = channels.iter().find(|&&channel| channel >= width) {
                    return invalid("pipeline filter channel", channel.to_string());
                }
                let uses = channels
                    .len()
                    .checked_mul(names.len())
                    .and_then(|uses| expanded_filter_uses.checked_add(uses))
                    .ok_or_else(|| CorrectionError::Invalid {
                        item: "expanded filter uses".into(),
                        detail: format!("exceeds limit {MAX_EXPANDED_FILTER_USES}"),
                    })?;
                if uses > MAX_EXPANDED_FILTER_USES {
                    return limit("expanded filter uses", MAX_EXPANDED_FILTER_USES);
                }
                expanded_filter_uses = uses;
            }
        }
    }
    if width != device_channels {
        return invalid(
            "pipeline output",
            format!("has {width} channels, expected {device_channels}"),
        );
    }
    Ok(())
}

fn names(
    text: &str,
    description: Option<&str>,
) -> Result<(Vec<String>, Vec<String>), CorrectionError> {
    let description_inputs = description.and_then(|value| parse_description_names(value, "inputs"));
    let description_outputs =
        description.and_then(|value| parse_description_names(value, "outputs"));
    let mut comment_inputs = None;
    let mut comment_outputs = None;
    for line in text.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("# inputs ") {
            let (input_part, output_part) = rest
                .split_once(" / outputs ")
                .map_or((rest, None), |(inputs, outputs)| (inputs, Some(outputs)));
            comment_inputs = Some(parse_comment_names(input_part)?);
            if let Some(output_part) = output_part {
                comment_outputs = Some(parse_comment_names(output_part)?);
            }
        } else if let Some(rest) = line.strip_prefix("# outputs ") {
            comment_outputs = Some(parse_comment_names(rest)?);
        }
    }
    let inputs = description_inputs
        .or(comment_inputs)
        .ok_or_else(|| CorrectionError::Invalid {
            item: "SECS input names".into(),
            detail: "description and header comment are missing".into(),
        })?;
    Ok((
        inputs,
        description_outputs.or(comment_outputs).unwrap_or_default(),
    ))
}

fn parse_description_names(description: &str, marker: &str) -> Option<Vec<String>> {
    let section = description
        .split('|')
        .map(str::trim)
        .find(|section| section.starts_with(marker))?;
    let rest = section.strip_prefix(marker)?.trim();
    let mut names = Vec::new();
    for item in rest.split(',') {
        let mut parts = item.split_whitespace();
        let index = parts.next()?.parse::<usize>().ok()?;
        let name = parts.next()?.to_owned();
        if index != names.len() || parts.next().is_some() {
            return None;
        }
        names.push(name);
    }
    (!names.is_empty()).then_some(names)
}

fn parse_comment_names(part: &str) -> Result<Vec<String>, CorrectionError> {
    let (count, list) = part
        .split_once(':')
        .ok_or_else(|| CorrectionError::Invalid {
            item: "header channel names".into(),
            detail: part.into(),
        })?;
    let count = count
        .trim()
        .parse::<usize>()
        .map_err(|_| CorrectionError::Invalid {
            item: "header channel count".into(),
            detail: count.into(),
        })?;
    let names = list
        .split(',')
        .map(|name| name.trim().to_owned())
        .collect::<Vec<_>>();
    if names.len() != count || names.iter().any(String::is_empty) {
        return invalid("header channel names", part);
    }
    Ok(names)
}

fn role_angles(name: &str, has_backs: bool) -> Result<Option<(f32, f32)>, CorrectionError> {
    Ok(match name {
        "L" => Some((30.0, 0.0)),
        "R" => Some((-30.0, 0.0)),
        "C" => Some((0.0, 0.0)),
        "Ls" => Some((if has_backs { 90.0 } else { 110.0 }, 0.0)),
        "Rs" => Some((if has_backs { -90.0 } else { -110.0 }, 0.0)),
        "Lb" => Some((150.0, 0.0)),
        "Rb" => Some((-150.0, 0.0)),
        "Lw" => Some((60.0, 0.0)),
        "Rw" => Some((-60.0, 0.0)),
        "Tfl" => Some((45.0, 45.0)),
        "Tfr" => Some((-45.0, 45.0)),
        "Tml" => Some((90.0, 45.0)),
        "Tmr" => Some((-90.0, 45.0)),
        "Tbl" => Some((135.0, 45.0)),
        "Tbr" => Some((-135.0, 45.0)),
        "Hfl" => Some((30.0, 30.0)),
        "Hfr" => Some((-30.0, 30.0)),
        "Hbl" => Some((150.0, 30.0)),
        "Hbr" => Some((-150.0, 30.0)),
        "LFE" => None,
        other => return invalid("SECS input role", other),
    })
}

fn route_output_names(
    names: &[String],
    device_channels: usize,
    pipeline: &[Step],
    mixers: &BTreeMap<String, Mixer>,
) -> Result<Vec<Option<String>>, CorrectionError> {
    let declared = names.iter().cloned().map(Some).collect::<Vec<_>>();
    let route_out_index = pipeline
        .iter()
        .rposition(|step| matches!(step, Step::Mixer(name) if name == "route_out"));
    let Some(route_out_index) = route_out_index else {
        return Ok((0..device_channels)
            .map(|channel| declared.get(channel).cloned().flatten())
            .collect());
    };
    if pipeline[route_out_index + 1..]
        .iter()
        .any(|step| matches!(step, Step::Mixer(_)))
    {
        return Ok(vec![None; device_channels]);
    }
    let Some(route_out) = mixers.get("route_out") else {
        unreachable!("validated route_out reference exists");
    };
    if route_out.outputs != device_channels {
        return invalid(
            "output names",
            "route_out does not reach the playback width",
        );
    }
    let mut routed = vec![None; device_channels];
    let mut destination_counts = vec![0_usize; device_channels];
    for destination in &route_out.mapping {
        destination_counts[destination.destination] += 1;
    }
    for destination in &route_out.mapping {
        if destination_counts[destination.destination] != 1
            || destination.mute
            || destination.sources.len() != 1
        {
            continue;
        }
        let source = &destination.sources[0];
        if source.identity {
            routed[destination.destination] = declared.get(source.channel).cloned().flatten();
        }
    }
    Ok(routed)
}

fn channel_mask(outputs: &[Output]) -> u32 {
    if outputs.len() == 2 {
        return 0x3;
    }
    for layout in ssr_types::app::SpeakerLayout::ALL {
        if layout.speakers().len() != outputs.len() {
            continue;
        }
        let mut subwoofers = 0;
        let matches = outputs
            .iter()
            .zip(layout.speakers())
            .all(
                |(output, expected)| match (output.name.as_deref(), expected.name()) {
                    (Some(name), "LFE") => {
                        subwoofers += usize::from(name == "SW1");
                        name == "SW1"
                    }
                    (Some("Lb"), "Lrs") | (Some("Rb"), "Rrs") => true,
                    (Some("Tbl"), "Trl") | (Some("Tbr"), "Trr") => true,
                    (Some("Ls"), "Lss") | (Some("Rs"), "Rss") => true,
                    (Some(name), expected) => name == expected,
                    _ => false,
                },
            );
        if matches && subwoofers <= 1 {
            return layout.output_layout().channel_mask;
        }
    }
    0
}

#[derive(Default)]
struct AliasDetector {
    found: bool,
}

impl EventReceiver for AliasDetector {
    fn on_event(&mut self, event: Event) {
        self.found |= matches!(event, Event::Alias(_));
    }
}

fn reject_yaml_aliases(text: &str) -> Result<(), CorrectionError> {
    let mut detector = AliasDetector::default();
    Parser::new_from_str(text)
        .load(&mut detector, true)
        .map_err(|error| CorrectionError::Yaml(error.to_string()))?;
    if detector.found {
        return invalid("YAML alias", "aliases are not supported");
    }
    Ok(())
}

fn mapping<'a>(yaml: &'a Yaml, item: &str) -> Result<&'a yaml_rust2::yaml::Hash, CorrectionError> {
    match yaml {
        Yaml::Hash(value) => Ok(value),
        _ => invalid(item, "must be a mapping"),
    }
}

fn array<'a>(yaml: &'a Yaml, item: &str) -> Result<&'a [Yaml], CorrectionError> {
    match yaml {
        Yaml::Array(value) => Ok(value),
        _ => invalid(item, "must be a sequence"),
    }
}

fn field<'a>(mapping: &'a yaml_rust2::yaml::Hash, name: &str) -> &'a Yaml {
    mapping
        .get(&Yaml::String(name.to_owned()))
        .unwrap_or(&Yaml::BadValue)
}

fn required_mapping<'a>(
    values: &'a yaml_rust2::yaml::Hash,
    name: &str,
) -> Result<&'a yaml_rust2::yaml::Hash, CorrectionError> {
    mapping(field(values, name), name)
}

fn string<'a>(yaml: &'a Yaml, item: &str) -> Result<&'a str, CorrectionError> {
    match yaml {
        Yaml::String(value) => Ok(value),
        _ => invalid(item, "must be a string"),
    }
}

fn required_string<'a>(
    mapping: &'a yaml_rust2::yaml::Hash,
    name: &str,
) -> Result<&'a str, CorrectionError> {
    string(field(mapping, name), name)
}

fn optional_string<'a>(yaml: &'a Yaml, item: &str) -> Result<Option<&'a str>, CorrectionError> {
    match yaml {
        Yaml::BadValue | Yaml::Null => Ok(None),
        Yaml::String(value) => Ok(Some(value)),
        _ => invalid(item, "must be a string"),
    }
}

fn usize_value(yaml: &Yaml, item: &str) -> Result<usize, CorrectionError> {
    match yaml {
        Yaml::Integer(value) => usize::try_from(*value).map_err(|_| CorrectionError::Invalid {
            item: item.into(),
            detail: value.to_string(),
        }),
        _ => invalid(item, "must be a non-negative integer"),
    }
}

fn required_usize(mapping: &yaml_rust2::yaml::Hash, name: &str) -> Result<usize, CorrectionError> {
    usize_value(field(mapping, name), name)
}

fn optional_usize(yaml: &Yaml, item: &str) -> Result<Option<usize>, CorrectionError> {
    match yaml {
        Yaml::BadValue | Yaml::Null => Ok(None),
        _ => usize_value(yaml, item).map(Some),
    }
}

fn required_u32(mapping: &yaml_rust2::yaml::Hash, name: &str) -> Result<u32, CorrectionError> {
    let value = required_usize(mapping, name)?;
    u32::try_from(value).map_err(|_| CorrectionError::Invalid {
        item: name.into(),
        detail: value.to_string(),
    })
}

fn f64_value(yaml: &Yaml, item: &str) -> Result<f64, CorrectionError> {
    match yaml {
        Yaml::Integer(value) => Ok(*value as f64),
        Yaml::Real(value) => value.parse().map_err(|_| CorrectionError::Invalid {
            item: item.into(),
            detail: value.clone(),
        }),
        _ => invalid(item, "must be a number"),
    }
}

fn required_f64(mapping: &yaml_rust2::yaml::Hash, name: &str) -> Result<f64, CorrectionError> {
    f64_value(field(mapping, name), name)
}

fn optional_f64(yaml: &Yaml, item: &str) -> Result<Option<f64>, CorrectionError> {
    match yaml {
        Yaml::BadValue | Yaml::Null => Ok(None),
        _ => f64_value(yaml, item).map(Some),
    }
}

fn optional_bool(yaml: &Yaml, item: &str) -> Result<Option<bool>, CorrectionError> {
    match yaml {
        Yaml::BadValue | Yaml::Null => Ok(None),
        Yaml::Boolean(value) => Ok(Some(*value)),
        _ => invalid(item, "must be true or false"),
    }
}

fn check_channel_count(item: &str, count: usize) -> Result<(), CorrectionError> {
    if count == 0 || count > MAX_CHANNELS {
        return invalid(item, format!("must be 1 through {MAX_CHANNELS}"));
    }
    Ok(())
}

fn invalid<T>(item: impl Into<String>, detail: impl Into<String>) -> Result<T, CorrectionError> {
    Err(CorrectionError::Invalid {
        item: item.into(),
        detail: detail.into(),
    })
}

fn limit<T>(item: impl Into<String>, maximum: usize) -> Result<T, CorrectionError> {
    invalid(item, format!("exceeds limit {maximum}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASIC: &str = r#"
title: Test correction
description: "inputs 0 L, 1 R, 2 LFE | outputs 0 L, 1 R, 2 SW1 | generated"
devices:
  samplerate: 48000
  capture: {channels: 3}
  playback: {channels: 3}
filters:
  trim: {type: Gain, parameters: {gain: -6.020599913, inverted: false}}
  delay: {type: Delay, parameters: {delay: 2, unit: samples, subsample: false}}
mixers:
  route_in: {channels: {in: 3, out: 3}, mapping: [{dest: 0, sources: [{channel: 0}]}, {dest: 1, sources: [{channel: 1}]}, {dest: 2, sources: [{channel: 2}]}]}
  route_out: {channels: {in: 3, out: 3}, mapping: [{dest: 0, sources: [{channel: 0}]}, {dest: 1, sources: [{channel: 1}]}, {dest: 2, sources: [{channel: 2}]}]}
pipeline:
  - {type: Mixer, name: route_in}
  - {type: Filter, channels: [0], names: [trim, delay]}
  - {type: Mixer, name: route_out}
"#;

    #[test]
    fn parses_roles_route_out_and_mask() {
        let correction = parse(BASIC, Path::new("/config")).unwrap();
        assert_eq!(correction.title, "Test correction");
        assert_eq!(correction.lfe_input(), Some(2));
        assert_eq!(correction.layout().unwrap().len(), 2);
        assert_eq!(correction.outputs[2].name.as_deref(), Some("SW1"));
        assert!(correction.outputs[2].subwoofer);
        assert_eq!(correction.channel_mask, 0xb);
        assert_eq!(correction.pipeline.len(), 2);
    }

    #[test]
    fn standard_and_multi_sub_output_names_and_masks_follow_final_route_out() {
        let outputs = [
            "L", "R", "C", "SW1", "Lb", "Rb", "Ls", "Rs", "Tfl", "Tfr", "Tbl", "Tbr",
        ];
        let declared = outputs
            .iter()
            .enumerate()
            .map(|(index, name)| format!("{index} {name}"))
            .collect::<Vec<_>>()
            .join(", ");
        let mapping = (0..12)
            .map(|channel| format!("{{dest: {channel}, sources: [{{channel: {channel}}}]}}"))
            .collect::<Vec<_>>()
            .join(", ");
        let yaml = format!(
            r#"
description: "inputs 0 L, 1 R | outputs {declared}"
devices:
  samplerate: 48000
  capture: {{channels: 2}}
  playback: {{channels: 12}}
filters: {{}}
mixers:
  bus:
    channels: {{in: 2, out: 12}}
    mapping:
      - {{dest: 0, sources: [{{channel: 0}}]}}
      - {{dest: 1, sources: [{{channel: 1}}]}}
      - {{dest: 3, sources: [{{channel: 0}}, {{channel: 1}}]}}
  route_out: {{channels: {{in: 12, out: 12}}, mapping: [{mapping}]}}
pipeline:
  - {{type: Mixer, name: bus}}
  - {{type: Mixer, name: route_out}}
"#
        );
        let correction = parse(&yaml, Path::new(".")).unwrap();
        assert_eq!(correction.channel_mask, 0x2d63f);
        assert_eq!(correction.outputs[3].name.as_deref(), Some("SW1"));

        let three_subs = yaml
            .replace("playback: {channels: 12}", "playback: {channels: 14}")
            .replace("channels: {in: 2, out: 12}", "channels: {in: 2, out: 14}")
            .replace("channels: {in: 12, out: 12}", "channels: {in: 14, out: 14}")
            .replace(
                "      - {dest: 3, sources: [{channel: 0}, {channel: 1}]}\n",
                "      - {dest: 3, sources: [{channel: 0}, {channel: 1}]}\n      - {dest: 12, sources: [{channel: 0}]}\n      - {dest: 13, sources: [{channel: 1}]}\n",
            )
            .replace("outputs 0 L, 1 R, 2 C, 3 SW1, 4 Lb, 5 Rb, 6 Ls, 7 Rs, 8 Tfl, 9 Tfr, 10 Tbl, 11 Tbr", "outputs 0 L, 1 R, 2 C, 3 SW1, 4 Lb, 5 Rb, 6 Ls, 7 Rs, 8 Tfl, 9 Tfr, 10 Tbl, 11 Tbr, 12 SW2, 13 SW3")
            .replace(&format!("mapping: [{mapping}]"), &format!("mapping: [{mapping}, {{dest: 12, sources: [{{channel: 12}}]}}, {{dest: 13, sources: [{{channel: 13}}]}}]"));
        let correction = parse(&three_subs, Path::new(".")).unwrap();
        assert_eq!(correction.channel_mask, 0);
        assert_eq!(
            correction
                .outputs
                .iter()
                .filter(|output| output.subwoofer)
                .count(),
            3
        );
    }

    #[test]
    fn gain_delay_and_mixer_run_end_to_end() {
        let correction = parse(BASIC, Path::new(".")).unwrap();
        let prepared = correction.prepare(48_000, 16).unwrap();
        let mut state = CorrectionState::new(&prepared, 16);
        state.input(0, 16)[0] = 1.0;
        state.input(1, 16)[0] = 0.25;
        state.input(2, 16)[0] = 0.75;
        let mut output = [0.0; 48];
        state.run(&prepared, 16, &mut output);
        assert!((output[2] - 0.5).abs() < 1.0e-5);
        assert_eq!(output[16], 0.25);
        assert_eq!(output[32], 0.75);
        assert!(state.is_quiet());
        state.reset();
        assert!(state.is_quiet());
    }

    #[test]
    fn delays_convert_at_the_target_rate_and_keep_state_across_blocks() {
        let yaml = BASIC
            .replace("delay: 2, unit: samples", "delay: 24, unit: samples")
            .replace("samplerate: 48000", "samplerate: 24000");
        let correction = parse(&yaml, Path::new(".")).unwrap();
        let prepared = correction.prepare(48_000, 16).unwrap();
        let mut state = CorrectionState::new(&prepared, 16);
        let mut output = [0.0; 48];
        state.input(0, 16)[0] = 1.0;
        state.run(&prepared, 16, &mut output);
        assert!(output[..16].iter().all(|sample| *sample == 0.0));
        state.run(&prepared, 16, &mut output);
        assert!(output[..16].iter().all(|sample| *sample == 0.0));
        state.run(&prepared, 16, &mut output);
        assert!(output[..16].iter().all(|sample| *sample == 0.0));
        state.run(&prepared, 16, &mut output);
        assert!((output[0] - 0.5).abs() < 1.0e-5);

        let yaml = BASIC.replace("delay: 2, unit: samples", "delay: 1, unit: ms");
        let correction = parse(&yaml, Path::new(".")).unwrap();
        let prepared = correction.prepare(96_000, 16).unwrap();
        let mut state = CorrectionState::new(&prepared, 16);
        let mut output = [0.0; 48];
        state.input(0, 16)[0] = 1.0;
        for _ in 0..7 {
            state.run(&prepared, 16, &mut output);
        }
        assert!((output[0] - 0.5).abs() < 1.0e-5);
    }

    #[test]
    fn fir_load_validates_relative_path_track_and_duration_and_runs() {
        let directory = std::env::temp_dir().join(format!(
            "correction-fir-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let wav_path = directory.join("filter.wav");
        write_wav(&wav_path, 48_000, 32, 2);
        let config_path = directory.join("profile.yml");
        let yaml = BASIC
            .replace(
                "delay: {type: Delay, parameters: {delay: 2, unit: samples, subsample: false}}",
                "fir: {type: Conv, parameters: {type: Wav, filename: filter.wav, channel: 1}}",
            )
            .replace("names: [trim, delay]", "names: [fir]");
        std::fs::write(&config_path, &yaml).unwrap();
        let correction = load(&config_path).unwrap();
        assert_eq!(correction.path, config_path);
        let prepared = correction.prepare(48_000, 16).unwrap();
        let mut state = CorrectionState::new(&prepared, 16);
        let mut output = [0.0; 48];
        state.input(0, 16)[0] = 1.0;
        state.run(&prepared, 16, &mut output);
        assert!((output[0] - 0.25).abs() < 1.0e-4);

        let missing_track = yaml.replace("channel: 1", "channel: 2");
        std::fs::write(&config_path, missing_track).unwrap();
        assert!(matches!(
            load(&config_path),
            Err(CorrectionError::FirTrack { .. })
        ));

        write_wav(&wav_path, 1, 5, 1);
        let one_track = yaml.replace("channel: 1", "channel: 0");
        std::fs::write(&config_path, one_track).unwrap();
        assert!(matches!(
            load(&config_path),
            Err(CorrectionError::FirTooLong { .. })
        ));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn synthetic_peq_fir_split_bus_allpass_delay_chain_matches_reference() {
        let directory = test_directory("synthetic-chain");
        std::fs::create_dir_all(&directory).unwrap();
        let wav_path = directory.join("filter.wav");
        write_wav_samples(&wav_path, 48_000, &[1.0, 0.25, -0.125]);
        let yaml = r#"
description: "inputs 0 L | outputs 0 L, 1 SW1"
devices:
  samplerate: 48000
  capture: {channels: 1}
  playback: {channels: 2}
filters:
  peq: {type: Biquad, parameters: {type: Peaking, freq: 1000, q: 0.8, gain: 6}}
  fir: {type: Conv, parameters: {type: Wav, filename: filter.wav, channel: 0}}
  allpass: {type: Biquad, parameters: {type: Allpass, freq: 120, q: 0.7}}
  delay: {type: Delay, parameters: {delay: 3, unit: samples}}
mixers:
  split:
    channels: {in: 1, out: 2}
    mapping:
      - {dest: 0, sources: [{channel: 0}]}
      - {dest: 1, sources: [{channel: 0}]}
  bus:
    channels: {in: 2, out: 2}
    mapping:
      - {dest: 0, sources: [{channel: 0}]}
      - {dest: 1, sources: [{channel: 1, inverted: true}]}
  route_out:
    channels: {in: 2, out: 2}
    mapping:
      - {dest: 0, sources: [{channel: 0}]}
      - {dest: 1, sources: [{channel: 1}]}
pipeline:
  - {type: Filter, channels: [0], names: [peq, fir]}
  - {type: Mixer, name: split}
  - {type: Mixer, name: bus}
  - {type: Filter, channels: [1], names: [allpass, delay]}
  - {type: Mixer, name: route_out}
"#;
        let correction = parse(yaml, &directory).unwrap();
        let prepared = correction.prepare(48_000, 16).unwrap();
        let mut state = CorrectionState::new(&prepared, 64);
        let input = (0..64)
            .map(|frame| ((frame * 19 % 31) as f32 - 15.0) / 32.0)
            .collect::<Vec<_>>();
        state.input(0, 64).copy_from_slice(&input);
        let mut output = vec![0.0; 128];
        state.run(&prepared, 64, &mut output);

        let peq = biquad(
            &BiquadSpec::Peaking {
                freq: 1_000.0,
                q: 0.8,
                gain_db: 6.0,
            },
            48_000,
        )
        .unwrap();
        let allpass = biquad(
            &BiquadSpec::Allpass {
                freq: 120.0,
                q: 0.7,
            },
            48_000,
        )
        .unwrap();
        let mut main = input.clone();
        BiquadState::default().process(peq, &mut main);
        main = reference_fir(&main, &[1.0, 0.25, -0.125]);
        let mut sub = main.iter().map(|sample| -*sample).collect::<Vec<_>>();
        BiquadState::default().process(allpass, &mut sub);
        sub = reference_delay(&sub, 3);
        for frame in 0..64 {
            assert!((output[frame] - main[frame]).abs() < 2.0e-5, "main {frame}");
            assert!(
                (output[64 + frame] - sub[frame]).abs() < 2.0e-5,
                "sub {frame}"
            );
        }
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn mimo_convolver_uses_share_fir_data_but_keep_independent_state() {
        let directory = test_directory("mimo-fir");
        std::fs::create_dir_all(&directory).unwrap();
        write_wav_samples(&directory.join("filter.wav"), 48_000, &[1.0, 0.5]);
        let yaml = r#"
description: "inputs 0 L, 1 R, 2 C, 3 LFE | outputs 0 L, 1 R, 2 C, 3 SW1"
devices:
  samplerate: 48000
  capture: {channels: 4}
  playback: {channels: 4}
filters:
  fir: {type: Conv, parameters: {type: Wav, filename: filter.wav, channel: 0}}
mixers:
  route_out:
    channels: {in: 4, out: 4}
    mapping:
      - {dest: 0, sources: [{channel: 0}]}
      - {dest: 1, sources: [{channel: 1}]}
      - {dest: 2, sources: [{channel: 2}]}
      - {dest: 3, sources: [{channel: 3}]}
pipeline:
  - {type: Filter, channels: [0, 1, 2, 3], names: [fir]}
  - {type: Mixer, name: route_out}
"#;
        let correction = parse(yaml, &directory).unwrap();
        let prepared = correction.prepare(48_000, 16).unwrap();
        let mut state = CorrectionState::new(&prepared, 16);
        for (channel, amplitude) in [1.0_f32, 0.0, -0.5, 0.25].into_iter().enumerate() {
            state.input(channel, 16)[0] = amplitude;
        }
        let mut output = [0.0; 64];
        state.run(&prepared, 16, &mut output);
        for (channel, amplitude) in [1.0_f32, 0.0, -0.5, 0.25].into_iter().enumerate() {
            let samples = &output[channel * 16..(channel + 1) * 16];
            assert!((samples[0] - amplitude).abs() < 1.0e-5);
            assert!((samples[1] - amplitude * 0.5).abs() < 1.0e-5);
            assert!(samples[2..].iter().all(|sample| sample.abs() < 1.0e-5));
        }
        assert!(output[16..32].iter().all(|sample| sample.abs() < 1.0e-6));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn nonidentity_route_in_is_skipped() {
        let yaml = BASIC.replacen(
            "{dest: 0, sources: [{channel: 0}]}, {dest: 1, sources: [{channel: 1}]}, {dest: 2, sources: [{channel: 2}]}",
            "{dest: 0, sources: [{channel: 2}]}, {dest: 1, sources: [{channel: 0}]}, {dest: 2, sources: [{channel: 1}]}",
            1,
        );
        let correction = parse(&yaml, Path::new(".")).unwrap();
        let prepared = correction.prepare(48_000, 16).unwrap();
        let mut state = CorrectionState::new(&prepared, 16);
        state.input(0, 16)[0] = 1.0;
        state.input(1, 16)[0] = 0.25;
        state.input(2, 16)[0] = 0.75;
        let mut output = [0.0; 48];
        state.run(&prepared, 16, &mut output);
        assert!((output[2] - 0.5).abs() < 1.0e-5);
        assert_eq!(output[16], 0.25);
        assert_eq!(output[32], 0.75);
    }

    #[test]
    fn mimo_mixers_apply_gain_inversion_mute_and_bypass() {
        let yaml = r#"
description: "inputs 0 L, 1 R | outputs 0 L, 1 R | generated"
devices:
  samplerate: 48000
  capture: {channels: 2}
  playback: {channels: 2}
filters:
  muted: {type: Gain, parameters: {gain: 20, scale: dB, mute: true}}
  inverted: {type: Gain, parameters: {gain: 0, scale: dB, inverted: true}}
mixers:
  fork:
    channels: {in: 2, out: 3}
    mapping:
      - {dest: 0, sources: [{channel: 0, gain: -6.020599913, scale: dB}, {channel: 1, inverted: true}]}
      - {dest: 1, sources: [{channel: 0, mute: true}, {channel: 1}]}
      - {dest: 2, mute: true, sources: [{channel: 0}]}
  merge:
    channels: {in: 3, out: 2}
    mapping:
      - {dest: 0, sources: [{channel: 0}]}
      - {dest: 1, sources: [{channel: 1}, {channel: 2}]}
pipeline:
  - {type: Mixer, name: fork}
  - {type: Filter, channels: [0], names: [muted], bypassed: true}
  - {type: Filter, channels: [1], names: [inverted]}
  - {type: Mixer, name: merge}
"#;
        let correction = parse(yaml, Path::new(".")).unwrap();
        let prepared = correction.prepare(48_000, 16).unwrap();
        let mut state = CorrectionState::new(&prepared, 16);
        let mut output = [0.0; 32];
        state.input(0, 16)[0] = 1.0;
        state.input(1, 16)[0] = 0.25;
        state.run(&prepared, 16, &mut output);
        assert!((output[0] - 0.25).abs() < 1.0e-5);
        assert!((output[16] + 0.25).abs() < 1.0e-5);
    }

    #[test]
    fn rejects_unknown_subtypes_scales_references_channels_and_widths() {
        for (from, to, expected) in [
            (
                "type: Gain, parameters: {gain: -6.020599913, inverted: false}",
                "type: Biquad, parameters: {type: Shelf, freq: 1000, q: 1}",
                "Shelf",
            ),
            (
                "type: Gain, parameters: {gain: -6.020599913, inverted: false}",
                "type: BiquadCombo, parameters: {type: ButterworthLowpass, freq: 1000, order: 9}",
                "order",
            ),
            (
                "type: Gain, parameters: {gain: -6.020599913, inverted: false}",
                "type: Conv, parameters: {type: Raw, filename: filter.wav}",
                "Wav",
            ),
            (
                "gain: -6.020599913",
                "gain: 101, scale: linear",
                "magnitude",
            ),
            (
                "type: Mixer, name: route_out",
                "type: Resampler, name: route_out",
                "Resampler",
            ),
            ("name: route_out", "name: missing", "missing"),
            ("channels: [0]", "channels: [8]", "8"),
            (
                "playback: {channels: 3}",
                "playback: {channels: 4}",
                "expected 4",
            ),
        ] {
            let yaml = BASIC.replace(from, to);
            let error = parse(&yaml, Path::new(".")).unwrap_err().to_string();
            assert!(
                error.contains(expected),
                "{error:?} did not contain {expected:?}"
            );
        }
    }

    fn test_directory(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "correction-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn write_wav(path: &Path, rate: u32, frames: usize, channels: u16) {
        let mut writer = hound::WavWriter::create(
            path,
            hound::WavSpec {
                channels,
                sample_rate: rate,
                bits_per_sample: 32,
                sample_format: hound::SampleFormat::Float,
            },
        )
        .unwrap();
        for frame in 0..frames {
            for channel in 0..channels {
                writer
                    .write_sample(if frame == 0 {
                        0.5 / (f32::from(channel) + 1.0)
                    } else {
                        0.0
                    })
                    .unwrap();
            }
        }
        writer.finalize().unwrap();
    }

    fn write_wav_samples(path: &Path, rate: u32, samples: &[f32]) {
        let mut writer = hound::WavWriter::create(
            path,
            hound::WavSpec {
                channels: 1,
                sample_rate: rate,
                bits_per_sample: 32,
                sample_format: hound::SampleFormat::Float,
            },
        )
        .unwrap();
        for &sample in samples {
            writer.write_sample(sample).unwrap();
        }
        writer.finalize().unwrap();
    }

    fn reference_fir(input: &[f32], taps: &[f32]) -> Vec<f32> {
        (0..input.len())
            .map(|frame| {
                taps.iter()
                    .enumerate()
                    .filter_map(|(tap, coefficient)| {
                        frame
                            .checked_sub(tap)
                            .map(|source| input[source] * coefficient)
                    })
                    .sum()
            })
            .collect()
    }

    fn reference_delay(input: &[f32], delay: usize) -> Vec<f32> {
        (0..input.len())
            .map(|frame| frame.checked_sub(delay).map_or(0.0, |source| input[source]))
            .collect()
    }

    #[test]
    fn input_metadata_defaults_linear_gain_and_filter_channels_are_supported() {
        for yaml in [
            BASIC.replace(
                "description: \"inputs 0 L, 1 R, 2 LFE | outputs 0 L, 1 R, 2 SW1 | generated\"",
                "description: \"inputs 0 L, 1 R, 2 LFE\"",
            ),
            BASIC.replace(
                "description: \"inputs 0 L, 1 R, 2 LFE | outputs 0 L, 1 R, 2 SW1 | generated\"",
                "# inputs 3: L, R, LFE",
            ),
        ] {
            let correction = parse(&yaml, Path::new(".")).unwrap();
            assert_eq!(correction.inputs.len(), 3);
            assert!(
                correction
                    .outputs
                    .iter()
                    .all(|output| output.name.is_none())
            );
        }

        let yaml = BASIC
            .replace(
                "gain: -6.020599913, inverted: false",
                "gain: 0.5, scale: linear, inverted: true",
            )
            .replace("channels: [0], names: [trim, delay]", "names: [trim]");
        let correction = parse(&yaml, Path::new(".")).unwrap();
        let prepared = correction.prepare(48_000, 16).unwrap();
        let mut state = CorrectionState::new(&prepared, 16);
        for channel in 0..3 {
            state.input(channel, 16)[0] = 1.0;
        }
        let mut output = [0.0; 48];
        state.run(&prepared, 16, &mut output);
        assert_eq!(output[0], -0.5);
        assert_eq!(output[16], -0.5);
        assert_eq!(output[32], -0.5);

        let default_ms = BASIC.replace("delay: 2, unit: samples, subsample: false", "delay: 1");
        assert_eq!(
            parse(&default_ms, Path::new("."))
                .unwrap()
                .prepare(48_000, 16)
                .unwrap()
                .tail_frames(),
            48
        );
        let fractional = BASIC.replace(
            "delay: 2, unit: samples, subsample: false",
            "delay: 1.5, unit: samples",
        );
        assert_eq!(
            parse(&fractional, Path::new("."))
                .unwrap()
                .prepare(96_000, 16)
                .unwrap()
                .tail_frames(),
            3
        );
    }

    #[test]
    fn route_out_names_only_identity_destinations_and_follows_reordering() {
        let reordered = BASIC.replace(
            "{dest: 0, sources: [{channel: 0}]}, {dest: 1, sources: [{channel: 1}]}, {dest: 2, sources: [{channel: 2}]}",
            "{dest: 0, sources: [{channel: 2}]}, {dest: 1, sources: [{channel: 0}]}, {dest: 2, sources: [{channel: 1}]}"
        );
        let correction = parse(&reordered, Path::new(".")).unwrap();
        assert_eq!(
            correction
                .outputs
                .iter()
                .map(|output| output.name.as_deref())
                .collect::<Vec<_>>(),
            [Some("SW1"), Some("L"), Some("R")]
        );
        assert_eq!(correction.channel_mask, 0);

        for source in ["{channel: 0, gain: -6}", "{channel: 0, inverted: true}"] {
            let yaml = BASIC.replacen("{channel: 0}", source, 2);
            let correction = parse(&yaml, Path::new(".")).unwrap();
            assert_eq!(correction.outputs[0].name, None);
            assert_eq!(correction.channel_mask, 0);
        }

        let direct = BASIC
            .replace("inputs 0 L, 1 R, 2 LFE", "inputs 0 L, 1 LFE")
            .replace("outputs 0 L, 1 R, 2 SW1", "outputs 0 L, 1 SW1")
            .replace("capture: {channels: 3}", "capture: {channels: 2}")
            .replace("playback: {channels: 3}", "playback: {channels: 2}")
            .replace("channels: {in: 3, out: 3}", "channels: {in: 2, out: 2}")
            .replace(", {dest: 2, sources: [{channel: 2}]}", "")
            .replace(
                "channels: [0], names: [trim, delay]",
                "channels: [0], names: [trim]",
            );
        let correction = parse(&direct, Path::new(".")).unwrap();
        assert_eq!(
            correction
                .outputs
                .iter()
                .map(|output| output.name.as_deref())
                .collect::<Vec<_>>(),
            [Some("L"), Some("SW1")]
        );
    }

    #[test]
    fn role_angle_table_and_surround_back_condition_are_complete() {
        let names = [
            "L", "R", "C", "Ls", "Rs", "Lb", "Rb", "Lw", "Rw", "Tfl", "Tfr", "Tml", "Tmr", "Tbl",
            "Tbr", "Hfl", "Hfr", "Hbl", "Hbr", "LFE",
        ];
        let expected = [
            Some((30.0, 0.0)),
            Some((-30.0, 0.0)),
            Some((0.0, 0.0)),
            Some((90.0, 0.0)),
            Some((-90.0, 0.0)),
            Some((150.0, 0.0)),
            Some((-150.0, 0.0)),
            Some((60.0, 0.0)),
            Some((-60.0, 0.0)),
            Some((45.0, 45.0)),
            Some((-45.0, 45.0)),
            Some((90.0, 45.0)),
            Some((-90.0, 45.0)),
            Some((135.0, 45.0)),
            Some((-135.0, 45.0)),
            Some((30.0, 30.0)),
            Some((-30.0, 30.0)),
            Some((150.0, 30.0)),
            Some((-150.0, 30.0)),
            None,
        ];
        for (&name, expected) in names.iter().zip(expected) {
            assert_eq!(role_angles(name, true).unwrap(), expected);
        }
        assert_eq!(role_angles("Ls", false).unwrap(), Some((110.0, 0.0)));
        assert_eq!(role_angles("Rs", false).unwrap(), Some((-110.0, 0.0)));
    }

    #[test]
    fn resource_numerical_tail_and_nonfinite_limits_are_enforced() {
        let seven_seconds = BASIC.replace("delay: 2, unit: samples", "delay: 7000, unit: ms");
        assert!(
            parse(&seven_seconds, Path::new("."))
                .unwrap()
                .prepare(48_000, 16)
                .unwrap_err()
                .to_string()
                .contains("each delay")
        );
        for (from, to, expected) in [
            ("gain: -6.020599913", "gain: 41", "40"),
            ("gain: -6.020599913", "gain: -151", "-150"),
            ("gain: -6.020599913", "gain: 101, scale: linear", "100"),
        ] {
            assert!(
                parse(&BASIC.replace(from, to), Path::new("."))
                    .unwrap_err()
                    .to_string()
                    .contains(expected)
            );
        }
        let peaking = BASIC.replace(
            "type: Gain, parameters: {gain: -6.020599913, inverted: false}",
            "type: Biquad, parameters: {type: Peaking, freq: 1000, q: 0.0001, gain: 41}",
        );
        assert!(
            parse(&peaking, Path::new("."))
                .unwrap()
                .prepare(48_000, 16)
                .unwrap_err()
                .to_string()
                .contains("biquad")
        );

        let alias = BASIC.replace(
            "capture: {channels: 3}",
            "capture: &capture {channels: 3}\n  ignored_alias: *capture",
        );
        assert!(
            parse(&alias, Path::new("."))
                .unwrap_err()
                .to_string()
                .contains("alias")
        );

        let repeated_sources = std::iter::repeat_n("{channel: 0}", 17)
            .collect::<Vec<_>>()
            .join(", ");
        let repeated_mixer = BASIC
            .replace(
                "  route_out: {channels: {in: 3, out: 3}, mapping: [{dest: 0, sources: [{channel: 0}]}, {dest: 1, sources: [{channel: 1}]}, {dest: 2, sources: [{channel: 2}]}]}",
                &format!("  route_out: {{channels: {{in: 3, out: 3}}, mapping: [{{dest: 0, sources: [{repeated_sources}]}}, {{dest: 1, sources: [{{channel: 1}}]}}, {{dest: 2, sources: [{{channel: 2}}]}}]}}"),
            )
            .replace(
                "  - {type: Filter, channels: [0], names: [trim, delay]}",
                &std::iter::repeat_n("  - {type: Mixer, name: route_out}", 4094)
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
        assert!(
            parse(&repeated_mixer, Path::new("."))
                .unwrap_err()
                .to_string()
                .contains("mixer source edges")
        );

        let directory = test_directory("nonfinite-fir");
        std::fs::create_dir_all(&directory).unwrap();
        write_wav_samples(&directory.join("nan.wav"), 48_000, &[f32::NAN]);
        let nonfinite_fir = BASIC
            .replace(
                "delay: {type: Delay, parameters: {delay: 2, unit: samples, subsample: false}}",
                "nan_fir: {type: Conv, parameters: {type: Wav, filename: nan.wav, channel: 0}}",
            )
            .replace("names: [trim, delay]", "names: [nan_fir]");
        assert!(
            parse(&nonfinite_fir, &directory)
                .unwrap()
                .prepare(48_000, 16)
                .unwrap_err()
                .to_string()
                .contains("not finite")
        );
        std::fs::remove_dir_all(directory).unwrap();

        let correction = parse(BASIC, Path::new(".")).unwrap();
        let prepared = correction.prepare(48_000, 16).unwrap();
        let mut state = CorrectionState::new(&prepared, 16);
        state.input(0, 16)[0] = f32::NAN;
        let mut output = [1.0; 48];
        state.run(&prepared, 16, &mut output);
        assert!(output.iter().all(|sample| *sample == 0.0));
        assert_eq!(state.nonfinite_resets(), 1);
        assert!(state.is_quiet());
    }

    #[test]
    fn flow_and_block_yaml_are_equivalent() {
        let block = BASIC
            .replace("capture: {channels: 3}", "capture:\n    channels: 3")
            .replace(
                "  trim: {type: Gain, parameters: {gain: -6.020599913, inverted: false}}",
                "  trim:\n    type: Gain\n    parameters:\n      gain: -6.020599913\n      inverted: false",
            )
            .replace(
                "  - {type: Filter, channels: [0], names: [trim, delay]}",
                "  - type: Filter\n    channels:\n      - 0\n    names:\n      - trim\n      - delay",
            );
        let flow = parse(BASIC, Path::new(".")).unwrap();
        let block = parse(&block, Path::new(".")).unwrap();
        assert_eq!(flow.inputs, block.inputs);
        assert_eq!(flow.outputs, block.outputs);
        assert_eq!(flow.channel_mask, block.channel_mask);
        let flow_prepared = flow.prepare(48_000, 16).unwrap();
        let block_prepared = block.prepare(48_000, 16).unwrap();
        let mut flow_state = CorrectionState::new(&flow_prepared, 16);
        let mut block_state = CorrectionState::new(&block_prepared, 16);
        for state in [&mut flow_state, &mut block_state] {
            state.input(0, 16)[0] = 1.0;
            state.input(1, 16)[1] = -0.25;
            state.input(2, 16)[2] = 0.75;
        }
        let mut flow_output = [0.0; 48];
        let mut block_output = [0.0; 48];
        flow_state.run(&flow_prepared, 16, &mut flow_output);
        block_state.run(&block_prepared, 16, &mut block_output);
        assert_eq!(flow_output, block_output);
    }

    #[test]
    fn rejects_unknown_role_filter_and_subsample_delay() {
        let role = BASIC.replace("inputs 0 L, 1 R, 2 LFE", "inputs 0 X, 1 R, 2 LFE");
        assert!(
            parse(&role, Path::new("."))
                .unwrap_err()
                .to_string()
                .contains("X")
        );
        let filter = BASIC.replace("type: Gain", "type: Loudness");
        assert!(
            parse(&filter, Path::new("."))
                .unwrap_err()
                .to_string()
                .contains("Loudness")
        );
        let subsample = BASIC.replace("subsample: false", "subsample: true");
        assert!(
            parse(&subsample, Path::new("."))
                .unwrap_err()
                .to_string()
                .contains("subsample")
        );
    }

    #[test]
    fn allpass_and_peaking_match_closed_form_at_three_rates() {
        for rate in [44_100, 48_000, 96_000] {
            let peaking = biquad(
                &BiquadSpec::Peaking {
                    freq: 1_000.0,
                    q: 1.0,
                    gain_db: 6.0,
                },
                rate,
            )
            .unwrap();
            let peak = magnitude(peaking, 1_000.0, rate);
            assert!((20.0 * peak.log10() - 6.0).abs() < 1.0e-8);
            let allpass = biquad(
                &BiquadSpec::Allpass {
                    freq: 800.0,
                    q: 0.7,
                },
                rate,
            )
            .unwrap();
            for frequency in [100.0, 800.0, 8_000.0] {
                assert!((magnitude(allpass, frequency, rate) - 1.0).abs() < 1.0e-10);
            }
        }
    }

    #[test]
    fn butterworth_orders_have_the_corner_response() {
        for order in 1..=8 {
            for kind in [
                ComboKind::ButterworthHighpass,
                ComboKind::ButterworthLowpass,
            ] {
                let sections = combo(
                    &ComboSpec {
                        kind,
                        freq: 1_000.0,
                        order,
                    },
                    48_000,
                )
                .unwrap();
                let magnitude = sections
                    .iter()
                    .map(|section| magnitude(*section, 1_000.0, 48_000))
                    .product::<f64>();
                assert!(
                    (magnitude - 2.0_f64.sqrt().recip()).abs() < 1.0e-9,
                    "order {order}"
                );
            }
        }
    }

    #[test]
    fn linkwitz_riley_four_and_eight_sum_flat() {
        for order in [4, 8] {
            let low = combo(
                &ComboSpec {
                    kind: ComboKind::LinkwitzRileyLowpass,
                    freq: 1_000.0,
                    order,
                },
                48_000,
            )
            .unwrap();
            let high = combo(
                &ComboSpec {
                    kind: ComboKind::LinkwitzRileyHighpass,
                    freq: 1_000.0,
                    order,
                },
                48_000,
            )
            .unwrap();
            for frequency in [20.0, 100.0, 1_000.0, 5_000.0, 20_000.0] {
                let low = response(&low, frequency, 48_000);
                let high = response(&high, frequency, 48_000);
                assert!(
                    ((low + high).norm() - 1.0).abs() < 1.0e-8,
                    "order {order}, {frequency}"
                );
            }
        }
    }

    #[test]
    fn crossover_passbands_stopbands_and_processed_sines_match_transfer_function() {
        for rate in [44_100, 48_000, 96_000] {
            for order in 1..=8 {
                for (kind, pass_frequency, stop_frequency) in [
                    (ComboKind::ButterworthLowpass, 100.0, 10_000.0),
                    (ComboKind::ButterworthHighpass, 10_000.0, 100.0),
                ] {
                    assert_crossover_response(kind, order, rate, pass_frequency, stop_frequency);
                }
            }
            for order in [2, 4, 8] {
                for (kind, pass_frequency, stop_frequency) in [
                    (ComboKind::LinkwitzRileyLowpass, 50.0, 10_000.0),
                    (ComboKind::LinkwitzRileyHighpass, 15_000.0, 50.0),
                ] {
                    assert_crossover_response(kind, order, rate, pass_frequency, stop_frequency);
                }
            }
        }
    }

    fn assert_crossover_response(
        kind: ComboKind,
        order: usize,
        rate: u32,
        pass_frequency: f64,
        stop_frequency: f64,
    ) {
        let sections = combo(
            &ComboSpec {
                kind,
                freq: 1_000.0,
                order,
            },
            rate,
        )
        .unwrap();
        let pass_db = 20.0 * response(&sections, pass_frequency, rate).norm().log10();
        let stop_db = 20.0 * response(&sections, stop_frequency, rate).norm().log10();
        assert!(
            pass_db.abs() <= 0.05,
            "{kind:?} order {order} at {rate}: pass {pass_db}"
        );
        assert!(
            stop_db <= -((20 * order) as f64) + 0.5,
            "{kind:?} order {order} at {rate}: stop {stop_db}"
        );

        let expected = response(&sections, 3_000.0, rate);
        let measured = processed_sine_response(&sections, 3_000.0, rate);
        assert!((measured.norm() - expected.norm()).abs() < 2.0e-4);
        assert!(phase_difference(measured, expected).abs() < 2.0e-4);
    }

    #[test]
    fn peaking_band_edges_and_allpass_phase_match_rbj() {
        for rate in [44_100, 48_000, 96_000] {
            let freq = 1_000.0;
            let q = 1.25;
            let gain_db = 12.0;
            let peaking = biquad(&BiquadSpec::Peaking { freq, q, gain_db }, rate).unwrap();
            let omega = 2.0 * PI * freq / f64::from(rate);
            let bandwidth = 2.0 / 2.0_f64.ln() * (omega.sin() / omega) * (1.0 / (2.0 * q)).asinh();
            for edge in [
                freq * 2.0_f64.powf(-bandwidth / 2.0),
                freq * 2.0_f64.powf(bandwidth / 2.0),
            ] {
                let edge_db = 20.0 * magnitude(peaking, edge, rate).log10();
                assert!(
                    (edge_db - gain_db / 2.0).abs() < 0.01,
                    "{rate} Hz edge {edge}: {edge_db}"
                );
            }
            let allpass = biquad(&BiquadSpec::Allpass { freq, q }, rate).unwrap();
            let at_center = section_response(allpass, freq, rate);
            assert!((at_center.phase().abs() - PI).abs() < 1.0e-10);
        }
    }

    fn processed_sine_response(sections: &[Coefficients], frequency: f64, rate: u32) -> Complex {
        let frames = rate as usize * 2;
        let mut samples = (0..frames)
            .map(|frame| (2.0 * PI * frequency * frame as f64 / f64::from(rate)).cos() as f32)
            .collect::<Vec<_>>();
        for &coefficients in sections {
            BiquadState::default().process(coefficients, &mut samples);
        }
        let start = frames / 2;
        let mut measured = Complex::new(0.0, 0.0);
        for (frame, &sample) in samples.iter().enumerate().skip(start) {
            let angle = -2.0 * PI * frequency * frame as f64 / f64::from(rate);
            measured = measured + Complex::from_angle(angle) * f64::from(sample);
        }
        measured * (2.0 / (frames - start) as f64)
    }

    fn phase_difference(left: Complex, right: Complex) -> f64 {
        (left * right.conjugate()).phase()
    }

    fn response(sections: &[Coefficients], frequency: f64, rate: u32) -> Complex {
        sections
            .iter()
            .fold(Complex::new(1.0, 0.0), |value, section| {
                value * section_response(*section, frequency, rate)
            })
    }

    fn magnitude(section: Coefficients, frequency: f64, rate: u32) -> f64 {
        section_response(section, frequency, rate).norm()
    }

    fn section_response(section: Coefficients, frequency: f64, rate: u32) -> Complex {
        let omega = 2.0 * PI * frequency / f64::from(rate);
        let z1 = Complex::from_angle(-omega);
        let z2 = z1 * z1;
        (Complex::new(section.b0, 0.0) + z1 * section.b1 + z2 * section.b2)
            / (Complex::new(1.0, 0.0) + z1 * section.a1 + z2 * section.a2)
    }

    #[derive(Clone, Copy)]
    struct Complex {
        re: f64,
        im: f64,
    }

    impl Complex {
        fn new(re: f64, im: f64) -> Self {
            Self { re, im }
        }
        fn from_angle(angle: f64) -> Self {
            Self::new(angle.cos(), angle.sin())
        }
        fn norm(self) -> f64 {
            (self.re * self.re + self.im * self.im).sqrt()
        }
        fn phase(self) -> f64 {
            self.im.atan2(self.re)
        }
        fn conjugate(self) -> Self {
            Self::new(self.re, -self.im)
        }
    }

    impl std::ops::Add for Complex {
        type Output = Self;
        fn add(self, rhs: Self) -> Self {
            Self::new(self.re + rhs.re, self.im + rhs.im)
        }
    }
    impl std::ops::Mul for Complex {
        type Output = Self;
        fn mul(self, rhs: Self) -> Self {
            Self::new(
                self.re * rhs.re - self.im * rhs.im,
                self.re * rhs.im + self.im * rhs.re,
            )
        }
    }
    impl std::ops::Mul<f64> for Complex {
        type Output = Self;
        fn mul(self, rhs: f64) -> Self {
            Self::new(self.re * rhs, self.im * rhs)
        }
    }
    impl std::ops::Div for Complex {
        type Output = Self;
        fn div(self, rhs: Self) -> Self {
            let d = rhs.re * rhs.re + rhs.im * rhs.im;
            Self::new(
                (self.re * rhs.re + self.im * rhs.im) / d,
                (self.im * rhs.re - self.re * rhs.im) / d,
            )
        }
    }
}
