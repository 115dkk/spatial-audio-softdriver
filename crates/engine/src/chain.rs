//! The DSP chain: per-endpoint deframe/decode fronts → common-rate scene
//! mixer → render/convolve/gain/limiter back. Runs on the record-pushing thread
//! and never allocates while pushing.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use brir::PreparedBrir;
use convolver::{Binauralizer, MonoConvolver};
use decode::{DecodeConfig, DecodeError, Decoders, MAX_SCENE_FRAMES, Route};
use render::PanGeometry;
use speakers::{BiquadCoefficients, CrossoverCoefficients, PreparedSpeakers};
use ssr_abi::{
    AbiError, MAX_ENDPOINTS, RECORD_ALIGN, RECORD_FLAG_DISCONTINUITY, Record, RecordHeader,
    VolumePayload, volume_gain,
};
use ssr_types::{BitstreamCodec, Feeds, OutputLayout, SceneBlock, SourceFormat};

use crate::MAX_OUTPUT_CHANNELS;
use crate::limiter::Limiter;
use crate::mixer::{
    BedBlock, MAX_ARRIVAL_SLACK_FRAMES, MIX_FRAMES, MIX_RATE, MixerConfig, SceneMixer,
};

const DEFAULT_LFE_GAIN: f32 = 1.0;
const SPEAKER_TAIL_LIMIT_FRAMES: usize = 6 * MIX_RATE as usize;
const BRIR_TAIL_MARGIN_FRAMES: usize = 2 * MIX_RATE as usize;
static SILENT_TAIL_BED: [f32; ssr_types::Layout::MAX_DIRECTIONS * MIX_FRAMES] =
    [0.0; ssr_types::Layout::MAX_DIRECTIONS * MIX_FRAMES];

/// Which render endpoints a chain consumes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EndpointFilter {
    #[default]
    All,
    Only(u16),
    Mask(u32),
}

impl EndpointFilter {
    /// Whether records of `endpoint` go through the chain.
    pub fn includes(self, endpoint: u16) -> bool {
        match self {
            Self::All => (endpoint as usize) < MAX_ENDPOINTS,
            Self::Only(wanted) => endpoint == wanted && (endpoint as usize) < MAX_ENDPOINTS,
            Self::Mask(mask) => {
                (endpoint as usize) < MAX_ENDPOINTS
                    && endpoint < 4
                    && mask & (1_u32 << endpoint) != 0
            }
        }
    }

    fn mask(self) -> u32 {
        (0..MAX_ENDPOINTS)
            .filter(|&endpoint| self.includes(endpoint as u16))
            .fold(0, |mask, endpoint| mask | (1_u32 << endpoint))
    }
}

/// Sizes fixed when the chain is built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChainConfig {
    /// Largest scene block in frames (DD+: 1536).
    pub max_block: usize,
    pub direct: decode::direct::DirectLayouts,
    pub endpoints: EndpointFilter,
    /// Time retained after an interval ends, in 48 kHz timeline frames.
    pub arrival_slack_frames: u64,
}

impl Default for ChainConfig {
    fn default() -> Self {
        Self {
            max_block: 1536,
            direct: decode::direct::DirectLayouts::default(),
            endpoints: EndpointFilter::All,
            arrival_slack_frames: 96,
        }
    }
}

/// The partition used by the one common-rate back end.
pub fn partition_for(block: usize) -> usize {
    if block == 0 {
        return 0;
    }
    if block <= 512 {
        return block;
    }
    (1..=512)
        .rev()
        .find(|candidate| block.is_multiple_of(*candidate))
        .expect("one divides every positive block")
}

fn supported_scene_rate(rate: u32) -> bool {
    matches!(
        rate,
        44_100 | MIX_RATE | 88_200 | 96_000 | 176_400 | 192_000
    )
}

/// Returns the sole back-end shape when a format has a decoder adapter.
pub fn scene_shape(format: &SourceFormat) -> Option<(u32, usize)> {
    let supported = match format {
        SourceFormat::Pcm(pcm) => supported_scene_rate(pcm.sample_rate),
        SourceFormat::Bitstream(stream) => {
            matches!(
                stream.codec,
                BitstreamCodec::Mat20
                    | BitstreamCodec::Mat21
                    | BitstreamCodec::Eac3
                    | BitstreamCodec::Eac3Joc
            ) && (stream.encoded_rate == 0 || supported_scene_rate(stream.encoded_rate))
        }
    };
    supported.then_some((MIX_RATE, MIX_FRAMES))
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChainError {
    #[error(transparent)]
    Decode(#[from] DecodeError),
    #[error("scene rate {0} Hz cannot be converted to the 48 kHz mix rate")]
    UnsupportedSceneRate(u32),
    #[error(transparent)]
    Record(#[from] AbiError),
}

#[derive(Debug, thiserror::Error)]
pub enum PrepareError {
    #[error(transparent)]
    Render(#[from] render::RenderError),
    #[error("max block {max_block} is not a multiple of output partition {partition}")]
    Block { max_block: usize, partition: usize },
    #[error("output device has {channels} channels; supported range is 1..={MAX_OUTPUT_CHANNELS}")]
    OutputChannels { channels: usize },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputKind {
    Headphones,
    Speakers,
}

#[derive(Debug)]
pub enum OutputProfile {
    Headphones(brir::Profile),
    Speakers(speakers::SpeakerProfile),
}

#[derive(Debug, thiserror::Error)]
pub enum OutputProfileError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{path}: output profile kind is not a string or TOML could not be inspected: {detail}")]
    BadKind { path: PathBuf, detail: String },
    #[error("{path}: unknown output profile kind {kind:?}")]
    UnknownKind { path: PathBuf, kind: String },
    #[error(transparent)]
    Headphones(#[from] brir::BrirError),
    #[error(transparent)]
    Speakers(#[from] speakers::SpeakerError),
}

/// Loads a speaker profile when TOML says `kind = "speakers"`; a missing kind
/// retains the legacy BRIR/headphone interpretation.
pub fn load_output_profile(path: &Path) -> Result<OutputProfile, OutputProfileError> {
    let contents = fs::read_to_string(path).map_err(|source| OutputProfileError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let value: toml::Value =
        toml::from_str(&contents).map_err(|error| OutputProfileError::BadKind {
            path: path.to_path_buf(),
            detail: error.to_string(),
        })?;
    match value.get("kind") {
        None => brir::load_profile(path)
            .map(OutputProfile::Headphones)
            .map_err(OutputProfileError::from),
        Some(kind) if kind.as_str() == Some("speakers") => speakers::load_profile(path)
            .map(OutputProfile::Speakers)
            .map_err(OutputProfileError::from),
        Some(kind) if kind.as_str() == Some("headphones") => brir::load_profile(path)
            .map(OutputProfile::Headphones)
            .map_err(OutputProfileError::from),
        Some(kind) => match kind.as_str() {
            Some(kind) => Err(OutputProfileError::UnknownKind {
                path: path.to_path_buf(),
                kind: kind.to_owned(),
            }),
            None => Err(OutputProfileError::BadKind {
                path: path.to_path_buf(),
                detail: "kind is not a string".into(),
            }),
        },
    }
}

impl OutputKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Headphones => "headphones",
            Self::Speakers => "speakers",
        }
    }
}

#[derive(Debug)]
enum PreparedOutput {
    Headphones {
        brir: Arc<PreparedBrir>,
        lfe_route: Option<(ssr_types::DirectionId, f32)>,
        binauralizer: Binauralizer,
    },
    Speakers {
        speakers: Arc<PreparedSpeakers>,
        state: SpeakerOutput,
    },
}

#[derive(Debug)]
pub struct Prepared {
    output: PreparedOutput,
    geometry: Arc<PanGeometry>,
    max_block: usize,
    feeds: Feeds,
    samples: Vec<f32>,
}

impl Prepared {
    pub fn new(brir: Arc<PreparedBrir>, max_block: usize) -> Result<Self, PrepareError> {
        if !max_block.is_multiple_of(brir.partition) {
            return Err(PrepareError::Block {
                max_block,
                partition: brir.partition,
            });
        }
        let geometry = Arc::new(PanGeometry::new(&brir.layout)?);
        if let Some((route, _)) = brir.lfe
            && route.index() >= geometry.directions()
        {
            return Err(render::RenderError::BadLfeRoute(route.0).into());
        }
        let feeds = Feeds::new(brir.layout.len(), max_block);
        let lfe_route = brir.lfe;
        let binauralizer = Binauralizer::new(Arc::clone(&brir.bank), max_block);
        Ok(Self {
            output: PreparedOutput::Headphones {
                brir,
                lfe_route,
                binauralizer,
            },
            geometry,
            max_block,
            feeds,
            samples: vec![0.0; 2 * max_block],
        })
    }

    pub fn speakers(
        speakers: Arc<PreparedSpeakers>,
        max_block: usize,
    ) -> Result<Self, PrepareError> {
        if !(1..=MAX_OUTPUT_CHANNELS).contains(&speakers.device_channels) {
            return Err(PrepareError::OutputChannels {
                channels: speakers.device_channels,
            });
        }
        if !max_block.is_multiple_of(speakers.partition) {
            return Err(PrepareError::Block {
                max_block,
                partition: speakers.partition,
            });
        }
        let geometry = Arc::new(PanGeometry::new(&speakers.layout)?);
        let feeds = Feeds::new(speakers.layout.len(), max_block);
        let state = SpeakerOutput::new(&speakers, max_block);
        let samples = vec![0.0; speakers.device_channels * max_block];
        Ok(Self {
            output: PreparedOutput::Speakers { speakers, state },
            geometry,
            max_block,
            feeds,
            samples,
        })
    }

    /// Panning geometry built from the output profile's layout.
    pub fn geometry(&self) -> &Arc<PanGeometry> {
        &self.geometry
    }

    pub fn channels(&self) -> usize {
        match &self.output {
            PreparedOutput::Headphones { .. } => 2,
            PreparedOutput::Speakers { speakers, .. } => speakers.device_channels,
        }
    }

    pub fn kind(&self) -> OutputKind {
        match self.output {
            PreparedOutput::Headphones { .. } => OutputKind::Headphones,
            PreparedOutput::Speakers { .. } => OutputKind::Speakers,
        }
    }

    pub fn channel_mask(&self) -> u32 {
        self.layout().channel_mask
    }

    pub fn layout(&self) -> OutputLayout {
        match &self.output {
            PreparedOutput::Headphones { .. } => OutputLayout::STEREO,
            PreparedOutput::Speakers { speakers, .. } => OutputLayout {
                channels: speakers.device_channels as u16,
                channel_mask: speakers.channel_mask,
            },
        }
    }

    fn rate(&self) -> u32 {
        match &self.output {
            PreparedOutput::Headphones { brir, .. } => brir.rate,
            PreparedOutput::Speakers { speakers, .. } => speakers.rate,
        }
    }

    fn partition(&self) -> usize {
        match &self.output {
            PreparedOutput::Headphones { brir, .. } => brir.partition,
            PreparedOutput::Speakers { speakers, .. } => speakers.partition,
        }
    }

    fn gain_and_ceiling(&self) -> (f32, f32) {
        match &self.output {
            PreparedOutput::Headphones { brir, .. } => (brir.gain, brir.limiter_ceiling),
            PreparedOutput::Speakers { speakers, .. } => (speakers.gain, speakers.limiter_ceiling),
        }
    }

    fn reset(&mut self) {
        match &mut self.output {
            PreparedOutput::Headphones { binauralizer, .. } => binauralizer.reset(),
            PreparedOutput::Speakers { state, .. } => state.reset(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct BlockOut<'a> {
    pub epoch: u32,
    pub rate: u32,
    pub source_qpc: u64,
    pub channels: usize,
    pub channel_mask: u32,
    pub frames: usize,
    pub samples: &'a [f32],
    pub silent: bool,
    pub idle: bool,
}

impl<'a> BlockOut<'a> {
    pub fn channel(&self, channel: usize) -> &'a [f32] {
        assert!(channel < self.channels, "output channel is out of range");
        let start = channel * self.frames;
        &self.samples[start..start + self.frames]
    }
}

/// The speaker output stage (ADR 0030): bass management, room FIR, alignment
/// delay and trim, per device channel.
#[derive(Debug)]
struct SpeakerOutput {
    /// Per speaker (direction); `Some` for small speakers.
    crossovers: Vec<Option<CrossoverState>>,
    /// Per device channel: the room FIR and its one-input feed.
    firs: Vec<Option<(MonoConvolver, Feeds)>>,
    /// Per device channel.
    delays: Vec<DelayLine>,
    /// Per device channel; 1 where nothing is assigned.
    trims: Vec<f32>,
    work: Vec<f32>,
    bass: Vec<f32>,
}

impl SpeakerOutput {
    fn new(prepared: &PreparedSpeakers, max_block: usize) -> Self {
        let crossovers = prepared
            .speakers
            .iter()
            .map(|speaker| speaker.crossover.map(CrossoverState::new))
            .collect();
        let channels = prepared.device_channels;
        let mut firs: Vec<Option<(MonoConvolver, Feeds)>> = (0..channels).map(|_| None).collect();
        let mut delay_frames = vec![0; channels];
        let mut trims = vec![1.0; channels];
        let loudspeakers = prepared
            .speakers
            .iter()
            .map(|speaker| {
                (
                    speaker.device_channel,
                    speaker.delay_frames,
                    speaker.gain,
                    &speaker.fir,
                )
            })
            .chain(prepared.subwoofers.iter().map(|subwoofer| {
                (
                    subwoofer.device_channel,
                    subwoofer.delay_frames,
                    subwoofer.gain,
                    &subwoofer.fir,
                )
            }));
        for (channel, delay, trim, fir) in loudspeakers {
            delay_frames[channel] = delay;
            trims[channel] = trim;
            firs[channel] = fir.as_ref().map(|bank| {
                (
                    MonoConvolver::new(Arc::clone(bank), max_block),
                    Feeds::new(1, max_block),
                )
            });
        }
        Self {
            crossovers,
            firs,
            delays: delay_frames.into_iter().map(DelayLine::new).collect(),
            trims,
            work: vec![0.0; channels * max_block],
            bass: vec![0.0; max_block],
        }
    }

    fn reset(&mut self) {
        for crossover in self.crossovers.iter_mut().flatten() {
            crossover.reset();
        }
        for (convolver, feed) in self.firs.iter_mut().flatten() {
            convolver.reset();
            feed.clear();
        }
        for delay in &mut self.delays {
            delay.reset();
        }
        self.work.fill(0.0);
        self.bass.fill(0.0);
    }

    fn is_quiet(&self) -> bool {
        self.crossovers
            .iter()
            .flatten()
            .all(CrossoverState::is_quiet)
            && self
                .firs
                .iter()
                .flatten()
                .all(|(convolver, _)| convolver.is_quiet())
            && self.delays.iter().all(DelayLine::is_quiet)
    }

    fn process(
        &mut self,
        prepared: &PreparedSpeakers,
        feeds: &Feeds,
        lfe: Option<&[f32]>,
        frames: usize,
        output: &mut [f32],
        lfe_gain_at: &dyn Fn(usize) -> f32,
    ) {
        let active = prepared.device_channels * frames;
        self.work[..active].fill(0.0);
        self.bass[..frames].fill(0.0);

        // Small speakers keep their high band; their low bands sum in `bass`.
        for (direction, speaker) in prepared.speakers.iter().enumerate() {
            let source = feeds.channel(direction);
            let channel_start = speaker.device_channel * frames;
            let destination = &mut self.work[channel_start..channel_start + frames];
            if let Some(crossover) = &mut self.crossovers[direction] {
                crossover.process(source, destination, &mut self.bass[..frames]);
            } else {
                destination.copy_from_slice(source);
            }
        }
        if let Some(lfe) = lfe {
            for (frame, (bass, sample)) in self.bass[..frames].iter_mut().zip(lfe).enumerate() {
                *bass += *sample * prepared.lfe_gain * lfe_gain_at(frame);
            }
        }
        let bass_channels = prepared
            .subwoofers
            .iter()
            .map(|subwoofer| subwoofer.device_channel)
            .chain(
                prepared
                    .bass_to
                    .iter()
                    .map(|&direction| prepared.speakers[direction].device_channel),
            );
        let share = 1.0 / (prepared.subwoofers.len() + prepared.bass_to.len()).max(1) as f32;
        for channel in bass_channels {
            let start = channel * frames;
            for (destination, bass) in self.work[start..start + frames]
                .iter_mut()
                .zip(&self.bass[..frames])
            {
                *destination += *bass * share;
            }
        }

        // The FIR's own input decides its quiet state: crossover tails keep a
        // channel sounding after the bed falls silent.
        for (channel, fir) in self.firs.iter_mut().enumerate() {
            let Some((convolver, feed)) = fir else {
                continue;
            };
            let start = channel * frames;
            let signal = &mut self.work[start..start + frames];
            let silent = signal.iter().all(|sample| *sample == 0.0);
            if silent && convolver.is_quiet() {
                continue;
            }
            feed.set_frames(frames);
            feed.channel_mut(0).copy_from_slice(signal);
            if !silent {
                convolver.note_active_input();
            }
            convolver.process(feed, signal);
            if silent {
                convolver.note_silent_input(frames);
            }
        }

        for channel in 0..prepared.device_channels {
            let start = channel * frames;
            let output = &mut output[start..start + frames];
            self.delays[channel].process(&self.work[start..start + frames], output);
            let trim = self.trims[channel];
            if trim != 1.0 {
                for sample in output {
                    *sample *= trim;
                }
            }
        }
    }
}

/// Direct form I in `f64`; see `speakers::BiquadCoefficients`.
#[derive(Clone, Copy, Debug, Default)]
struct BiquadState {
    x1: f64,
    x2: f64,
    y1: f64,
    y2: f64,
}

impl BiquadState {
    fn process(&mut self, coefficients: BiquadCoefficients, sample: f64) -> f64 {
        let output =
            coefficients.b0 * sample + coefficients.b1 * self.x1 + coefficients.b2 * self.x2
                - coefficients.a1 * self.y1
                - coefficients.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = sample;
        self.y2 = self.y1;
        self.y1 = output;
        output
    }

    /// A decaying tail would otherwise reach subnormal numbers, which are
    /// slow on x86; below 1e-30 (-600 dB) the state is silence.
    fn flush_tail(&mut self) {
        for value in [&mut self.x1, &mut self.x2, &mut self.y1, &mut self.y2] {
            if value.abs() < 1e-30 {
                *value = 0.0;
            }
        }
    }
}

#[derive(Debug)]
struct CrossoverState {
    coefficients: CrossoverCoefficients,
    low: [BiquadState; 2],
    high: [BiquadState; 2],
}

impl CrossoverState {
    fn new(coefficients: CrossoverCoefficients) -> Self {
        Self {
            coefficients,
            low: [BiquadState::default(); 2],
            high: [BiquadState::default(); 2],
        }
    }

    fn process(&mut self, input: &[f32], high: &mut [f32], bass: &mut [f32]) {
        let [low_a, low_b] = self.coefficients.low;
        let [high_a, high_b] = self.coefficients.high;
        for ((input, high), bass) in input.iter().zip(high).zip(bass) {
            let input = f64::from(*input);
            let low = self.low[0].process(low_a, input);
            let low = self.low[1].process(low_b, low);
            let upper = self.high[0].process(high_a, input);
            let upper = self.high[1].process(high_b, upper);
            *high = upper as f32;
            *bass += low as f32;
        }
        for state in self.low.iter_mut().chain(&mut self.high) {
            state.flush_tail();
        }
    }

    fn reset(&mut self) {
        self.low = [BiquadState::default(); 2];
        self.high = [BiquadState::default(); 2];
    }

    fn is_quiet(&self) -> bool {
        self.low
            .iter()
            .chain(&self.high)
            .flat_map(|state| [state.x1, state.x2, state.y1, state.y2])
            .all(|value| value == 0.0)
    }
}

#[derive(Debug)]
struct DelayLine {
    samples: Vec<f32>,
    next: usize,
}

impl DelayLine {
    fn new(frames: usize) -> Self {
        Self {
            samples: vec![0.0; frames],
            next: 0,
        }
    }

    fn process(&mut self, input: &[f32], output: &mut [f32]) {
        if self.samples.is_empty() {
            output.copy_from_slice(input);
            return;
        }
        for (input, output) in input.iter().zip(output) {
            *output = self.samples[self.next];
            self.samples[self.next] = *input;
            self.next += 1;
            if self.next == self.samples.len() {
                self.next = 0;
            }
        }
    }

    fn reset(&mut self) {
        self.samples.fill(0.0);
        self.next = 0;
    }

    fn is_quiet(&self) -> bool {
        self.samples.iter().all(|sample| *sample == 0.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormatSupport {
    Supported,
    UnsupportedFormat,
    UnsupportedSceneRate,
    UnsupportedDirectLayout,
}

#[derive(Debug)]
pub enum ChainEvent<'a> {
    Block(BlockOut<'a>),
    OutputChannels {
        channels: usize,
    },
    FormatChanged {
        endpoint: u16,
        epoch: u32,
        format: SourceFormat,
        support: FormatSupport,
    },
    NeedsBrir {
        rate: u32,
        block: usize,
    },
    Overrun {
        endpoint: u16,
        lost_bytes: u64,
    },
    Error {
        endpoint: Option<u16>,
        error: ChainError,
    },
    Volume {
        endpoint: u16,
        gain: f32,
        muted: bool,
    },
}

#[derive(Debug)]
struct Front {
    deframer: bitstream::Deframer,
    decoders: Decoders,
    route: Option<Route>,
    format: Option<(u32, SourceFormat)>,
    volume: Option<VolumePayload>,
    byte_rate: u64,
    error_epoch: u32,
    error_kind: Option<ssr_types::ChainErrorKind>,
    transport_remainder: u128,
    block_align: usize,
    deframer_bytes: u64,
}

pub(crate) fn chain_error_kind(error: &ChainError) -> ssr_types::ChainErrorKind {
    use ssr_types::ChainErrorKind;

    match error {
        ChainError::Decode(DecodeError::UnsupportedCodec(_)) => ChainErrorKind::UnsupportedCodec,
        ChainError::Decode(DecodeError::Unsupported { .. }) => ChainErrorKind::UnsupportedFormat,
        ChainError::Decode(DecodeError::Corrupt { .. }) => ChainErrorKind::CorruptPayload,
        ChainError::Decode(DecodeError::NoFormat) => ChainErrorKind::MissingFormat,
        ChainError::Decode(DecodeError::WrongRoute(_)) => ChainErrorKind::WrongRoute,
        ChainError::UnsupportedSceneRate(_) => ChainErrorKind::UnsupportedSceneRate,
        ChainError::Record(AbiError::Truncated { .. }) => ChainErrorKind::TruncatedRecord,
        ChainError::Record(AbiError::BadLength { .. }) => ChainErrorKind::BadRecordLength,
        ChainError::Record(AbiError::BadFormat { .. }) => ChainErrorKind::BadFormatRecord,
        ChainError::Record(AbiError::BadVolume { .. }) => ChainErrorKind::BadVolumeRecord,
        ChainError::Decode(DecodeError::NoDirectLayout(_)) => ChainErrorKind::MissingDirectLayout,
    }
}

fn emit_front_error(
    front: &mut Front,
    suppressed_errors: &mut u64,
    endpoint: u16,
    error: ChainError,
    out: &mut dyn FnMut(ChainEvent<'_>),
) {
    let kind = chain_error_kind(&error);
    let epoch = front.format.map_or(0, |value| value.0);
    if front.error_epoch == epoch && front.error_kind == Some(kind) {
        *suppressed_errors = suppressed_errors.saturating_add(1);
    } else {
        front.error_epoch = epoch;
        front.error_kind = Some(kind);
        out(ChainEvent::Error {
            endpoint: Some(endpoint),
            error,
        });
    }
}

fn qpc_before_end(qpc: u64, bytes_through_end: usize, byte_rate: u64, frequency: u64) -> u64 {
    if byte_rate == 0 {
        return qpc;
    }
    let ticks = bytes_through_end as u128 * u128::from(frequency) / u128::from(byte_rate);
    qpc.saturating_sub(ticks.min(u128::from(u64::MAX)) as u64)
}

fn error_endpoint(error: &AbiError) -> Option<u16> {
    match error {
        AbiError::Truncated { endpoint, .. } => *endpoint,
        AbiError::BadLength { endpoint, .. }
        | AbiError::BadFormat { endpoint, .. }
        | AbiError::BadVolume { endpoint, .. } => Some(*endpoint),
    }
}

fn rebase_abi_error(error: AbiError, base: usize) -> AbiError {
    match error {
        AbiError::BadVolume { offset, endpoint } => AbiError::BadVolume {
            offset: base.saturating_add(offset),
            endpoint,
        },
        AbiError::Truncated {
            offset,
            endpoint,
            need,
            have,
        } => AbiError::Truncated {
            offset: base.saturating_add(offset),
            endpoint,
            need,
            have,
        },
        AbiError::BadLength {
            offset,
            endpoint,
            kind,
            len,
        } => AbiError::BadLength {
            offset: base.saturating_add(offset),
            endpoint,
            kind,
            len,
        },
        AbiError::BadFormat {
            offset,
            endpoint,
            source,
        } => AbiError::BadFormat {
            offset: base.saturating_add(offset),
            endpoint,
            source,
        },
    }
}

const RECORD_HEADER_SIZE: usize = size_of::<RecordHeader>();

#[derive(Debug)]
struct RecordCursor<'a> {
    stream: &'a [u8],
    offset: usize,
    head: Option<Result<Record<'a>, AbiError>>,
    data_offset: usize,
    start_processed: bool,
    done: bool,
}

fn padded_record_size(payload_len: usize) -> usize {
    RECORD_HEADER_SIZE
        .saturating_add(payload_len)
        .div_ceil(RECORD_ALIGN as usize)
        .saturating_mul(RECORD_ALIGN as usize)
}

fn read_record_header(bytes: &[u8]) -> Option<RecordHeader> {
    if bytes.len() < RECORD_HEADER_SIZE {
        return None;
    }
    Some(RecordHeader {
        kind: u16::from_ne_bytes(bytes[0..2].try_into().ok()?),
        flags: u16::from_ne_bytes(bytes[2..4].try_into().ok()?),
        epoch: u32::from_ne_bytes(bytes[4..8].try_into().ok()?),
        byte_pos: u64::from_ne_bytes(bytes[8..16].try_into().ok()?),
        qpc: u64::from_ne_bytes(bytes[16..24].try_into().ok()?),
        qpc_written: u64::from_ne_bytes(bytes[24..32].try_into().ok()?),
        len: u32::from_ne_bytes(bytes[32..36].try_into().ok()?),
        endpoint: u16::from_ne_bytes(bytes[36..38].try_into().ok()?),
        reserved: u16::from_ne_bytes(bytes[38..40].try_into().ok()?),
    })
}

fn record_owner(header: &RecordHeader) -> usize {
    (header.endpoint as usize).min(MAX_ENDPOINTS - 1)
}

fn next_for_endpoint<'a>(cursor: &mut RecordCursor<'a>, endpoint: usize) {
    while !cursor.done && cursor.offset < cursor.stream.len() {
        let rest = &cursor.stream[cursor.offset..];
        let Some(header) = read_record_header(rest) else {
            cursor.done = true;
            cursor.head = (endpoint == 0).then_some(Err(AbiError::Truncated {
                offset: cursor.offset,
                endpoint: None,
                need: RECORD_HEADER_SIZE,
                have: rest.len(),
            }));
            return;
        };
        let total = RECORD_HEADER_SIZE.saturating_add(header.len as usize);
        if rest.len() < total {
            cursor.done = true;
            let owner = record_owner(&header);
            cursor.head = (owner == endpoint).then_some(Err(AbiError::Truncated {
                offset: cursor.offset,
                endpoint: Some(header.endpoint),
                need: total,
                have: rest.len(),
            }));
            return;
        }
        let owner = record_owner(&header);
        if owner != endpoint {
            cursor.offset = cursor
                .offset
                .saturating_add(padded_record_size(header.len as usize))
                .min(cursor.stream.len());
            continue;
        }
        let physical_size = padded_record_size(header.len as usize).min(rest.len());
        let Some(item) = ssr_abi::records(&rest[..physical_size]).next() else {
            // The parser skips kinds it does not know. Step over this one
            // record and keep looking for the endpoint's next record.
            cursor.offset = cursor
                .offset
                .saturating_add(physical_size)
                .min(cursor.stream.len());
            continue;
        };
        cursor.head = Some(item.map_err(|error| rebase_abi_error(error, cursor.offset)));
        return;
    }
    cursor.head = None;
}

impl Front {
    fn new(max_block: usize, direct: decode::direct::DirectLayouts) -> Self {
        Self {
            deframer: bitstream::Deframer::new(),
            // The decoders take whole source blocks (PCM 10 ms up to 192 kHz),
            // which can exceed the output stage's block.
            decoders: Decoders::new(DecodeConfig {
                max_frames: max_block.max(MAX_SCENE_FRAMES),
                direct,
            }),
            route: None,
            format: None,
            volume: None,
            byte_rate: 0,
            error_epoch: 0,
            error_kind: None,
            transport_remainder: 0,
            block_align: 1,
            deframer_bytes: 0,
        }
    }

    fn reset(&mut self) {
        self.deframer.reset();
        self.decoders.reset();
        self.transport_remainder = 0;
        self.deframer_bytes = 0;
    }

    fn configure_transport(&mut self, format: SourceFormat) {
        self.deframer_bytes = 0;
        match format {
            SourceFormat::Pcm(pcm) => {
                self.block_align = pcm.bytes_per_frame();
                self.byte_rate = u64::from(pcm.sample_rate).saturating_mul(self.block_align as u64);
            }
            SourceFormat::Bitstream(stream) => {
                self.block_align = 1;
                self.byte_rate = u64::from(stream.container_rate)
                    .saturating_mul(u64::from(stream.container_channels))
                    .saturating_mul(2);
            }
        }
        self.transport_remainder = 0;
    }

    fn chunk_bytes(&self) -> usize {
        if self.byte_rate == 0 {
            return 1;
        }
        let maximum = (self.byte_rate / 100).max(1) as usize;
        if self.block_align == 1 {
            maximum
        } else {
            (maximum / self.block_align * self.block_align).max(self.block_align)
        }
    }

    fn advance_bytes(&mut self, bytes: usize) -> u64 {
        if self.byte_rate == 0 {
            return 0;
        }
        let numerator = self
            .transport_remainder
            .saturating_add(bytes as u128 * u128::from(MIX_RATE));
        let frames = numerator / u128::from(self.byte_rate);
        self.transport_remainder = numerator % u128::from(self.byte_rate);
        frames.min(u128::from(u64::MAX)) as u64
    }
}

#[derive(Debug)]
struct Back {
    prepared: Option<Box<Prepared>>,
    limiter: Limiter,
    lfe_gain: f32,
    lfe_from: f32,
    lfe_target: f32,
    lfe_ramp_frames: usize,
    last_needs_brir: bool,
    #[cfg(test)]
    disable_quiet_fast_path: bool,
}

impl Back {
    fn new() -> Self {
        Self {
            prepared: None,
            limiter: Limiter::new(),
            lfe_gain: DEFAULT_LFE_GAIN,
            lfe_from: DEFAULT_LFE_GAIN,
            lfe_target: DEFAULT_LFE_GAIN,
            lfe_ramp_frames: 0,
            last_needs_brir: false,
            #[cfg(test)]
            disable_quiet_fast_path: false,
        }
    }

    fn set_lfe_gain(&mut self, gain: f32) {
        self.lfe_from = self.lfe_gain;
        self.lfe_target = gain;
        self.lfe_ramp_frames = MIX_FRAMES;
    }

    fn is_quiet(&self) -> bool {
        self.prepared
            .as_ref()
            .is_none_or(|prepared| match &prepared.output {
                PreparedOutput::Headphones { binauralizer, .. } => binauralizer.is_quiet(),
                PreparedOutput::Speakers { state, .. } => state.is_quiet(),
            })
    }

    fn process(&mut self, bed: &BedBlock<'_>, out: &mut dyn FnMut(ChainEvent<'_>)) {
        let fits = self.prepared.as_ref().is_some_and(|prepared| {
            prepared.rate() == MIX_RATE
                && bed.rate == MIX_RATE
                && bed.frames == MIX_FRAMES
                && bed.frames <= prepared.max_block
                && bed.frames.is_multiple_of(prepared.partition())
                && bed.directions == prepared.feeds.directions()
                && bed.channels.len() == bed.directions * bed.frames
        });
        if !fits {
            if !self.last_needs_brir {
                self.last_needs_brir = true;
                out(ChainEvent::NeedsBrir {
                    rate: MIX_RATE,
                    block: MIX_FRAMES,
                });
            }
            return;
        }

        let prepared = self
            .prepared
            .as_mut()
            .expect("fit requires prepared output");
        prepared.feeds.set_frames(bed.frames);
        for direction in 0..bed.directions {
            let start = direction * bed.frames;
            prepared
                .feeds
                .channel_mut(direction)
                .copy_from_slice(&bed.channels[start..start + bed.frames]);
        }

        let channels = prepared.channels();
        let channel_mask = prepared.channel_mask();
        let active = channels * bed.frames;
        let samples = &mut prepared.samples[..active];
        let lfe_from = self.lfe_from;
        let lfe_target = self.lfe_target;
        let lfe_ramp_frames = self.lfe_ramp_frames;
        let lfe_gain_at = |frame: usize| {
            if lfe_ramp_frames == 0 || frame >= lfe_ramp_frames {
                lfe_target
            } else {
                lfe_from + (lfe_target - lfe_from) * frame as f32 / lfe_ramp_frames as f32
            }
        };
        match &mut prepared.output {
            PreparedOutput::Headphones {
                lfe_route,
                binauralizer,
                ..
            } => {
                if let (Some((route, gain)), Some(lfe)) = (*lfe_route, bed.lfe) {
                    for (frame, (output, sample)) in prepared
                        .feeds
                        .channel_mut(route.index())
                        .iter_mut()
                        .zip(lfe)
                        .enumerate()
                    {
                        *output += *sample * gain * lfe_gain_at(frame);
                    }
                }
                #[cfg(test)]
                let use_quiet_fast_path = !self.disable_quiet_fast_path;
                #[cfg(not(test))]
                let use_quiet_fast_path = true;
                let (left, right) = samples.split_at_mut(bed.frames);
                if bed.silent && use_quiet_fast_path && binauralizer.is_quiet() {
                    left.fill(0.0);
                    right.fill(0.0);
                } else {
                    if !bed.silent {
                        binauralizer.note_active_input();
                    }
                    binauralizer.process(&prepared.feeds, left, right);
                    if bed.silent {
                        binauralizer.note_silent_input(bed.frames);
                    }
                }
            }
            PreparedOutput::Speakers { speakers, state } => state.process(
                speakers,
                &prepared.feeds,
                bed.lfe,
                bed.frames,
                samples,
                &lfe_gain_at,
            ),
        }
        if self.lfe_ramp_frames != 0 {
            if bed.frames >= self.lfe_ramp_frames {
                self.lfe_gain = self.lfe_target;
                self.lfe_from = self.lfe_target;
                self.lfe_ramp_frames = 0;
            } else {
                self.lfe_gain = lfe_gain_at(bed.frames);
                self.lfe_from = self.lfe_gain;
                self.lfe_ramp_frames -= bed.frames;
            }
        }
        self.limiter.process(samples, channels, bed.frames);
        let silent = samples.iter().all(|sample| *sample == 0.0);
        out(ChainEvent::Block(BlockOut {
            epoch: bed.epoch,
            rate: MIX_RATE,
            source_qpc: bed.source_qpc,
            channels,
            channel_mask,
            frames: bed.frames,
            samples,
            silent,
            idle: bed.idle,
        }));
    }
}

#[derive(Debug)]
pub struct Chain {
    config: ChainConfig,
    fronts: Box<[Front]>,
    mixer: SceneMixer,
    back: Back,
    /// The installed set's panning geometry, held apart from `back` so lanes
    /// can pan while the back end renders. Replaced in `set_prepared` before
    /// the old set goes back to the caller, so this reference is never the
    /// last one and the geometry is never freed on the real-time thread.
    geometry: Option<Arc<PanGeometry>>,
    qpc_frequency: u64,
    skipped_records: u64,
    timeline_qpc: Option<u64>,
    anchor: Option<(u64, u64)>,
    idle_advanced_since_clock: bool,
    suppressed_errors: u64,
    finish: Option<FinishState>,
}

#[derive(Clone, Copy, Debug)]
struct FinishState {
    front: usize,
    mixer_finished: bool,
    back_frames: usize,
    back_limit: usize,
}

impl Chain {
    pub fn new(config: ChainConfig) -> Self {
        Self::with_qpc_frequency(config, 10_000_000)
    }

    /// Builds a chain for a source clock whose ticks advance at `qpc_frequency`.
    pub fn with_qpc_frequency(mut config: ChainConfig, qpc_frequency: u64) -> Self {
        config.arrival_slack_frames = config.arrival_slack_frames.min(MAX_ARRIVAL_SLACK_FRAMES);
        Self {
            config,
            fronts: (0..MAX_ENDPOINTS)
                .map(|_| Front::new(config.max_block, config.direct))
                .collect(),
            mixer: SceneMixer::new(MixerConfig {
                arrival_slack_frames: config.arrival_slack_frames,
                qpc_frequency,
                endpoint_mask: config.endpoints.mask(),
            }),
            back: Back::new(),
            qpc_frequency,
            skipped_records: 0,
            timeline_qpc: None,
            anchor: None,
            idle_advanced_since_clock: false,
            suppressed_errors: 0,
            finish: None,
            geometry: None,
        }
    }

    pub fn set_prepared(&mut self, prepared: Box<Prepared>) -> Option<Box<Prepared>> {
        self.set_prepared_with(prepared, &mut |_| {})
    }

    /// Changes the LFE route without replacing prepared output state. The next
    /// common 10 ms block linearly reaches `gain`; preceding blocks are untouched.
    pub fn set_lfe_gain(&mut self, gain: f32) {
        self.back.set_lfe_gain(gain);
    }

    pub fn set_prepared_with(
        &mut self,
        mut prepared: Box<Prepared>,
        out: &mut dyn FnMut(ChainEvent<'_>),
    ) -> Option<Box<Prepared>> {
        let previous_channels = self.back.prepared.as_ref().map(|output| output.channels());
        let channels = prepared.channels();
        self.mixer
            .change_layout(self.geometry.as_deref(), &prepared.geometry);
        self.geometry = Some(Arc::clone(&prepared.geometry));
        prepared.reset();
        let (gain, ceiling) = prepared.gain_and_ceiling();
        self.back.limiter.reset(MIX_RATE, gain, ceiling);
        prepared.samples.fill(0.0);
        self.back.last_needs_brir = false;
        let previous = self.back.prepared.replace(prepared);
        if previous_channels != Some(channels) {
            out(ChainEvent::OutputChannels { channels });
        }
        previous
    }

    pub fn arrival_slack_frames(&self) -> u64 {
        self.config.arrival_slack_frames
    }

    /// Records skipped because their endpoint is outside the driver's endpoint range.
    pub fn skipped_records(&self) -> u64 {
        self.skipped_records
    }

    /// The QPC and timeline frame a lane's start is placed against: another
    /// clocked lane's latest progress, else the idle clock once it has moved.
    fn start_reference(&self, endpoint: u16) -> Option<(u64, u64)> {
        if self.mixer.has_clocked_lane_except(endpoint) {
            self.timeline_qpc.map(|qpc| (qpc, self.mixer.now()))
        } else if self.idle_advanced_since_clock {
            self.anchor
        } else {
            None
        }
    }

    fn start_offset(&self, endpoint: u16, event_qpc: u64) -> i64 {
        let Some((timeline_qpc, anchor_frame)) = self.start_reference(endpoint) else {
            return 0;
        };
        if event_qpc == 0 || timeline_qpc == 0 || self.qpc_frequency == 0 {
            return 0;
        }
        let delta = i128::from(event_qpc) - i128::from(timeline_qpc);
        let event_frame = i128::from(anchor_frame).saturating_add(
            delta.saturating_mul(i128::from(MIX_RATE)) / i128::from(self.qpc_frequency),
        );
        let frames = event_frame.saturating_sub(i128::from(self.mixer.now()));
        let back = (MIX_FRAMES as i128 + i128::from(self.config.arrival_slack_frames))
            .min(i128::from(i64::MAX));
        // Processing-order skew inside one READ is at most one <=10 ms chunk.
        // Anything larger means the other lane's reference QPC is stale. A start
        // moves back by at most a chunk plus slack and forward by at most a
        // chunk, which keeps its first scene inside the mixer's ahead limit for
        // any slack.
        frames.clamp(-back, MIX_FRAMES as i128) as i64
    }

    fn lane_start_at(&mut self, endpoint: u16, event_qpc: u64) {
        let referenced = self.start_reference(endpoint).is_some();
        self.mixer
            .lane_start(endpoint, self.start_offset(endpoint, event_qpc));
        self.anchor_unreferenced_start(referenced, event_qpc);
    }

    fn lane_discontinuity_at(&mut self, endpoint: u16, event_qpc: u64) {
        let referenced = self.start_reference(endpoint).is_some();
        self.mixer
            .lane_discontinuity(endpoint, self.start_offset(endpoint, event_qpc));
        self.anchor_unreferenced_start(referenced, event_qpc);
    }

    /// A start with nothing to place it against begins at `now`, so its event
    /// QPC is `now` from then on. Without this, an idle anchor taken before
    /// the lane existed (the first idle only records one) put the same frame
    /// at another QPC; idle then moved `now` ahead of the lane before its
    /// first data, the lane stayed behind every later lane for good, and was
    /// counted late whenever they drove `now`.
    fn anchor_unreferenced_start(&mut self, referenced: bool, event_qpc: u64) {
        if !referenced && event_qpc != 0 {
            self.anchor = Some((event_qpc, self.mixer.now()));
            self.idle_advanced_since_clock = false;
        }
    }

    fn emit_error(
        &mut self,
        endpoint: Option<u16>,
        error: ChainError,
        out: &mut dyn FnMut(ChainEvent<'_>),
    ) {
        let Some(endpoint) = endpoint.filter(|value| (*value as usize) < MAX_ENDPOINTS) else {
            out(ChainEvent::Error { endpoint, error });
            return;
        };
        emit_front_error(
            &mut self.fronts[endpoint as usize],
            &mut self.suppressed_errors,
            endpoint,
            error,
            out,
        );
    }

    /// Processes one already-parsed record chunk strictly in call order.
    fn push_record_chunk(&mut self, record: Record<'_>, out: &mut dyn FnMut(ChainEvent<'_>)) {
        let endpoint = record.endpoint();
        if !self.config.endpoints.includes(endpoint) {
            if endpoint as usize >= MAX_ENDPOINTS {
                self.skipped_records = self.skipped_records.saturating_add(1);
            }
            return;
        }
        let index = endpoint as usize;
        match record {
            Record::Volume {
                qpc,
                repeat,
                channel_count,
                mute_mask,
                levels,
                ..
            } => {
                let payload = VolumePayload {
                    channel_count,
                    mute_mask,
                    level_q16: levels,
                };
                if repeat && self.fronts[index].volume == Some(payload) {
                    return;
                }
                self.fronts[index].volume = Some(payload);
                let gain = volume_gain(&payload);
                // The record parser admits 1..=32 channels; 32 must not shift by 32.
                let all_mask = u32::MAX >> (32 - channel_count.clamp(1, 32));
                let muted = mute_mask == all_mask;
                // Volume follows the same QPC-to-timeline mapping as starts.
                // Applying it at emission also changes already placed but not yet
                // emitted current and tail samples, and idle lanes retain it.
                let frame = u64::try_from(
                    i128::from(self.mixer.now()) + i128::from(self.start_offset(endpoint, qpc)),
                )
                .unwrap_or(0);
                self.mixer.set_lane_volume(endpoint, gain, frame);
                out(ChainEvent::Volume {
                    endpoint,
                    gain,
                    muted,
                });
            }
            Record::FormatChange {
                epoch,
                format,
                qpc,
                repeat,
                ..
            } => {
                if repeat && self.fronts[index].format == Some((epoch, format)) {
                    return;
                }
                let epoch_changed = self.fronts[index]
                    .format
                    .is_none_or(|(previous, _)| previous != epoch);
                self.fronts[index].format = Some((epoch, format));
                if epoch_changed {
                    self.fronts[index].error_epoch = epoch;
                    self.fronts[index].error_kind = None;
                }
                self.fronts[index].deframer.reset();
                self.fronts[index].configure_transport(format);
                let unsupported_rate = match format {
                    SourceFormat::Pcm(pcm) => {
                        (!supported_scene_rate(pcm.sample_rate)).then_some(pcm.sample_rate)
                    }
                    SourceFormat::Bitstream(stream)
                        if matches!(
                            stream.codec,
                            BitstreamCodec::Mat20
                                | BitstreamCodec::Mat21
                                | BitstreamCodec::Eac3
                                | BitstreamCodec::Eac3Joc
                        ) =>
                    {
                        (stream.encoded_rate != 0 && !supported_scene_rate(stream.encoded_rate))
                            .then_some(stream.encoded_rate)
                    }
                    SourceFormat::Bitstream(_) => None,
                };
                let error = if let Some(rate) = unsupported_rate {
                    self.fronts[index].route = None;
                    Some(ChainError::UnsupportedSceneRate(rate))
                } else {
                    match self.fronts[index].decoders.select(&format, epoch) {
                        Ok(route) => {
                            self.fronts[index].route = Some(route);
                            None
                        }
                        Err(error) => {
                            self.fronts[index].route = None;
                            Some(ChainError::Decode(error))
                        }
                    }
                };
                let support = match error.as_ref() {
                    Some(ChainError::UnsupportedSceneRate(_)) => {
                        FormatSupport::UnsupportedSceneRate
                    }
                    Some(ChainError::Decode(DecodeError::NoDirectLayout(_))) => {
                        FormatSupport::UnsupportedDirectLayout
                    }
                    Some(ChainError::Decode(
                        DecodeError::UnsupportedCodec(_) | DecodeError::Unsupported { .. },
                    )) => FormatSupport::UnsupportedFormat,
                    _ => FormatSupport::Supported,
                };
                out(ChainEvent::FormatChanged {
                    endpoint,
                    epoch,
                    format,
                    support,
                });
                if let Some(error) = error {
                    self.emit_error(Some(endpoint), error, out);
                }
                self.lane_discontinuity_at(endpoint, qpc);
            }
            Record::Data {
                flags, bytes, qpc, ..
            } => {
                if flags & RECORD_FLAG_DISCONTINUITY != 0 {
                    self.fronts[index].reset();
                    self.lane_discontinuity_at(endpoint, qpc);
                }
                match self.fronts[index].route {
                    Some(Route::Pcm) => self.push_pcm(index, endpoint, bytes, qpc, out),
                    Some(Route::Bitstream) => self.push_bitstream(index, endpoint, bytes, qpc, out),
                    None => {}
                }
            }
            Record::Overrun {
                lost_bytes, qpc, ..
            } => {
                self.fronts[index].reset();
                self.lane_discontinuity_at(endpoint, qpc);
                out(ChainEvent::Overrun {
                    endpoint,
                    lost_bytes,
                });
            }
            Record::StreamStart { qpc, repeat, .. } => {
                if !repeat || self.mixer.lane_is_idle(endpoint) {
                    self.fronts[index].reset();
                    self.lane_start_at(endpoint, qpc);
                }
            }
            Record::StreamStop { repeat, .. } => {
                if !repeat || !self.mixer.lane_is_idle(endpoint) {
                    self.flush_front(index, endpoint, out);
                    self.fronts[index].reset();
                    let (mixer, back) = (&mut self.mixer, &mut self.back);
                    mixer.lane_stop(endpoint, &mut |scene| back.process(scene, out));
                }
            }
        }
    }

    /// Processes one record with the same start handling and 10 ms chunking as
    /// a one-record READ passed to [`Self::push_records`].
    pub fn push_record(&mut self, record: Record<'_>, out: &mut dyn FnMut(ChainEvent<'_>)) {
        let Record::Data {
            endpoint,
            epoch,
            flags,
            pos,
            qpc,
            qpc_written,
            bytes,
        } = record
        else {
            self.push_record_chunk(record, out);
            return;
        };
        if !self.config.endpoints.includes(endpoint) {
            self.push_record_chunk(
                Record::Data {
                    endpoint,
                    epoch,
                    flags,
                    pos,
                    qpc,
                    qpc_written,
                    bytes,
                },
                out,
            );
            return;
        }
        let index = endpoint as usize;
        if flags & RECORD_FLAG_DISCONTINUITY != 0 || self.mixer.lane_is_idle(endpoint) {
            let first_qpc = qpc_before_end(
                qpc,
                bytes.len(),
                self.fronts[index].byte_rate,
                self.qpc_frequency,
            );
            self.fronts[index].reset();
            if self.mixer.lane_is_idle(endpoint) {
                self.lane_start_at(endpoint, first_qpc);
            } else {
                self.lane_discontinuity_at(endpoint, first_qpc);
            }
        }
        let mut start = 0;
        while start < bytes.len() {
            let remaining = bytes.len() - start;
            let chunk_len = if self.fronts[index].byte_rate == 0 {
                remaining
            } else {
                self.fronts[index].chunk_bytes().min(remaining)
            };
            if chunk_len == 0 {
                break;
            }
            let end = start + chunk_len;
            let chunk_end_qpc = qpc_before_end(
                qpc,
                bytes.len() - end,
                self.fronts[index].byte_rate,
                self.qpc_frequency,
            );
            let before = self.mixer.now();
            self.push_record_chunk(
                Record::Data {
                    endpoint,
                    epoch,
                    flags: 0,
                    pos: pos.saturating_add(start as u64),
                    qpc: chunk_end_qpc,
                    qpc_written,
                    bytes: &bytes[start..end],
                },
                out,
            );
            self.update_anchor_after_progress(before, chunk_end_qpc);
            start = end;
        }
    }

    fn update_anchor_after_progress(&mut self, before: u64, qpc: u64) {
        if self.mixer.now() > before && qpc != 0 {
            self.timeline_qpc = Some(self.timeline_qpc.map_or(qpc, |current| current.max(qpc)));
            self.anchor = Some((qpc, self.mixer.now()));
            self.idle_advanced_since_clock = false;
        }
    }

    pub fn idle(&mut self, qpc: u64, out: &mut dyn FnMut(ChainEvent<'_>)) {
        if self.mixer.has_clocked_lane() || qpc == 0 || self.qpc_frequency == 0 {
            return;
        }
        let Some((anchor_qpc, anchor_frame)) = self.anchor else {
            self.anchor = Some((qpc, self.mixer.now()));
            return;
        };
        let delta = i128::from(qpc) - i128::from(anchor_qpc);
        if delta <= 0 {
            return;
        }
        let target = i128::from(anchor_frame).saturating_add(
            delta.saturating_mul(i128::from(MIX_RATE)) / i128::from(self.qpc_frequency),
        );
        let Ok(target) = u64::try_from(target) else {
            return;
        };
        let before = self.mixer.now();
        let (mixer, back) = (&mut self.mixer, &mut self.back);
        mixer.idle_to(target, &mut |scene| back.process(scene, out));
        let advanced = mixer.now() > before;
        let now = mixer.now();
        self.anchor = Some((qpc, now));
        self.idle_advanced_since_clock |= advanced;
    }

    /// Starts the bounded live finish state. `finish_step` then advances it in
    /// small no-allocation batches so a full crossing ring can drain between
    /// batches. Offline callers retain the one-shot `finish` wrapper below.
    pub fn begin_finish(&mut self) {
        let back_limit = self
            .back
            .prepared
            .as_ref()
            .map_or(0, |prepared| match &prepared.output {
                PreparedOutput::Headphones { brir, .. } => {
                    brir.ir_frames.saturating_add(BRIR_TAIL_MARGIN_FRAMES)
                }
                PreparedOutput::Speakers { .. } => SPEAKER_TAIL_LIMIT_FRAMES,
            });
        self.finish = Some(FinishState {
            front: 0,
            mixer_finished: false,
            back_frames: 0,
            back_limit,
        });
    }

    /// Emits no more than `block_budget` output blocks. Returns true once all
    /// decoder, mixer and prepared-output state is quiet. A zero budget may
    /// inspect already-complete stages, but never emits a block.
    pub fn finish_step(
        &mut self,
        block_budget: usize,
        out: &mut dyn FnMut(ChainEvent<'_>),
    ) -> bool {
        let Some(mut state) = self.finish else {
            return true;
        };
        let mut emitted = 0;
        if block_budget == 0 {
            let front_quiet = self.fronts.iter().all(|front| front.format.is_none());
            if (state.front == MAX_ENDPOINTS || front_quiet)
                && (state.mixer_finished || self.mixer.is_quiet())
                && (self.back.is_quiet() || state.back_frames >= state.back_limit)
            {
                self.finish = None;
                return true;
            }
            self.finish = Some(state);
            return false;
        }
        while state.front < MAX_ENDPOINTS && emitted < block_budget {
            self.flush_front(state.front, state.front as u16, &mut |event| {
                if matches!(event, ChainEvent::Block(_)) {
                    emitted += 1;
                }
                out(event);
            });
            state.front += 1;
        }
        if state.front < MAX_ENDPOINTS {
            self.finish = Some(state);
            return false;
        }
        if !state.mixer_finished && emitted < block_budget {
            if state.front == MAX_ENDPOINTS {
                self.mixer.begin_finish();
                state.front += 1;
            }
            let (mixer, back) = (&mut self.mixer, &mut self.back);
            state.mixer_finished = mixer.finish_step(&mut |scene| {
                back.process(scene, &mut |event| {
                    if matches!(event, ChainEvent::Block(_)) {
                        emitted += 1;
                    }
                    out(event);
                });
            });
        }
        if !state.mixer_finished {
            self.finish = Some(state);
            return false;
        }
        if self.back.is_quiet() || state.back_frames >= state.back_limit {
            self.finish = None;
            return true;
        }
        let Some(prepared) = self.back.prepared.as_ref() else {
            self.finish = None;
            return true;
        };
        let directions = prepared.feeds.directions();
        let silent = &SILENT_TAIL_BED[..directions * MIX_FRAMES];
        while emitted < block_budget
            && !self.back.is_quiet()
            && state.back_frames < state.back_limit
        {
            let bed = BedBlock {
                epoch: 0,
                rate: MIX_RATE,
                source_qpc: 0,
                directions,
                frames: MIX_FRAMES,
                channels: silent,
                lfe: None,
                silent: true,
                idle: true,
            };
            self.back.process(&bed, out);
            state.back_frames = state.back_frames.saturating_add(MIX_FRAMES);
            emitted += 1;
        }
        let done = self.back.is_quiet() || state.back_frames >= state.back_limit;
        self.finish = (!done).then_some(state);
        done
    }

    /// Pads and emits every endpoint's final partial mix interval, then drains
    /// the prepared output's convolution, crossover and alignment state.
    pub fn finish(&mut self, out: &mut dyn FnMut(ChainEvent<'_>)) {
        self.begin_finish();
        while !self.finish_step(usize::MAX, out) {}
    }

    pub fn output_channels(&self) -> Option<usize> {
        self.back
            .prepared
            .as_ref()
            .map(|prepared| prepared.channels())
    }

    pub fn lane_underrun_frames(&self) -> u64 {
        self.mixer.lane_underrun_frames()
    }

    pub fn dropped_frames(&self) -> u64 {
        self.mixer.dropped_frames()
    }

    pub fn volume_transitions_coalesced(&self) -> u64 {
        self.mixer.volume_transitions_coalesced()
    }

    pub fn suppressed_errors(&self) -> u64 {
        self.suppressed_errors
    }

    /// Processes one READ: every endpoint's records, interleaved by DMA time
    /// in chunks of at most 10 ms. A lane with data later in the READ for a
    /// moment already processed is waited for (see
    /// [`SceneMixer::set_waiting`]); at the READ's end nothing is.
    pub fn push_records(&mut self, stream: &[u8], out: &mut dyn FnMut(ChainEvent<'_>)) {
        self.push_records_merged(stream, out);
        let (mixer, back) = (&mut self.mixer, &mut self.back);
        mixer.release_waiting(&mut |scene| back.process(scene, out));
    }

    fn push_records_merged(&mut self, stream: &[u8], out: &mut dyn FnMut(ChainEvent<'_>)) {
        let mut cursors: [RecordCursor<'_>; MAX_ENDPOINTS] = std::array::from_fn(|endpoint| {
            let mut cursor = RecordCursor {
                stream,
                offset: 0,
                head: None,
                data_offset: 0,
                start_processed: false,
                done: false,
            };
            next_for_endpoint(&mut cursor, endpoint);
            cursor
        });

        loop {
            let mut selected: Option<(u64, usize)> = None;
            for (endpoint, cursor) in cursors.iter().enumerate() {
                let Some(head) = &cursor.head else {
                    continue;
                };
                let key = match head {
                    Ok(Record::Data {
                        endpoint: record_endpoint,
                        qpc,
                        flags,
                        bytes,
                        ..
                    }) if self.config.endpoints.includes(*record_endpoint)
                        && cursor.data_offset == 0
                        && !cursor.start_processed
                        && (*flags & RECORD_FLAG_DISCONTINUITY != 0
                            || self.mixer.lane_is_idle(*record_endpoint)) =>
                    {
                        qpc_before_end(
                            *qpc,
                            bytes.len(),
                            self.fronts[endpoint].byte_rate,
                            self.qpc_frequency,
                        )
                    }
                    Ok(Record::Data { qpc, bytes, .. }) => {
                        let byte_rate = self.fronts[endpoint].byte_rate;
                        if byte_rate == 0 {
                            *qpc
                        } else {
                            let chunk = self.fronts[endpoint]
                                .chunk_bytes()
                                .min(bytes.len().saturating_sub(cursor.data_offset));
                            let remaining = bytes
                                .len()
                                .saturating_sub(cursor.data_offset.saturating_add(chunk));
                            qpc_before_end(*qpc, remaining, byte_rate, self.qpc_frequency)
                        }
                    }
                    Ok(Record::Volume { qpc, .. })
                    | Ok(Record::FormatChange { qpc, .. })
                    | Ok(Record::StreamStart { qpc, .. })
                    | Ok(Record::StreamStop { qpc, .. })
                    | Ok(Record::Overrun { qpc, .. }) => *qpc,
                    Err(_) => 0,
                };
                if selected.is_none_or(|current| (key, endpoint) < current) {
                    selected = Some((key, endpoint));
                }
            }
            let Some((key, endpoint)) = selected else {
                break;
            };
            let mut waiting = 0_u32;
            for (other, cursor) in cursors.iter().enumerate() {
                if other != endpoint
                    && let Some(Ok(Record::Data { qpc, bytes, .. })) = &cursor.head
                    && qpc_before_end(
                        *qpc,
                        bytes.len().saturating_sub(cursor.data_offset),
                        self.fronts[other].byte_rate,
                        self.qpc_frequency,
                    ) < key
                {
                    waiting |= 1_u32 << other;
                }
            }
            self.mixer.set_waiting(waiting);

            let cursor = &mut cursors[endpoint];
            let item = cursor.head.take().expect("selected cursor has a head");
            if let Ok(Record::Data {
                endpoint: record_endpoint,
                flags,
                qpc,
                bytes,
                ..
            }) = &item
                && self.config.endpoints.includes(*record_endpoint)
                && cursor.data_offset == 0
                && !cursor.start_processed
                && (*flags & RECORD_FLAG_DISCONTINUITY != 0
                    || self.mixer.lane_is_idle(*record_endpoint))
            {
                let first_qpc = qpc_before_end(
                    *qpc,
                    bytes.len(),
                    self.fronts[endpoint].byte_rate,
                    self.qpc_frequency,
                );
                if self.mixer.lane_is_idle(*record_endpoint) {
                    self.fronts[endpoint].reset();
                    self.lane_start_at(*record_endpoint, first_qpc);
                } else {
                    self.fronts[endpoint].reset();
                    self.lane_discontinuity_at(*record_endpoint, first_qpc);
                }
                cursor.start_processed = true;
                cursor.head = Some(item);
                continue;
            }
            match item {
                Ok(Record::Data {
                    endpoint: record_endpoint,
                    epoch,
                    flags: _,
                    pos,
                    qpc,
                    qpc_written,
                    bytes,
                }) if self.config.endpoints.includes(record_endpoint) => {
                    let chunk_len = if self.fronts[endpoint].byte_rate == 0 {
                        bytes.len().saturating_sub(cursor.data_offset)
                    } else {
                        self.fronts[endpoint]
                            .chunk_bytes()
                            .min(bytes.len().saturating_sub(cursor.data_offset))
                    };
                    let start = cursor.data_offset;
                    let end = start.saturating_add(chunk_len);
                    let chunk = &bytes[start..end];
                    let chunk_end_qpc = qpc_before_end(
                        qpc,
                        bytes.len().saturating_sub(end),
                        self.fronts[endpoint].byte_rate,
                        self.qpc_frequency,
                    );
                    let before = self.mixer.now();
                    self.push_record_chunk(
                        Record::Data {
                            endpoint: record_endpoint,
                            epoch,
                            flags: 0,
                            pos: pos.saturating_add(start as u64),
                            qpc: chunk_end_qpc,
                            qpc_written,
                            bytes: chunk,
                        },
                        out,
                    );
                    self.update_anchor_after_progress(before, chunk_end_qpc);
                    cursor.data_offset = end;
                    if end < bytes.len() {
                        cursor.head = Some(Ok(Record::Data {
                            endpoint: record_endpoint,
                            epoch,
                            flags: 0,
                            pos,
                            qpc,
                            qpc_written,
                            bytes,
                        }));
                        continue;
                    }
                }
                Ok(record) => self.push_record_chunk(record, out),
                Err(error) => self.handle_record_error(error, out),
            }
            // The head parser has already validated this record. Read only its
            // raw length to advance without parsing it again.
            let consumed_len = read_record_header(&cursor.stream[cursor.offset..])
                .map_or(0, |header| header.len as usize);
            cursor.offset = cursor
                .offset
                .saturating_add(padded_record_size(consumed_len))
                .min(cursor.stream.len());
            cursor.data_offset = 0;
            cursor.start_processed = false;
            next_for_endpoint(cursor, endpoint);
        }
    }

    fn handle_record_error(&mut self, error: AbiError, out: &mut dyn FnMut(ChainEvent<'_>)) {
        let endpoint = error_endpoint(&error);
        if endpoint.is_some_and(|value| value as usize >= MAX_ENDPOINTS) {
            self.skipped_records = self.skipped_records.saturating_add(1);
        } else if endpoint.is_none_or(|value| self.config.endpoints.includes(value)) {
            self.emit_error(endpoint, ChainError::Record(error), out);
        }
    }

    fn flush_front(&mut self, index: usize, endpoint: u16, out: &mut dyn FnMut(ChainEvent<'_>)) {
        let (fronts, mixer) = (&mut self.fronts, &mut self.mixer);
        let geometry = self.geometry.as_deref();
        let mut scene_rate_error = None;
        let result = fronts[index].decoders.flush(&mut |scene| {
            if let Err(rate) = mixer.push_scene(endpoint, scene, geometry) {
                scene_rate_error = Some(rate);
            }
        });
        let error = if let Some(rate) = scene_rate_error {
            Some(ChainError::UnsupportedSceneRate(rate))
        } else if let Err(error) = result {
            Some(ChainError::Decode(error))
        } else if let Err(rate) = mixer.flush_lane(endpoint, geometry) {
            Some(ChainError::UnsupportedSceneRate(rate))
        } else {
            None
        };
        if let Some(error) = error {
            mixer.lane_discontinuity(endpoint, 0);
            emit_front_error(
                &mut fronts[index],
                &mut self.suppressed_errors,
                endpoint,
                error,
                out,
            );
        }
    }

    fn push_pcm(
        &mut self,
        index: usize,
        endpoint: u16,
        bytes: &[u8],
        qpc: u64,
        out: &mut dyn FnMut(ChainEvent<'_>),
    ) {
        let frequency = self.qpc_frequency;
        let (fronts, mixer, back) = (&mut self.fronts, &mut self.mixer, &mut self.back);
        let geometry = self.geometry.as_deref();
        let byte_rate = fronts[index].byte_rate;
        let chunk_bytes = fronts[index].chunk_bytes();
        for (chunk_index, chunk) in bytes.chunks(chunk_bytes).enumerate() {
            let start = chunk_index.saturating_mul(chunk_bytes);
            let chunk_qpc =
                qpc_before_end(qpc, bytes.len().saturating_sub(start), byte_rate, frequency);
            let mut scene_rate_error = None;
            let decoders = &mut fronts[index].decoders;
            let mut place = |scene: &SceneBlock<'_>| {
                if let Err(rate) = mixer.push_scene(endpoint, scene, geometry) {
                    scene_rate_error = Some(rate);
                }
            };
            // Every complete frame is placed before the lane's transport
            // moves, so the lane never holds part of a 10 ms block behind the
            // bytes it received (the mixer would count that as its underrun).
            let result = decoders.push_pcm(chunk, chunk_qpc, &mut place);
            if result.is_ok() {
                decoders.emit_buffered(&mut place);
            }
            let error = if let Some(rate) = scene_rate_error {
                Some(ChainError::UnsupportedSceneRate(rate))
            } else {
                result.err().map(ChainError::Decode)
            };
            if let Some(error) = error {
                fronts[index].reset();
                mixer.lane_discontinuity(endpoint, 0);
                emit_front_error(
                    &mut fronts[index],
                    &mut self.suppressed_errors,
                    endpoint,
                    error,
                    out,
                );
                break;
            }
            let transport_frames = fronts[index].advance_bytes(chunk.len());
            mixer.advance(endpoint, transport_frames, &mut |scene| {
                back.process(scene, out)
            });
        }
    }

    fn push_bitstream(
        &mut self,
        index: usize,
        endpoint: u16,
        bytes: &[u8],
        qpc: u64,
        out: &mut dyn FnMut(ChainEvent<'_>),
    ) {
        let frequency = self.qpc_frequency;
        let (fronts, mixer, back) = (&mut self.fronts, &mut self.mixer, &mut self.back);
        let geometry = self.geometry.as_deref();
        let byte_rate = fronts[index].byte_rate;
        let chunk_bytes = fronts[index].chunk_bytes();
        for (chunk_index, chunk) in bytes.chunks(chunk_bytes).enumerate() {
            let chunk_start = chunk_index.saturating_mul(chunk_bytes);
            let chunk_end_qpc = qpc_before_end(
                qpc,
                bytes.len().saturating_sub(chunk_start + chunk.len()),
                byte_rate,
                frequency,
            );
            let front = &mut fronts[index];
            let chunk_end = front.deframer_bytes.saturating_add(chunk.len() as u64);
            let transport_frames = front.advance_bytes(chunk.len());
            let mut transport_advanced = false;
            let (deframer, decoders) = (&mut front.deframer, &mut front.decoders);
            let mut reported_error = None;
            deframer.push(chunk, &mut |burst| {
                // Burst.start is Pa's byte offset since the deframer's reset.
                // Re-anchor to the completing chunk's observed DMA end, not a
                // once-per-stream origin: later records observe clock trim and
                // stalls too. Subtraction also covers a split preamble/payload.
                let burst_qpc = if byte_rate == 0 {
                    chunk_end_qpc
                } else {
                    let ticks = u128::from(chunk_end.saturating_sub(burst.start))
                        * u128::from(frequency)
                        / u128::from(byte_rate);
                    chunk_end_qpc.saturating_sub(ticks.min(u128::from(u64::MAX)) as u64)
                };
                let mut scene_rate_error = None;
                if let Err(error) = decoders.push_burst(&burst, burst_qpc, &mut |scene| {
                    if let Err(rate) = mixer.push_scene(endpoint, scene, geometry) {
                        scene_rate_error = Some(rate);
                    }
                }) {
                    decoders.reset();
                    reported_error = Some(ChainError::Decode(error));
                } else if let Some(rate) = scene_rate_error {
                    decoders.reset();
                    reported_error = Some(ChainError::UnsupportedSceneRate(rate));
                } else {
                    return;
                }
                // Consume this chunk in the old transport generation first.
                // Rebasing to the common now before advancing would add the
                // chunk a second time when another endpoint was already ahead,
                // prematurely emitting that endpoint's still-pending samples.
                if !transport_advanced {
                    mixer.advance(endpoint, transport_frames, &mut |scene| {
                        back.process(scene, out)
                    });
                    transport_advanced = true;
                }
                mixer.lane_discontinuity(endpoint, 0);
            });
            fronts[index].deframer_bytes = fronts[index]
                .deframer_bytes
                .saturating_add(chunk.len() as u64);
            if let Some(error) = reported_error {
                emit_front_error(
                    &mut fronts[index],
                    &mut self.suppressed_errors,
                    endpoint,
                    error,
                    out,
                );
            }
            if !transport_advanced {
                mixer.advance(endpoint, transport_frames, &mut |scene| {
                    back.process(scene, out)
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use ssr_types::{BitstreamFormat, PcmFormat, SampleType};

    use super::*;

    fn bitstream(codec: BitstreamCodec, encoded_rate: u32) -> SourceFormat {
        SourceFormat::Bitstream(BitstreamFormat {
            codec,
            container_rate: 192_000,
            container_channels: 8,
            encoded_rate,
            encoded_channels: 0,
        })
    }

    fn volume(gain_db: i32, muted: bool) -> VolumePayload {
        VolumePayload {
            channel_count: 2,
            mute_mask: if muted { 3 } else { 0 },
            level_q16: {
                let mut levels = [0; ssr_abi::MAX_VOLUME_CHANNELS];
                levels[..2].fill(gain_db * 65_536);
                levels
            },
        }
    }

    #[test]
    fn volume_repeat_with_the_same_value_is_a_noop() {
        let mut chain = Chain::new(ChainConfig::default());
        let payload = volume(-6, false);
        let mut events = 0;
        chain.push_record(
            Record::Volume {
                endpoint: 0,
                repeat: false,
                qpc: 10,
                channel_count: payload.channel_count,
                mute_mask: payload.mute_mask,
                levels: payload.level_q16,
            },
            &mut |event| events += usize::from(matches!(event, ChainEvent::Volume { .. })),
        );
        chain.push_record(
            Record::Volume {
                endpoint: 0,
                repeat: true,
                qpc: 20,
                channel_count: payload.channel_count,
                mute_mask: payload.mute_mask,
                levels: payload.level_q16,
            },
            &mut |event| events += usize::from(matches!(event, ChainEvent::Volume { .. })),
        );
        assert_eq!(events, 1);
    }

    #[test]
    fn every_channel_muted_publishes_muted_up_to_32_channels() {
        for (channels, mask, expected) in [
            (1, 1, true),
            (16, u32::from(u16::MAX), true),
            (31, u32::MAX >> 1, true),
            (32, u32::MAX, true),
            (32, u32::MAX >> 1, false),
            (32, 0, false),
        ] {
            let mut chain = Chain::new(ChainConfig::default());
            let mut muted = None;
            chain.push_record(
                Record::Volume {
                    endpoint: 0,
                    repeat: false,
                    qpc: 10,
                    channel_count: channels,
                    mute_mask: mask,
                    levels: [0; ssr_abi::MAX_VOLUME_CHANNELS],
                },
                &mut |event| {
                    if let ChainEvent::Volume { muted: value, .. } = event {
                        muted = Some(value);
                    }
                },
            );
            assert_eq!(muted, Some(expected), "{channels} channels, mask {mask:#x}");
        }
    }

    #[test]
    fn repeated_different_format_recovers_the_lost_change() {
        let mut chain = Chain::new(ChainConfig::default());
        let old = bitstream(BitstreamCodec::Mat20, 48_000);
        let recovered = bitstream(BitstreamCodec::Eac3, 48_000);
        let mut formats = Vec::new();

        for record in [
            Record::FormatChange {
                endpoint: 0,
                epoch: 1,
                qpc: 10,
                repeat: false,
                format: old,
            },
            Record::FormatChange {
                endpoint: 0,
                epoch: 2,
                qpc: 20,
                repeat: true,
                format: recovered,
            },
        ] {
            chain.push_record(record, &mut |event| {
                if let ChainEvent::FormatChanged { epoch, format, .. } = event {
                    formats.push((epoch, format));
                }
            });
        }

        assert_eq!(formats, [(1, old), (2, recovered)]);
        assert_eq!(chain.fronts[0].format, Some((2, recovered)));
        assert_eq!(chain.fronts[0].route, Some(Route::Bitstream));
    }

    #[test]
    fn repeated_unsupported_format_publishes_support_each_time_but_one_error() {
        let mut chain = Chain::new(ChainConfig::default());
        let unsupported = bitstream(BitstreamCodec::Ac3, 48_000);
        let mut support = Vec::new();
        let mut errors = 0;

        for qpc in [10, 20] {
            chain.push_record(
                Record::FormatChange {
                    endpoint: 0,
                    epoch: 1,
                    qpc,
                    repeat: false,
                    format: unsupported,
                },
                &mut |event| match event {
                    ChainEvent::FormatChanged { support: value, .. } => support.push(value),
                    ChainEvent::Error { .. } => errors += 1,
                    _ => {}
                },
            );
        }

        assert_eq!(
            support,
            [
                FormatSupport::UnsupportedFormat,
                FormatSupport::UnsupportedFormat
            ]
        );
        assert_eq!(errors, 1);
    }

    #[test]
    fn identical_errors_are_suppressed_until_the_epoch_changes() {
        let mut chain = Chain::new(ChainConfig::default());
        let unsupported = bitstream(BitstreamCodec::Ac3, 48_000);
        let mut errors = 0;

        for epoch in 1..=2 {
            for _ in 0..1_000 {
                chain.push_record(
                    Record::FormatChange {
                        endpoint: 0,
                        epoch,
                        qpc: u64::from(epoch),
                        repeat: false,
                        format: unsupported,
                    },
                    &mut |event| errors += usize::from(matches!(event, ChainEvent::Error { .. })),
                );
            }
            assert_eq!(errors, epoch as usize);
        }

        assert_eq!(chain.suppressed_errors(), 1_998);
    }

    fn identity_prepared(ir_frames: usize, delayed_tap: usize) -> Box<Prepared> {
        use brir::PreparedBrir;
        use ssr_types::{Direction, Layout};

        let mut ir = vec![0.0_f32; ir_frames];
        ir[delayed_tap] = 1.0;
        let filters = [[ir.as_slice(), ir.as_slice()]];
        let brir = Arc::new(PreparedBrir {
            layout: Layout::new(vec![Direction::new("front", 0.0, 0.0)]).unwrap(),
            bank: Arc::new(convolver::FilterBank::new(&filters, MIX_FRAMES).unwrap()),
            rate: MIX_RATE,
            partition: MIX_FRAMES,
            lfe: None,
            gain: 1.0,
            limiter_ceiling: 1.0,
            ir_frames,
            omitted_silent_directions: Vec::new(),
            trimmed_leading_frames: 0,
        });
        Box::new(Prepared::new(brir, MIX_FRAMES).unwrap())
    }

    fn silent_bed(channels: &[f32]) -> BedBlock<'_> {
        test_bed(channels, true, true)
    }

    fn test_bed(channels: &[f32], silent: bool, idle: bool) -> BedBlock<'_> {
        BedBlock {
            epoch: 0,
            rate: MIX_RATE,
            frames: MIX_FRAMES,
            source_qpc: 0,
            directions: 1,
            channels,
            lfe: None,
            silent,
            idle,
        }
    }

    #[test]
    fn silent_idle_blocks_play_the_brir_tail_to_completion() {
        let active = vec![0.0_f32; MIX_FRAMES];
        let silent = vec![0.0_f32; MIX_FRAMES];
        let mut active = active;
        active[0] = 1.0;
        let active_bed = BedBlock {
            epoch: 1,
            rate: MIX_RATE,
            frames: MIX_FRAMES,
            source_qpc: 0,
            directions: 1,
            channels: &active,
            lfe: None,
            silent: false,
            idle: false,
        };
        let mut back = Back::new();
        back.prepared = Some(identity_prepared(20_000, 19_999));
        back.process(&active_bed, &mut |_| {});

        let mut audible_idle_blocks = 0;
        let mut silent_after_tail = 0;
        for _ in 0..60 {
            back.process(&silent_bed(&silent), &mut |event| {
                if let ChainEvent::Block(block) = event {
                    assert!(block.idle);
                    if block.silent {
                        silent_after_tail += 1;
                    } else {
                        audible_idle_blocks += 1;
                    }
                }
            });
        }
        assert!(audible_idle_blocks > 0);
        assert!(silent_after_tail > 0);
    }

    #[test]
    fn a_new_sound_after_long_idle_has_no_old_brir_tail() {
        let old = vec![0.0_f32; MIX_FRAMES];
        let mut old = old;
        old[0] = 1.0;
        let new = vec![0.25_f32; MIX_FRAMES];
        let silent = vec![0.0_f32; MIX_FRAMES];
        let render_new = |with_old: bool| {
            let mut back = Back::new();
            back.prepared = Some(identity_prepared(20_000, 19_999));
            if with_old {
                back.process(&test_bed(&old, false, false), &mut |_| {});
                for _ in 0..60 {
                    back.process(&silent_bed(&silent), &mut |_| {});
                }
            }
            let mut output = Vec::new();
            back.process(&test_bed(&new, false, false), &mut |event| {
                if let ChainEvent::Block(block) = event {
                    output.extend(block.channel(0).iter().map(|sample| sample.to_bits()));
                    output.extend(block.channel(1).iter().map(|sample| sample.to_bits()));
                }
            });
            output
        };

        assert_eq!(render_new(true), render_new(false));
    }

    #[test]
    fn quiet_fast_path_is_sample_exact() {
        let active = vec![1.0_f32; MIX_FRAMES];
        let silent = vec![0.0_f32; MIX_FRAMES];
        let active_bed = BedBlock {
            epoch: 1,
            rate: MIX_RATE,
            frames: MIX_FRAMES,
            source_qpc: 0,
            directions: 1,
            channels: &active,
            lfe: None,
            silent: false,
            idle: false,
        };
        let sequence = [false; 1]
            .into_iter()
            .chain(std::iter::repeat_n(true, 280))
            .chain([false])
            .collect::<Vec<_>>();

        let run = |disable_quiet_fast_path| {
            let mut back = Back::new();
            back.prepared = Some(identity_prepared(120_000, 119_999));
            back.disable_quiet_fast_path = disable_quiet_fast_path;
            let mut output = Vec::new();
            for &is_silent in &sequence {
                let bed = if is_silent {
                    silent_bed(&silent)
                } else {
                    active_bed
                };
                back.process(&bed, &mut |event| {
                    if let ChainEvent::Block(block) = event {
                        output.extend(block.channel(0).iter().map(|sample| sample.to_bits()));
                        output.extend(block.channel(1).iter().map(|sample| sample.to_bits()));
                    }
                });
            }
            output
        };

        assert_eq!(run(false), run(true));
    }

    fn speaker_prepared(
        speakers: Vec<speakers::PreparedSpeaker>,
        subwoofers: Vec<speakers::PreparedSubwoofer>,
        bass_to: Vec<usize>,
        device_channels: usize,
    ) -> speakers::PreparedSpeakers {
        let layout = ssr_types::Layout::new(
            speakers
                .iter()
                .enumerate()
                .map(|(index, speaker)| {
                    ssr_types::Direction::new(speaker.name.clone(), 30.0 - 60.0 * index as f32, 0.0)
                })
                .collect(),
        )
        .unwrap();
        speakers::PreparedSpeakers {
            layout,
            rate: MIX_RATE,
            partition: MIX_FRAMES,
            device_channels,
            channel_mask: 0,
            speakers,
            subwoofers,
            bass_to,
            lfe_gain: 10.0_f32.sqrt(),
            gain: 1.0,
            limiter_ceiling: 10.0,
        }
    }

    fn prepared_speaker(
        name: &str,
        device_channel: usize,
        gain: f32,
        delay_frames: usize,
        fir: Option<Arc<convolver::FilterBank>>,
    ) -> speakers::PreparedSpeaker {
        speakers::PreparedSpeaker {
            name: name.into(),
            device_channel,
            small: false,
            crossover: None,
            delay_frames,
            gain,
            fir,
        }
    }

    #[test]
    fn speaker_preparation_rejects_output_channel_counts_outside_the_bound() {
        for channels in [0, MAX_OUTPUT_CHANNELS + 1] {
            let prepared = Arc::new(speaker_prepared(
                vec![prepared_speaker("front", 0, 1.0, 0, None)],
                Vec::new(),
                Vec::new(),
                channels,
            ));
            assert!(matches!(
                Prepared::speakers(prepared, MIX_FRAMES),
                Err(PrepareError::OutputChannels { channels: actual }) if actual == channels
            ));
        }
    }

    #[test]
    fn speaker_output_routes_delays_trims_and_keeps_fir_mono() {
        let fir_ir = [0.0_f32, 1.0];
        let fir_filters = [fir_ir.as_slice()];
        let fir = Arc::new(convolver::FilterBank::new_mono(&fir_filters, MIX_FRAMES).unwrap());
        let prepared = speaker_prepared(
            vec![
                prepared_speaker("L", 2, 0.5, 2, Some(fir)),
                prepared_speaker("R", 0, 1.0, 0, None),
            ],
            Vec::new(),
            Vec::new(),
            4,
        );
        let mut state = SpeakerOutput::new(&prepared, MIX_FRAMES);
        let mut feeds = Feeds::new(2, MIX_FRAMES);
        feeds.channel_mut(0)[0] = 1.0;
        feeds.channel_mut(1)[0] = 0.25;
        let mut output = vec![0.0; 4 * MIX_FRAMES];

        state.process(&prepared, &feeds, None, MIX_FRAMES, &mut output, &|_| 1.0);

        assert_eq!(output[0], 0.25);
        assert!(
            output[MIX_FRAMES..2 * MIX_FRAMES]
                .iter()
                .all(|sample| *sample == 0.0)
        );
        assert_eq!(output[2 * MIX_FRAMES + 3], 0.5);
        assert!(
            output[2 * MIX_FRAMES..2 * MIX_FRAMES + 3]
                .iter()
                .all(|sample| *sample == 0.0)
        );
        assert!(output[3 * MIX_FRAMES..].iter().all(|sample| *sample == 0.0));
    }

    #[test]
    fn speaker_output_routes_lfe_and_fallback_bass() {
        let speaker = prepared_speaker("L", 1, 1.0, 0, None);
        let with_subwoofer = speaker_prepared(
            vec![speaker],
            vec![
                prepared_subwoofer(0, 1.0, 0, None),
                prepared_subwoofer(2, 1.0, 0, None),
            ],
            Vec::new(),
            3,
        );
        let mut feeds = Feeds::new(1, MIX_FRAMES);
        feeds.channel_mut(0)[0] = 0.25;
        let mut lfe = vec![0.0; MIX_FRAMES];
        lfe[0] = 1.0;
        let mut output = vec![0.0; 3 * MIX_FRAMES];
        let mut state = SpeakerOutput::new(&with_subwoofer, MIX_FRAMES);
        state.process(
            &with_subwoofer,
            &feeds,
            Some(&lfe),
            MIX_FRAMES,
            &mut output,
            &|_| 1.0,
        );
        let expected = 10.0_f32.sqrt() * 0.5;
        assert!((output[0] - expected).abs() <= 1.0e-6);
        assert_eq!(output[MIX_FRAMES], 0.25);
        assert!((output[2 * MIX_FRAMES] - expected).abs() <= 1.0e-6);

        let fallback = speaker_prepared(
            vec![prepared_speaker("L", 1, 1.0, 0, None)],
            Vec::new(),
            vec![0],
            2,
        );
        let mut state = SpeakerOutput::new(&fallback, MIX_FRAMES);
        let mut output = vec![0.0; 2 * MIX_FRAMES];
        state.process(
            &fallback,
            &feeds,
            Some(&lfe),
            MIX_FRAMES,
            &mut output,
            &|_| 1.0,
        );
        assert!((output[MIX_FRAMES] - (0.25 + 10.0_f32.sqrt())).abs() <= 1.0e-6);
        assert!(output[..MIX_FRAMES].iter().all(|sample| *sample == 0.0));
    }

    #[test]
    fn speaker_output_crosses_low_frequency_energy_to_subwoofer() {
        let coefficients = speakers::crossover_coefficients(MIX_RATE, 80.0);
        let speaker = speakers::PreparedSpeaker {
            name: "L".into(),
            device_channel: 0,
            small: true,
            crossover: Some(coefficients),
            delay_frames: 0,
            gain: 1.0,
            fir: None,
        };
        let prepared = speaker_prepared(
            vec![speaker],
            vec![prepared_subwoofer(1, 1.0, 0, None)],
            Vec::new(),
            2,
        );
        let mut state = SpeakerOutput::new(&prepared, MIX_FRAMES);
        let mut feeds = Feeds::new(1, MIX_FRAMES);
        for (frame, sample) in feeds.channel_mut(0).iter_mut().enumerate() {
            *sample = (2.0 * std::f32::consts::PI * 30.0 * frame as f32 / MIX_RATE as f32).sin();
        }
        let mut output = vec![0.0; 2 * MIX_FRAMES];
        state.process(&prepared, &feeds, None, MIX_FRAMES, &mut output, &|_| 1.0);
        let rms = |samples: &[f32]| {
            (samples.iter().map(|sample| sample * sample).sum::<f32>() / samples.len() as f32)
                .sqrt()
        };
        let main = rms(&output[..MIX_FRAMES]);
        let bass = rms(&output[MIX_FRAMES..]);
        assert!(
            bass > main * 4.0,
            "bass {bass} is not dominant over main {main}"
        );
    }

    fn prepared_subwoofer(
        device_channel: usize,
        gain: f32,
        delay_frames: usize,
        fir: Option<Arc<convolver::FilterBank>>,
    ) -> speakers::PreparedSubwoofer {
        speakers::PreparedSubwoofer {
            device_channel,
            delay_frames,
            gain,
            fir,
        }
    }

    #[test]
    fn subwoofers_take_their_own_fir_delay_and_trim() {
        let fir_ir = [0.0_f32, 0.5];
        let fir_filters = [fir_ir.as_slice()];
        let fir = Arc::new(convolver::FilterBank::new_mono(&fir_filters, MIX_FRAMES).unwrap());
        let prepared = speaker_prepared(
            vec![prepared_speaker("L", 0, 1.0, 0, None)],
            vec![
                prepared_subwoofer(1, 0.5, 3, Some(fir)),
                prepared_subwoofer(2, 1.0, 0, None),
            ],
            Vec::new(),
            3,
        );
        let mut state = SpeakerOutput::new(&prepared, MIX_FRAMES);
        let feeds = Feeds::new(1, MIX_FRAMES);
        let mut lfe = vec![0.0; MIX_FRAMES];
        lfe[0] = 1.0;
        let mut output = vec![0.0; 3 * MIX_FRAMES];
        state.process(
            &prepared,
            &feeds,
            Some(&lfe),
            MIX_FRAMES,
            &mut output,
            &|_| 1.0,
        );

        // Each subwoofer gets half of the LFE at +10 dB.
        let half = 10.0_f32.sqrt() * 0.5;
        let first = &output[MIX_FRAMES..2 * MIX_FRAMES];
        // FIR delays one frame and halves, the alignment delays three more,
        // and the trim halves again (FFT convolution leaves ~1e-9 residue).
        assert!(first[..4].iter().all(|sample| sample.abs() <= 1e-6));
        assert!((first[4] - half * 0.25).abs() <= 1e-6);
        assert!(first[5..].iter().all(|sample| sample.abs() <= 1e-6));
        assert!((output[2 * MIX_FRAMES] - half).abs() <= 1e-6);
    }

    #[test]
    fn crossover_tails_reach_the_room_fir_after_the_bed_falls_silent() {
        let fir_ir = [1.0_f32];
        let fir_filters = [fir_ir.as_slice()];
        let fir = Arc::new(convolver::FilterBank::new_mono(&fir_filters, MIX_FRAMES).unwrap());
        let speaker = speakers::PreparedSpeaker {
            small: true,
            crossover: Some(speakers::crossover_coefficients(MIX_RATE, 80.0)),
            ..prepared_speaker("L", 0, 1.0, 0, None)
        };
        let prepared = speaker_prepared(
            vec![speaker],
            vec![prepared_subwoofer(1, 1.0, 0, Some(fir))],
            Vec::new(),
            2,
        );
        let mut state = SpeakerOutput::new(&prepared, MIX_FRAMES);
        let mut feeds = Feeds::new(1, MIX_FRAMES);
        feeds.channel_mut(0).fill(0.5);
        let mut output = vec![0.0; 2 * MIX_FRAMES];
        state.process(&prepared, &feeds, None, MIX_FRAMES, &mut output, &|_| 1.0);
        feeds.channel_mut(0).fill(0.0);
        // The low-pass keeps ringing into the subwoofer's FIR.
        state.process(&prepared, &feeds, None, MIX_FRAMES, &mut output, &|_| 1.0);
        assert!(output[MIX_FRAMES..].iter().any(|sample| *sample != 0.0));
        // The tail decays to exact silence instead of lingering as subnormals.
        let mut blocks = 2;
        while output.iter().any(|sample| *sample != 0.0) {
            state.process(&prepared, &feeds, None, MIX_FRAMES, &mut output, &|_| 1.0);
            blocks += 1;
            assert!(blocks < 500, "the crossover tail never reached silence");
        }
        let crossover = state.crossovers[0].as_ref().unwrap();
        assert!(crossover.low.iter().chain(&crossover.high).all(|state| {
            [state.x1, state.x2, state.y1, state.y2]
                .iter()
                .all(|value| *value == 0.0 || value.is_normal())
        }));
    }

    #[test]
    fn finish_drains_speaker_alignment_delay_without_idle() {
        let prepared = speaker_prepared(
            vec![prepared_speaker("L", 0, 1.0, 4_800, None)],
            Vec::new(),
            Vec::new(),
            1,
        );
        let mut chain = Chain::new(ChainConfig::default());
        drop(chain.set_prepared(Box::new(
            Prepared::speakers(Arc::new(prepared), MIX_FRAMES).unwrap(),
        )));
        let mut input = vec![0.0; MIX_FRAMES];
        input[0] = 1.0;
        let mut audible = Vec::new();

        chain
            .back
            .process(&test_bed(&input, false, false), &mut |_| {});
        chain.finish(&mut |event| {
            if let ChainEvent::Block(block) = event {
                for (frame, sample) in block.channel(0).iter().enumerate() {
                    if *sample != 0.0 {
                        audible.push(frame);
                    }
                }
            }
        });

        assert_eq!(audible, [0]);
        assert!(chain.back.is_quiet());
    }

    #[test]
    fn finish_drains_headphone_brir_without_idle() {
        let mut chain = Chain::new(ChainConfig::default());
        drop(chain.set_prepared(identity_prepared(4_801, 4_800)));
        let mut input = vec![0.0; MIX_FRAMES];
        input[0] = 1.0;
        let mut audible = 0;

        chain
            .back
            .process(&test_bed(&input, false, false), &mut |_| {});
        chain.finish(&mut |event| {
            if let ChainEvent::Block(block) = event {
                audible += block
                    .channel(0)
                    .iter()
                    .filter(|sample| **sample != 0.0)
                    .count();
            }
        });

        assert!(audible > 0);
        assert!(chain.back.is_quiet());
    }

    #[test]
    fn zero_budget_finish_completes_an_already_quiet_chain() {
        let mut chain = Chain::new(ChainConfig::default());
        chain.begin_finish();
        assert!(chain.finish_step(0, &mut |_| {}));
    }

    #[test]
    fn incremental_finish_matches_the_offline_one_shot_tail() {
        let mut one_shot = Chain::new(ChainConfig::default());
        let mut incremental = Chain::new(ChainConfig::default());
        drop(one_shot.set_prepared(identity_prepared(4_801, 4_800)));
        drop(incremental.set_prepared(identity_prepared(4_801, 4_800)));
        let mut input = vec![0.0; MIX_FRAMES];
        input[0] = 1.0;
        one_shot
            .back
            .process(&test_bed(&input, false, false), &mut |_| {});
        incremental
            .back
            .process(&test_bed(&input, false, false), &mut |_| {});
        let mut expected = Vec::new();
        one_shot.finish(&mut |event| {
            if let ChainEvent::Block(block) = event {
                expected.extend_from_slice(block.samples);
            }
        });
        let mut actual = Vec::new();
        incremental.begin_finish();
        assert!(!incremental.finish_step(0, &mut |_| {}));
        let mut steps = 0;
        loop {
            steps += 1;
            let mut blocks = 0;
            let done = incremental.finish_step(1, &mut |event| {
                if let ChainEvent::Block(block) = event {
                    blocks += 1;
                    actual.extend_from_slice(block.samples);
                }
            });
            assert!(blocks <= 1, "one finish step exceeded its block budget");
            if done {
                break;
            }
            assert!(steps < 500, "incremental finish did not terminate");
        }

        assert!(steps > 1);
        assert_eq!(actual, expected);
    }

    #[test]
    fn runtime_lfe_gain_ramps_over_one_block_for_speakers() {
        let prepared = speaker_prepared(
            vec![prepared_speaker("L", 1, 1.0, 0, None)],
            vec![prepared_subwoofer(0, 1.0, 0, None)],
            Vec::new(),
            2,
        );
        let mut back = Back::new();
        let prepared = Box::new(Prepared::speakers(Arc::new(prepared), MIX_FRAMES).unwrap());
        let (gain, ceiling) = prepared.gain_and_ceiling();
        back.limiter.reset(MIX_RATE, gain, ceiling);
        back.prepared = Some(prepared);
        let channels = vec![0.0; MIX_FRAMES];
        let lfe = vec![1.0; MIX_FRAMES];
        let bed = BedBlock {
            epoch: 1,
            rate: MIX_RATE,
            source_qpc: 0,
            directions: 1,
            frames: MIX_FRAMES,
            channels: &channels,
            lfe: Some(&lfe),
            silent: false,
            idle: false,
        };
        let profile_gain = 10.0_f32.sqrt();
        let target = 0.5;
        let mut blocks = Vec::new();

        back.process(&bed, &mut |event| {
            if let ChainEvent::Block(block) = event {
                blocks.push((block.channel(0)[0], block.channel(0)[MIX_FRAMES - 1]));
            }
        });
        back.set_lfe_gain(target);
        back.process(&bed, &mut |event| {
            if let ChainEvent::Block(block) = event {
                blocks.push((block.channel(0)[0], block.channel(0)[MIX_FRAMES - 1]));
            }
        });
        back.process(&bed, &mut |event| {
            if let ChainEvent::Block(block) = event {
                blocks.push((block.channel(0)[0], block.channel(0)[MIX_FRAMES - 1]));
            }
        });

        assert_eq!(blocks[0], (profile_gain, profile_gain));
        assert_eq!(blocks[1].0, profile_gain);
        let ramp_last = profile_gain * (1.0 + (target - 1.0) * 479.0 / 480.0);
        assert!((blocks[1].1 - ramp_last).abs() <= 1.0e-6);
        assert_eq!(blocks[2], (profile_gain * target, profile_gain * target));
    }

    #[test]
    fn speaker_limiter_links_all_channels_at_the_ceiling() {
        let mut prepared = speaker_prepared(
            vec![
                prepared_speaker("L", 0, 1.0, 0, None),
                prepared_speaker("R", 1, 1.0, 0, None),
                prepared_speaker("C", 2, 1.0, 0, None),
            ],
            Vec::new(),
            Vec::new(),
            4,
        );
        prepared.gain = 2.0;
        prepared.limiter_ceiling = 0.5;
        let mut back = Back::new();
        let mut prepared = Box::new(Prepared::speakers(Arc::new(prepared), MIX_FRAMES).unwrap());
        let (gain, ceiling) = prepared.gain_and_ceiling();
        back.limiter.reset(MIX_RATE, gain, ceiling);
        prepared.reset();
        back.prepared = Some(prepared);
        let mut channels = vec![0.0; 3 * MIX_FRAMES];
        channels[0] = 0.4;
        channels[MIX_FRAMES] = 0.4;
        channels[2 * MIX_FRAMES] = 0.4;
        let bed = BedBlock {
            epoch: 1,
            rate: MIX_RATE,
            source_qpc: 0,
            directions: 3,
            frames: MIX_FRAMES,
            channels: &channels,
            lfe: None,
            silent: false,
            idle: false,
        };
        let mut output = None;

        back.process(&bed, &mut |event| {
            if let ChainEvent::Block(block) = event {
                output = Some(block.samples[..4 * MIX_FRAMES].to_vec());
            }
        });

        let output = output.unwrap();
        assert_eq!(output[0], 0.5);
        assert!(output[1..MIX_FRAMES].iter().all(|sample| *sample == 0.0));
        assert_eq!(output[MIX_FRAMES], 0.5);
        assert!(
            output[MIX_FRAMES + 1..2 * MIX_FRAMES]
                .iter()
                .all(|sample| *sample == 0.0)
        );
        assert_eq!(output[2 * MIX_FRAMES], 0.5);
        assert!(
            output[2 * MIX_FRAMES + 1..]
                .iter()
                .all(|sample| *sample == 0.0)
        );
    }

    #[test]
    fn output_channel_event_reports_initial_and_changed_counts() {
        let headphones = {
            let ir = [1.0_f32];
            let filters = [[ir.as_slice(), ir.as_slice()]];
            Arc::new(PreparedBrir {
                layout: ssr_types::Layout::new(vec![ssr_types::Direction::new("front", 0.0, 0.0)])
                    .unwrap(),
                bank: Arc::new(convolver::FilterBank::new(&filters, MIX_FRAMES).unwrap()),
                rate: MIX_RATE,
                partition: MIX_FRAMES,
                lfe: None,
                gain: 1.0,
                limiter_ceiling: 1.0,
                ir_frames: 1,
                omitted_silent_directions: Vec::new(),
                trimmed_leading_frames: 0,
            })
        };
        let mut chain = Chain::new(ChainConfig::default());
        let mut channels = Vec::new();
        chain.set_prepared_with(
            Box::new(Prepared::new(headphones, MIX_FRAMES).unwrap()),
            &mut |event| {
                if let ChainEvent::OutputChannels { channels: count } = event {
                    channels.push(count);
                }
            },
        );
        let speakers = speaker_prepared(
            vec![prepared_speaker("L", 0, 1.0, 0, None)],
            Vec::new(),
            Vec::new(),
            4,
        );
        chain.set_prepared_with(
            Box::new(Prepared::speakers(Arc::new(speakers), MIX_FRAMES).unwrap()),
            &mut |event| {
                if let ChainEvent::OutputChannels { channels: count } = event {
                    channels.push(count);
                }
            },
        );
        assert_eq!(channels, [2, 4]);
    }

    #[test]
    fn output_profile_loader_detects_legacy_and_speaker_profiles() {
        let directory =
            std::env::temp_dir().join(format!("engine-output-profile-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let legacy = directory.join("legacy.toml");
        std::fs::write(
            &legacy,
            r#"name = "legacy"
gain_db = 0.0
limiter_ceiling_dbfs = 0.0

[brir]
directions = []
"#,
        )
        .unwrap();
        let speaker =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/profiles/speakers-5.1.toml");
        assert!(matches!(
            load_output_profile(&legacy),
            Ok(OutputProfile::Headphones(_))
        ));
        assert!(matches!(
            load_output_profile(&speaker),
            Ok(OutputProfile::Speakers(_))
        ));
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn partition_for_uses_the_largest_divisor_at_most_512() {
        for (block, expected) in [(0, 0), (1536, 512), (480, 480), (441, 441), (513, 171)] {
            assert_eq!(partition_for(block), expected);
        }
    }

    #[test]
    fn scene_shape_matches_decoder_output() {
        let pcm = SourceFormat::Pcm(PcmFormat {
            channels: 2,
            sample_rate: 44_100,
            bits_per_sample: 32,
            valid_bits: 32,
            sample_type: SampleType::Float,
            channel_mask: 3,
        });
        assert_eq!(scene_shape(&pcm), Some((MIX_RATE, MIX_FRAMES)));
        for rate in [88_200, 176_400, 192_000] {
            let pcm = SourceFormat::Pcm(PcmFormat {
                channels: 2,
                sample_rate: rate,
                bits_per_sample: 32,
                valid_bits: 32,
                sample_type: SampleType::Float,
                channel_mask: 3,
            });
            assert_eq!(scene_shape(&pcm), Some((MIX_RATE, MIX_FRAMES)));
        }
        assert_eq!(
            scene_shape(&bitstream(BitstreamCodec::Mat20, 96_000)),
            Some((MIX_RATE, MIX_FRAMES))
        );
        assert_eq!(
            scene_shape(&bitstream(BitstreamCodec::Eac3Joc, 44_100)),
            Some((MIX_RATE, MIX_FRAMES))
        );
        assert_eq!(
            scene_shape(&bitstream(BitstreamCodec::Mat21, 0)),
            Some((MIX_RATE, MIX_FRAMES))
        );
        assert_eq!(
            scene_shape(&bitstream(BitstreamCodec::Eac3, 0)),
            Some((MIX_RATE, MIX_FRAMES))
        );
        assert_eq!(scene_shape(&bitstream(BitstreamCodec::Ac3, 48_000)), None);
        assert_eq!(scene_shape(&bitstream(BitstreamCodec::Eac3, 32_000)), None);
    }
}
