//! Buffers an adapter owns and lends to the scenes it emits.

use ssr_types::{MAX_OBJECTS, Object, Position, SceneBlock};

#[derive(Clone, Copy, Debug, PartialEq)]
struct Meta {
    position: Position,
    gain: f32,
    spread: f32,
}

const SILENT_META: Meta = Meta {
    position: Position::FRONT,
    gain: 0.0,
    spread: 0.0,
};

/// Planar object signals plus LFE, allocated once for `MAX_OBJECTS` objects of
/// `max_frames` each. An adapter writes signals with [`signal_mut`], sets each
/// object's position with [`set_object`], and calls [`emit`], which builds the
/// [`SceneBlock`] on the stack without allocating.
///
/// [`signal_mut`]: SceneBuffers::signal_mut
/// [`set_object`]: SceneBuffers::set_object
/// [`emit`]: SceneBuffers::emit
#[derive(Debug, Clone)]
pub struct SceneBuffers {
    max_frames: usize,
    signals: Vec<f32>,
    meta: Vec<Meta>,
    objects: usize,
    lfe: Vec<f32>,
    has_lfe: bool,
}

impl SceneBuffers {
    pub fn new(max_frames: usize) -> Self {
        Self {
            max_frames,
            signals: vec![0.0; MAX_OBJECTS * max_frames],
            meta: vec![SILENT_META; MAX_OBJECTS],
            objects: 0,
            lfe: vec![0.0; max_frames],
            has_lfe: false,
        }
    }

    pub fn max_frames(&self) -> usize {
        self.max_frames
    }

    /// Sets how many objects the next scene carries and whether it has LFE.
    ///
    /// # Panics
    /// When `objects` exceeds [`MAX_OBJECTS`].
    pub fn set_layout(&mut self, objects: usize, has_lfe: bool) {
        assert!(
            objects <= MAX_OBJECTS,
            "{objects} objects exceed MAX_OBJECTS"
        );
        self.objects = objects;
        self.has_lfe = has_lfe;
    }

    pub fn objects(&self) -> usize {
        self.objects
    }

    pub fn set_object(&mut self, index: usize, position: Position, gain: f32, spread: f32) {
        self.meta[index] = Meta {
            position,
            gain,
            spread,
        };
    }

    /// The first `frames` samples of object `index`'s buffer.
    pub fn signal_mut(&mut self, index: usize, frames: usize) -> &mut [f32] {
        let start = index * self.max_frames;
        &mut self.signals[start..start + frames]
    }

    pub fn lfe_mut(&mut self, frames: usize) -> &mut [f32] {
        &mut self.lfe[..frames]
    }

    /// Lends the first `frames` samples of every active buffer to `out`.
    ///
    /// # Panics
    /// When `frames` exceeds `max_frames`.
    pub fn emit(
        &self,
        frames: usize,
        epoch: u32,
        rate: u32,
        source_qpc: u64,
        out: &mut dyn FnMut(&SceneBlock<'_>),
    ) {
        self.emit_range(0..frames, epoch, rate, source_qpc, out);
    }

    /// Lends a range from every active signal without copying or allocating.
    /// `source_qpc` remains the arrival time of the source burst; splitting a
    /// burst does not invent a QPC frequency or a later capture timestamp.
    pub fn emit_range(
        &self,
        range: std::ops::Range<usize>,
        epoch: u32,
        rate: u32,
        source_qpc: u64,
        out: &mut dyn FnMut(&SceneBlock<'_>),
    ) {
        assert!(range.start <= range.end && range.end <= self.max_frames);
        let frames = range.len();
        let mut objects = [Object::SILENT; MAX_OBJECTS];
        for (i, slot) in objects.iter_mut().enumerate().take(self.objects) {
            let start = i * self.max_frames;
            let m = self.meta[i];
            *slot = Object {
                signal: &self.signals[start + range.start..start + range.end],
                position: m.position,
                gain: m.gain,
                spread: m.spread,
            };
        }
        let block = SceneBlock {
            epoch,
            frames,
            rate,
            source_qpc,
            objects: &objects[..self.objects],
            lfe: self.has_lfe.then(|| &self.lfe[range]),
            new_objects: 0,
        };
        debug_assert!(block.is_consistent());
        out(&block);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emitted_scene_borrows_the_written_signals() {
        let mut b = SceneBuffers::new(8);
        b.set_layout(2, true);
        b.signal_mut(1, 4).fill(0.5);
        b.lfe_mut(4).fill(0.25);
        b.set_object(1, Position::new(1.0, 0.0, 0.0), 0.8, 0.0);
        let mut seen = 0;
        b.emit(4, 7, 48_000, 99, &mut |scene| {
            seen += 1;
            assert_eq!((scene.epoch, scene.frames, scene.source_qpc), (7, 4, 99));
            assert_eq!(scene.objects.len(), 2);
            assert_eq!(scene.objects[1].signal, &[0.5; 4]);
            assert_eq!(scene.objects[1].gain, 0.8);
            assert_eq!(scene.lfe, Some(&[0.25; 4][..]));
        });
        assert_eq!(seen, 1);
    }
}
