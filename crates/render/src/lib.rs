#![forbid(unsafe_code)]

//! Scene to direction feeds: VBAP over the directions of a BRIR layout.
//!
//! The panner triangulates the convex hull of the layout's directions once,
//! then for every object finds the triangle its direction falls in and gives
//! the three corners power-normalised gains. Where the layout leaves a gap
//! (no direction below ear level, nothing overhead) virtual directions fill it
//! and their share is passed on to the real neighbours, so an object placed
//! there keeps its energy (the ceiling-centre object of the build logs).
//! Layouts whose directions all sit at ear level pan pairwise in the
//! horizontal plane. Gains change with a short linear ramp at each block start
//! so moving objects do not click. LFE follows the profile's route.
//!
//! One adapter (VBAP), so no trait (ADR 0010). Spread is ignored for now.

use bio_types::{DirectionId, Feeds, Layout, Position, SceneBlock};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PannerConfig {
    /// Direction that receives the LFE signal, with its linear gain; `None`
    /// drops LFE.
    pub lfe_route: Option<(DirectionId, f32)>,
    /// Frames over which a gain change is ramped at the start of a block.
    pub ramp_frames: usize,
}

impl Default for PannerConfig {
    fn default() -> Self {
        Self {
            lfe_route: None,
            ramp_frames: 64,
        }
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum RenderError {
    #[error("the layout cannot be triangulated: {0}")]
    DegenerateLayout(String),
    #[error("the LFE route names direction {0}, which the layout lacks")]
    BadLfeRoute(u16),
}

#[derive(Debug)]
pub struct Panner {
    // Implementation detail: triangles, inverse matrices, per-object gain cache
    // (MAX_OBJECTS x directions), all sized in `new`.
}

impl Panner {
    pub fn new(layout: &Layout, config: PannerConfig) -> Result<Self, RenderError> {
        let _ = (layout, config);
        todo!("M1: render worker")
    }

    pub fn directions(&self) -> usize {
        todo!("M1: render worker")
    }

    /// Writes the gain of every direction for an object at `position` into
    /// `out` (`out.len() == directions()`). The squares sum to 1. A position on
    /// a measured direction gives that direction 1 and the rest 0. The centre
    /// of the room, which has no direction, spreads equal power to all.
    pub fn gains(&self, position: Position, out: &mut [f32]) {
        let _ = (position, out);
        todo!("M1: render worker")
    }

    /// Clears `feeds`, sets its length to `scene.frames` and adds every object
    /// (signal x object gain x panning gain, ramped from the previous block's
    /// gains for the same object index) and the routed LFE. Never allocates.
    pub fn render(&mut self, scene: &SceneBlock<'_>, feeds: &mut Feeds) {
        let _ = (scene, feeds);
        todo!("M1: render worker")
    }

    /// Forgets the previous gains, so the next block starts without a ramp.
    pub fn reset(&mut self) {
        todo!("M1: render worker")
    }
}
