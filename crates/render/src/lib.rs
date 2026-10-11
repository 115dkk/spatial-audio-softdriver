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
//! One adapter (VBAP), so no trait (ADR 0010). Object spread is ignored for
//! now. Construction allocates; [`Panner::gains`], [`Panner::render`], and
//! [`Panner::reset`] do not.

use bio_types::{DirectionId, Feeds, Layout, MAX_OBJECTS, Position, SceneBlock};

const GAIN_EPSILON: f32 = 1.0e-6;
const HULL_SIDE_EPSILON: f32 = 2.0e-5;
const HULL_VOLUME_EPSILON: f32 = 1.0e-6;

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

#[derive(Clone, Copy, Debug)]
struct Pair {
    directions: [usize; 2],
    inverse: [[f32; 2]; 2],
}

#[derive(Clone, Copy, Debug)]
enum Vertex {
    Real(usize),
    Virtual(usize),
}

#[derive(Clone, Copy, Debug)]
struct HullPoint {
    vector: [f32; 3],
    vertex: Vertex,
}

#[derive(Debug)]
struct HullFace {
    point_indices: Vec<usize>,
    outward_normal: [f32; 3],
}

#[derive(Clone, Copy, Debug)]
struct Triangle {
    vertices: [Vertex; 3],
    inverse: [[f32; 3]; 3],
}

#[derive(Debug)]
struct VirtualDirection {
    real_neighbours: Vec<usize>,
}

#[derive(Debug)]
enum Geometry {
    OneDirection,
    Pairwise(Vec<Pair>),
    ThreeDimensional {
        triangles: Vec<Triangle>,
        virtual_directions: Vec<VirtualDirection>,
    },
}

#[derive(Debug)]
pub struct Panner {
    real_vectors: Vec<[f32; 3]>,
    geometry: Geometry,
    config: PannerConfig,
    gain_cache: Vec<f32>,
    target_gains: Vec<f32>,
    previous_object_count: usize,
}

impl Panner {
    pub fn new(layout: &Layout, config: PannerConfig) -> Result<Self, RenderError> {
        if let Some((route, _)) = config.lfe_route
            && route.index() >= layout.len()
        {
            return Err(RenderError::BadLfeRoute(route.0));
        }

        let real_vectors: Vec<_> = layout
            .directions()
            .iter()
            .map(|direction| {
                direction
                    .position()
                    .direction()
                    .expect("a direction made from finite angles is non-zero")
            })
            .collect();
        for (index, &vector) in real_vectors.iter().enumerate() {
            if real_vectors[..index]
                .iter()
                .any(|&earlier| squared_distance(earlier, vector) <= GAIN_EPSILON.powi(2))
            {
                return Err(RenderError::DegenerateLayout(
                    "two measured directions occupy the same position".into(),
                ));
            }
        }

        let is_horizontal = layout
            .directions()
            .iter()
            .all(|direction| direction.elevation_deg.abs() <= 5.0);

        let geometry = if real_vectors.len() == 1 {
            Geometry::OneDirection
        } else if is_horizontal {
            build_horizontal_geometry(&real_vectors)?
        } else {
            if !spans_three_dimensions(&real_vectors) {
                return Err(RenderError::DegenerateLayout(
                    "the real directions are coplanar through the listener".into(),
                ));
            }
            let add_nadir = !layout
                .directions()
                .iter()
                .any(|direction| direction.elevation_deg < -10.0);
            let zenith = [0.0, 0.0, 1.0];
            let add_zenith = !layout
                .directions()
                .iter()
                .any(|direction| direction.elevation_deg > 60.0)
                && !point_is_in_hull(zenith, &real_vectors);
            build_three_dimensional_geometry(&real_vectors, add_nadir, add_zenith)?
        };

        let directions = real_vectors.len();
        Ok(Self {
            real_vectors,
            geometry,
            config,
            gain_cache: vec![0.0; MAX_OBJECTS * directions],
            target_gains: vec![0.0; directions],
            previous_object_count: 0,
        })
    }

    pub fn directions(&self) -> usize {
        self.real_vectors.len()
    }

    /// Writes the gain of every direction for an object at `position` into
    /// `out` (`out.len() == directions()`). The squares sum to 1. A position on
    /// a measured direction gives that direction 1 and the rest 0. The centre
    /// of the room, which has no direction, spreads equal power to all.
    pub fn gains(&self, position: Position, out: &mut [f32]) {
        compute_gains(&self.real_vectors, &self.geometry, position, out);
    }

    /// Clears `feeds`, sets its length to `scene.frames` and adds every object
    /// (signal x object gain x panning gain, ramped from the previous block's
    /// gains for the same object index) and the routed LFE. Never allocates.
    /// Object spread is currently ignored.
    ///
    /// # Panics
    ///
    /// Panics when the scene is inconsistent, contains more than
    /// [`MAX_OBJECTS`], or `feeds` has the wrong direction count or too little
    /// frame capacity.
    pub fn render(&mut self, scene: &SceneBlock<'_>, feeds: &mut Feeds) {
        assert!(
            scene.is_consistent(),
            "scene signal lengths must match frames"
        );
        assert!(
            scene.objects.len() <= MAX_OBJECTS,
            "scene has more than {MAX_OBJECTS} objects"
        );
        assert_eq!(
            feeds.directions(),
            self.directions(),
            "feed direction count does not match the panner"
        );

        feeds.set_frames(scene.frames);
        feeds.clear();

        let ramp_frames = self.config.ramp_frames.min(scene.frames);
        for (object_index, object) in scene.objects.iter().enumerate() {
            compute_gains(
                &self.real_vectors,
                &self.geometry,
                object.position,
                &mut self.target_gains,
            );
            for gain in &mut self.target_gains {
                *gain *= object.gain;
            }

            let row_start = object_index * self.directions();
            let row_end = row_start + self.directions();
            let cached = &mut self.gain_cache[row_start..row_end];
            let should_ramp = object_index < self.previous_object_count
                && ramp_frames != 0
                && cached
                    .iter()
                    .zip(&self.target_gains)
                    .any(|(old, new)| old != new);

            let mut active_directions = [0_usize; Layout::MAX_DIRECTIONS];
            let mut active_count = 0;
            for (direction, (&old, &target)) in cached.iter().zip(&self.target_gains).enumerate() {
                if target != 0.0 || (should_ramp && old != 0.0) {
                    active_directions[active_count] = direction;
                    active_count += 1;
                }
            }

            for &direction in &active_directions[..active_count] {
                let old = cached[direction];
                let target = self.target_gains[direction];
                let channel = feeds.channel_mut(direction);
                if should_ramp {
                    let step = (target - old) / ramp_frames as f32;
                    for (frame, (&sample, output)) in object.signal.iter().zip(channel).enumerate()
                    {
                        let gain = if frame < ramp_frames {
                            old + step * (frame + 1) as f32
                        } else {
                            target
                        };
                        *output += sample * gain;
                    }
                } else {
                    for (&sample, output) in object.signal.iter().zip(channel) {
                        *output += sample * target;
                    }
                }
            }

            cached.copy_from_slice(&self.target_gains);
        }
        self.previous_object_count = scene.objects.len();

        if let (Some((route, route_gain)), Some(lfe)) = (self.config.lfe_route, scene.lfe) {
            for (&sample, output) in lfe.iter().zip(feeds.channel_mut(route.index())) {
                *output += sample * route_gain;
            }
        }
    }

    /// Forgets the previous gains, so the next block starts without a ramp.
    /// Never allocates.
    pub fn reset(&mut self) {
        self.gain_cache.fill(0.0);
        self.target_gains.fill(0.0);
        self.previous_object_count = 0;
    }
}

fn build_horizontal_geometry(real_vectors: &[[f32; 3]]) -> Result<Geometry, RenderError> {
    if real_vectors.len() == 1 {
        return Ok(Geometry::OneDirection);
    }

    let mut order: Vec<_> = (0..real_vectors.len()).collect();
    order.sort_by(|&left, &right| {
        horizontal_angle(real_vectors[left]).total_cmp(&horizontal_angle(real_vectors[right]))
    });

    let mut pairs = Vec::with_capacity(order.len());
    for pair_index in 0..order.len() {
        let left = order[pair_index];
        let right = order[(pair_index + 1) % order.len()];
        let a = horizontal_unit(real_vectors[left]);
        let b = horizontal_unit(real_vectors[right]);
        let determinant = a[0] * b[1] - b[0] * a[1];
        if determinant.abs() <= GAIN_EPSILON {
            continue;
        }
        pairs.push(Pair {
            directions: [left, right],
            inverse: [
                [b[1] / determinant, -b[0] / determinant],
                [-a[1] / determinant, a[0] / determinant],
            ],
        });
    }

    if pairs.is_empty() {
        return Err(RenderError::DegenerateLayout(
            "the horizontal directions do not form a usable pair".into(),
        ));
    }
    Ok(Geometry::Pairwise(pairs))
}

fn build_three_dimensional_geometry(
    real_vectors: &[[f32; 3]],
    add_nadir: bool,
    add_zenith: bool,
) -> Result<Geometry, RenderError> {
    let mut points =
        Vec::with_capacity(real_vectors.len() + usize::from(add_nadir) + usize::from(add_zenith));
    points.extend(
        real_vectors
            .iter()
            .copied()
            .enumerate()
            .map(|(index, vector)| HullPoint {
                vector,
                vertex: Vertex::Real(index),
            }),
    );

    let mut virtual_directions = Vec::with_capacity(2);
    if add_nadir {
        let index = virtual_directions.len();
        virtual_directions.push(VirtualDirection {
            real_neighbours: Vec::new(),
        });
        points.push(HullPoint {
            vector: [0.0, 0.0, -1.0],
            vertex: Vertex::Virtual(index),
        });
    }
    if add_zenith {
        let index = virtual_directions.len();
        virtual_directions.push(VirtualDirection {
            real_neighbours: Vec::new(),
        });
        points.push(HullPoint {
            vector: [0.0, 0.0, 1.0],
            vertex: Vertex::Virtual(index),
        });
    }

    if !has_three_dimensional_volume(&points) {
        return Err(RenderError::DegenerateLayout(
            "the directions are coplanar or coincident".into(),
        ));
    }

    let faces = find_hull_faces(&points)?;
    let mut triangles = Vec::new();
    for face in &faces {
        let perimeter = hull_face_perimeter(face, &points)?;
        collect_virtual_neighbours(&perimeter, &points, &mut virtual_directions);
        triangulate_hull_face(&perimeter, &points, &mut triangles)?;
    }

    if virtual_directions
        .iter()
        .any(|direction| direction.real_neighbours.is_empty())
    {
        return Err(RenderError::DegenerateLayout(
            "a virtual direction has no real hull neighbour".into(),
        ));
    }

    Ok(Geometry::ThreeDimensional {
        triangles,
        virtual_directions,
    })
}

fn find_hull_faces(points: &[HullPoint]) -> Result<Vec<HullFace>, RenderError> {
    let mut faces = Vec::new();
    for first in 0..points.len() - 2 {
        for second in first + 1..points.len() - 1 {
            for third in second + 1..points.len() {
                let a = points[first].vector;
                let b = points[second].vector;
                let c = points[third].vector;
                let raw_normal = cross(subtract(b, a), subtract(c, a));
                let normal_length = length(raw_normal);
                if normal_length <= HULL_VOLUME_EPSILON {
                    continue;
                }
                let mut normal = scale(raw_normal, normal_length.recip());

                let mut has_positive = false;
                let mut has_negative = false;
                for point in points {
                    let side = dot(normal, subtract(point.vector, a));
                    has_positive |= side > HULL_SIDE_EPSILON;
                    has_negative |= side < -HULL_SIDE_EPSILON;
                }
                if has_positive && has_negative || !has_positive && !has_negative {
                    continue;
                }
                if has_positive {
                    normal = scale(normal, -1.0);
                }

                let plane_offset = dot(normal, a);
                if plane_offset <= HULL_SIDE_EPSILON {
                    return Err(RenderError::DegenerateLayout(
                        "the direction hull does not enclose the listener".into(),
                    ));
                }

                let mut point_indices = Vec::new();
                for (index, point) in points.iter().enumerate() {
                    if dot(normal, subtract(point.vector, a)).abs() <= HULL_SIDE_EPSILON {
                        point_indices.push(index);
                    }
                }
                point_indices.sort_unstable();
                if faces
                    .iter()
                    .any(|face: &HullFace| face.point_indices == point_indices)
                {
                    continue;
                }
                faces.push(HullFace {
                    point_indices,
                    outward_normal: normal,
                });
            }
        }
    }

    if faces.is_empty() {
        return Err(RenderError::DegenerateLayout(
            "the directions have no non-degenerate hull faces".into(),
        ));
    }
    Ok(faces)
}

fn hull_face_perimeter(face: &HullFace, points: &[HullPoint]) -> Result<Vec<usize>, RenderError> {
    if face.point_indices.len() < 3 {
        return Err(RenderError::DegenerateLayout(
            "a hull face has fewer than three corners".into(),
        ));
    }

    let mut centroid = [0.0; 3];
    for &index in &face.point_indices {
        centroid = add(centroid, points[index].vector);
    }
    centroid = scale(centroid, (face.point_indices.len() as f32).recip());

    let reference = if face.outward_normal[2].abs() < 0.9 {
        [0.0, 0.0, 1.0]
    } else {
        [0.0, 1.0, 0.0]
    };
    let axis_u = normalise(cross(reference, face.outward_normal)).ok_or_else(|| {
        RenderError::DegenerateLayout("a hull face has no stable projection axis".into())
    })?;
    let axis_v = cross(face.outward_normal, axis_u);

    let mut perimeter = face.point_indices.clone();
    perimeter.sort_by(|&left, &right| {
        let left_offset = subtract(points[left].vector, centroid);
        let right_offset = subtract(points[right].vector, centroid);
        let left_angle = dot(left_offset, axis_v).atan2(dot(left_offset, axis_u));
        let right_angle = dot(right_offset, axis_v).atan2(dot(right_offset, axis_u));
        left_angle.total_cmp(&right_angle)
    });
    Ok(perimeter)
}

fn collect_virtual_neighbours(
    perimeter: &[usize],
    points: &[HullPoint],
    virtual_directions: &mut [VirtualDirection],
) {
    for edge in 0..perimeter.len() {
        let left = points[perimeter[edge]].vertex;
        let right = points[perimeter[(edge + 1) % perimeter.len()]].vertex;
        match (left, right) {
            (Vertex::Virtual(virtual_index), Vertex::Real(real_index))
            | (Vertex::Real(real_index), Vertex::Virtual(virtual_index)) => {
                let neighbours = &mut virtual_directions[virtual_index].real_neighbours;
                if !neighbours.contains(&real_index) {
                    neighbours.push(real_index);
                }
            }
            (Vertex::Real(_), Vertex::Real(_)) | (Vertex::Virtual(_), Vertex::Virtual(_)) => {}
        }
    }
}

fn triangulate_hull_face(
    perimeter: &[usize],
    points: &[HullPoint],
    triangles: &mut Vec<Triangle>,
) -> Result<(), RenderError> {
    let anchor = perimeter[0];
    for edge in 1..perimeter.len() - 1 {
        let indices = [anchor, perimeter[edge], perimeter[edge + 1]];
        let a = points[indices[0]].vector;
        let b = points[indices[1]].vector;
        let c = points[indices[2]].vector;
        let Some(inverse) = inverse_columns(a, b, c) else {
            return Err(RenderError::DegenerateLayout(
                "a hull face could not be triangulated".into(),
            ));
        };
        triangles.push(Triangle {
            vertices: [
                points[indices[0]].vertex,
                points[indices[1]].vertex,
                points[indices[2]].vertex,
            ],
            inverse,
        });
    }
    Ok(())
}

fn compute_gains(
    real_vectors: &[[f32; 3]],
    geometry: &Geometry,
    position: Position,
    out: &mut [f32],
) {
    assert_eq!(
        out.len(),
        real_vectors.len(),
        "gain output has wrong length"
    );
    out.fill(0.0);

    let Some(direction) = position.direction() else {
        fill_equal_power(out);
        return;
    };

    if let Some(index) = real_vectors
        .iter()
        .position(|&measured| squared_distance(measured, direction) <= GAIN_EPSILON.powi(2))
    {
        out[index] = 1.0;
        return;
    }

    match geometry {
        Geometry::OneDirection => out[0] = 1.0,
        Geometry::Pairwise(pairs) => compute_pairwise_gains(direction, pairs, real_vectors, out),
        Geometry::ThreeDimensional {
            triangles,
            virtual_directions,
        } => compute_three_dimensional_gains(
            direction,
            triangles,
            virtual_directions,
            real_vectors,
            out,
        ),
    }
}

fn compute_pairwise_gains(
    direction: [f32; 3],
    pairs: &[Pair],
    real_vectors: &[[f32; 3]],
    out: &mut [f32],
) {
    let horizontal_length = direction[0].hypot(direction[1]);
    if horizontal_length <= GAIN_EPSILON {
        fill_equal_power(out);
        return;
    }
    let projected = [
        direction[0] / horizontal_length,
        direction[1] / horizontal_length,
    ];

    for pair in pairs {
        let gains = [
            pair.inverse[0][0] * projected[0] + pair.inverse[0][1] * projected[1],
            pair.inverse[1][0] * projected[0] + pair.inverse[1][1] * projected[1],
        ];
        if gains.iter().all(|gain| *gain >= -GAIN_EPSILON) {
            out[pair.directions[0]] = gains[0].max(0.0);
            out[pair.directions[1]] = gains[1].max(0.0);
            if normalise_power(out) {
                return;
            }
            out.fill(0.0);
        }
    }

    let nearest = nearest_horizontal_direction(projected, real_vectors);
    out[nearest] = 1.0;
}

fn compute_three_dimensional_gains(
    direction: [f32; 3],
    triangles: &[Triangle],
    virtual_directions: &[VirtualDirection],
    real_vectors: &[[f32; 3]],
    out: &mut [f32],
) {
    for triangle in triangles {
        let gains = multiply_matrix_vector(triangle.inverse, direction);
        if !gains.iter().all(|gain| *gain >= -GAIN_EPSILON) {
            continue;
        }

        for (&vertex, gain) in triangle.vertices.iter().zip(gains) {
            let gain = gain.max(0.0);
            match vertex {
                Vertex::Real(index) => out[index] += gain,
                Vertex::Virtual(index) => {
                    let neighbours = &virtual_directions[index].real_neighbours;
                    let shared_gain = gain / (neighbours.len() as f32).sqrt();
                    for &neighbour in neighbours {
                        out[neighbour] += shared_gain;
                    }
                }
            }
        }
        if normalise_power(out) {
            return;
        }
        out.fill(0.0);
    }

    let nearest = nearest_direction(direction, real_vectors);
    out[nearest] = 1.0;
}

fn fill_equal_power(out: &mut [f32]) {
    let gain = (out.len() as f32).sqrt().recip();
    out.fill(gain);
}

fn normalise_power(gains: &mut [f32]) -> bool {
    let power: f32 = gains.iter().map(|gain| gain * gain).sum();
    if !power.is_finite() || power <= f32::EPSILON {
        return false;
    }
    let scale = power.sqrt().recip();
    for gain in gains {
        *gain *= scale;
    }
    true
}

fn nearest_direction(direction: [f32; 3], real_vectors: &[[f32; 3]]) -> usize {
    real_vectors
        .iter()
        .enumerate()
        .max_by(|(_, left), (_, right)| dot(**left, direction).total_cmp(&dot(**right, direction)))
        .map_or(0, |(index, _)| index)
}

fn nearest_horizontal_direction(projected: [f32; 2], real_vectors: &[[f32; 3]]) -> usize {
    real_vectors
        .iter()
        .enumerate()
        .max_by(|(_, left), (_, right)| {
            let left = horizontal_unit(**left);
            let right = horizontal_unit(**right);
            let left_dot = left[0] * projected[0] + left[1] * projected[1];
            let right_dot = right[0] * projected[0] + right[1] * projected[1];
            left_dot.total_cmp(&right_dot)
        })
        .map_or(0, |(index, _)| index)
}

fn horizontal_angle(vector: [f32; 3]) -> f32 {
    vector[1].atan2(vector[0])
}

fn horizontal_unit(vector: [f32; 3]) -> [f32; 2] {
    let horizontal_length = vector[0].hypot(vector[1]);
    [vector[0] / horizontal_length, vector[1] / horizontal_length]
}

fn point_is_in_hull(point: [f32; 3], real_vectors: &[[f32; 3]]) -> bool {
    if real_vectors
        .iter()
        .any(|&vector| squared_distance(vector, point) <= GAIN_EPSILON.powi(2))
    {
        return true;
    }

    for first in 0..real_vectors.len().saturating_sub(2) {
        for second in first + 1..real_vectors.len().saturating_sub(1) {
            for third in second + 1..real_vectors.len() {
                let Some(inverse) = inverse_columns(
                    real_vectors[first],
                    real_vectors[second],
                    real_vectors[third],
                ) else {
                    continue;
                };
                let gains = multiply_matrix_vector(inverse, point);
                if gains.iter().all(|gain| *gain >= -GAIN_EPSILON)
                    && (gains.iter().sum::<f32>() - 1.0).abs() <= GAIN_EPSILON
                {
                    return true;
                }
            }
        }
    }
    false
}

fn spans_three_dimensions(vectors: &[[f32; 3]]) -> bool {
    for first in 0..vectors.len().saturating_sub(2) {
        for second in first + 1..vectors.len().saturating_sub(1) {
            for third in second + 1..vectors.len() {
                if dot(vectors[first], cross(vectors[second], vectors[third])).abs()
                    > HULL_VOLUME_EPSILON
                {
                    return true;
                }
            }
        }
    }
    false
}

fn has_three_dimensional_volume(points: &[HullPoint]) -> bool {
    let origin = points[0].vector;
    for first in 1..points.len() - 2 {
        let a = subtract(points[first].vector, origin);
        for second in first + 1..points.len() - 1 {
            let b = subtract(points[second].vector, origin);
            let normal = cross(a, b);
            for point in &points[second + 1..] {
                let c = subtract(point.vector, origin);
                if dot(normal, c).abs() > HULL_VOLUME_EPSILON {
                    return true;
                }
            }
        }
    }
    false
}

fn inverse_columns(a: [f32; 3], b: [f32; 3], c: [f32; 3]) -> Option<[[f32; 3]; 3]> {
    let b_cross_c = cross(b, c);
    let determinant = dot(a, b_cross_c);
    if determinant.abs() <= HULL_VOLUME_EPSILON {
        return None;
    }
    let reciprocal = determinant.recip();
    Some([
        scale(b_cross_c, reciprocal),
        scale(cross(c, a), reciprocal),
        scale(cross(a, b), reciprocal),
    ])
}

fn multiply_matrix_vector(matrix: [[f32; 3]; 3], vector: [f32; 3]) -> [f32; 3] {
    [
        dot(matrix[0], vector),
        dot(matrix[1], vector),
        dot(matrix[2], vector),
    ]
}

fn dot(left: [f32; 3], right: [f32; 3]) -> f32 {
    left[0] * right[0] + left[1] * right[1] + left[2] * right[2]
}

fn cross(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [
        left[1] * right[2] - left[2] * right[1],
        left[2] * right[0] - left[0] * right[2],
        left[0] * right[1] - left[1] * right[0],
    ]
}

fn subtract(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [left[0] - right[0], left[1] - right[1], left[2] - right[2]]
}

fn add(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [left[0] + right[0], left[1] + right[1], left[2] + right[2]]
}

fn scale(vector: [f32; 3], scalar: f32) -> [f32; 3] {
    [vector[0] * scalar, vector[1] * scalar, vector[2] * scalar]
}

fn length(vector: [f32; 3]) -> f32 {
    dot(vector, vector).sqrt()
}

fn normalise(vector: [f32; 3]) -> Option<[f32; 3]> {
    let vector_length = length(vector);
    (vector_length > HULL_VOLUME_EPSILON).then(|| scale(vector, vector_length.recip()))
}

fn squared_distance(left: [f32; 3], right: [f32; 3]) -> f32 {
    let difference = subtract(left, right);
    dot(difference, difference)
}
