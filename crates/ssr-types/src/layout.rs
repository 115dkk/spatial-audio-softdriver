//! Directions and layouts.
//!
//! A direction is one speaker position a BRIR was measured at. A layout is the
//! ordered list of directions a BRIR set holds; the renderer pans between them
//! and the convolver takes one feed per direction in the same order. The order
//! is fixed once, by `brir::prepare`, and every other crate takes it from there.

use crate::scene::Position;

/// Index of a direction within its [`Layout`].
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[cfg_attr(feature = "ts", derive(specta::Type))]
pub struct DirectionId(pub u16);

impl DirectionId {
    pub fn index(self) -> usize {
        usize::from(self.0)
    }
}

/// One measured speaker position as shown by the application.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(specta::Type))]
pub struct DirectionView {
    pub id: DirectionId,
    pub name: String,
    pub azimuth_deg: f32,
    pub elevation_deg: f32,
}

/// One measured speaker position.
#[derive(Clone, Debug, PartialEq)]
pub struct Direction {
    /// Speaker name as the BRIR set spells it (`FL`, `TFL`, `WL`, ...).
    pub name: String,
    /// Degrees, 0 ahead, positive to the left (see [`Position::from_angles`]).
    pub azimuth_deg: f32,
    /// Degrees, positive upwards.
    pub elevation_deg: f32,
}

impl Direction {
    pub fn new(name: impl Into<String>, azimuth_deg: f32, elevation_deg: f32) -> Self {
        Self {
            name: name.into(),
            azimuth_deg,
            elevation_deg,
        }
    }

    pub fn position(&self) -> Position {
        Position::from_angles(self.azimuth_deg, self.elevation_deg)
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum LayoutError {
    #[error("a layout needs at least one direction")]
    Empty,
    #[error("direction {0} appears more than once")]
    DuplicateName(String),
    #[error("direction {name} has a non-finite angle")]
    BadAngle { name: String },
    #[error("a layout holds at most {max} directions, got {got}")]
    TooMany { max: usize, got: usize },
}

/// The directions of one BRIR set, in feed order.
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    directions: Vec<Direction>,
}

impl Layout {
    /// Upper bound on directions; feeds and the convolver size their buffers
    /// from the layout, so this keeps a malformed set from asking for gigabytes.
    pub const MAX_DIRECTIONS: usize = 64;

    pub fn new(directions: Vec<Direction>) -> Result<Self, LayoutError> {
        if directions.is_empty() {
            return Err(LayoutError::Empty);
        }
        if directions.len() > Self::MAX_DIRECTIONS {
            return Err(LayoutError::TooMany {
                max: Self::MAX_DIRECTIONS,
                got: directions.len(),
            });
        }
        for (i, d) in directions.iter().enumerate() {
            if !d.azimuth_deg.is_finite() || !d.elevation_deg.is_finite() {
                return Err(LayoutError::BadAngle {
                    name: d.name.clone(),
                });
            }
            if directions[..i].iter().any(|e| e.name == d.name) {
                return Err(LayoutError::DuplicateName(d.name.clone()));
            }
        }
        Ok(Self { directions })
    }

    pub fn len(&self) -> usize {
        self.directions.len()
    }

    /// Always false: [`Layout::new`] rejects an empty list.
    pub fn is_empty(&self) -> bool {
        self.directions.is_empty()
    }

    pub fn directions(&self) -> &[Direction] {
        &self.directions
    }

    pub fn get(&self, id: DirectionId) -> Option<&Direction> {
        self.directions.get(id.index())
    }

    pub fn find(&self, name: &str) -> Option<DirectionId> {
        self.directions
            .iter()
            .position(|d| d.name == name)
            .map(|i| DirectionId(i as u16))
    }

    pub fn ids(&self) -> impl Iterator<Item = DirectionId> + '_ {
        (0..self.directions.len()).map(|i| DirectionId(i as u16))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_rejects_duplicates_empty_and_nan() {
        assert_eq!(Layout::new(vec![]), Err(LayoutError::Empty));
        let dup = vec![
            Direction::new("FL", 30.0, 0.0),
            Direction::new("FL", 30.0, 0.0),
        ];
        assert_eq!(
            Layout::new(dup),
            Err(LayoutError::DuplicateName("FL".into()))
        );
        let nan = vec![Direction::new("FL", f32::NAN, 0.0)];
        assert!(matches!(
            Layout::new(nan),
            Err(LayoutError::BadAngle { .. })
        ));
    }

    #[test]
    fn layout_keeps_feed_order() {
        let layout = Layout::new(vec![
            Direction::new("FL", 30.0, 0.0),
            Direction::new("FR", -30.0, 0.0),
            Direction::new("FC", 0.0, 0.0),
        ])
        .unwrap();
        assert_eq!(layout.find("FC"), Some(DirectionId(2)));
        assert_eq!(layout.get(DirectionId(1)).unwrap().name, "FR");
        assert_eq!(layout.ids().count(), 3);
    }
}
