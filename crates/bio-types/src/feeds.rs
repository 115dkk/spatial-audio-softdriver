//! Feeds: one signal per layout direction.

/// Planar buffers, one per direction, all `frames` long.
///
/// Allocated once for the largest block a stream can produce; `set_frames`
/// changes the active length without allocating.
#[derive(Clone, Debug)]
pub struct Feeds {
    directions: usize,
    capacity: usize,
    frames: usize,
    data: Vec<f32>,
}

impl Feeds {
    pub fn new(directions: usize, max_frames: usize) -> Self {
        Self {
            directions,
            capacity: max_frames,
            frames: max_frames,
            data: vec![0.0; directions * max_frames],
        }
    }

    pub fn directions(&self) -> usize {
        self.directions
    }

    pub fn frames(&self) -> usize {
        self.frames
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Sets the active block length.
    ///
    /// # Panics
    /// When `frames` exceeds the capacity given to [`Feeds::new`].
    pub fn set_frames(&mut self, frames: usize) {
        assert!(
            frames <= self.capacity,
            "block of {frames} frames exceeds feed capacity {}",
            self.capacity
        );
        self.frames = frames;
    }

    pub fn channel(&self, direction: usize) -> &[f32] {
        let start = direction * self.capacity;
        &self.data[start..start + self.frames]
    }

    pub fn channel_mut(&mut self, direction: usize) -> &mut [f32] {
        let start = direction * self.capacity;
        &mut self.data[start..start + self.frames]
    }

    /// Zeroes the active part of every channel.
    pub fn clear(&mut self) {
        for d in 0..self.directions {
            self.channel_mut(d).fill(0.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channels_are_disjoint_and_follow_the_active_length() {
        let mut feeds = Feeds::new(3, 8);
        feeds.set_frames(4);
        feeds.channel_mut(1).fill(1.0);
        assert_eq!(feeds.channel(0), &[0.0; 4]);
        assert_eq!(feeds.channel(1), &[1.0; 4]);
        assert_eq!(feeds.channel(2).len(), 4);
        feeds.clear();
        assert_eq!(feeds.channel(1), &[0.0; 4]);
    }

    #[test]
    #[should_panic(expected = "exceeds feed capacity")]
    fn set_frames_rejects_more_than_capacity() {
        Feeds::new(1, 4).set_frames(5);
    }
}
