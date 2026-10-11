//! DD+ (E-AC-3) adapter, with JOC objects when the stream carries them.
//! Wraps harletty-bridge's `eac3` crate (Apache-2.0, ADR 0007).
//!
//! `eac3` takes one syncframe, not an IEC access unit. We decode independent
//! substream 0 and overlay its dependent substreams before reconstructing JOC.
//! Short syncframes are concatenated to the access unit's 1536 samples.
//! The upstream decoder allocates per frame; this adapter adds no allocations
//! in `push`, including error paths (owned error details are empty).
//!
//! Bed angles (azimuth positive left, elevation positive up): FL/FR +/-30/0,
//! C 0/0, Ls/Rs +/-110/0, rear centre 180/0, Lrs/Rrs +/-150/0,
//! Lw/Rw +/-60/0, top front +/-45/45, top surround +/-90/45,
//! top rear +/-135/45. Both LFE labels are unpositioned.
//!
//! With JOC, reconstructed channels replace the fullband downmix; emitting
//! both would count the same audio twice. Only the aligned core LFE is kept.
//! `eac3::JOC_LATENCY_SAMPLES` (577) is already applied to that LFE upstream.
//! A scene has one position per object: we use the first OAMD block of the
//! access unit, holding that position across its 1536 samples. Sub-block ramps,
//! screen geometry and distance are not represented by SceneBlock.

use bio_types::{MAX_OBJECTS, Position, SceneBlock};
use bitstream::{Burst, DataType};
use eac3::{
    BedChannel, CorePcmFrame, OamdElementKind, ObjectAnchor, ObjectPcmFrame, ParsedEmdfPayloadData,
    StreamType, Vec3,
};

use crate::{DecodeConfig, DecodeError, SceneBuffers};

const FRAMES: usize = 1536;

#[derive(Clone, Copy, Debug)]
struct ObjectMeta {
    position: Position,
    gain: f32,
    spread: f32,
    known: bool,
}

const EMPTY_META: ObjectMeta = ObjectMeta {
    position: Position::FRONT,
    gain: 1.0,
    spread: 0.0,
    known: false,
};

#[derive(Debug)]
pub struct DdpDecoder {
    buffers: SceneBuffers,
    core: eac3::PcmDecoder,
    dependent: [eac3::PcmDecoder; 8],
    objects: eac3::ObjectPcmDecoder,
    metadata: [ObjectMeta; MAX_OBJECTS],
    epoch: Option<u32>,
    joc_sequence: Option<u16>,
}

impl DdpDecoder {
    pub fn new(config: DecodeConfig) -> Self {
        Self {
            buffers: SceneBuffers::new(config.max_frames),
            core: eac3::PcmDecoder::new(),
            dependent: std::array::from_fn(|_| eac3::PcmDecoder::new()),
            objects: eac3::ObjectPcmDecoder::new(),
            metadata: [EMPTY_META; MAX_OBJECTS],
            epoch: None,
            joc_sequence: None,
        }
    }

    /// Decodes the E-AC-3 syncframes in one burst payload and emits one scene
    /// per access unit (1536 frames): bed channels as objects at their
    /// standard positions, JOC objects at the positions their OAMD gives.
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
        if self.epoch != Some(epoch) {
            self.reset();
            self.epoch = Some(epoch);
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
        if self.buffers.max_frames() < FRAMES {
            return Err(unsupported(
                "max_frames is below one 1536-frame access unit",
            ));
        }
        validate_access_unit(burst.payload)?;
        let mut bytes = burst.payload;
        let mut offset = 0;
        let mut shape = None;
        let mut order = [None; MAX_OBJECTS];
        let mut rate = 0;
        while !bytes.is_empty() {
            let (frame, header, rest) = syncframe(bytes)?;
            bytes = rest;
            let decoded = self.core.push_access_unit(frame).map_err(|_| corrupt())?;
            let mut joc_frame = has_joc(&decoded.info).then_some(frame);
            let mut core = decoded.pcm;
            while !bytes.is_empty() {
                let (dep_frame, dep_header, rest) = syncframe(bytes)?;
                if dep_header.stream_type != StreamType::Dependent {
                    break;
                }
                // ETSI TS 103 420 puts JOC in the last substream. In
                // particular, never overlay discrete surrounds onto a core
                // whose own five-channel JOC matrices expect folded surrounds.
                if joc_frame.is_some() {
                    return Err(corrupt());
                }
                let dep = self.dependent[usize::from(dep_header.substream_id)]
                    .push_access_unit(dep_frame)
                    .map_err(|_| corrupt())?;
                if has_joc(&dep.info) {
                    if dep.info.payloads().any(|p| {
                        matches!(&p.parsed, ParsedEmdfPayloadData::Joc(joc) if joc.channel_count == 5)
                    }) {
                        return Err(unsupported("five-channel JOC on an overlaid dependent bed"));
                    }
                    joc_frame = Some(dep_frame);
                }
                // Allocations in this merge belong to the upstream eac3 API.
                core = eac3::merge_core_with_decoded_dependent(&core, &dep.pcm, &dep.info)
                    .ok_or_else(corrupt)?;
                bytes = rest;
            }
            let samples = usize::from(header.samples);
            rate = header.sample_rate;
            let object_frame = if let Some(frame) = joc_frame {
                Some(
                    self.objects
                        .push_access_unit_with_core(frame, core)
                        .map_err(|_| corrupt())?
                        .ok_or_else(corrupt)?,
                )
            } else {
                self.objects.note_non_joc_presentation();
                self.joc_sequence = None;
                self.metadata.fill(EMPTY_META);
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
                write_core(&mut self.buffers, &core, offset, samples)?;
                None
            };
            if let Some(decoded) = object_frame {
                let pcm = decoded.pcm;
                let sequence = pcm.joc.sequence_counter;
                let continuous = self.joc_sequence.is_some_and(|previous| {
                    sequence != 0 && sequence == if previous == 1023 { 1 } else { previous + 1 }
                });
                if !continuous {
                    self.metadata.fill(EMPTY_META);
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
                if offset == 0 {
                    read_object_metadata(&pcm, &mut self.metadata)?;
                }
                write_objects(&mut self.buffers, &pcm, &self.metadata, offset, samples)?;
            }
            offset += samples;
        }
        self.buffers.emit(FRAMES, epoch, rate, qpc, out);
        Ok(())
    }

    pub fn reset(&mut self) {
        self.core.reset();
        for decoder in &mut self.dependent {
            decoder.reset();
        }
        self.objects.reset();
        self.metadata.fill(EMPTY_META);
        self.epoch = None;
        self.joc_sequence = None;
        self.buffers.set_layout(0, false);
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
    metadata: &[ObjectMeta; MAX_OBJECTS],
    offset: usize,
    samples: usize,
) -> Result<(), DecodeError> {
    buffers.set_layout(pcm.object_channels.len(), pcm.core.lfe_channel.is_some());
    for (i, channel) in pcm.object_channels.iter().enumerate() {
        if channel.len() != samples || !metadata[i].known {
            return Err(corrupt());
        }
        let m = metadata[i];
        buffers.set_object(i, m.position, m.gain, m.spread);
        // Preserve QMF tails even when JocObject.active becomes false.
        buffers.signal_mut(i, FRAMES)[offset..offset + samples].copy_from_slice(channel);
    }
    write_lfe(buffers, &pcm.core, offset, samples)
}

fn read_object_metadata(
    pcm: &ObjectPcmFrame,
    metadata: &mut [ObjectMeta; MAX_OBJECTS],
) -> Result<(), DecodeError> {
    if pcm.object_channels.len() > MAX_OBJECTS {
        return Err(corrupt());
    }
    // A scene samples only the first payload, not a future metadata update.
    if let Some((payload, _)) = pcm.oamd_payloads.first() {
        // Pure-bed JOC reconstructs the assigned speakers, excluding bypass LFE.
        if payload.dynamic_objects == 0 {
            let mut count = 0;
            for &bed in payload.bed_assignment.iter().flatten() {
                if let Some(position) = bed_position(bed) {
                    let slot = metadata.get_mut(count).ok_or_else(corrupt)?;
                    *slot = ObjectMeta {
                        position,
                        known: true,
                        ..EMPTY_META
                    };
                    count += 1;
                }
            }
            if count != pcm.object_channels.len() {
                return Err(corrupt());
            }
            return Ok(());
        }
        if payload.dynamic_objects != pcm.object_channels.len() {
            // Mixed reconstructed beds/ISF need an explicit channel mapping.
            return Err(unsupported("mixed reconstructed beds and dynamic objects"));
        }
        for element in &payload.elements {
            let OamdElementKind::Object(element) = &element.kind else {
                continue;
            };
            for (i, blocks) in element
                .object_blocks
                .iter()
                .skip(payload.bed_or_isf_objects)
                .take(pcm.object_channels.len())
                .enumerate()
            {
                let Some(block) = blocks.first() else {
                    continue;
                };
                if block.anchor == ObjectAnchor::Speaker {
                    return Err(corrupt());
                }
                let slot = &mut metadata[i];
                if block.valid_position {
                    let position = block.position.ok_or_else(corrupt)?;
                    if block.differential_position {
                        // The parser preserves differential flags. First metadata
                        // blocks are absolute in supported OAMD presentations.
                        return Err(unsupported(
                            "a first OAMD block with a differential position",
                        ));
                    }
                    slot.position = oamd_position(position).ok_or_else(corrupt)?;
                    slot.known = true;
                }
                if let Some(gain) = block.gain {
                    if !gain.is_finite() || gain < 0.0 {
                        return Err(corrupt());
                    }
                    slot.gain = gain;
                }
                if let Some(size) = block.size {
                    if size.iter().any(|v| !v.is_finite()) {
                        return Err(corrupt());
                    }
                    slot.spread = size.into_iter().fold(0.0_f32, f32::max).clamp(0.0, 1.0);
                }
            }
        }
    }
    Ok(())
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
    Some(Position::new(
        2.0 * p.x.clamp(0.0, 1.0) - 1.0,
        1.0 - 2.0 * p.y.clamp(0.0, 1.0),
        p.z.clamp(-1.0, 1.0),
    ))
}

fn bed_position(bed: BedChannel) -> Option<Position> {
    use BedChannel::*;
    let (az, el) = match bed {
        FrontLeft => (30.0, 0.0),
        FrontRight => (-30.0, 0.0),
        Center => (0.0, 0.0),
        SurroundLeft => (110.0, 0.0),
        SurroundRight => (-110.0, 0.0),
        RearCenter => (180.0, 0.0),
        RearLeft => (150.0, 0.0),
        RearRight => (-150.0, 0.0),
        WideLeft => (60.0, 0.0),
        WideRight => (-60.0, 0.0),
        TopFrontLeft => (45.0, 45.0),
        TopFrontRight => (-45.0, 45.0),
        TopSurroundLeft => (90.0, 45.0),
        TopSurroundRight => (-90.0, 45.0),
        TopRearLeft => (135.0, 45.0),
        TopRearRight => (-135.0, 45.0),
        LowFrequencyEffects | LowFrequencyEffects2 => return None,
    };
    Some(Position::from_angles(az, el))
}

#[cfg(test)]
mod tests {
    use super::*;

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
                Some(Position::new(expected[0], expected[1], expected[2]))
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
                            block_updates: vec![],
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
        let mut metadata = [EMPTY_META; MAX_OBJECTS];
        read_object_metadata(&pcm, &mut metadata).unwrap();
        let mut buffers = SceneBuffers::new(FRAMES);
        write_objects(&mut buffers, &pcm, &metadata, 0, FRAMES).unwrap();
        buffers.emit(FRAMES, 1, 48_000, 2, &mut |scene| {
            assert_eq!(scene.objects.len(), 1);
            let object = scene.objects[0];
            assert_eq!(object.position, Position::new(-0.5, 1.0, 0.5));
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
        let mut metadata = [EMPTY_META; MAX_OBJECTS];
        read_object_metadata(&pcm, &mut metadata).unwrap();
        assert_eq!(metadata[0].position, Position::from_angles(45.0, 45.0));
        assert!(metadata[0].known);
        assert!(!metadata[1].known);
    }

    #[test]
    fn unknown_object_position_is_not_silently_rendered_at_front() {
        let mut pcm = synthetic_objects();
        pcm.oamd_payloads.clear();
        let mut metadata = [EMPTY_META; MAX_OBJECTS];
        read_object_metadata(&pcm, &mut metadata).unwrap();
        let mut buffers = SceneBuffers::new(FRAMES);
        assert!(write_objects(&mut buffers, &pcm, &metadata, 0, FRAMES).is_err());
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
