//! The endpoint upmix stage (ADR 0021, ADR 0035 with its 2026-10-16
//! correction).
//!
//! Stereo and mono scenes (at most two objects besides LFE) of the main, music
//! and PASS endpoints (0, 1 and 2) go through the endpoint's [`Upmixer`] when
//! one is installed, the output kind in use applies upmixing and the scene
//! rate is the one the upmixer runs at. Anything else, and native surround
//! always, reaches the mixer untouched and undelayed. Whether a scene is
//! stereo is judged from the decoded scene's shape, not the source format:
//! a bitstream's layout changes mid-stream.
//!
//! While it upmixes, the stage is a constant delay with as many frames out as
//! in, so the mixer lane's transport accounting does not change:
//!
//! ```text
//! D = 2·B + L + C      B = rate/100 (block), L = upmixer latency, C = rate/500 (2 ms)
//! ```
//!
//! 1. **Fold.** The scene's objects are panned onto the upmixer's input
//!    speakers with the renderer's [`PanGeometry`], interpolating each
//!    object's gains across the scene. LFE goes to the input LFE, or around
//!    the upmixer through its own delay line of D frames.
//! 2. **Blocks.** Every B folded frames are one block for the upmixer, which
//!    returns the block before (one block of pipeline, ADR 0021).
//! 3. **Fallback.** A block the upmixer does not return in time is the
//!    folded input delayed by L, laid onto the outputs: input speakers that are
//!    outputs go there, others are panned onto the outputs.
//! 4. **FIFO.** Each restart prefills the output FIFO with B + C frames of
//!    silence. It never runs dry, and it still holds C frames of the last
//!    plugin block when a miss is known, so they fade out to the fallback.
//! 5. **Emit.** Each scene of n frames leaves as one scene of n frames: an
//!    object at each non-LFE output speaker, and the LFE.
//!
//! The chain sends every scene of endpoints 0 to 2 through one helper,
//! `place_scene`, so no decode path skips the stage.
//! The stage decides between upmixing and transparent at the first scene after
//! each restart of its mixer lane, and a change of that decision (a setting,
//! or a bitstream's stereo/surround flip) restarts the lane.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};

use render::{PanGeometry, Ramp};
use ssr_types::app::UpmixBypass;
use ssr_types::{Direction, Layout, MAX_OBJECTS, Object, Position, SceneBlock, Speaker};
use streamring::{Pipeline, Publish, Publisher, RegionMemory, Signal};

use crate::chain::supported_source_rate;
use crate::mixer::SceneMixer;

/// Endpoints with an upmix stage: main (0), music (1) and PASS (2).
pub const UPMIX_ENDPOINTS: usize = 3;
/// Most input or output speakers an upmixer may have.
pub const MAX_UPMIX_CHANNELS: usize = 32;
/// The longest upmixer latency accepted, in seconds (ADR 0021).
pub const MAX_UPMIX_LATENCY_SECONDS: usize = 2;
/// Objects of a stereo or mono scene: at most two besides LFE.
const STEREO_OBJECTS: usize = 2;
/// The longest scene the stage emits at once; a longer incoming scene (only
/// from a chain built with a larger `max_block`) leaves in pieces.
const EMIT_FRAMES: usize = decode::MAX_SCENE_FRAMES;
const _: () = assert!(MAX_UPMIX_CHANNELS < MAX_OBJECTS);
const _: () = assert!(UPMIX_ENDPOINTS <= ssr_abi::MAX_ENDPOINTS);

/// An upmixer the stage drives one 10 ms block at a time.
pub trait Upmixer: Send {
    fn rate(&self) -> u32;
    /// `rate / 100` (10 ms).
    fn block_frames(&self) -> usize;
    /// Input speakers in plane order: what the plugin's input bus is (1..=32,
    /// distinct, at most one LFE).
    fn inputs(&self) -> &[Speaker];
    /// Output speakers in plane order (1..=32, distinct, at most one LFE).
    fn outputs(&self) -> &[Speaker];
    /// Plugin latency in frames at `rate()` (≤ 2 s).
    fn latency_frames(&self) -> usize;
    /// Submits app block `block` (`inputs().len()` planes of `block_frames`,
    /// plane-major) under `generation`. Also writes block `block - 1`'s result
    /// into `output` (`outputs().len()` planes), but only when that block was
    /// submitted under the same generation and has completed. Returns whether
    /// `output` holds it. Never blocks, locks or allocates.
    fn process(&mut self, block: u64, generation: u64, input: &[f32], output: &mut [f32]) -> bool;
}

/// What is wrong with an upmixer's input or output speakers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpeakerFault {
    /// Not 1 to [`MAX_UPMIX_CHANNELS`] speakers.
    Count,
    /// A speaker appears twice.
    Duplicate,
    /// More than one LFE (`Lfe` and `Lfe2` both count).
    Lfe,
}

/// How a shared-memory ring disagrees with the [`RingUpmixer`] built on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RingFault {
    /// The ring has more than one lane.
    Lanes,
    /// The ring's input channels are not the input speaker count.
    InputChannels,
    /// The ring's output channels are not the output speaker count.
    OutputChannels,
}

/// Why an upmixer cannot be installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum UpmixError {
    #[error("upmixing runs on endpoints 0, 1 and 2 only")]
    Endpoint,
    #[error("the upmixer's input speakers are invalid: {0:?}")]
    Inputs(SpeakerFault),
    #[error("the upmixer's output speakers are invalid: {0:?}")]
    Outputs(SpeakerFault),
    #[error("the upmixer's sample rate is not a supported source rate")]
    Rate,
    #[error("the upmixer's block is not 10 ms")]
    BlockFrames,
    #[error("the upmixer's latency exceeds two seconds")]
    Latency,
    #[error("scenes cannot be panned onto the upmixer's input speakers")]
    InputLayout,
    #[error("the fallback cannot be panned onto the upmixer's output speakers")]
    OutputLayout,
    #[error("the shared-memory ring does not match the upmixer: {0:?}")]
    Ring(RingFault),
}

fn is_lfe(speaker: Speaker) -> bool {
    matches!(speaker, Speaker::Lfe | Speaker::Lfe2)
}

fn check_speakers(speakers: &[Speaker]) -> Result<(), SpeakerFault> {
    if !(1..=MAX_UPMIX_CHANNELS).contains(&speakers.len()) {
        return Err(SpeakerFault::Count);
    }
    for (index, speaker) in speakers.iter().enumerate() {
        if speakers[..index].contains(speaker) {
            return Err(SpeakerFault::Duplicate);
        }
    }
    if speakers.iter().filter(|speaker| is_lfe(**speaker)).count() > 1 {
        return Err(SpeakerFault::Lfe);
    }
    Ok(())
}

/// Checks what [`RingUpmixer::new`] and [`UpmixStage::new`] share: speakers,
/// rate, block and latency.
fn check_shape(
    inputs: &[Speaker],
    outputs: &[Speaker],
    rate: u32,
    block_frames: usize,
    latency_frames: usize,
) -> Result<(), UpmixError> {
    check_speakers(inputs).map_err(UpmixError::Inputs)?;
    check_speakers(outputs).map_err(UpmixError::Outputs)?;
    if !supported_source_rate(rate) {
        return Err(UpmixError::Rate);
    }
    if block_frames != rate as usize / 100 {
        return Err(UpmixError::BlockFrames);
    }
    if latency_frames > rate as usize * MAX_UPMIX_LATENCY_SECONDS {
        return Err(UpmixError::Latency);
    }
    Ok(())
}

/// The panning geometry of `speakers`' positioned (non-LFE) members, and the
/// plane of each of its directions. `None` when there are none or the
/// renderer cannot pan between them.
fn speaker_geometry(speakers: &[Speaker]) -> Option<(PanGeometry, Vec<usize>)> {
    let mut directions = Vec::with_capacity(speakers.len());
    let mut planes = Vec::with_capacity(speakers.len());
    for (plane, speaker) in speakers.iter().enumerate() {
        if let Some((azimuth, elevation)) = speaker.angles() {
            directions.push(Direction::new(speaker.name(), azimuth, elevation));
            planes.push(plane);
        }
    }
    let layout = Layout::new(directions).ok()?;
    let geometry = PanGeometry::new(&layout).ok()?;
    Some((geometry, planes))
}

/// A deterministic in-process upmixer: a fixed matrix behind a delay line, one
/// block behind like a plugin. The tests use it, and so will the supervisor's
/// fake plugin host.
///
/// The matrix, per frame (stereo → 5.1, outputs L R C LFE Ls Rs):
///
/// ```text
/// L  = L          C   = 0.5·(L + R)
/// R  = R          LFE = one-pole low-pass at 120 Hz of 0.5·(L + R), plus the LFE input
/// Ls = 0.5·L      Rs  = 0.5·R
/// ```
///
/// Every output then passes a delay line of `latency_frames`. Fault script:
/// [`MatrixUpmixer::with_misses`] holds back chosen blocks' results,
/// [`MatrixUpmixer::with_nan`] makes one block's result NaN.
pub struct MatrixUpmixer {
    rate: u32,
    block_frames: usize,
    latency_frames: usize,
    inputs: &'static [Speaker],
    alpha: f32,
    low_pass: f32,
    /// `OUTPUTS_5_1` rings of `latency_frames`.
    delay: Vec<f32>,
    cursor: usize,
    /// The last submitted block's result, `OUTPUTS_5_1` planes.
    result: Vec<f32>,
    /// That block and its generation.
    ready: Option<(u64, u64)>,
    misses: Vec<u64>,
    nan: Option<u64>,
}

impl fmt::Debug for MatrixUpmixer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MatrixUpmixer")
            .field("rate", &self.rate)
            .field("inputs", &self.inputs)
            .field("latency_frames", &self.latency_frames)
            .field("misses", &self.misses)
            .field("nan", &self.nan)
            .finish_non_exhaustive()
    }
}

impl MatrixUpmixer {
    pub const STEREO: [Speaker; 2] = [Speaker::L, Speaker::R];
    pub const STEREO_LFE: [Speaker; 3] = [Speaker::L, Speaker::R, Speaker::Lfe];
    pub const OUTPUTS_5_1: [Speaker; 6] = [
        Speaker::L,
        Speaker::R,
        Speaker::C,
        Speaker::Lfe,
        Speaker::Ls,
        Speaker::Rs,
    ];

    /// Stereo (L R) to 5.1.
    pub fn stereo_to_5_1(rate: u32, latency_frames: usize) -> Self {
        Self::new(rate, latency_frames, &Self::STEREO)
    }

    /// Stereo with LFE (L R LFE) to 5.1; the LFE input is added to the LFE
    /// output.
    pub fn stereo_lfe_to_5_1(rate: u32, latency_frames: usize) -> Self {
        Self::new(rate, latency_frames, &Self::STEREO_LFE)
    }

    fn new(rate: u32, latency_frames: usize, inputs: &'static [Speaker]) -> Self {
        let block_frames = rate as usize / 100;
        let outputs = Self::OUTPUTS_5_1.len();
        Self {
            rate,
            block_frames,
            latency_frames,
            inputs,
            alpha: 1.0 - (-2.0 * std::f32::consts::PI * 120.0 / rate.max(1) as f32).exp(),
            low_pass: 0.0,
            delay: vec![0.0; outputs * latency_frames],
            cursor: 0,
            result: vec![0.0; outputs * block_frames],
            ready: None,
            misses: Vec::new(),
            nan: None,
        }
    }

    /// The results of these app blocks never arrive in time: the call that
    /// would return one returns false.
    pub fn with_misses(mut self, blocks: &[u64]) -> Self {
        self.misses = blocks.to_vec();
        self
    }

    /// Every sample of app block `block`'s result is NaN.
    pub fn with_nan(mut self, block: u64) -> Self {
        self.nan = Some(block);
        self
    }

    /// The matrix and delay line alone, without the pipeline: `input` holds
    /// `inputs().len()` planes of one block, `output` gets the six output
    /// planes. A host-side test double runs this on the other end of a ring.
    pub fn matrix_block(&mut self, input: &[f32], output: &mut [f32]) {
        let frames = self.block_frames;
        let outputs = Self::OUTPUTS_5_1.len();
        let has_lfe = self.inputs.len() == Self::STEREO_LFE.len();
        for frame in 0..frames {
            let left = input[frame];
            let right = input[frames + frame];
            let lfe = if has_lfe {
                input[2 * frames + frame]
            } else {
                0.0
            };
            let middle = 0.5 * (left + right);
            self.low_pass += self.alpha * (middle - self.low_pass);
            let values = [
                left,
                right,
                middle,
                self.low_pass + lfe,
                0.5 * left,
                0.5 * right,
            ];
            for (channel, value) in values.into_iter().enumerate() {
                output[channel * frames + frame] = if self.latency_frames == 0 {
                    value
                } else {
                    let index = channel * self.latency_frames + self.cursor;
                    std::mem::replace(&mut self.delay[index], value)
                };
            }
            if self.latency_frames != 0 {
                self.cursor = (self.cursor + 1) % self.latency_frames;
            }
        }
        debug_assert!(output.len() >= outputs * frames);
    }
}

impl Upmixer for MatrixUpmixer {
    fn rate(&self) -> u32 {
        self.rate
    }

    fn block_frames(&self) -> usize {
        self.block_frames
    }

    fn inputs(&self) -> &[Speaker] {
        self.inputs
    }

    fn outputs(&self) -> &[Speaker] {
        &Self::OUTPUTS_5_1
    }

    fn latency_frames(&self) -> usize {
        self.latency_frames
    }

    fn process(&mut self, block: u64, generation: u64, input: &[f32], output: &mut [f32]) -> bool {
        let delivered = match self.ready {
            Some((ready, ready_generation)) => {
                ready.checked_add(1) == Some(block)
                    && ready_generation == generation
                    && !self.misses.contains(&ready)
            }
            None => false,
        };
        if delivered {
            let samples = self.result.len();
            if self.nan == self.ready.map(|(ready, _)| ready) {
                output[..samples].fill(f32::NAN);
            } else {
                output[..samples].copy_from_slice(&self.result);
            }
        }
        let mut result = std::mem::take(&mut self.result);
        self.matrix_block(input, &mut result);
        self.result = result;
        self.ready = Some((block, generation));
        delivered
    }
}

/// The producer side of a plugin host (ADR 0021): each block is published to
/// a one-lane `streamring` region, and the block before is read back when the
/// host has completed it in time and under the same generation.
pub struct RingUpmixer {
    memory: Box<dyn RegionMemory>,
    work: Box<dyn Signal>,
    publisher: Publisher,
    pipeline: Pipeline,
    /// The last published sequence (0 before the first).
    sequence: u32,
    inputs: Vec<Speaker>,
    outputs: Vec<Speaker>,
    latency_frames: usize,
}

impl fmt::Debug for RingUpmixer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RingUpmixer")
            .field("geometry", &self.publisher.geometry())
            .field("inputs", &self.inputs)
            .field("outputs", &self.outputs)
            .field("latency_frames", &self.latency_frames)
            .field("sequence", &self.sequence)
            .finish_non_exhaustive()
    }
}

impl RingUpmixer {
    /// `publisher` is the announced region's real-time half; `memory` keeps
    /// that region mapped and `work` wakes the host after each publish.
    pub fn new(
        memory: Box<dyn RegionMemory>,
        work: Box<dyn Signal>,
        publisher: Publisher,
        inputs: Vec<Speaker>,
        outputs: Vec<Speaker>,
        latency_frames: usize,
    ) -> Result<Self, UpmixError> {
        let geometry = publisher.geometry();
        if geometry.lanes != 1 {
            return Err(UpmixError::Ring(RingFault::Lanes));
        }
        if geometry.input_channels as usize != inputs.len() {
            return Err(UpmixError::Ring(RingFault::InputChannels));
        }
        if geometry.output_channels as usize != outputs.len() {
            return Err(UpmixError::Ring(RingFault::OutputChannels));
        }
        check_shape(
            &inputs,
            &outputs,
            geometry.sample_rate,
            geometry.max_frames as usize,
            latency_frames,
        )?;
        Ok(Self {
            memory,
            work,
            publisher,
            pipeline: Pipeline::new(),
            sequence: 0,
            inputs,
            outputs,
            latency_frames,
        })
    }
}

impl Upmixer for RingUpmixer {
    fn rate(&self) -> u32 {
        self.publisher.geometry().sample_rate
    }

    fn block_frames(&self) -> usize {
        self.publisher.geometry().max_frames as usize
    }

    fn inputs(&self) -> &[Speaker] {
        &self.inputs
    }

    fn outputs(&self) -> &[Speaker] {
        &self.outputs
    }

    fn latency_frames(&self) -> usize {
        self.latency_frames
    }

    fn process(&mut self, block: u64, generation: u64, input: &[f32], output: &mut [f32]) -> bool {
        let region = self.memory.region();
        let frames = self.publisher.geometry().max_frames as usize;
        let inputs = self.inputs.len();
        // The next sequence: 1-based, wrapping, never 0. It advances only
        // when published, so the host never meets a gap.
        let sequence = match self.sequence.wrapping_add(1) {
            0 => 1,
            next => next,
        };
        let published =
            self.publisher
                .try_publish(region, 0, sequence, Some(self.work.as_ref()), |slot| {
                    for plane in 0..inputs {
                        for frame in 0..frames {
                            slot.write(plane, frame, input[plane * frames + frame]);
                        }
                    }
                });
        if published == Publish::Published {
            self.sequence = sequence;
            self.pipeline.published(sequence, block, generation);
        }
        let completed = self.publisher.completed(region, 0);
        let Some(result) = self.pipeline.output_sequence(block, generation, completed) else {
            return false;
        };
        let outputs = self.outputs.len();
        self.publisher.try_read(region, 0, result, |slot| {
            for plane in 0..outputs {
                for frame in 0..frames {
                    output[plane * frames + frame] = slot.read(plane, frame);
                }
            }
        })
    }
}

/// Where an upmixing stage takes its output from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpmixMode {
    /// The upmixer's blocks (a single missed block is still concealed).
    Plugin,
    /// Three blocks in a row were missed; the fallback plays until the
    /// upmixer delivers again (ADR 0021).
    Fallback,
}

/// An endpoint's upmix stage as the control thread sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpmixStatus {
    pub installed: bool,
    /// The stage is upmixing now (not transparent).
    pub upmixing: bool,
    /// Why it is transparent while installed; `None` while upmixing or while
    /// nothing plays.
    pub bypass: Option<UpmixBypass>,
    /// Plugin or Fallback, meaningful while upmixing.
    pub mode: UpmixMode,
    /// D, at `rate`.
    pub delay_frames: u32,
    pub rate: u32,
    pub inputs: Vec<Speaker>,
    pub outputs: Vec<Speaker>,
    pub delivered_blocks: u64,
    pub missed_blocks: u64,
    pub nonfinite_samples: u64,
}

impl UpmixStatus {
    /// An endpoint without an upmixer.
    pub fn not_installed() -> Self {
        Self {
            installed: false,
            upmixing: false,
            bypass: None,
            mode: UpmixMode::Plugin,
            delay_frames: 0,
            rate: 0,
            inputs: Vec::new(),
            outputs: Vec::new(),
            delivered_blocks: 0,
            missed_blocks: 0,
            nonfinite_samples: 0,
        }
    }

    pub fn delay_ms(&self) -> f32 {
        if self.rate == 0 {
            0.0
        } else {
            self.delay_frames as f32 * 1_000.0 / self.rate as f32
        }
    }
}

const BYPASS_NONE: u8 = 0;

fn bypass_wire(bypass: Option<UpmixBypass>) -> u8 {
    match bypass {
        None => BYPASS_NONE,
        Some(UpmixBypass::NativeSurround) => 1,
        Some(UpmixBypass::KindOff) => 2,
        Some(UpmixBypass::RateChange) => 3,
    }
}

fn bypass_from_wire(value: u8) -> Option<UpmixBypass> {
    match value {
        1 => Some(UpmixBypass::NativeSurround),
        2 => Some(UpmixBypass::KindOff),
        3 => Some(UpmixBypass::RateChange),
        _ => None,
    }
}

/// The stage's live state, written by the capture thread without allocating.
#[derive(Debug, Default)]
struct UpmixShared {
    upmixing: AtomicBool,
    bypass: AtomicU8,
    fallback: AtomicBool,
    delivered: AtomicU64,
    missed: AtomicU64,
    nonfinite: AtomicU64,
}

/// Reads a stage's [`UpmixStatus`] from any thread, after the stage has moved
/// to the capture thread. The speakers are copied when it is made.
#[derive(Clone, Debug)]
pub struct UpmixMonitor {
    shared: Arc<UpmixShared>,
    inputs: Vec<Speaker>,
    outputs: Vec<Speaker>,
    delay_frames: u32,
    rate: u32,
}

impl UpmixMonitor {
    pub fn status(&self) -> UpmixStatus {
        let shared = &self.shared;
        UpmixStatus {
            installed: true,
            upmixing: shared.upmixing.load(Ordering::Acquire),
            bypass: bypass_from_wire(shared.bypass.load(Ordering::Acquire)),
            mode: if shared.fallback.load(Ordering::Acquire) {
                UpmixMode::Fallback
            } else {
                UpmixMode::Plugin
            },
            delay_frames: self.delay_frames,
            rate: self.rate,
            inputs: self.inputs.clone(),
            outputs: self.outputs.clone(),
            delivered_blocks: shared.delivered.load(Ordering::Acquire),
            missed_blocks: shared.missed.load(Ordering::Acquire),
            nonfinite_samples: shared.nonfinite.load(Ordering::Acquire),
        }
    }
}

/// What the stage does with the scenes of one lane generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Route {
    Upmix,
    Bypass(UpmixBypass),
}

/// The shape of a scene that decides its route.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Content {
    /// At most two objects besides LFE: stereo or mono.
    stereo: bool,
    rate: u32,
}

impl Content {
    fn of(scene: &SceneBlock<'_>) -> Self {
        Self {
            stereo: scene.objects.len() <= STEREO_OBJECTS,
            rate: scene.rate,
        }
    }
}

#[inline]
fn pan_ramp_gain(from: f32, to: f32, frame: usize, frames: usize) -> f32 {
    // The mixer's renderer ramp: one step per frame, the last frame at `to`.
    if frame < frames {
        from + (to - from) / frames as f32 * (frame + 1) as f32
    } else {
        to
    }
}

/// One endpoint's upmixer with the buffers that fold, delay and conceal around
/// it. Built and validated on the control thread ([`UpmixStage::new`]),
/// installed through `LiveChange::Upmixer`; on the capture thread it neither
/// allocates nor locks.
pub struct UpmixStage {
    endpoint: u16,
    upmixer: Box<dyn Upmixer>,
    qpc_frequency: u64,
    rate: u32,
    /// B.
    block: usize,
    /// L.
    latency: usize,
    /// C.
    fade: usize,
    /// D = 2·B + L + C.
    delay: usize,
    delay_ticks: u64,
    /// Upmixer input planes.
    inputs: usize,
    input_lfe: Option<usize>,
    /// Upmixer output planes.
    outputs: usize,
    /// FIFO planes: the outputs, plus an LFE plane when they have none.
    planes: usize,
    lfe_plane: usize,
    /// The emitted scene always carries an LFE (the outputs or inputs have one).
    fixed_lfe: bool,
    /// The emitted objects: FIFO plane and position, in output order.
    objects: Vec<(usize, Position)>,
    // Folding.
    input_geometry: PanGeometry,
    /// The input plane of each of `input_geometry`'s directions.
    input_planes: Vec<usize>,
    unit: [f32; MAX_UPMIX_CHANNELS],
    previous_gains: [[f32; MAX_UPMIX_CHANNELS]; STEREO_OBJECTS],
    target_gains: [[f32; MAX_UPMIX_CHANNELS]; STEREO_OBJECTS],
    gains_valid: [bool; STEREO_OBJECTS],
    previous_objects: usize,
    // Blocks.
    /// The block being collected, `inputs` planes of B.
    input_block: Vec<f32>,
    collected: usize,
    /// The folded input of the last 2·B + L frames, `inputs` rings.
    history: Vec<f32>,
    history_frames: usize,
    /// Fallback entries: FIFO plane, input plane, gain.
    fallback_matrix: Vec<(usize, usize, f32)>,
    /// The fallback of the block being appended, `planes` of B.
    fallback: Vec<f32>,
    /// The fallback of the last C frames appended, `planes` of C.
    fallback_tail: Vec<f32>,
    /// The upmixer's result, `outputs` planes of B.
    result: Vec<f32>,
    generation: u64,
    next_block: u64,
    plugin_previous: bool,
    misses: u32,
    // Output.
    /// `planes` rings of `capacity` frames.
    fifo: Vec<f32>,
    capacity: usize,
    head: usize,
    len: usize,
    /// The bypassed source LFE (inputs without LFE), D + B frames.
    lfe_line: Vec<f32>,
    lfe_seen: bool,
    frames_in: u64,
    frames_out: u64,
    /// `objects.len() + 1` planes of `EMIT_FRAMES`, the LFE last.
    emit: Vec<f32>,
    first_scene: bool,
    // State.
    upmixing: bool,
    route: Option<Route>,
    shared: Arc<UpmixShared>,
    monitor: UpmixMonitor,
    #[cfg(test)]
    low_water: Option<usize>,
}

impl fmt::Debug for UpmixStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UpmixStage")
            .field("endpoint", &self.endpoint)
            .field("rate", &self.rate)
            .field("inputs", &self.monitor.inputs)
            .field("outputs", &self.monitor.outputs)
            .field("delay_frames", &self.delay)
            .field("upmixing", &self.upmixing)
            .finish_non_exhaustive()
    }
}

impl UpmixStage {
    /// Validates `upmixer` for `endpoint` (0 to 2) and allocates every buffer
    /// the stage will use. Call on the control thread.
    pub fn new(
        endpoint: u16,
        upmixer: Box<dyn Upmixer>,
        qpc_frequency: u64,
    ) -> Result<Self, UpmixError> {
        if endpoint as usize >= UPMIX_ENDPOINTS {
            return Err(UpmixError::Endpoint);
        }
        let input_speakers = upmixer.inputs().to_vec();
        let output_speakers = upmixer.outputs().to_vec();
        let rate = upmixer.rate();
        let latency = upmixer.latency_frames();
        check_shape(
            &input_speakers,
            &output_speakers,
            rate,
            upmixer.block_frames(),
            latency,
        )?;
        let (input_geometry, input_planes) =
            speaker_geometry(&input_speakers).ok_or(UpmixError::InputLayout)?;
        let (output_geometry, output_planes) =
            speaker_geometry(&output_speakers).ok_or(UpmixError::OutputLayout)?;

        let block = rate as usize / 100;
        let fade = rate as usize / 500;
        let delay = 2 * block + latency + fade;
        let inputs = input_speakers.len();
        let outputs = output_speakers.len();
        let input_lfe = input_speakers.iter().position(|speaker| is_lfe(*speaker));
        let output_lfe = output_speakers.iter().position(|speaker| is_lfe(*speaker));
        let planes = outputs + usize::from(output_lfe.is_none());
        let lfe_plane = output_lfe.unwrap_or(outputs);

        // Input speakers that are outputs go there; LFE to LFE; the rest are
        // panned onto the outputs as the renderer would.
        let mut fallback_matrix = Vec::new();
        let mut gains = vec![0.0; output_geometry.directions()];
        for (input, &speaker) in input_speakers.iter().enumerate() {
            if is_lfe(speaker) {
                fallback_matrix.push((lfe_plane, input, 1.0));
            } else if let Some(plane) = output_speakers.iter().position(|output| *output == speaker)
            {
                fallback_matrix.push((plane, input, 1.0));
            } else if let Some(position) = speaker.position() {
                output_geometry.gains(position, 0.0, &mut gains);
                for (direction, &gain) in gains.iter().enumerate() {
                    if gain != 0.0 {
                        fallback_matrix.push((output_planes[direction], input, gain));
                    }
                }
            }
        }
        let objects: Vec<(usize, Position)> = output_speakers
            .iter()
            .enumerate()
            .filter_map(|(plane, speaker)| speaker.position().map(|position| (plane, position)))
            .collect();

        let history_frames = 2 * block + latency;
        // B + C of prefill and one block: the most ever held (see `process`).
        let capacity = 2 * block + fade;
        let shared = Arc::new(UpmixShared::default());
        let monitor = UpmixMonitor {
            shared: Arc::clone(&shared),
            inputs: input_speakers,
            outputs: output_speakers,
            delay_frames: delay as u32,
            rate,
        };
        Ok(Self {
            endpoint,
            upmixer,
            qpc_frequency,
            rate,
            block,
            latency,
            fade,
            delay,
            delay_ticks: (delay as u128 * u128::from(qpc_frequency) / u128::from(rate)) as u64,
            inputs,
            input_lfe,
            outputs,
            planes,
            lfe_plane,
            fixed_lfe: output_lfe.is_some() || input_lfe.is_some(),
            input_geometry,
            input_planes,
            unit: [0.0; MAX_UPMIX_CHANNELS],
            previous_gains: [[0.0; MAX_UPMIX_CHANNELS]; STEREO_OBJECTS],
            target_gains: [[0.0; MAX_UPMIX_CHANNELS]; STEREO_OBJECTS],
            gains_valid: [false; STEREO_OBJECTS],
            previous_objects: 0,
            input_block: vec![0.0; inputs * block],
            collected: 0,
            history: vec![0.0; inputs * history_frames],
            history_frames,
            fallback_matrix,
            fallback: vec![0.0; planes * block],
            fallback_tail: vec![0.0; planes * fade],
            result: vec![0.0; outputs * block],
            generation: 0,
            next_block: 0,
            plugin_previous: false,
            misses: 0,
            fifo: vec![0.0; planes * capacity],
            capacity,
            head: 0,
            len: 0,
            lfe_line: if input_lfe.is_none() {
                vec![0.0; delay + block]
            } else {
                Vec::new()
            },
            lfe_seen: false,
            frames_in: 0,
            frames_out: 0,
            emit: vec![0.0; (objects.len() + 1) * EMIT_FRAMES],
            objects,
            first_scene: true,
            upmixing: false,
            route: None,
            shared,
            monitor,
            #[cfg(test)]
            low_water: None,
        })
    }

    pub fn endpoint(&self) -> u16 {
        self.endpoint
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    pub fn qpc_frequency(&self) -> u64 {
        self.qpc_frequency
    }

    /// D = 2·B + L + C frames at [`UpmixStage::rate`].
    pub fn delay_frames(&self) -> usize {
        self.delay
    }

    /// A status reader that stays valid after the stage moves to the capture
    /// thread. Allocates; call on the control thread.
    pub fn monitor(&self) -> UpmixMonitor {
        self.monitor.clone()
    }

    pub fn status(&self) -> UpmixStatus {
        self.monitor.status()
    }

    /// Whether the scenes of the current lane generation are upmixed.
    pub fn upmixing(&self) -> bool {
        self.upmixing
    }

    fn route_for(&self, apply: bool, content: Content) -> Route {
        if !content.stereo {
            Route::Bypass(UpmixBypass::NativeSurround)
        } else if !apply {
            Route::Bypass(UpmixBypass::KindOff)
        } else if content.rate != self.rate {
            Route::Bypass(UpmixBypass::RateChange)
        } else {
            Route::Upmix
        }
    }

    /// Publishes the route; `None` while nothing plays.
    fn set_route(&mut self, route: Option<Route>) {
        self.route = route;
        let (upmixing, bypass) = match route {
            Some(Route::Upmix) => (true, None),
            Some(Route::Bypass(reason)) => (false, Some(reason)),
            None => (false, None),
        };
        self.shared.upmixing.store(upmixing, Ordering::Release);
        self.shared
            .bypass
            .store(bypass_wire(bypass), Ordering::Release);
    }

    /// Starts a lane generation on `route`. Upmixing starts a new upmixer
    /// generation from block 0 with the FIFO prefilled with B + C frames of
    /// silence. Clears only what the new generation reads, so a long latency
    /// costs nothing here.
    fn restart(&mut self, route: Route) {
        self.upmixing = route == Route::Upmix;
        self.set_route(Some(route));
        if !self.upmixing {
            return;
        }
        self.generation = self.generation.wrapping_add(1);
        self.next_block = 0;
        self.collected = 0;
        self.frames_in = 0;
        self.frames_out = 0;
        self.head = 0;
        self.len = self.block + self.fade;
        for plane in 0..self.planes {
            let start = plane * self.capacity;
            self.fifo[start..start + self.len].fill(0.0);
        }
        self.fallback_tail.fill(0.0);
        self.plugin_previous = false;
        self.misses = 0;
        self.shared.fallback.store(false, Ordering::Release);
        self.gains_valid = [false; STEREO_OBJECTS];
        self.previous_objects = 0;
        self.lfe_seen = false;
        self.first_scene = true;
    }

    fn fifo_index(&self, plane: usize, offset: usize) -> usize {
        plane * self.capacity + (self.head + offset) % self.capacity
    }

    /// Upmixes one stereo or mono scene and hands `out` the same number of
    /// frames, D later: one scene, or pieces of at most `EMIT_FRAMES`.
    fn process(&mut self, scene: &SceneBlock<'_>, out: &mut dyn FnMut(&SceneBlock<'_>)) {
        debug_assert!(self.upmixing && scene.is_consistent());
        let count = scene.objects.len().min(STEREO_OBJECTS);
        self.prepare_gains(scene, count);
        let frames = scene.frames;
        let mut offset = 0;
        let mut piece_start = 0;
        let mut emitted = 0;
        // FIFO level between chunks is B + C - collected: a chunk of at most
        // B - collected frames finds at least as many and leaves at most
        // 2·B + C (`capacity`).
        while offset < frames {
            let take = (self.block - self.collected)
                .min(frames - offset)
                .min(EMIT_FRAMES - emitted);
            self.fold(scene, count, offset, take);
            if self.collected == self.block {
                self.finish_block();
                self.collected = 0;
            }
            self.pop(emitted, take);
            emitted += take;
            offset += take;
            if emitted == EMIT_FRAMES || offset == frames {
                self.emit(scene, piece_start, emitted, out);
                piece_start = offset;
                emitted = 0;
            }
        }
        for object in 0..count {
            self.previous_gains[object] = self.target_gains[object];
            self.gains_valid[object] = true;
        }
        self.previous_objects = count;
    }

    /// Each object's input-plane gains for this scene, and where its ramp
    /// starts: the previous scene's gains, unless the object is new.
    fn prepare_gains(&mut self, scene: &SceneBlock<'_>, count: usize) {
        let directions = self.input_geometry.directions();
        for object in 0..count {
            let source = &scene.objects[object];
            self.input_geometry
                .gains(source.position, source.spread, &mut self.unit[..directions]);
            let target = &mut self.target_gains[object];
            target.fill(0.0);
            for (direction, unit) in self.unit[..directions].iter().enumerate() {
                target[self.input_planes[direction]] = *unit * source.gain;
            }
            let new = !self.gains_valid[object]
                || object >= self.previous_objects
                || scene.new_objects & (1_u64 << object) != 0;
            if new {
                self.previous_gains[object] = self.target_gains[object];
            }
        }
    }

    /// Folds `take` frames of the scene from `offset` into the block being
    /// collected and the input history; routes the source LFE.
    fn fold(&mut self, scene: &SceneBlock<'_>, count: usize, offset: usize, take: usize) {
        let frames = scene.frames;
        let block = self.block;
        let history = self.history_frames as u64;
        for index in 0..take {
            let frame = offset + index;
            let time = self.frames_in + index as u64;
            let slot = (time % history) as usize;
            let column = self.collected + index;
            let source_lfe = scene.lfe.map_or(0.0, |lfe| lfe[frame]);
            for plane in 0..self.inputs {
                let value = if Some(plane) == self.input_lfe {
                    source_lfe
                } else {
                    let mut value = 0.0;
                    for object in 0..count {
                        let from = self.previous_gains[object][plane];
                        let to = self.target_gains[object][plane];
                        if from == 0.0 && to == 0.0 {
                            continue;
                        }
                        value += scene.objects[object].signal[frame]
                            * pan_ramp_gain(from, to, frame, frames);
                    }
                    value
                };
                self.input_block[plane * block + column] = value;
                self.history[plane * self.history_frames + slot] = value;
            }
            if !self.lfe_line.is_empty() {
                let line = self.lfe_line.len() as u64;
                self.lfe_line[(time % line) as usize] = source_lfe;
            }
        }
        self.lfe_seen |= scene.lfe.is_some();
        self.collected += take;
        self.frames_in += take as u64;
    }

    /// Submits the full block k and appends output block k - 1 to the FIFO:
    /// the upmixer's result, or the fallback when it has none.
    fn finish_block(&mut self) {
        let block = self.next_block;
        self.next_block = self.next_block.wrapping_add(1);
        self.compute_fallback(block);
        let delivered =
            self.upmixer
                .process(block, self.generation, &self.input_block, &mut self.result);
        // The last C frames of the previous block are still in the FIFO.
        debug_assert!(self.len >= self.fade, "upmix FIFO lost its fade window");
        #[cfg(test)]
        {
            self.low_water = Some(self.low_water.map_or(self.len, |low| low.min(self.len)));
        }
        if block == 0 {
            // Nothing before the restart to return (ADR 0021): the fallback,
            // silent here.
            self.append_fallback();
        } else if delivered {
            self.shared.delivered.fetch_add(1, Ordering::Relaxed);
            self.misses = 0;
            self.shared.fallback.store(false, Ordering::Release);
            self.append_plugin();
        } else {
            self.shared.missed.fetch_add(1, Ordering::Relaxed);
            self.misses = self.misses.saturating_add(1);
            if self.misses >= 3 {
                self.shared.fallback.store(true, Ordering::Release);
            }
            if self.plugin_previous {
                self.fade_out_tail();
            }
            self.append_fallback();
        }
        let (block_frames, fade) = (self.block, self.fade);
        for plane in 0..self.planes {
            let source = plane * block_frames + block_frames - fade;
            self.fallback_tail[plane * fade..(plane + 1) * fade]
                .copy_from_slice(&self.fallback[source..source + fade]);
        }
        self.len += block_frames;
        debug_assert!(self.len <= self.capacity, "upmix FIFO overflowed");
    }

    /// Output block k - 1's fallback: the folded input of that block's time,
    /// L earlier, through the fallback matrix. Silence before the restart.
    fn compute_fallback(&mut self, block: u64) {
        self.fallback.fill(0.0);
        let Some(previous) = block.checked_sub(1) else {
            return;
        };
        let start = previous as i128 * self.block as i128 - self.latency as i128;
        let history = self.history_frames as i128;
        for &(plane, input, gain) in &self.fallback_matrix {
            for frame in 0..self.block {
                let time = start + frame as i128;
                if time < 0 {
                    continue;
                }
                let sample = self.history[input * self.history_frames + (time % history) as usize];
                self.fallback[plane * self.block + frame] += gain * sample;
            }
        }
    }

    fn fade_ramp(&self, frame: usize) -> f32 {
        Ramp {
            from: 0.0,
            to: 1.0,
            frames: self.fade,
        }
        .gain(frame)
    }

    /// Appends the upmixer's block, non-finite samples as 0, crossfading in
    /// over C from the fallback when the block before was the fallback.
    fn append_plugin(&mut self) {
        let fade_in = !self.plugin_previous;
        let mut nonfinite = 0_u64;
        for plane in 0..self.planes {
            for frame in 0..self.block {
                let mut plugin = if plane < self.outputs {
                    self.result[plane * self.block + frame]
                } else {
                    0.0
                };
                if !plugin.is_finite() {
                    plugin = 0.0;
                    nonfinite += 1;
                }
                let value = if fade_in && frame < self.fade {
                    let fallback = self.fallback[plane * self.block + frame];
                    fallback + (plugin - fallback) * self.fade_ramp(frame)
                } else {
                    plugin
                };
                let index = self.fifo_index(plane, self.len + frame);
                self.fifo[index] = value;
            }
        }
        if nonfinite != 0 {
            self.shared
                .nonfinite
                .fetch_add(nonfinite, Ordering::Relaxed);
        }
        self.plugin_previous = true;
    }

    fn append_fallback(&mut self) {
        for plane in 0..self.planes {
            for frame in 0..self.block {
                let index = self.fifo_index(plane, self.len + frame);
                self.fifo[index] = self.fallback[plane * self.block + frame];
            }
        }
        self.plugin_previous = false;
    }

    /// Rewrites the last C frames of the previous plugin block, not yet
    /// emitted, as a linear crossfade to the fallback of the same frames.
    fn fade_out_tail(&mut self) {
        // The FIFO always holds the C frames (`finish_block` asserts it); the
        // bound only keeps a broken invariant from touching other frames.
        let held = self.fade.min(self.len);
        let start = self.len - held;
        let skipped = self.fade - held;
        for plane in 0..self.planes {
            for frame in skipped..self.fade {
                let index = self.fifo_index(plane, start + frame - skipped);
                let plugin = self.fifo[index];
                let fallback = self.fallback_tail[plane * self.fade + frame];
                self.fifo[index] = plugin + (fallback - plugin) * self.fade_ramp(frame);
            }
        }
    }

    /// Moves `take` frames from the FIFO into the emit buffer at `at`, adding
    /// the bypassed source LFE delayed by D.
    fn pop(&mut self, at: usize, take: usize) {
        debug_assert!(self.len >= take, "upmix FIFO ran dry");
        for (row, &(plane, _)) in self.objects.iter().enumerate() {
            for frame in 0..take {
                self.emit[row * EMIT_FRAMES + at + frame] =
                    self.fifo[self.fifo_index(plane, frame)];
            }
        }
        let row = self.objects.len();
        for frame in 0..take {
            let mut value = self.fifo[self.fifo_index(self.lfe_plane, frame)];
            let time = self.frames_out + frame as u64;
            if !self.lfe_line.is_empty() && time >= self.delay as u64 {
                let line = self.lfe_line.len() as u64;
                value += self.lfe_line[((time - self.delay as u64) % line) as usize];
            }
            self.emit[row * EMIT_FRAMES + at + frame] = value;
        }
        self.head = (self.head + take) % self.capacity;
        self.len -= take;
        self.frames_out += take as u64;
    }

    /// Hands `out` the `frames` frames in the emit buffer: frames `start..` of
    /// `scene`, D later.
    fn emit(
        &mut self,
        scene: &SceneBlock<'_>,
        start: usize,
        frames: usize,
        out: &mut dyn FnMut(&SceneBlock<'_>),
    ) {
        let count = self.objects.len();
        let new_objects = if std::mem::take(&mut self.first_scene) {
            (1_u64 << count) - 1
        } else {
            0
        };
        let start_ticks =
            (start as u128 * u128::from(self.qpc_frequency) / u128::from(self.rate)) as u64;
        let source_qpc = scene
            .source_qpc
            .saturating_add(start_ticks)
            .saturating_sub(self.delay_ticks);
        let mut objects = [Object::SILENT; MAX_UPMIX_CHANNELS];
        for (row, &(_, position)) in self.objects.iter().enumerate() {
            objects[row] = Object {
                signal: &self.emit[row * EMIT_FRAMES..row * EMIT_FRAMES + frames],
                position,
                gain: 1.0,
                spread: 0.0,
            };
        }
        let lfe = (self.fixed_lfe || self.lfe_seen)
            .then(|| &self.emit[count * EMIT_FRAMES..count * EMIT_FRAMES + frames]);
        out(&SceneBlock {
            epoch: scene.epoch,
            frames,
            rate: scene.rate,
            source_qpc,
            objects: &objects[..count],
            lfe,
            new_objects,
        });
    }
}

/// One endpoint lane's stage and what it has seen since the lane last
/// restarted.
#[derive(Debug, Default)]
struct UpmixLane {
    stage: Option<Box<UpmixStage>>,
    /// The shape of the last scene placed since the lane's last restart;
    /// `None` until the first.
    content: Option<Content>,
    /// The mixer lane's restart count `content` belongs to.
    restarts: u64,
}

impl UpmixLane {
    /// Forgets the content when the mixer lane has restarted since.
    fn sync(&mut self, mixer: &SceneMixer, endpoint: u16) {
        let restarts = mixer.lane_restarts(endpoint);
        if restarts != self.restarts {
            self.restarts = restarts;
            self.content = None;
        }
    }

    /// Restarts the mixer lane; the stage decides again at its next scene.
    fn restart_lane(&mut self, mixer: &mut SceneMixer, endpoint: u16) {
        mixer.lane_discontinuity(endpoint, 0);
        self.restarts = mixer.lane_restarts(endpoint);
        self.content = None;
    }
}

/// The chain's upmix stages, one slot per upmixable endpoint, and the output
/// kind's apply switch.
#[derive(Debug)]
pub(crate) struct UpmixLanes {
    lanes: [UpmixLane; UPMIX_ENDPOINTS],
    apply: bool,
}

impl UpmixLanes {
    pub(crate) fn new() -> Self {
        Self {
            lanes: Default::default(),
            apply: true,
        }
    }

    /// Installs or removes `endpoint`'s stage and returns the old one, for the
    /// caller to drop off the real-time thread (`stage` itself, changing
    /// nothing, when the endpoint has no stage slot or the stage was built
    /// for another endpoint). While the lane plays, it restarts when the old
    /// stage was upmixing or the new one will.
    pub(crate) fn install(
        &mut self,
        mixer: &mut SceneMixer,
        endpoint: u16,
        stage: Option<Box<UpmixStage>>,
    ) -> Option<Box<UpmixStage>> {
        let apply = self.apply;
        if stage
            .as_deref()
            .is_some_and(|stage| stage.endpoint() != endpoint)
        {
            return stage;
        }
        let Some(lane) = self.lanes.get_mut(endpoint as usize) else {
            return stage;
        };
        lane.sync(mixer, endpoint);
        let was_upmixing = lane.stage.as_deref().is_some_and(UpmixStage::upmixing);
        let old = std::mem::replace(&mut lane.stage, stage);
        let content = lane.content;
        let route = content.and_then(|content| {
            lane.stage
                .as_deref()
                .map(|stage| stage.route_for(apply, content))
        });
        if content.is_some() && (was_upmixing || route == Some(Route::Upmix)) {
            lane.restart_lane(mixer, endpoint);
            if let Some(stage) = lane.stage.as_deref_mut() {
                stage.set_route(route);
            }
        } else if let Some(stage) = lane.stage.as_deref_mut() {
            match route {
                // Transparent before and after: no restart.
                Some(route) => stage.restart(route),
                None => stage.set_route(None),
            }
        }
        old
    }

    /// The output kind's apply switch. A lane that plays restarts when its
    /// stage changes between upmixing and transparent.
    pub(crate) fn set_apply(&mut self, mixer: &mut SceneMixer, on: bool) {
        self.apply = on;
        for (endpoint, lane) in self.lanes.iter_mut().enumerate() {
            let endpoint = endpoint as u16;
            lane.sync(mixer, endpoint);
            let Some(content) = lane.content else {
                continue;
            };
            let Some((upmixing, route)) = lane
                .stage
                .as_deref()
                .map(|stage| (stage.upmixing(), stage.route_for(on, content)))
            else {
                continue;
            };
            if upmixing != (route == Route::Upmix) {
                lane.restart_lane(mixer, endpoint);
            }
            if let Some(stage) = lane.stage.as_deref_mut() {
                stage.set_route(Some(route));
            }
        }
    }

    /// The lane stopped: nothing plays until its next scene.
    pub(crate) fn stopped(&mut self, endpoint: u16) {
        if let Some(stage) = self
            .lanes
            .get_mut(endpoint as usize)
            .and_then(|lane| lane.stage.as_deref_mut())
        {
            stage.set_route(None);
        }
    }

    pub(crate) fn status(&self, endpoint: u16) -> Option<UpmixStatus> {
        let lane = self.lanes.get(endpoint as usize)?;
        Some(
            lane.stage
                .as_deref()
                .map_or_else(UpmixStatus::not_installed, UpmixStage::status),
        )
    }
}

/// Places one decoded scene of `endpoint` into its mixer lane, through the
/// endpoint's upmix stage. Every scene of endpoints 0 to 2 goes through here.
///
/// The first scene after a restart of the mixer lane decides the stage's
/// route. A later scene whose shape (stereo or surround) or rate changes that
/// decision restarts the lane first (`before_restart` runs just before, so a
/// caller can settle its transport in the old lane generation), as a format
/// change would. Never allocates.
pub(crate) fn place_scene(
    upmix: &mut UpmixLanes,
    mixer: &mut SceneMixer,
    endpoint: u16,
    scene: &SceneBlock<'_>,
    geometry: Option<&PanGeometry>,
    before_restart: &mut dyn FnMut(&mut SceneMixer),
) -> Result<(), u32> {
    let apply = upmix.apply;
    let Some(lane) = upmix.lanes.get_mut(endpoint as usize) else {
        return mixer.push_scene(endpoint, scene, geometry);
    };
    lane.sync(mixer, endpoint);
    if mixer.lane_is_idle(endpoint) {
        // The scene starts the lane.
        lane.content = None;
    }
    let content = Content::of(scene);
    let result = match lane.stage.as_deref_mut() {
        None => mixer.push_scene(endpoint, scene, geometry),
        Some(stage) => {
            let route = stage.route_for(apply, content);
            if lane.content.is_some() && stage.upmixing() != (route == Route::Upmix) {
                before_restart(mixer);
                mixer.lane_discontinuity(endpoint, 0);
                lane.restarts = mixer.lane_restarts(endpoint);
                lane.content = None;
            }
            if lane.content.is_none() {
                stage.restart(route);
            } else if stage.route != Some(route) {
                stage.set_route(Some(route));
            }
            if stage.upmixing() {
                let mut error = None;
                stage.process(scene, &mut |emitted| {
                    if let Err(rate) = mixer.push_scene(endpoint, emitted, geometry) {
                        error = Some(rate);
                    }
                });
                error.map_or(Ok(()), Err)
            } else {
                mixer.push_scene(endpoint, scene, geometry)
            }
        }
    };
    lane.content = Some(content);
    // A scene for an idle lane starts it; that start is this scene's own.
    lane.restarts = mixer.lane_restarts(endpoint);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mixer::{BedBlock, MIX_FRAMES, MixerConfig};

    const QPC: u64 = 10_000_000;
    const RATE: u32 = 48_000;
    const B: usize = 480;
    const C: usize = 96;
    /// The FIFO prefill.
    const P: usize = B + C;

    fn stage(upmixer: impl Upmixer + 'static) -> UpmixStage {
        UpmixStage::new(0, Box::new(upmixer), QPC).unwrap()
    }

    fn upmixing(upmixer: impl Upmixer + 'static) -> UpmixStage {
        let mut stage = stage(upmixer);
        stage.restart(Route::Upmix);
        stage
    }

    fn impulse(frames: usize, at: usize, value: f32) -> Vec<f32> {
        let mut signal = vec![0.0; frames];
        signal[at] = value;
        signal
    }

    /// A deterministic signal in [-0.5, 0.5).
    fn noise(frames: usize, seed: u32) -> Vec<f32> {
        let mut state = seed.wrapping_mul(747_796_405).wrapping_add(2_891_336_453);
        (0..frames)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (state >> 8) as f32 / (1 << 24) as f32 - 0.5
            })
            .collect()
    }

    /// What a stage emitted: each output object's plane and the LFE, as one
    /// stream.
    #[derive(Debug, Default)]
    struct Emitted {
        planes: Vec<Vec<f32>>,
        positions: Vec<Position>,
        lfe: Vec<f32>,
        scenes: usize,
        new_objects: Vec<u64>,
        source_qpcs: Vec<u64>,
    }

    impl Emitted {
        fn push(&mut self, scene: &SceneBlock<'_>) {
            assert!(scene.is_consistent());
            if self.planes.is_empty() {
                self.planes = vec![Vec::new(); scene.objects.len()];
                self.positions = scene.objects.iter().map(|object| object.position).collect();
            }
            assert_eq!(scene.objects.len(), self.planes.len());
            for (plane, object) in self.planes.iter_mut().zip(scene.objects) {
                assert_eq!((object.gain, object.spread), (1.0, 0.0));
                plane.extend_from_slice(object.signal);
            }
            match scene.lfe {
                Some(lfe) => self.lfe.extend_from_slice(lfe),
                None => self.lfe.extend(std::iter::repeat_n(0.0, scene.frames)),
            }
            self.scenes += 1;
            self.new_objects.push(scene.new_objects);
            self.source_qpcs.push(scene.source_qpc);
        }

        /// The plane of the output object at `speaker`.
        fn plane(&self, speaker: Speaker) -> &[f32] {
            let position = speaker.position().unwrap();
            let index = self
                .positions
                .iter()
                .position(|candidate| *candidate == position)
                .unwrap();
            &self.planes[index]
        }
    }

    /// Feeds `signals` (one per object at `speakers`) through an upmixing
    /// stage in scenes of `sizes` (cycling) and checks every scene leaves with
    /// exactly its own frame count.
    fn run(
        stage: &mut UpmixStage,
        speakers: &[Speaker],
        signals: &[Vec<f32>],
        lfe: Option<&[f32]>,
        sizes: &[usize],
    ) -> Emitted {
        let frames = signals[0].len();
        let mut emitted = Emitted::default();
        let mut offset = 0;
        let mut size = sizes.iter().cycle();
        while offset < frames {
            let length = (*size.next().unwrap()).min(frames - offset);
            let objects: Vec<Object<'_>> = speakers
                .iter()
                .zip(signals)
                .map(|(speaker, signal)| Object {
                    signal: &signal[offset..offset + length],
                    position: speaker.position().unwrap(),
                    gain: 1.0,
                    spread: 0.0,
                })
                .collect();
            let scene = SceneBlock {
                epoch: 1,
                frames: length,
                rate: RATE,
                source_qpc: 1_000_000 + offset as u64 * QPC / u64::from(RATE),
                objects: &objects,
                lfe: lfe.map(|lfe| &lfe[offset..offset + length]),
                new_objects: 0,
            };
            let mut out_frames = 0;
            stage.process(&scene, &mut |scene| {
                out_frames += scene.frames;
                emitted.push(scene);
            });
            assert_eq!(out_frames, length, "a scene of {length} frames");
            offset += length;
        }
        emitted
    }

    fn only_at(signal: &[f32], at: usize, value: f32) {
        for (frame, sample) in signal.iter().enumerate() {
            let expected = if frame == at { value } else { 0.0 };
            assert_eq!(*sample, expected, "frame {frame}");
        }
    }

    #[test]
    fn delay_is_two_blocks_plus_latency_plus_two_ms() {
        for latency in [0, 37, 480] {
            let mut stage = upmixing(MatrixUpmixer::stereo_to_5_1(RATE, latency));
            let delay = 2 * B + latency + C;
            assert_eq!(stage.delay_frames(), delay);
            assert_eq!(stage.status().delay_frames as usize, delay);
            let t = 1_234;
            let frames = t + delay + 3 * B;
            let left = impulse(frames, t, 1.0);
            let right = vec![0.0; frames];
            let emitted = run(
                &mut stage,
                &MatrixUpmixer::STEREO,
                &[left, right],
                None,
                &[B],
            );
            only_at(emitted.plane(Speaker::L), t + delay, 1.0);
            only_at(emitted.plane(Speaker::C), t + delay, 0.5);
            only_at(emitted.plane(Speaker::Ls), t + delay, 0.5);
            only_at(emitted.plane(Speaker::R), usize::MAX, 0.0);
            only_at(emitted.plane(Speaker::Rs), usize::MAX, 0.0);
            // The low-passed LFE starts at the same frame.
            assert!(emitted.lfe[..t + delay].iter().all(|sample| *sample == 0.0));
            assert!(emitted.lfe[t + delay] > 0.0);
            let status = stage.status();
            assert_eq!(status.missed_blocks, 0);
            assert_eq!(status.delivered_blocks as usize, frames / B - 1);
            // The first scene says every output object is new; its time
            // moves back by D.
            assert_eq!(emitted.new_objects[0], 0b11111);
            assert!(emitted.new_objects[1..].iter().all(|bits| *bits == 0));
            assert_eq!(
                emitted.source_qpcs[1],
                1_000_000 + B as u64 * QPC / u64::from(RATE) - delay as u64 * QPC / u64::from(RATE)
            );
        }
    }

    #[test]
    fn adversarial_scene_sizes_never_drain_the_fifo() {
        let sizes = [1, 7, B - 1, B, B + 1, 3 * B + 5, 1, 2 * EMIT_FRAMES + 3];
        for misses in [&[][..], &[5, 9, 10, 11, 40][..]] {
            let latency = 37;
            let mut stage =
                upmixing(MatrixUpmixer::stereo_to_5_1(RATE, latency).with_misses(misses));
            let delay = 2 * B + latency + C;
            let frames = 2 * RATE as usize;
            let mut left = vec![0.0; frames];
            let impulses = [700, 4_801, 33_333, 70_001];
            for at in impulses {
                left[at] = 1.0;
            }
            let emitted = run(
                &mut stage,
                &MatrixUpmixer::STEREO,
                &[left, vec![0.0; frames]],
                None,
                &sizes,
            );
            assert_eq!(emitted.plane(Speaker::L).len(), frames);
            // Every block boundary found the previous block's last C frames
            // still unsent: the FIFO never runs dry and a miss can fade.
            assert!(stage.low_water.unwrap() >= C, "{:?}", stage.low_water);
            let ones: Vec<usize> = emitted
                .plane(Speaker::L)
                .iter()
                .enumerate()
                .filter(|(_, sample)| **sample != 0.0)
                .map(|(frame, _)| frame)
                .collect();
            let expected: Vec<usize> = impulses.iter().map(|at| at + delay).collect();
            assert_eq!(ones, expected);
            // A scene longer than the emit buffer leaves in pieces.
            assert!(emitted.scenes > frames / (3 * B));
            assert_eq!(stage.status().missed_blocks as usize, misses.len());
        }
    }

    #[test]
    fn stereo_folds_bit_exact_and_mono_splits_by_the_panning_law() {
        let mut stage = upmixing(MatrixUpmixer::stereo_to_5_1(RATE, 0));
        let delay = 2 * B + C;
        let frames = 8 * B;
        let left = noise(frames, 1);
        let right = noise(frames, 2);
        let emitted = run(
            &mut stage,
            &MatrixUpmixer::STEREO,
            &[left.clone(), right.clone()],
            None,
            &[B - 3, 11],
        );
        for frame in 0..frames - delay {
            let out = frame + delay;
            assert_eq!(
                emitted.plane(Speaker::L)[out].to_bits(),
                left[frame].to_bits()
            );
            assert_eq!(
                emitted.plane(Speaker::R)[out].to_bits(),
                right[frame].to_bits()
            );
            // The centre, which the fallback lacks, fades in over the first
            // C frames after the restart.
            if frame >= C {
                assert_eq!(
                    emitted.plane(Speaker::C)[out].to_bits(),
                    (0.5 * (left[frame] + right[frame])).to_bits()
                );
            }
        }

        let mut stage = upmixing(MatrixUpmixer::stereo_to_5_1(RATE, 0));
        let mono = noise(frames, 3);
        let emitted = run(
            &mut stage,
            &[Speaker::C],
            std::slice::from_ref(&mono),
            None,
            &[B],
        );
        let (left, right) = (emitted.plane(Speaker::L), emitted.plane(Speaker::R));
        for (frame, sample) in mono.iter().enumerate().take(frames - delay).skip(B) {
            let (l, r) = (left[frame + delay], right[frame + delay]);
            assert_eq!(l.to_bits(), r.to_bits(), "frame {frame}");
            assert!((l - sample * std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
        }
    }

    #[test]
    fn object_gains_ramp_across_the_scene_unless_new() {
        for new in [false, true] {
            let mut stage = upmixing(MatrixUpmixer::stereo_to_5_1(RATE, 0));
            let ones = vec![1.0; B];
            for (speaker, new_objects) in [(Speaker::L, 0), (Speaker::R, u64::from(new))] {
                let objects = [Object {
                    signal: &ones,
                    position: speaker.position().unwrap(),
                    gain: 1.0,
                    spread: 0.0,
                }];
                let scene = SceneBlock {
                    epoch: 1,
                    frames: B,
                    rate: RATE,
                    source_qpc: 0,
                    objects: &objects,
                    lfe: None,
                    new_objects,
                };
                stage.process(&scene, &mut |_| {});
            }
            // The block just submitted is the second scene.
            for frame in 0..B {
                let (left, right) = (stage.input_block[frame], stage.input_block[B + frame]);
                if new {
                    assert_eq!((left, right), (0.0, 1.0));
                } else {
                    assert_eq!(left, pan_ramp_gain(1.0, 0.0, frame, B));
                    assert_eq!(right, pan_ramp_gain(0.0, 1.0, frame, B));
                }
            }
        }
    }

    #[test]
    fn source_lfe_bypasses_an_upmixer_without_lfe_input_delayed_by_d() {
        let latency = 37;
        let mut stage = upmixing(MatrixUpmixer::stereo_to_5_1(RATE, latency));
        let delay = 2 * B + latency + C;
        let t = 999;
        let frames = t + delay + 2 * B;
        let lfe = impulse(frames, t, 1.0);
        let silent = vec![0.0; frames];
        let emitted = run(
            &mut stage,
            &MatrixUpmixer::STEREO,
            &[silent.clone(), silent],
            Some(&lfe),
            &[333],
        );
        only_at(&emitted.lfe, t + delay, 1.0);
    }

    #[test]
    fn source_lfe_goes_through_an_upmixer_with_lfe_input() {
        let latency = 37;
        let delay = 2 * B + latency + C;
        let t = 999;
        // Block 2 holds t; with its result missed the fallback carries the
        // input LFE to the output LFE the same.
        for misses in [&[][..], &[2][..]] {
            let mut stage =
                upmixing(MatrixUpmixer::stereo_lfe_to_5_1(RATE, latency).with_misses(misses));
            assert!(stage.lfe_line.is_empty());
            let frames = t + delay + 2 * B;
            let lfe = impulse(frames, t, 1.0);
            let silent = vec![0.0; frames];
            let emitted = run(
                &mut stage,
                &MatrixUpmixer::STEREO,
                &[silent.clone(), silent],
                Some(&lfe),
                &[333],
            );
            only_at(&emitted.lfe, t + delay, 1.0);
        }
    }

    /// Always "delivers" a constant: shows which blocks the stage takes from
    /// the upmixer.
    struct Constant(f32);

    impl Upmixer for Constant {
        fn rate(&self) -> u32 {
            RATE
        }
        fn block_frames(&self) -> usize {
            B
        }
        fn inputs(&self) -> &[Speaker] {
            &MatrixUpmixer::STEREO
        }
        fn outputs(&self) -> &[Speaker] {
            &MatrixUpmixer::STEREO
        }
        fn latency_frames(&self) -> usize {
            0
        }
        fn process(&mut self, _: u64, _: u64, _: &[f32], output: &mut [f32]) -> bool {
            output.fill(self.0);
            true
        }
    }

    #[test]
    fn the_first_block_after_a_restart_is_the_fallback() {
        let mut stage = upmixing(Constant(0.25));
        let frames = 6 * B;
        let silent = vec![0.0; frames];
        let emitted = run(
            &mut stage,
            &MatrixUpmixer::STEREO,
            &[silent.clone(), silent],
            None,
            &[B],
        );
        let left = emitted.plane(Speaker::L);
        // Prefill, then block -1 (the fallback of nothing): silence.
        assert!(left[..P + B].iter().all(|sample| *sample == 0.0));
        // Block 0 is the upmixer's, crossfading in from the fallback.
        assert!(
            left[P + B..P + B + C]
                .windows(2)
                .all(|pair| pair[0] < pair[1])
        );
        assert!(left[P + B + C..].iter().all(|sample| *sample == 0.25));
        assert_eq!(stage.status().delivered_blocks as usize, frames / B - 1);
    }

    #[test]
    fn a_generation_change_discards_a_late_result() {
        let mut matrix = MatrixUpmixer::stereo_to_5_1(RATE, 0);
        let input = vec![0.5; 2 * B];
        let mut output = vec![0.0; 6 * B];
        assert!(!matrix.process(0, 1, &input, &mut output));
        assert!(matrix.process(1, 1, &input, &mut output));
        // A restart: block 1's result belongs to generation 1.
        assert!(!matrix.process(0, 2, &input, &mut output));
        assert!(matrix.process(1, 2, &input, &mut output));
        // Out of order within a generation is no result either.
        assert!(!matrix.process(3, 2, &input, &mut output));
    }

    #[test]
    fn a_single_miss_is_concealed_for_exactly_that_block() {
        let mut stage = upmixing(MatrixUpmixer::stereo_to_5_1(RATE, 0).with_misses(&[6]));
        let frames = 12 * B;
        let level = vec![0.4; frames];
        let emitted = run(
            &mut stage,
            &MatrixUpmixer::STEREO,
            &[level.clone(), level],
            None,
            &[B],
        );
        // Output block m sits at P + (m + 1)·B; block 6 is the fallback,
        // where the centre (no input speaker) is silent.
        let centre = emitted.plane(Speaker::C);
        let missed = P + 7 * B;
        assert!(centre[P + B + C..missed - C].iter().all(|s| *s == 0.4));
        let fade_out = &centre[missed - C..missed];
        assert!(fade_out.windows(2).all(|pair| pair[0] > pair[1]));
        assert!(fade_out.iter().all(|s| (0.0..0.4).contains(s)));
        assert_eq!(fade_out[C - 1], 0.0);
        assert!(centre[missed..missed + B].iter().all(|s| *s == 0.0));
        let fade_in = &centre[missed + B..missed + B + C];
        assert!(fade_in.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(fade_in.iter().all(|s| *s > 0.0 && *s <= 0.4));
        assert!(centre[missed + B + C..].iter().all(|s| *s == 0.4));
        // Left is the same in the plugin and the fallback: no step at all.
        assert!(emitted.plane(Speaker::L)[P + B..].iter().all(|s| *s == 0.4));
        let status = stage.status();
        assert_eq!((status.missed_blocks, status.mode), (1, UpmixMode::Plugin));
    }

    #[test]
    fn three_misses_enter_fallback_mode_and_a_delivery_leaves_it() {
        let mut stage = upmixing(MatrixUpmixer::stereo_to_5_1(RATE, 0).with_misses(&[4, 5, 6]));
        let level = vec![0.4; B];
        let mut modes = Vec::new();
        for _ in 0..10 {
            run(
                &mut stage,
                &MatrixUpmixer::STEREO,
                &[level.clone(), level.clone()],
                None,
                &[B],
            );
            modes.push(stage.status().mode);
        }
        // Scene k completes block k; the call for block k + 1 misses k.
        use UpmixMode::{Fallback, Plugin};
        assert_eq!(
            modes,
            [
                Plugin, Plugin, Plugin, Plugin, Plugin, Plugin, Plugin, Fallback, Plugin, Plugin
            ]
        );
        assert_eq!(stage.status().missed_blocks, 3);
    }

    #[test]
    fn a_nan_block_gives_zeros_and_counts_every_sample() {
        let mut stage = upmixing(MatrixUpmixer::stereo_to_5_1(RATE, 0).with_nan(5));
        let frames = 10 * B;
        let level = vec![0.4; frames];
        let emitted = run(
            &mut stage,
            &MatrixUpmixer::STEREO,
            &[level.clone(), level],
            None,
            &[B],
        );
        let block = P + 6 * B;
        for plane in &emitted.planes {
            assert!(plane[block..block + B].iter().all(|s| *s == 0.0));
            assert!(plane[block - 1] != 0.0 && plane[block + B] != 0.0);
        }
        assert!(emitted.lfe[block..block + B].iter().all(|s| *s == 0.0));
        assert_eq!(stage.status().nonfinite_samples, 6 * B as u64);
    }

    // ---- Routing through `place_scene`, as the chain does ----

    fn mixer() -> SceneMixer {
        SceneMixer::new(MixerConfig {
            arrival_slack_frames: 0,
            qpc_frequency: QPC,
            endpoint_mask: u32::MAX,
        })
    }

    fn geometry() -> PanGeometry {
        let directions = [
            ("FL", 30.0),
            ("FR", -30.0),
            ("FC", 0.0),
            ("SL", 110.0),
            ("SR", -110.0),
        ]
        .into_iter()
        .map(|(name, azimuth)| Direction::new(name, azimuth, 0.0))
        .collect();
        PanGeometry::new(&Layout::new(directions).unwrap()).unwrap()
    }

    fn lanes_with(stage: Option<UpmixStage>) -> (UpmixLanes, SceneMixer) {
        let mut lanes = UpmixLanes::new();
        let mut mixer = mixer();
        assert!(lanes.install(&mut mixer, 0, stage.map(Box::new)).is_none());
        (lanes, mixer)
    }

    /// One scene of `speakers` (objects at their positions, `moving` turns
    /// each a little every scene) through `place_scene` or straight into the
    /// mixer, then the lane's transport; returns the mixed beds.
    #[allow(clippy::too_many_arguments)]
    fn place(
        lanes: Option<&mut UpmixLanes>,
        mixer: &mut SceneMixer,
        geometry: &PanGeometry,
        speakers: &[Speaker],
        index: usize,
        moving: bool,
        lfe: bool,
        restarts: &mut usize,
    ) -> Vec<f32> {
        let signals: Vec<Vec<f32>> = (0..speakers.len())
            .map(|object| noise(B, (index * 64 + object) as u32))
            .collect();
        let lfe_signal = noise(B, 9_999 + index as u32);
        let objects: Vec<Object<'_>> = speakers
            .iter()
            .zip(&signals)
            .map(|(speaker, signal)| {
                let (azimuth, elevation) = speaker.angles().unwrap();
                let turn = if moving { index as f32 * 7.0 } else { 0.0 };
                Object {
                    signal,
                    position: Position::from_angles(azimuth + turn, elevation),
                    gain: 0.8,
                    spread: 0.0,
                }
            })
            .collect();
        let scene = SceneBlock {
            epoch: 1,
            frames: B,
            rate: RATE,
            source_qpc: index as u64 * 100_000,
            objects: &objects,
            lfe: lfe.then_some(lfe_signal.as_slice()),
            new_objects: 0,
        };
        match lanes {
            Some(lanes) => place_scene(lanes, mixer, 0, &scene, Some(geometry), &mut |_| {
                *restarts += 1;
            })
            .unwrap(),
            None => mixer.push_scene(0, &scene, Some(geometry)).unwrap(),
        }
        let mut beds = Vec::new();
        mixer.advance(0, B as u64, &mut |bed: &BedBlock<'_>| {
            beds.extend_from_slice(bed.channels);
            beds.extend_from_slice(bed.lfe.unwrap_or(&[0.0; MIX_FRAMES]));
        });
        beds
    }

    const SURROUND: [Speaker; 5] = [Speaker::L, Speaker::R, Speaker::C, Speaker::Ls, Speaker::Rs];

    #[test]
    fn native_surround_is_transparent_and_bit_exact() {
        let geometry = geometry();
        let objects = [Speaker::L, Speaker::C, Speaker::Rs];
        let (mut lanes, mut mixer) = lanes_with(Some(stage(MatrixUpmixer::stereo_to_5_1(RATE, 0))));
        let mut plain = self::mixer();
        let mut restarts = 0;
        for index in 0..40 {
            let staged = place(
                Some(&mut lanes),
                &mut mixer,
                &geometry,
                &objects,
                index,
                true,
                true,
                &mut restarts,
            );
            let direct = place(
                None,
                &mut plain,
                &geometry,
                &objects,
                index,
                true,
                true,
                &mut restarts,
            );
            assert_eq!(
                staged.iter().map(|s| s.to_bits()).collect::<Vec<_>>(),
                direct.iter().map(|s| s.to_bits()).collect::<Vec<_>>(),
                "scene {index}"
            );
        }
        let status = lanes.status(0).unwrap();
        assert!(status.installed && !status.upmixing);
        assert_eq!(status.bypass, Some(UpmixBypass::NativeSurround));
        assert_eq!(restarts, 0);
        assert_eq!(mixer.lane_restarts(0), plain.lane_restarts(0));
    }

    #[test]
    fn a_shape_flip_restarts_the_lane_once_per_flip() {
        let geometry = geometry();
        let (mut lanes, mut mixer) = lanes_with(Some(stage(MatrixUpmixer::stereo_to_5_1(RATE, 0))));
        let mut restarts = 0;
        let mut index = 0;
        let mut part = |lanes: &mut UpmixLanes,
                        mixer: &mut SceneMixer,
                        speakers: &[Speaker],
                        lfe: bool,
                        restarts: &mut usize| {
            let before = mixer.lane_restarts(0);
            for _ in 0..12 {
                place(
                    Some(lanes),
                    mixer,
                    &geometry,
                    speakers,
                    index,
                    false,
                    lfe,
                    restarts,
                );
                index += 1;
            }
            (mixer.lane_restarts(0) - before, lanes.status(0).unwrap())
        };
        // 2.0, then a 5.1 programme, then 2.0 again, in one epoch.
        let (first, stereo) = part(
            &mut lanes,
            &mut mixer,
            &MatrixUpmixer::STEREO,
            false,
            &mut restarts,
        );
        assert_eq!(first, 1, "the lane's own start");
        assert!(stereo.upmixing && stereo.bypass.is_none());
        let objects = mixer
            .scene()
            .iter()
            .filter(|object| object.lane == 0)
            .count();
        assert_eq!(objects, 5, "the upmixer's outputs reach the mixer");
        let (flips, surround) = part(&mut lanes, &mut mixer, &SURROUND, true, &mut restarts);
        assert_eq!(flips, 1);
        assert!(!surround.upmixing);
        assert_eq!(surround.bypass, Some(UpmixBypass::NativeSurround));
        let (flips, stereo) = part(
            &mut lanes,
            &mut mixer,
            &MatrixUpmixer::STEREO,
            false,
            &mut restarts,
        );
        assert_eq!(flips, 1);
        assert!(stereo.upmixing);
        assert_eq!(restarts, 2, "the caller settles its transport before each");
    }

    #[test]
    fn format_changes_restart_the_stage_for_each_shape() {
        let geometry = geometry();
        let (mut lanes, mut mixer) = lanes_with(Some(stage(MatrixUpmixer::stereo_to_5_1(RATE, 0))));
        let mut restarts = 0;
        let mut upmixed = Vec::new();
        let mut generations = Vec::new();
        // Stereo PCM, 5.1 PCM, a 2.0 bitstream, stereo PCM: each after a
        // format change, which restarts the lane.
        for (part, speakers) in [
            &MatrixUpmixer::STEREO[..],
            &SURROUND,
            &MatrixUpmixer::STEREO,
            &MatrixUpmixer::STEREO,
        ]
        .into_iter()
        .enumerate()
        {
            if part != 0 {
                mixer.lane_discontinuity(0, 0);
            }
            for index in 0..6 {
                place(
                    Some(&mut lanes),
                    &mut mixer,
                    &geometry,
                    speakers,
                    part * 6 + index,
                    false,
                    false,
                    &mut restarts,
                );
            }
            upmixed.push(lanes.status(0).unwrap().upmixing);
            generations.push(lanes.lanes[0].stage.as_ref().unwrap().generation);
        }
        assert_eq!(upmixed, [true, false, true, true]);
        assert_eq!(generations, [1, 1, 2, 3]);
        assert_eq!(restarts, 0, "format changes are the chain's restarts");
    }

    #[test]
    fn setting_changes_restart_a_playing_lane_only_across_upmixing() {
        let geometry = geometry();
        let (mut lanes, mut mixer) = lanes_with(Some(stage(MatrixUpmixer::stereo_to_5_1(RATE, 0))));
        let mut restarts = 0;
        let mut index = 0;
        let mut play = |lanes: &mut UpmixLanes, mixer: &mut SceneMixer, speakers: &[Speaker]| {
            for _ in 0..3 {
                place(
                    Some(lanes),
                    mixer,
                    &geometry,
                    speakers,
                    index,
                    false,
                    false,
                    &mut restarts,
                );
                index += 1;
            }
        };
        play(&mut lanes, &mut mixer, &MatrixUpmixer::STEREO);
        let count = |mixer: &SceneMixer| mixer.lane_restarts(0);
        let before = count(&mixer);
        lanes.set_apply(&mut mixer, false);
        assert_eq!(count(&mixer), before + 1);
        assert_eq!(lanes.status(0).unwrap().bypass, Some(UpmixBypass::KindOff));
        play(&mut lanes, &mut mixer, &MatrixUpmixer::STEREO);
        assert!(!lanes.status(0).unwrap().upmixing);
        lanes.set_apply(&mut mixer, true);
        assert_eq!(count(&mixer), before + 2);
        play(&mut lanes, &mut mixer, &MatrixUpmixer::STEREO);
        assert!(lanes.status(0).unwrap().upmixing);
        // An upmixer for another rate: transparent, after one restart.
        let other = stage(MatrixUpmixer::stereo_to_5_1(44_100, 0));
        assert!(
            lanes
                .install(&mut mixer, 0, Some(Box::new(other)))
                .is_some()
        );
        assert_eq!(count(&mixer), before + 3);
        play(&mut lanes, &mut mixer, &MatrixUpmixer::STEREO);
        assert_eq!(
            lanes.status(0).unwrap().bypass,
            Some(UpmixBypass::RateChange)
        );
        // Transparent before and after: the switch only changes the reason.
        lanes.set_apply(&mut mixer, false);
        assert_eq!(count(&mixer), before + 3);
        assert_eq!(lanes.status(0).unwrap().bypass, Some(UpmixBypass::KindOff));
        lanes.set_apply(&mut mixer, true);
        // Removing a transparent stage, then installing one that upmixes.
        assert!(lanes.install(&mut mixer, 0, None).is_some());
        assert_eq!(count(&mixer), before + 3);
        assert_eq!(lanes.status(0).unwrap(), UpmixStatus::not_installed());
        let upmixer = stage(MatrixUpmixer::stereo_to_5_1(RATE, 0));
        assert!(
            lanes
                .install(&mut mixer, 0, Some(Box::new(upmixer)))
                .is_none()
        );
        assert_eq!(count(&mixer), before + 4);
        play(&mut lanes, &mut mixer, &MatrixUpmixer::STEREO);
        assert!(lanes.status(0).unwrap().upmixing);
        // Removing an upmixing stage restarts the lane once.
        assert!(lanes.install(&mut mixer, 0, None).is_some());
        assert_eq!(count(&mixer), before + 5);
        // A stage built for another endpoint, or endpoint 3: handed back.
        let music = UpmixStage::new(1, Box::new(MatrixUpmixer::stereo_to_5_1(RATE, 0)), QPC);
        let music = Box::new(music.unwrap());
        assert!(lanes.install(&mut mixer, 0, Some(music)).is_some());
        assert_eq!(lanes.status(0).unwrap(), UpmixStatus::not_installed());
        assert!(lanes.status(3).is_none());
    }

    #[test]
    fn a_stopped_lane_reports_nothing_playing() {
        let geometry = geometry();
        let (mut lanes, mut mixer) = lanes_with(Some(stage(MatrixUpmixer::stereo_to_5_1(RATE, 0))));
        let mut restarts = 0;
        place(
            Some(&mut lanes),
            &mut mixer,
            &geometry,
            &MatrixUpmixer::STEREO,
            0,
            false,
            false,
            &mut restarts,
        );
        assert!(lanes.status(0).unwrap().upmixing);
        mixer.lane_stop(0, &mut |_| {});
        lanes.stopped(0);
        let status = lanes.status(0).unwrap();
        assert!(status.installed && !status.upmixing && status.bypass.is_none());
    }
}
