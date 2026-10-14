//! Bounded OAMD update queue and room-coordinate ramps. No push-time allocation.

use eac3::{OamdElementKind, OamdObjectBlock, OamdPayload, ObjectAnchor, ObjectPcmFrame, Vec3};
use ssr_types::{MAX_OBJECTS, Position};

use super::{EMPTY_META, FRAMES, ObjectMeta, bed_position, corrupt, oamd_position, unsupported};
use crate::{DecodeError, SceneBuffers};

// A frame has at most eight updates. This also covers six short syncframes
// and updates whose EMDF sample offset carries into later access units.
const MAX_EVENTS: usize = 128;

#[derive(Clone, Copy, Debug)]
struct Update {
    room: Option<Vec3>,
    differential: bool,
    speaker: Option<Position>,
    gain: Option<f32>,
    spread: Option<f32>,
}

const EMPTY_UPDATE: Update = Update {
    room: None,
    differential: false,
    speaker: None,
    gain: None,
    spread: None,
};

#[derive(Clone, Copy, Debug)]
struct Event {
    sample: u64,
    ramp: u64,
    reset: bool,
    objects: usize,
    updates: [Update; MAX_OBJECTS],
}

#[derive(Clone, Copy, Debug)]
struct Ramp {
    from: Vec3,
    start: u64,
    end: u64,
}

const NO_RAMP: Ramp = Ramp {
    from: Vec3 {
        x: 0.5,
        y: 0.5,
        z: 0.0,
    },
    start: 0,
    end: 0,
};

#[derive(Debug)]
pub(super) struct Timeline {
    events: Vec<Event>,
    metadata: [ObjectMeta; MAX_OBJECTS],
    ramps: [Ramp; MAX_OBJECTS],
    clock: u64,
}

impl Timeline {
    pub(super) fn new() -> Self {
        Self {
            events: Vec::with_capacity(MAX_EVENTS),
            metadata: [EMPTY_META; MAX_OBJECTS],
            ramps: [NO_RAMP; MAX_OBJECTS],
            clock: 0,
        }
    }

    pub(super) fn reset(&mut self) {
        self.events.clear();
        self.metadata.fill(EMPTY_META);
        self.ramps.fill(NO_RAMP);
        self.clock = 0;
    }

    fn insert(&mut self, event: Event) -> Result<(), DecodeError> {
        if self.events.len() == self.events.capacity() {
            return Err(unsupported("too many pending OAMD updates"));
        }
        let index = self.events.partition_point(|e| e.sample <= event.sample);
        self.events.insert(index, event);
        Ok(())
    }

    pub(super) fn discontinuity(&mut self, offset: usize) -> Result<(), DecodeError> {
        let sample = self.clock + offset as u64;
        // Old future metadata belongs to the discontinued presentation. Keep
        // earlier updates in this access unit; no scene has been emitted yet.
        self.events.retain(|event| event.sample < sample);
        self.insert(Event {
            sample,
            ramp: 0,
            reset: true,
            objects: 0,
            updates: [EMPTY_UPDATE; MAX_OBJECTS],
        })
    }

    pub(super) fn schedule(
        &mut self,
        pcm: &ObjectPcmFrame,
        offset: usize,
    ) -> Result<(), DecodeError> {
        if pcm.object_channels.len() > MAX_OBJECTS {
            return Err(corrupt());
        }
        for (payload, payload_offset) in &pcm.oamd_payloads {
            let ObjectMapping { mapping, speakers } =
                object_mapping(payload, pcm.object_channels.len())?;
            let base = self.clock + offset as u64 + u64::from(payload_offset.unwrap_or(0));
            let mut has_element = false;
            for element in &payload.elements {
                let OamdElementKind::Object(element) = &element.kind else {
                    continue;
                };
                has_element = true;
                if element.block_updates.is_empty()
                    || element.block_updates.len() > 8
                    || element.object_blocks.len() != payload.object_count
                    || element
                        .object_blocks
                        .iter()
                        .any(|b| b.len() != element.block_updates.len())
                {
                    return Err(corrupt());
                }
                let mut previous = None;
                for (block_index, timing) in element.block_updates.iter().enumerate() {
                    // Pinned eac3 metadata.rs stores factor + sample_offset,
                    // not a sample count. TS 103 420 §5.3.2: so + 32 * factor.
                    let factor = i32::from(timing.offset) - i32::from(element.sample_offset);
                    if !(0..=63).contains(&factor)
                        || element.sample_offset > 31
                        || !(0..=2048).contains(&timing.ramp_duration)
                    {
                        return Err(corrupt());
                    }
                    let sample = base + u64::from(element.sample_offset) + 32 * factor as u64;
                    if previous.is_some_and(|p| p >= sample) {
                        return Err(corrupt());
                    }
                    previous = Some(sample);
                    let mut event = Event {
                        sample,
                        ramp: timing.ramp_duration as u64,
                        reset: false,
                        objects: pcm.object_channels.len(),
                        updates: [EMPTY_UPDATE; MAX_OBJECTS],
                    };
                    for (oamd_index, &output) in
                        mapping.iter().enumerate().take(payload.object_count)
                    {
                        let Some(output) = output else { continue };
                        event.updates[output] = update_from_block(
                            &element.object_blocks[oamd_index][block_index],
                            speakers[output],
                        )?;
                    }
                    self.insert(event)?;
                }
            }
            // A bed assignment is sufficient even if no object element exists.
            if !has_element {
                let mut event = Event {
                    sample: base,
                    ramp: 0,
                    reset: false,
                    objects: pcm.object_channels.len(),
                    updates: [EMPTY_UPDATE; MAX_OBJECTS],
                };
                for (update, speaker) in event.updates.iter_mut().zip(speakers) {
                    update.speaker = speaker;
                }
                self.insert(event)?;
            }
        }
        Ok(())
    }

    fn room_at(&self, index: usize, sample: u64) -> Vec3 {
        let target = self.metadata[index].room;
        let ramp = self.ramps[index];
        if ramp.end <= sample || ramp.end <= ramp.start {
            return target;
        }
        let t = sample.saturating_sub(ramp.start) as f32 / (ramp.end - ramp.start) as f32;
        Vec3 {
            x: ramp.from.x + (target.x - ramp.from.x) * t,
            y: ramp.from.y + (target.y - ramp.from.y) * t,
            z: ramp.from.z + (target.z - ramp.from.z) * t,
        }
    }

    fn apply(&mut self, event: Event) {
        if event.reset {
            self.metadata.fill(EMPTY_META);
            self.ramps.fill(NO_RAMP);
            return;
        }
        for (i, update) in event.updates.iter().enumerate().take(event.objects) {
            let from = self.room_at(i, event.sample);
            let slot = &mut self.metadata[i];
            if let Some(position) = update.speaker {
                slot.position = position;
                slot.known = true;
                slot.is_room = false;
                self.ramps[i] = NO_RAMP;
            } else if let Some(room) = update.room
                && (!update.differential || (slot.known && slot.is_room))
            {
                let was_known = slot.known && slot.is_room;
                // TS 103 420 §5.6.1.1.12–14: deltas refer to the previous
                // standard-precision target, NOT the current ramp value.
                slot.room = if update.differential {
                    Vec3 {
                        x: (slot.room.x + room.x).clamp(0.0, 1.0),
                        y: (slot.room.y + room.y).clamp(0.0, 1.0),
                        z: (slot.room.z + room.z).clamp(-1.0, 1.0),
                    }
                } else {
                    Vec3 {
                        x: room.x.clamp(0.0, 1.0),
                        y: room.y.clamp(0.0, 1.0),
                        z: room.z.clamp(-1.0, 1.0),
                    }
                };
                slot.known = true;
                slot.is_room = true;
                self.ramps[i] = if was_known && event.ramp != 0 {
                    Ramp {
                        from,
                        start: event.sample,
                        end: event.sample + event.ramp,
                    }
                } else {
                    NO_RAMP
                };
            }
            // Unknown differential targets stay diffuse until an absolute
            // update arrives. Their audio and gain are still preserved.
            if let Some(gain) = update.gain {
                slot.gain = gain;
            }
            if let Some(spread) = update.spread {
                slot.spread = spread;
            }
        }
    }

    pub(super) fn emit(
        &mut self,
        buffers: &mut SceneBuffers,
        epoch: u32,
        rate: u32,
        qpc: u64,
        out: &mut dyn FnMut(&ssr_types::SceneBlock<'_>),
    ) {
        let end = self.clock + FRAMES as u64;
        let mut sample = self.clock;
        let mut consumed = 0;
        while sample < end {
            while consumed < self.events.len() && self.events[consumed].sample <= sample {
                self.apply(self.events[consumed]);
                consumed += 1;
            }
            let mut next = self.events.get(consumed).map_or(end, |e| e.sample.min(end));
            for i in 0..buffers.objects() {
                let meta = self.metadata[i];
                let (position, spread) = if !meta.known {
                    (Position::default(), 1.0)
                } else if meta.is_room {
                    let room = self.room_at(i, sample);
                    (
                        oamd_position(room).expect("validated finite room coordinates"),
                        meta.spread,
                    )
                } else {
                    (meta.position, meta.spread)
                };
                buffers.set_object(i, position, meta.gain, spread);
                if self.ramps[i].end > sample {
                    // Also split short ramps at their end. Long ramps and ramps
                    // crossing an update/frame boundary never exceed 256 samples.
                    next = next.min(self.ramps[i].end).min(sample + 256);
                }
            }
            buffers.emit_range(
                (sample - self.clock) as usize..(next - self.clock) as usize,
                epoch,
                rate,
                qpc,
                out,
            );
            sample = next;
        }
        self.events.drain(..consumed);
        self.clock = end;
    }
}

fn update_from_block(
    block: &OamdObjectBlock,
    speaker: Option<Position>,
) -> Result<Update, DecodeError> {
    // eac3 copies cached fields into reuse blocks (including delta flags).
    // Never apply a cached delta again, or reset an in-flight ramp to a cached
    // absolute target. Mixed updates use eac3's decoded low-bit position mask.
    let position_updated = block.render_info_status != 2
        && (block.render_info_status != 3 || block.render_info_blocks.is_some_and(|b| b & 1 != 0));
    let room = if speaker.is_none() && block.valid_position && position_updated {
        if block.anchor == ObjectAnchor::Speaker {
            return Err(corrupt());
        }
        let p = block.position.ok_or_else(corrupt)?;
        if ![p.x, p.y, p.z].into_iter().all(f32::is_finite) {
            return Err(corrupt());
        }
        Some(p)
    } else {
        None
    };
    if block.gain.is_some_and(|g| !g.is_finite() || g < 0.0)
        || block.size.is_some_and(|s| s.iter().any(|v| !v.is_finite()))
    {
        return Err(corrupt());
    }
    Ok(Update {
        room,
        differential: block.differential_position,
        speaker,
        gain: if block.basic_info_status == 2
            || (block.basic_info_status == 3 && block.basic_info_blocks.is_none_or(|b| b & 2 == 0))
        {
            None
        } else {
            block.gain
        },
        spread: if block.render_info_status == 2
            || (block.render_info_status == 3
                && block.render_info_blocks.is_none_or(|b| b & 4 == 0))
        {
            None
        } else {
            block
                .size
                .map(|s| s.into_iter().fold(0.0_f32, f32::max).clamp(0.0, 1.0))
        },
    })
}

// TS 103 420 §5.6.4.8: bed objects, then ISF, then dynamic objects.
// JOC bypasses LFE (§6.3.2.2 table 47), so it consumes no reconstructed slot.
// ISF is not a speaker bed and the upstream discards its extra rendering data.
struct ObjectMapping {
    mapping: [Option<usize>; MAX_OBJECTS],
    speakers: [Option<Position>; MAX_OBJECTS],
}

fn object_mapping(payload: &OamdPayload, objects: usize) -> Result<ObjectMapping, DecodeError> {
    if payload.isf_in_use || payload.alternate_object_present {
        return Err(unsupported("ISF or alternate OAMD object mapping"));
    }
    if payload.object_count > MAX_OBJECTS || objects > MAX_OBJECTS {
        return Err(corrupt());
    }
    let mut mapping = [None; MAX_OBJECTS];
    let mut speakers = [None; MAX_OBJECTS];
    let mut oamd = 0;
    let mut output = 0;
    for &bed in payload.bed_assignment.iter().flatten() {
        if oamd >= payload.object_count {
            return Err(corrupt());
        }
        if let Some(position) = bed_position(bed) {
            mapping[oamd] = Some(output);
            speakers[output] = Some(position);
            output += 1;
        }
        oamd += 1;
    }
    if oamd != payload.bed_or_isf_objects || oamd + payload.dynamic_objects != payload.object_count
    {
        return Err(corrupt());
    }
    for slot in &mut mapping[oamd..payload.object_count] {
        *slot = Some(output);
        output += 1;
    }
    if output != objects {
        return Err(corrupt());
    }
    Ok(ObjectMapping { mapping, speakers })
}
