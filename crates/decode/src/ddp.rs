//! DD+ (E-AC-3) adapter, with JOC objects when the stream carries them.
//! Wraps harletty-bridge's `eac3` crate (Apache-2.0, ADR 0007).
//!
//! `eac3` takes one syncframe, not an IEC access unit. We decode independent
//! substream 0 and overlay its dependent substreams before reconstructing JOC.
//! Short syncframes are concatenated to the access unit's 1536 samples.
//!
//! The upstream decoder allocates per frame, so the adapter is split in two
//! halves (ADR 0031). [`DdpFront`] runs the upstream decoders on the decode
//! thread and hands their frames over in a [`DdpDecoded`]; [`DdpBack`] checks
//! their shape and writes scenes on the capture thread without allocating,
//! including on error paths (owned error details are empty). [`DdpDecoder`]
//! runs both halves in one place for offline callers and tests; its output is
//! the same scenes, cuts and errors as the halves run apart.
//!
//! Nominal bed positions come from `ssr_types::Speaker`, shared with PCM;
//! measured BRIR directions remain separate. OAMD room coordinates are axis
//! converted, then mapped to directions using ITU-R BS.2127-1 §10.1.
//!
//! With JOC, reconstructed bed and dynamic channels replace the fullband
//! downmix; emitting both would count the same audio twice. Only the aligned
//! core LFE is kept (`eac3::JOC_LATENCY_SAMPLES`, 577, applied upstream).
//! Each OAMD update starts a scene; room-coordinate ramps are sampled in
//! pieces no longer than 256 samples, with state carried across access units.
//! All audio/LFE samples are preserved. Unknown absolute positions stay at
//! the directionless centre with spread 1, including mid-stream delta updates.
//!
//! Screen-anchored objects use the room axes because SceneBlock has no screen
//! dimensions; screen warping and distance are not rendered because this is a
//! direction-only renderer. ISF/alternate-object mappings and a five-channel
//! JOC payload on an expanded bed still require information upstream omits.

use bitstream::{Burst, DataType};
use eac3::{BedChannel, CorePcmFrame, ObjectPcmFrame, ParsedEmdfPayloadData, StreamType, Vec3};
use ssr_types::{MAX_OBJECTS, Position, SceneBlock, Speaker, room_to_direction};

use crate::{DecodeConfig, DecodeError, SceneBuffers};

mod timeline;

use timeline::Timeline;

const FRAMES: usize = 1536;

#[derive(Clone, Copy, Debug)]
struct ObjectMeta {
    position: Position,
    room: Vec3,
    is_room: bool,
    gain: f32,
    spread: f32,
    known: bool,
}

const EMPTY_META: ObjectMeta = ObjectMeta {
    position: Position::new(0.0, 0.0, 0.0),
    room: Vec3 {
        x: 0.5,
        y: 0.5,
        z: 0.0,
    },
    is_room: true,
    gain: 1.0,
    spread: 0.0,
    known: false,
};

/// Decode-thread half: the upstream decoders. Allocates; never runs on a
/// real-time thread.
#[derive(Debug)]
pub struct DdpFront {
    core: eac3::PcmDecoder,
    dependent: [eac3::PcmDecoder; 8],
    objects: eac3::ObjectPcmDecoder,
}

impl Default for DdpFront {
    fn default() -> Self {
        Self::new()
    }
}

impl DdpFront {
    pub fn new() -> Self {
        Self {
            core: eac3::PcmDecoder::new(),
            dependent: std::array::from_fn(|_| eac3::PcmDecoder::new()),
            objects: eac3::ObjectPcmDecoder::new(),
        }
    }

    /// Forgets the cross-frame state of every upstream decoder.
    pub fn reset(&mut self) {
        self.core.reset();
        for decoder in &mut self.dependent {
            decoder.reset();
        }
        self.objects.reset();
    }

    /// Decodes one burst payload (one 1536-sample access unit) into `into`,
    /// one entry per independent syncframe with its dependents merged and
    /// JOC objects reconstructed. The payload must contain exactly 1536
    /// independent samples, each followed by its dependents; alternate
    /// independent presentations are refused. On error the upstream state is
    /// reset, `into` is left empty and nothing is handed over.
    pub fn decode(&mut self, payload: &[u8], into: &mut DdpDecoded) -> Result<(), DecodeError> {
        into.clear();
        let result = self.decode_groups(payload, into);
        if result.is_err() {
            self.reset();
            into.clear();
        }
        result
    }

    fn decode_groups(&mut self, payload: &[u8], into: &mut DdpDecoded) -> Result<(), DecodeError> {
        validate_access_unit(payload)?;
        let mut bytes = payload;
        while !bytes.is_empty() {
            let (frame, header, rest) = syncframe(bytes)?;
            bytes = rest;
            let decoded = self.core.push_access_unit(frame).map_err(|_| corrupt())?;
            let mut joc_frame = has_joc(&decoded.info).then_some(frame);
            let mut joc_channels = joc_channel_count(&decoded.info);
            let mut core = decoded.pcm;
            while !bytes.is_empty() {
                let (dep_frame, dep_header, rest) = syncframe(bytes)?;
                if dep_header.stream_type != StreamType::Dependent {
                    break;
                }
                // ETSI TS 103 420 §8.2 puts JOC in the last substream.
                if joc_frame.is_some() {
                    return Err(corrupt());
                }
                let dep = self.dependent[usize::from(dep_header.substream_id)]
                    .push_access_unit(dep_frame)
                    .map_err(|_| corrupt())?;
                if has_joc(&dep.info) {
                    joc_channels = joc_channel_count(&dep.info);
                    joc_frame = Some(dep_frame);
                }
                // Allocations in this merge belong to the upstream eac3 API.
                core = eac3::merge_core_with_decoded_dependent(&core, &dep.pcm, &dep.info)
                    .ok_or_else(corrupt)?;
                bytes = rest;
            }
            let kind = if let Some(frame) = joc_frame {
                validate_joc_bed(&core, joc_channels.ok_or_else(corrupt)?)?;
                let objects = self
                    .objects
                    .push_access_unit_with_core(frame, core)
                    .map_err(|_| corrupt())?
                    .ok_or_else(corrupt)?;
                DecodedKind::Objects(objects.pcm)
            } else {
                self.objects.note_non_joc_presentation();
                DecodedKind::Bed(core)
            };
            into.groups.push(DecodedGroup {
                samples: usize::from(header.samples),
                rate: header.sample_rate,
                kind,
            });
        }
        Ok(())
    }
}

/// What crosses from the decode thread to the capture thread: the
/// upstream-owned frames of one access unit, one entry per independent
/// syncframe group. Created, filled and dropped on the decode thread only; the
/// capture thread borrows it.
#[derive(Debug, Default)]
pub struct DdpDecoded {
    groups: Vec<DecodedGroup>,
}

impl DdpDecoded {
    pub fn new() -> Self {
        Self { groups: Vec::new() }
    }

    /// Drops the frames held (this frees them; call it on the decode thread).
    pub fn clear(&mut self) {
        self.groups.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }

    /// Source frames held: the sum of the groups' samples (1536 for a decoded
    /// access unit, 0 when empty).
    pub fn frames(&self) -> usize {
        self.groups.iter().map(|group| group.samples).sum()
    }
}

#[derive(Debug)]
struct DecodedGroup {
    samples: usize,
    rate: u32,
    kind: DecodedKind,
}

#[derive(Debug)]
enum DecodedKind {
    /// No JOC: the merged core channels, laid out as a bed.
    Bed(CorePcmFrame),
    /// JOC: reconstructed objects plus the aligned core LFE.
    Objects(ObjectPcmFrame),
}

/// Capture-thread half: shape checks, channel order, scene buffers, the OAMD
/// timeline, the epoch and the JOC sequence. Never allocates in `place`.
#[derive(Debug)]
pub struct DdpBack {
    buffers: SceneBuffers,
    timeline: Timeline,
    epoch: Option<u32>,
    joc_sequence: Option<u16>,
}

impl DdpBack {
    pub fn new(config: DecodeConfig) -> Self {
        Self {
            buffers: SceneBuffers::new(config.max_frames),
            timeline: Timeline::new(),
            epoch: None,
            joc_sequence: None,
        }
    }

    /// Forgets the timeline, epoch and JOC sequence. Does not allocate.
    pub fn reset(&mut self) {
        self.timeline.reset();
        self.epoch = None;
        self.joc_sequence = None;
        self.buffers.set_layout(0, false);
    }

    /// Writes one decoded access unit as scenes. Plain beds emit one scene;
    /// JOC emits scenes at OAMD update/ramp boundaries whose frame counts sum
    /// to 1536. Bed channels use nominal positions and JOC uses converted room
    /// positions. An epoch other than the last one resets first. Errors reset
    /// this half and emit no scene; the caller resets the decode-thread half.
    pub fn place(
        &mut self,
        decoded: &DdpDecoded,
        qpc: u64,
        epoch: u32,
        out: &mut dyn FnMut(&SceneBlock<'_>),
    ) -> Result<(), DecodeError> {
        if self.epoch != Some(epoch) {
            self.reset();
            self.epoch = Some(epoch);
        }
        let result = self.write(decoded, qpc, epoch, out);
        if result.is_err() {
            self.reset();
        }
        result
    }

    fn check_capacity(&self) -> Result<(), DecodeError> {
        if self.buffers.max_frames() < FRAMES {
            return Err(unsupported(
                "max_frames is below one 1536-frame access unit",
            ));
        }
        Ok(())
    }

    fn write(
        &mut self,
        decoded: &DdpDecoded,
        qpc: u64,
        epoch: u32,
        out: &mut dyn FnMut(&SceneBlock<'_>),
    ) -> Result<(), DecodeError> {
        self.check_capacity()?;
        if decoded.is_empty() {
            return Err(DecodeError::WrongRoute(
                "no decoded E-AC-3 access unit to place",
            ));
        }
        let mut offset = 0;
        let mut shape = None;
        let mut order = [None; MAX_OBJECTS];
        let mut rate = 0;
        for group in &decoded.groups {
            let samples = group.samples;
            rate = group.rate;
            match &group.kind {
                DecodedKind::Bed(core) => {
                    self.joc_sequence = None;
                    self.timeline.reset();
                    let current = (
                        false,
                        core.fullband_channels.len(),
                        core.lfe_channel.is_some(),
                    );
                    check_shape(&mut shape, current)?;
                    if offset == 0 {
                        if core.fullband_channel_order.len() > MAX_OBJECTS {
                            return Err(corrupt());
                        }
                        for (slot, &channel) in order.iter_mut().zip(&core.fullband_channel_order) {
                            *slot = Some(channel);
                        }
                    } else if core
                        .fullband_channel_order
                        .iter()
                        .enumerate()
                        .any(|(i, channel)| order[i] != Some(*channel))
                    {
                        return Err(corrupt());
                    }
                    write_core(&mut self.buffers, core, offset, samples)?;
                }
                DecodedKind::Objects(pcm) => {
                    let sequence = pcm.joc.sequence_counter;
                    let continuous = self.joc_sequence.is_some_and(|previous| {
                        sequence != 0 && sequence == if previous == 1023 { 1 } else { previous + 1 }
                    });
                    if !continuous {
                        self.timeline.discontinuity(offset)?;
                    }
                    self.joc_sequence = Some(sequence);
                    check_shape(
                        &mut shape,
                        (
                            true,
                            pcm.object_channels.len(),
                            pcm.core.lfe_channel.is_some(),
                        ),
                    )?;
                    self.timeline.schedule(pcm, offset)?;
                    write_objects(&mut self.buffers, pcm, offset, samples)?;
                }
            }
            offset += samples;
        }
        if shape.is_some_and(|s| s.0) {
            self.timeline.emit(&mut self.buffers, epoch, rate, qpc, out);
        } else {
            self.buffers.emit(FRAMES, epoch, rate, qpc, out);
        }
        Ok(())
    }
}

/// Both halves in one place, for this crate's tests and offline callers.
#[derive(Debug)]
pub struct DdpDecoder {
    front: DdpFront,
    back: DdpBack,
    decoded: DdpDecoded,
}

impl DdpDecoder {
    pub fn new(config: DecodeConfig) -> Self {
        Self {
            front: DdpFront::new(),
            back: DdpBack::new(config),
            decoded: DdpDecoded::new(),
        }
    }

    /// Decodes one 1536-frame access unit. Plain beds emit one scene; JOC emits
    /// scenes at OAMD update/ramp boundaries whose frame counts sum to 1536.
    /// Bed channels use nominal positions and JOC uses converted room positions.
    /// The payload must contain exactly 1536 independent samples, each followed
    /// by its dependents. Alternate independent presentations are refused.
    /// Errors discard cross-frame state and emit no scene. Epoch changes reset.
    pub fn push(
        &mut self,
        burst: &Burst<'_>,
        qpc: u64,
        epoch: u32,
        out: &mut dyn FnMut(&SceneBlock<'_>),
    ) -> Result<(), DecodeError> {
        if self.back.epoch != Some(epoch) {
            self.reset();
        }
        let result = self.decode(burst, qpc, epoch, out);
        if result.is_err() {
            self.reset();
        }
        result
    }

    fn decode(
        &mut self,
        burst: &Burst<'_>,
        qpc: u64,
        epoch: u32,
        out: &mut dyn FnMut(&SceneBlock<'_>),
    ) -> Result<(), DecodeError> {
        if burst.data_type != DataType::Eac3 {
            return Err(DecodeError::WrongRoute("a non-E-AC-3 burst in a DD+ epoch"));
        }
        // Refused before any upstream call, as the one-piece adapter did.
        self.back.check_capacity()?;
        self.front.decode(burst.payload, &mut self.decoded)?;
        self.back.place(&self.decoded, qpc, epoch, out)
    }

    pub fn reset(&mut self) {
        self.front.reset();
        self.back.reset();
    }
}

fn corrupt() -> DecodeError {
    DecodeError::Corrupt {
        codec: "E-AC-3",
        detail: "a syncframe, substream or object payload failed to parse or decode",
    }
}

fn unsupported(detail: &'static str) -> DecodeError {
    DecodeError::Unsupported {
        codec: "E-AC-3",
        detail,
    }
}

fn syncframe(bytes: &[u8]) -> Result<(&[u8], eac3::FrameInfo, &[u8]), DecodeError> {
    let header = eac3::parse_header(bytes).map_err(|_| corrupt())?;
    if header.frame_size < 7 || header.frame_size > bytes.len() {
        return Err(corrupt());
    }
    let (frame, rest) = bytes.split_at(header.frame_size);
    Ok((frame, header, rest))
}

fn validate_access_unit(mut bytes: &[u8]) -> Result<(), DecodeError> {
    let mut samples = 0;
    let mut primary = None;
    let mut dependents = 0_u16;
    let mut can_have_dependents = false;
    while !bytes.is_empty() {
        let (_, header, rest) = syncframe(bytes)?;
        bytes = rest;
        match header.stream_type {
            StreamType::Independent | StreamType::Ac3Converted if header.substream_id == 0 => {
                if primary.is_some_and(|(rate, _)| rate != header.sample_rate) {
                    return Err(corrupt());
                }
                primary = Some((header.sample_rate, header.samples));
                can_have_dependents = header.stream_type == StreamType::Independent;
                samples += usize::from(header.samples);
                dependents = 0;
            }
            StreamType::Dependent => {
                if !can_have_dependents
                    || primary != Some((header.sample_rate, header.samples))
                    || dependents & (1 << header.substream_id) != 0
                {
                    return Err(corrupt());
                }
                dependents |= 1 << header.substream_id;
            }
            _ => return Err(corrupt()),
        }
        if samples > FRAMES {
            return Err(corrupt());
        }
    }
    if samples != FRAMES {
        return Err(corrupt());
    }
    Ok(())
}

fn has_joc(info: &eac3::AccessUnitInfo) -> bool {
    info.payloads()
        .any(|p| matches!(p.parsed, ParsedEmdfPayloadData::Joc(_)))
}

fn joc_channel_count(info: &eac3::AccessUnitInfo) -> Option<usize> {
    info.payloads().find_map(|p| match &p.parsed {
        ParsedEmdfPayloadData::Joc(joc) => Some(joc.channel_count),
        _ => None,
    })
}

fn validate_joc_bed(core: &CorePcmFrame, channels: usize) -> Result<(), DecodeError> {
    if channels == 5 {
        // TS 103 420 §8.1 requires the merged presentation; table 47 requires
        // five inputs. A dependent may simply replace those same five channels.
        // An expanded bed is different: eac3 does not expose the fold-down
        // coefficients/phase data needed to restore the specified 5.X input.
        let required = [
            BedChannel::FrontLeft,
            BedChannel::FrontRight,
            BedChannel::Center,
            BedChannel::SurroundLeft,
            BedChannel::SurroundRight,
        ];
        if core.fullband_channel_order.len() != required.len()
            || required
                .iter()
                .any(|c| !core.fullband_channel_order.contains(c))
        {
            return Err(unsupported(
                "five-channel JOC with an expanded bed needs unavailable fold-down metadata",
            ));
        }
    }
    Ok(())
}

fn check_shape(
    shape: &mut Option<(bool, usize, bool)>,
    current: (bool, usize, bool),
) -> Result<(), DecodeError> {
    if current.1 > MAX_OBJECTS || shape.is_some_and(|old| old != current) {
        return Err(corrupt());
    }
    *shape = Some(current);
    Ok(())
}

fn write_core(
    buffers: &mut SceneBuffers,
    core: &CorePcmFrame,
    offset: usize,
    samples: usize,
) -> Result<(), DecodeError> {
    if core.fullband_channels.len() != core.fullband_channel_order.len()
        || core.fullband_channels.len() > MAX_OBJECTS
    {
        return Err(corrupt());
    }
    buffers.set_layout(core.fullband_channels.len(), core.lfe_channel.is_some());
    for (i, (channel, &bed)) in core
        .fullband_channels
        .iter()
        .zip(&core.fullband_channel_order)
        .enumerate()
    {
        if channel.len() != samples {
            return Err(corrupt());
        }
        let position = bed_position(bed).ok_or_else(corrupt)?;
        buffers.set_object(i, position, 1.0, 0.0);
        buffers.signal_mut(i, FRAMES)[offset..offset + samples].copy_from_slice(channel);
    }
    write_lfe(buffers, core, offset, samples)
}

fn write_lfe(
    buffers: &mut SceneBuffers,
    core: &CorePcmFrame,
    offset: usize,
    samples: usize,
) -> Result<(), DecodeError> {
    if let Some(lfe) = &core.lfe_channel {
        if lfe.len() != samples {
            return Err(corrupt());
        }
        buffers.lfe_mut(FRAMES)[offset..offset + samples].copy_from_slice(lfe);
    }
    Ok(())
}

fn write_objects(
    buffers: &mut SceneBuffers,
    pcm: &ObjectPcmFrame,
    offset: usize,
    samples: usize,
) -> Result<(), DecodeError> {
    buffers.set_layout(pcm.object_channels.len(), pcm.core.lfe_channel.is_some());
    for (i, channel) in pcm.object_channels.iter().enumerate() {
        if channel.len() != samples {
            return Err(corrupt());
        }
        // Preserve QMF tails even when JocObject.active becomes false.
        buffers.signal_mut(i, FRAMES)[offset..offset + samples].copy_from_slice(channel);
    }
    write_lfe(buffers, &pcm.core, offset, samples)
}

/// OAMD room coordinates are x=0 left / 1 right, y=0 front / 1 back,
/// z=-1 below / 0 ear / 1 ceiling (ETSI TS 103 420, upstream metadata.rs).
/// Thus x'=2x-1, y'=1-2y, z'=z. Screen anchoring uses the same axes here;
/// no screen dimensions exist in SceneBlock. Reject nonfinite values, clamp
/// quantization overshoot to the room. Speaker anchors use bed_position.
fn oamd_position(p: Vec3) -> Option<Position> {
    if !p.x.is_finite() || !p.y.is_finite() || !p.z.is_finite() {
        return None;
    }
    Some(room_to_direction(
        2.0 * p.x.clamp(0.0, 1.0) - 1.0,
        1.0 - 2.0 * p.y.clamp(0.0, 1.0),
        p.z.clamp(-1.0, 1.0),
    ))
}

fn bed_position(bed: BedChannel) -> Option<Position> {
    use BedChannel::*;
    let speaker = match bed {
        FrontLeft => Speaker::L,
        FrontRight => Speaker::R,
        Center => Speaker::C,
        SurroundLeft => Speaker::Ls,
        SurroundRight => Speaker::Rs,
        RearCenter => Speaker::Cs,
        RearLeft => Speaker::Lrs,
        RearRight => Speaker::Rrs,
        WideLeft => Speaker::Lw,
        WideRight => Speaker::Rw,
        TopFrontLeft => Speaker::Tfl,
        TopFrontRight => Speaker::Tfr,
        TopSurroundLeft => Speaker::Tsl,
        TopSurroundRight => Speaker::Tsr,
        TopRearLeft => Speaker::Trl,
        TopRearRight => Speaker::Trr,
        LowFrequencyEffects => Speaker::Lfe,
        LowFrequencyEffects2 => Speaker::Lfe2,
    };
    speaker.position()
}

#[cfg(test)]
mod tests {
    use super::*;
    use eac3::OamdElementKind;

    #[cfg(debug_assertions)]
    #[global_allocator]
    static ALLOCATOR: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

    #[test]
    fn access_unit_groups_short_frames_and_matching_dependents() {
        // Header-only eight-byte frames: stereo, 48 kHz, bsid 16.
        // This tests framing, not successful audio decoding.
        let independent = [0x0b, 0x77, 0, 3, 0x04, 0x80, 0, 0];
        let dependent = [0x0b, 0x77, 0x40, 3, 0x04, 0x80, 0, 0];
        let mut access_unit = Vec::new();
        for _ in 0..6 {
            access_unit.extend_from_slice(&independent);
            access_unit.extend_from_slice(&dependent);
        }
        validate_access_unit(&access_unit).unwrap();
        assert!(validate_access_unit(&access_unit[..access_unit.len() - 16]).is_err());
        let mut duplicate = access_unit.clone();
        duplicate.extend_from_slice(&dependent);
        assert!(validate_access_unit(&duplicate).is_err());
        let mut mismatch = access_unit.clone();
        mismatch[12] |= 0x10; // Dependent length: 512 vs independent 256.
        assert!(validate_access_unit(&mismatch).is_err());
        let mut converted = access_unit;
        converted[2] |= 0x80; // Converted AC-3 cannot carry dependents.
        assert!(validate_access_unit(&converted).is_err());
    }

    #[test]
    fn oamd_axes_convert_to_scene_coordinates() {
        for (input, expected) in [
            ([0.0, 0.0, 0.0], [-1.0, 1.0, 0.0]),
            ([1.0, 1.0, 1.0], [1.0, -1.0, 1.0]),
            ([0.5, 0.5, -1.0], [0.0, 0.0, -1.0]),
            ([0.25, 0.25, 0.5], [-0.5, 0.5, 0.5]),
            ([-0.1, 1.1, 1.1], [-1.0, -1.0, 1.0]),
        ] {
            let [x, y, z] = input;
            assert_eq!(
                oamd_position(Vec3 { x, y, z }),
                Some(room_to_direction(expected[0], expected[1], expected[2]))
            );
        }
        assert_eq!(
            oamd_position(Vec3 {
                x: f32::NAN,
                y: 0.0,
                z: 0.0
            }),
            None
        );
    }

    fn synthetic_objects() -> ObjectPcmFrame {
        ObjectPcmFrame {
            core: CorePcmFrame {
                sample_rate: 48_000,
                fullband_channel_order: vec![BedChannel::FrontLeft],
                fullband_channels: vec![vec![0.9; FRAMES]],
                lfe_channel: Some(vec![0.1; FRAMES]),
            },
            object_channels: vec![vec![0.25; FRAMES]],
            object_active: vec![true],
            joc: eac3::JocPayload {
                downmix_config: 0,
                channel_count: 5,
                object_count: 1,
                gain: 1.0,
                sequence_counter: 1,
                objects: vec![],
            },
            oamd_payloads: vec![(
                eac3::OamdPayload {
                    version: 0,
                    object_count: 2,
                    alternate_object_present: false,
                    element_count: 1,
                    beds: 1,
                    bed_instances: 1,
                    bed_or_isf_objects: 1,
                    dynamic_objects: 1,
                    isf_in_use: false,
                    isf_index: None,
                    bed_assignment: vec![vec![BedChannel::LowFrequencyEffects]],
                    elements: vec![eac3::OamdElement {
                        element_index: 0,
                        byte_length: 0,
                        kind: OamdElementKind::Object(eac3::OamdObjectElement {
                            sample_offset: 0,
                            block_updates: vec![eac3::OamdBlockUpdate {
                                offset: 0,
                                ramp_duration: 0,
                            }],
                            object_blocks: vec![
                                vec![eac3::OamdObjectBlock::default()],
                                vec![eac3::OamdObjectBlock {
                                    valid_position: true,
                                    position: Some(Vec3 {
                                        x: 0.25,
                                        y: 0.0,
                                        z: 0.5,
                                    }),
                                    gain: Some(0.75),
                                    size: Some([0.2, 0.3, 0.1]),
                                    ..eac3::OamdObjectBlock::default()
                                }],
                            ],
                        }),
                    }],
                },
                None,
            )],
        }
    }

    #[test]
    fn object_mapping_skips_oamd_beds_and_does_not_duplicate_downmix() {
        let pcm = synthetic_objects();
        let mut timeline = Timeline::new();
        timeline.schedule(&pcm, 0).unwrap();
        let mut buffers = SceneBuffers::new(FRAMES);
        write_objects(&mut buffers, &pcm, 0, FRAMES).unwrap();
        timeline.emit(&mut buffers, 1, 48_000, 2, &mut |scene| {
            assert_eq!(scene.objects.len(), 1);
            let object = scene.objects[0];
            assert_eq!(object.position, room_to_direction(-0.5, 1.0, 0.5));
            assert_eq!((object.gain, object.spread), (0.75, 0.3));
            assert_eq!(object.signal, &[0.25; FRAMES]);
            assert_eq!(scene.lfe.unwrap(), &[0.1; FRAMES]);
        });
    }

    #[test]
    fn pure_bed_joc_uses_assignments_without_lfe_object() {
        let mut pcm = synthetic_objects();
        let payload = &mut pcm.oamd_payloads[0].0;
        payload.dynamic_objects = 0;
        payload.bed_assignment = vec![vec![
            BedChannel::LowFrequencyEffects,
            BedChannel::TopFrontLeft,
        ]];
        payload.beds = 2;
        payload.bed_or_isf_objects = 2;
        let mut timeline = Timeline::new();
        timeline.schedule(&pcm, 0).unwrap();
        let mut buffers = SceneBuffers::new(FRAMES);
        write_objects(&mut buffers, &pcm, 0, FRAMES).unwrap();
        timeline.emit(&mut buffers, 1, 48_000, 2, &mut |scene| {
            assert_eq!(scene.objects.len(), 1);
            assert_eq!(scene.objects[0].position, Position::from_angles(45.0, 45.0));
        });
    }

    #[test]
    fn unknown_object_position_is_not_silently_rendered_at_front() {
        let mut pcm = synthetic_objects();
        pcm.oamd_payloads.clear();
        let mut timeline = Timeline::new();
        timeline.schedule(&pcm, 0).unwrap();
        let mut buffers = SceneBuffers::new(FRAMES);
        write_objects(&mut buffers, &pcm, 0, FRAMES).unwrap();
        timeline.emit(&mut buffers, 1, 48_000, 2, &mut |scene| {
            let object = scene.objects[0];
            assert_eq!(object.position, Position::default());
            assert_eq!(object.position.direction(), None);
            assert_eq!(object.spread, 1.0);
            assert_eq!(object.signal, &[0.25; FRAMES]);
        });
    }

    fn element(pcm: &mut ObjectPcmFrame) -> &mut eac3::OamdObjectElement {
        let OamdElementKind::Object(element) = &mut pcm.oamd_payloads[0].0.elements[0].kind else {
            panic!("object element")
        };
        element
    }

    fn add_block(
        pcm: &mut ObjectPcmFrame,
        offset: i16,
        ramp: i16,
        position: Vec3,
        differential: bool,
    ) {
        let element = element(pcm);
        element.block_updates.push(eac3::OamdBlockUpdate {
            offset,
            ramp_duration: ramp,
        });
        element.object_blocks[0].push(eac3::OamdObjectBlock::default());
        element.object_blocks[1].push(eac3::OamdObjectBlock {
            valid_position: true,
            differential_position: differential,
            position: Some(position),
            ..eac3::OamdObjectBlock::default()
        });
    }

    #[test]
    fn two_oamd_blocks_emit_two_scenes_and_preserve_every_sample() {
        let mut pcm = synthetic_objects();
        let target = Vec3 {
            x: 1.0,
            y: 1.0,
            z: 0.0,
        };
        add_block(&mut pcm, 24, 0, target, false); // factor 24 -> sample 768.
        for (i, s) in pcm.object_channels[0].iter_mut().enumerate() {
            *s = i as f32;
        }
        for (i, s) in pcm
            .core
            .lfe_channel
            .as_mut()
            .unwrap()
            .iter_mut()
            .enumerate()
        {
            *s = -(i as f32);
        }
        let mut timeline = Timeline::new();
        let mut buffers = SceneBuffers::new(FRAMES);
        let mut frames = 0;
        let mut scenes = 0;
        assert_no_alloc::assert_no_alloc(|| {
            timeline.schedule(&pcm, 0).unwrap();
            write_objects(&mut buffers, &pcm, 0, FRAMES).unwrap();
            timeline.emit(&mut buffers, 9, 48_000, 123, &mut |scene| {
                assert_eq!(scene.frames, 768);
                assert_eq!(
                    (scene.epoch, scene.rate, scene.source_qpc),
                    (9, 48_000, 123)
                );
                assert_eq!(
                    scene.objects[0].position,
                    if scenes == 0 {
                        room_to_direction(-0.5, 1.0, 0.5)
                    } else {
                        oamd_position(target).unwrap()
                    }
                );
                assert_eq!(
                    scene.objects[0].signal,
                    &pcm.object_channels[0][frames..frames + scene.frames]
                );
                assert_eq!(
                    scene.lfe.unwrap(),
                    &pcm.core.lfe_channel.as_ref().unwrap()[frames..frames + scene.frames]
                );
                frames += scene.frames;
                scenes += 1;
            });
        });
        assert_eq!((scenes, frames), (2, FRAMES));
    }

    #[test]
    fn long_oamd_ramp_is_interpolated_in_room_coordinates() {
        let mut pcm = synthetic_objects();
        let start = Vec3 {
            x: 0.0,
            y: 0.0,
            z: 0.0,
        };
        element(&mut pcm).object_blocks[1][0].position = Some(start);
        add_block(
            &mut pcm,
            8,
            1024,
            Vec3 {
                x: 1.0,
                y: 0.0,
                z: 1.0,
            },
            false,
        );
        let mut timeline = Timeline::new();
        let mut buffers = SceneBuffers::new(FRAMES);
        timeline.schedule(&pcm, 0).unwrap();
        write_objects(&mut buffers, &pcm, 0, FRAMES).unwrap();
        let mut frames = 0;
        let mut scenes = 0;
        timeline.emit(&mut buffers, 1, 48_000, 0, &mut |scene| {
            assert_eq!(scene.frames, 256);
            let t = (frames as f32 - 256.0).clamp(0.0, 1024.0) / 1024.0;
            assert_eq!(
                scene.objects[0].position,
                oamd_position(Vec3 { x: t, y: 0.0, z: t }).unwrap()
            );
            frames += scene.frames;
            scenes += 1;
        });
        assert_eq!((frames, scenes), (FRAMES, 6));
    }

    #[test]
    fn oamd_offsets_and_ramps_continue_across_access_units() {
        let mut pcm = synthetic_objects();
        element(&mut pcm).object_blocks[1][0].position = Some(Vec3 {
            x: 0.0,
            y: 0.0,
            z: 0.0,
        });
        add_block(
            &mut pcm,
            40,
            1024,
            Vec3 {
                x: 1.0,
                y: 0.0,
                z: 0.0,
            },
            false,
        );
        let mut timeline = Timeline::new();
        let mut buffers = SceneBuffers::new(FRAMES);
        timeline.schedule(&pcm, 0).unwrap();
        write_objects(&mut buffers, &pcm, 0, FRAMES).unwrap();
        timeline.emit(&mut buffers, 1, 48_000, 0, &mut |_| {});
        let mut frames = 0;
        timeline.emit(&mut buffers, 1, 48_000, 1, &mut |scene| {
            let t = ((256 + frames) as f32 / 1024.0).min(1.0);
            assert_eq!(
                scene.objects[0].position,
                oamd_position(Vec3 {
                    x: t,
                    y: 0.0,
                    z: 0.0
                })
                .unwrap()
            );
            frames += scene.frames;
        });
        assert_eq!(frames, FRAMES);

        timeline.reset();
        element(&mut pcm).block_updates.truncate(1);
        for blocks in &mut element(&mut pcm).object_blocks {
            blocks.truncate(1);
        }
        pcm.oamd_payloads[0].1 = Some(1536);
        let element = element(&mut pcm);
        element.sample_offset = 7;
        element.block_updates[0].offset = 9; // 7 + factor 2, NOT sample 9.
        timeline.schedule(&pcm, 0).unwrap();
        timeline.emit(&mut buffers, 1, 48_000, 0, &mut |s| {
            assert_eq!(s.objects[0].spread, 1.0)
        });
        let mut lengths = Vec::new();
        timeline.emit(&mut buffers, 1, 48_000, 1, &mut |s| {
            lengths.push(s.frames);
            assert_eq!(
                s.objects[0].spread,
                if lengths.len() == 1 { 1.0 } else { 0.3 }
            );
        });
        assert_eq!(lengths, [71, FRAMES - 71]);
    }

    #[test]
    fn unknown_differential_stays_diffuse_until_absolute_then_adds_delta() {
        let mut pcm = synthetic_objects();
        element(&mut pcm).object_blocks[1][0].differential_position = true;
        add_block(
            &mut pcm,
            8,
            0,
            Vec3 {
                x: 0.5,
                y: 0.0,
                z: 0.0,
            },
            false,
        );
        add_block(
            &mut pcm,
            16,
            0,
            Vec3 {
                x: 1.0 / 62.0,
                y: -1.0 / 62.0,
                z: 1.0 / 15.0,
            },
            true,
        );
        let mut timeline = Timeline::new();
        let mut buffers = SceneBuffers::new(FRAMES);
        timeline.schedule(&pcm, 0).unwrap();
        write_objects(&mut buffers, &pcm, 0, FRAMES).unwrap();
        let mut scene_index = 0;
        timeline.emit(&mut buffers, 1, 48_000, 0, &mut |s| {
            match scene_index {
                0 => {
                    assert_eq!(s.objects[0].position, Position::default());
                    assert_eq!(s.objects[0].spread, 1.0);
                }
                1 => assert_eq!(s.objects[0].position, Position::FRONT),
                2 => assert_eq!(
                    s.objects[0].position,
                    oamd_position(Vec3 {
                        x: 0.5 + 1.0 / 62.0,
                        y: 0.0,
                        z: 1.0 / 15.0,
                    })
                    .unwrap()
                ),
                _ => panic!("unexpected scene"),
            }
            scene_index += 1;
        });
        assert_eq!(scene_index, 3);
    }

    #[test]
    fn differential_uses_previous_target_not_interpolated_position() {
        let mut pcm = synthetic_objects();
        element(&mut pcm).object_blocks[1][0].position = Some(Vec3 {
            x: 0.0,
            y: 0.0,
            z: 0.0,
        });
        add_block(
            &mut pcm,
            8,
            1024,
            Vec3 {
                x: 0.5,
                y: 0.0,
                z: 0.0,
            },
            false,
        );
        add_block(
            &mut pcm,
            16,
            0,
            Vec3 {
                x: 1.0 / 62.0,
                y: 0.0,
                z: 0.0,
            },
            true,
        );
        let mut timeline = Timeline::new();
        let mut buffers = SceneBuffers::new(FRAMES);
        timeline.schedule(&pcm, 0).unwrap();
        write_objects(&mut buffers, &pcm, 0, FRAMES).unwrap();
        let mut frames = 0;
        timeline.emit(&mut buffers, 1, 48_000, 0, &mut |s| {
            if frames == 512 {
                assert_eq!(
                    s.objects[0].position,
                    oamd_position(Vec3 {
                        x: 0.5 + 1.0 / 62.0,
                        y: 0.0,
                        z: 0.0
                    })
                    .unwrap()
                );
            }
            frames += s.frames;
        });
        assert_eq!(frames, FRAMES);
    }

    #[test]
    fn indexed_2048_sample_ramp_and_short_syncframes_preserve_timing() {
        let mut pcm = synthetic_objects();
        let mut timeline = Timeline::new();
        let mut buffers = SceneBuffers::new(FRAMES);
        for part in 0..6 {
            element(&mut pcm).object_blocks[1][0].position = Some(Vec3 {
                x: part as f32 / 5.0,
                y: 0.0,
                z: 0.0,
            });
            timeline.schedule(&pcm, part * 256).unwrap();
            for channel in &mut pcm.object_channels {
                channel.truncate(256);
            }
            pcm.core.lfe_channel.as_mut().unwrap().truncate(256);
            write_objects(&mut buffers, &pcm, part * 256, 256).unwrap();
        }
        let mut scenes = 0;
        timeline.emit(&mut buffers, 1, 48_000, 0, &mut |s| {
            assert_eq!(s.frames, 256);
            assert_eq!(
                s.objects[0].position,
                oamd_position(Vec3 {
                    x: scenes as f32 / 5.0,
                    y: 0.0,
                    z: 0.0,
                })
                .unwrap()
            );
            scenes += 1;
        });
        assert_eq!(scenes, 6);
        // Table 25 permits 2048 even though the explicit 11-bit field stops at 2047.
        element(&mut pcm).block_updates[0].ramp_duration = 2048;
        element(&mut pcm).object_blocks[1][0].position = Some(Vec3 {
            x: 0.0,
            y: 0.0,
            z: 0.0,
        });
        timeline.schedule(&pcm, 0).unwrap();
        let mut frames = 0;
        assert_no_alloc::assert_no_alloc(|| {
            timeline.emit(&mut buffers, 1, 48_000, 0, &mut |s| {
                assert_eq!(s.frames, 256);
                assert_eq!(
                    s.objects[0].position,
                    oamd_position(Vec3 {
                        x: 1.0 - frames as f32 / 2048.0,
                        y: 0.0,
                        z: 0.0,
                    })
                    .unwrap()
                );
                frames += s.frames;
            });
        });
        assert_eq!(frames, FRAMES);
    }

    #[test]
    fn discontinuity_discards_future_updates_and_reset_forgets_positions() {
        let mut pcm = synthetic_objects();
        pcm.oamd_payloads[0].1 = Some(1600);
        let mut timeline = Timeline::new();
        let mut buffers = SceneBuffers::new(FRAMES);
        write_objects(&mut buffers, &pcm, 0, FRAMES).unwrap();
        timeline.schedule(&pcm, 0).unwrap();
        timeline.discontinuity(0).unwrap();
        for _ in 0..2 {
            timeline.emit(&mut buffers, 1, 48_000, 0, &mut |s| {
                assert_eq!(s.objects[0].position, Position::default());
                assert_eq!(s.objects[0].spread, 1.0);
                assert_eq!(s.frames, FRAMES);
            });
        }
        pcm.oamd_payloads[0].1 = None;
        timeline.schedule(&pcm, 0).unwrap();
        timeline.emit(&mut buffers, 1, 48_000, 0, &mut |s| {
            assert_ne!(s.objects[0].position, Position::default())
        });
        timeline.reset();
        timeline.emit(&mut buffers, 1, 48_000, 0, &mut |s| {
            assert_eq!(s.objects[0].spread, 1.0)
        });
    }

    #[test]
    fn mixed_reconstructed_beds_and_dynamic_objects_follow_oamd_order() {
        let mut pcm = synthetic_objects();
        pcm.object_channels.insert(0, vec![0.6; FRAMES]);
        pcm.joc.object_count = 2;
        pcm.object_active.push(true);
        let payload = &mut pcm.oamd_payloads[0].0;
        payload.object_count = 3;
        payload.beds = 2;
        payload.bed_or_isf_objects = 2;
        payload.bed_assignment[0].insert(0, BedChannel::FrontLeft);
        element(&mut pcm)
            .object_blocks
            .insert(0, vec![eac3::OamdObjectBlock::default()]);
        let mut timeline = Timeline::new();
        let mut buffers = SceneBuffers::new(FRAMES);
        timeline.schedule(&pcm, 0).unwrap();
        write_objects(&mut buffers, &pcm, 0, FRAMES).unwrap();
        timeline.emit(&mut buffers, 1, 48_000, 0, &mut |s| {
            assert_eq!(s.objects.len(), 2);
            assert_eq!(s.objects[0].position, Speaker::L.position().unwrap());
            assert_eq!(s.objects[0].signal, &[0.6; FRAMES]);
            assert_eq!(s.objects[1].position, room_to_direction(-0.5, 1.0, 0.5));
            assert_eq!(s.objects[1].signal, &[0.25; FRAMES]);
            assert_eq!(s.lfe.unwrap(), &[0.1; FRAMES]);
        });
    }

    #[test]
    fn reused_metadata_does_not_reapply_delta_or_stale_gain() {
        let mut pcm = synthetic_objects();
        add_block(
            &mut pcm,
            8,
            0,
            Vec3 {
                x: 1.0 / 62.0,
                y: 0.0,
                z: 0.0,
            },
            true,
        );
        add_block(
            &mut pcm,
            16,
            0,
            Vec3 {
                x: 1.0 / 62.0,
                y: 0.0,
                z: 0.0,
            },
            true,
        );
        let reuse = &mut element(&mut pcm).object_blocks[1][2];
        reuse.render_info_status = 2;
        reuse.basic_info_status = 2;
        reuse.gain = Some(0.1); // Upstream's cached value, not an update.
        let mut timeline = Timeline::new();
        let mut buffers = SceneBuffers::new(FRAMES);
        timeline.schedule(&pcm, 0).unwrap();
        write_objects(&mut buffers, &pcm, 0, FRAMES).unwrap();
        let mut index = 0;
        timeline.emit(&mut buffers, 1, 48_000, 0, &mut |s| {
            if index > 0 {
                assert_eq!(
                    s.objects[0].position,
                    oamd_position(Vec3 {
                        x: 0.25 + 1.0 / 62.0,
                        y: 0.0,
                        z: 0.5,
                    })
                    .unwrap()
                );
                assert_eq!(s.objects[0].gain, 0.75);
            }
            index += 1;
        });
        assert_eq!(index, 3);
    }

    #[test]
    fn malformed_oamd_and_queue_overflow_fail_without_allocating() {
        let mut pcm = synthetic_objects();
        let mut timeline = Timeline::new();
        for _ in 0..128 {
            timeline.schedule(&pcm, 0).unwrap();
        }
        assert_no_alloc::assert_no_alloc(|| assert!(timeline.schedule(&pcm, 0).is_err()));
        timeline.reset();
        element(&mut pcm).sample_offset = 1;
        assert_no_alloc::assert_no_alloc(|| assert!(timeline.schedule(&pcm, 0).is_err()));
        element(&mut pcm).sample_offset = 0;
        element(&mut pcm).object_blocks[1][0]
            .position
            .as_mut()
            .unwrap()
            .x = f32::NAN;
        assert_no_alloc::assert_no_alloc(|| assert!(timeline.schedule(&pcm, 0).is_err()));
        element(&mut pcm).object_blocks[1][0]
            .position
            .as_mut()
            .unwrap()
            .x = 0.0;
        pcm.oamd_payloads[0].0.isf_in_use = true;
        assert_no_alloc::assert_no_alloc(|| assert!(timeline.schedule(&pcm, 0).is_err()));
    }

    fn objects_delivery(sequence: u16, metadata: bool) -> DdpDecoded {
        let mut pcm = synthetic_objects();
        pcm.joc.sequence_counter = sequence;
        if !metadata {
            pcm.oamd_payloads.clear();
        }
        DdpDecoded {
            groups: vec![DecodedGroup {
                samples: FRAMES,
                rate: 48_000,
                kind: DecodedKind::Objects(pcm),
            }],
        }
    }

    fn place(
        back: &mut DdpBack,
        decoded: &DdpDecoded,
        epoch: u32,
    ) -> Result<Vec<(u32, Position, f32)>, DecodeError> {
        let mut scenes = Vec::new();
        back.place(decoded, 9, epoch, &mut |scene| {
            let object = scene.objects[0];
            scenes.push((scene.epoch, object.position, object.spread));
        })?;
        Ok(scenes)
    }

    #[test]
    fn ddp_back_resets_itself_on_an_epoch_change_and_after_a_place_error() {
        let known = (room_to_direction(-0.5, 1.0, 0.5), 0.3);
        let unknown = (Position::default(), 1.0);
        let mut back = DdpBack::new(DecodeConfig::default());
        let at = |epoch, (position, spread): (Position, f32)| vec![(epoch, position, spread)];

        // OAMD positions carry over a continuous JOC sequence in one epoch.
        assert_eq!(
            place(&mut back, &objects_delivery(1, true), 1),
            Ok(at(1, known))
        );
        assert_eq!(
            place(&mut back, &objects_delivery(2, false), 1),
            Ok(at(1, known))
        );
        // A new epoch forgets them although the sequence is continuous.
        assert_eq!(
            place(&mut back, &objects_delivery(3, false), 2),
            Ok(at(2, unknown))
        );
        assert_eq!(
            place(&mut back, &objects_delivery(4, true), 2),
            Ok(at(2, known))
        );

        // A delivery that fails while it is written (an object channel shorter
        // than its syncframe) emits nothing and resets: the next continuous
        // delivery without metadata no longer finds the old position.
        let mut short = objects_delivery(5, true);
        let DecodedKind::Objects(pcm) = &mut short.groups[0].kind else {
            unreachable!()
        };
        pcm.object_channels[0].truncate(FRAMES - 1);
        assert!(matches!(
            place(&mut back, &short, 2),
            Err(DecodeError::Corrupt { .. })
        ));
        assert_eq!(
            place(&mut back, &objects_delivery(6, false), 2),
            Ok(at(2, unknown))
        );

        // So does an empty delivery.
        assert_eq!(
            place(&mut back, &objects_delivery(7, true), 2),
            Ok(at(2, known))
        );
        assert!(matches!(
            place(&mut back, &DdpDecoded::new(), 2),
            Err(DecodeError::WrongRoute(_))
        ));
        assert_eq!(
            place(&mut back, &objects_delivery(8, false), 2),
            Ok(at(2, unknown))
        );
    }

    #[test]
    fn five_channel_joc_accepts_replacement_bed_but_refuses_expanded_bed() {
        let mut core = synthetic_objects().core;
        core.fullband_channel_order = vec![
            BedChannel::FrontLeft,
            BedChannel::Center,
            BedChannel::FrontRight,
            BedChannel::SurroundLeft,
            BedChannel::SurroundRight,
        ];
        assert_no_alloc::assert_no_alloc(|| validate_joc_bed(&core, 5).unwrap());
        core.fullband_channel_order
            .extend([BedChannel::RearLeft, BedChannel::RearRight]);
        assert_no_alloc::assert_no_alloc(|| {
            assert!(matches!(
                validate_joc_bed(&core, 5),
                Err(DecodeError::Unsupported { .. })
            ));
            validate_joc_bed(&core, 7).unwrap();
        });
    }

    #[test]
    fn ddp_and_pcm_use_the_same_nominal_speaker_table() {
        for (bed, bit, speaker) in [
            (BedChannel::FrontLeft, 1, Speaker::L),
            (BedChannel::FrontRight, 2, Speaker::R),
            (BedChannel::Center, 4, Speaker::C),
            (BedChannel::RearLeft, 0x10, Speaker::Lrs),
            (BedChannel::RearRight, 0x20, Speaker::Rrs),
            (BedChannel::RearCenter, 0x100, Speaker::Cs),
            (BedChannel::TopFrontLeft, 0x1000, Speaker::Tfl),
            (BedChannel::TopFrontRight, 0x4000, Speaker::Tfr),
            (BedChannel::TopRearLeft, 0x8000, Speaker::Trl),
            (BedChannel::TopRearRight, 0x20000, Speaker::Trr),
            (BedChannel::LowFrequencyEffects, 8, Speaker::Lfe),
        ] {
            assert_eq!(bed_position(bed), speaker.position());
            assert_eq!(crate::pcm::speaker_position(bit), speaker.position());
        }
        // Preserve the existing distinction, rather than moving PCM sides 20°.
        assert_eq!(
            bed_position(BedChannel::SurroundLeft),
            Speaker::Ls.position()
        );
        assert_eq!(crate::pcm::speaker_position(0x200), Speaker::Lss.position());
    }

    #[test]
    fn bed_table_uses_left_positive_angles() {
        use BedChannel::*;
        for (left, right, az, el) in [
            (FrontLeft, FrontRight, 30.0, 0.0),
            (SurroundLeft, SurroundRight, 110.0, 0.0),
            (RearLeft, RearRight, 150.0, 0.0),
            (WideLeft, WideRight, 60.0, 0.0),
            (TopFrontLeft, TopFrontRight, 45.0, 45.0),
            (TopSurroundLeft, TopSurroundRight, 90.0, 45.0),
            (TopRearLeft, TopRearRight, 135.0, 45.0),
        ] {
            assert_eq!(bed_position(left), Some(Position::from_angles(az, el)));
            assert_eq!(bed_position(right), Some(Position::from_angles(-az, el)));
        }
        assert_eq!(bed_position(Center), Some(Position::FRONT));
        assert_eq!(
            bed_position(RearCenter),
            Some(Position::from_angles(180.0, 0.0))
        );
        assert_eq!(bed_position(LowFrequencyEffects), None);
        assert_eq!(bed_position(LowFrequencyEffects2), None);
        assert_eq!(eac3::JOC_LATENCY_SAMPLES, 577);
    }
}
