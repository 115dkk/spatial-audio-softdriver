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
//! horizontal plane.
//!
//! One adapter (VBAP), so no trait (ADR 0010). Object spread is rendered by
//! MDAP rings around the object direction. Geometry construction allocates;
//! [`PanGeometry::gains`] does not.

use ssr_types::{Layout, Position};

const GAIN_EPSILON: f32 = 1.0e-6;
const HULL_SIDE_EPSILON: f32 = 2.0e-5;
const HULL_VOLUME_EPSILON: f32 = 1.0e-6;

/// A linear gain ramp from `from` to `to` over `frames` steps. Step `k` is
/// `from + (to - from) * k / frames`, evaluated left to right as written, so
/// every caller rounds the same way.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ramp {
    pub from: f32,
    pub to: f32,
    pub frames: usize,
}

impl Ramp {
    #[inline]
    fn step(&self, step: usize) -> f32 {
        self.from + (self.to - self.from) * step as f32 / self.frames as f32
    }

    /// Gain of ramp frame `index` when the first frame is already one step
    /// past `from`: step `index + 1` while `index < frames`, then `to`. A
    /// zero-length ramp is `to` everywhere.
    #[inline]
    pub fn gain(&self, index: usize) -> f32 {
        if index < self.frames {
            self.step(index + 1)
        } else {
            self.to
        }
    }

    /// Gain of ramp frame `frame` when the first frame plays `from`: step
    /// `frame` while `frame < frames`, then `to`. A zero-length ramp is `to`
    /// everywhere.
    #[inline]
    pub fn gain_with_start(&self, frame: usize) -> f32 {
        if frame < self.frames {
            self.step(frame)
        } else {
            self.to
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
pub struct PanGeometry {
    real_vectors: Vec<[f32; 3]>,
    geometry: Geometry,
}

impl PanGeometry {
    pub fn new(layout: &Layout) -> Result<Self, RenderError> {
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
        Ok(Self {
            real_vectors,
            geometry,
        })
    }

    pub fn directions(&self) -> usize {
        self.real_vectors.len()
    }

    pub fn direction_position(&self, index: usize) -> Position {
        let [x, y, z] = self.real_vectors[index];
        Position::new(x, y, z)
    }

    /// Writes power-normalised direction gains without allocating.
    pub fn gains(&self, position: Position, spread: f32, out: &mut [f32]) {
        compute_spread_gains(&self.real_vectors, &self.geometry, position, spread, out);
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

    let faces = find_hull_faces(&points)?;
    let mut triangles = Vec::new();
    for face in &faces {
        let perimeter = hull_face_perimeter(face, &points)?;
        collect_virtual_neighbours(&perimeter, &points, &mut virtual_directions);
        triangulate_hull_face(&perimeter, &points, &mut triangles)?;
    }
    for direction in &mut virtual_directions {
        direction.real_neighbours.sort_unstable();
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

/// Every plane through three directions that has all directions on one side is
/// a hull face. At most 66 points (64 directions and two virtual poles) make
/// this a few million plane tests, run once when the geometry is prepared on
/// the control thread; a plane is dropped as soon as points lie on both sides.
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
                    // Points on both sides already rule the plane out; the
                    // rest of the scan cannot change that decision.
                    if has_positive && has_negative {
                        break;
                    }
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

fn compute_spread_gains(
    real_vectors: &[[f32; 3]],
    geometry: &Geometry,
    position: Position,
    spread: f32,
    out: &mut [f32],
) {
    let spread = if spread.is_finite() {
        spread.clamp(0.0, 1.0)
    } else {
        0.0
    };
    if spread == 0.0 {
        compute_gains(real_vectors, geometry, position, out);
        return;
    }
    assert_eq!(
        out.len(),
        real_vectors.len(),
        "gain output has wrong length"
    );
    let Some(axis) = position.direction() else {
        out.fill(0.0);
        fill_equal_power(out);
        return;
    };
    if matches!(geometry, Geometry::OneDirection) {
        out.fill(0.0);
        out[0] = 1.0;
        return;
    }
    if matches!(geometry, Geometry::Pairwise(_)) && axis[0].hypot(axis[1]) <= GAIN_EPSILON {
        out.fill(0.0);
        fill_equal_power(out);
        return;
    }

    let mut powers = [0.0_f32; Layout::MAX_DIRECTIONS];
    let mut sample_gains = [0.0_f32; Layout::MAX_DIRECTIONS];
    let mut total_weight = 0.0;
    accumulate_spread_sample(
        real_vectors,
        geometry,
        axis,
        1.0,
        &mut powers,
        &mut sample_gains,
        &mut total_weight,
    );

    let theta = spread * std::f32::consts::PI;
    match geometry {
        Geometry::Pairwise(_) => {
            let Some(horizontal) = normalise([axis[0], axis[1], 0.0]) else {
                out.fill(0.0);
                fill_equal_power(out);
                return;
            };
            for ring in 1..=3 {
                let angle = theta * ring as f32 / 3.0;
                let (sine, cosine) = angle.sin_cos();
                for sign in [-1.0, 1.0] {
                    let sample = [
                        horizontal[0] * cosine - horizontal[1] * sine * sign,
                        horizontal[0] * sine * sign + horizontal[1] * cosine,
                        0.0,
                    ];
                    accumulate_spread_sample(
                        real_vectors,
                        geometry,
                        sample,
                        1.0,
                        &mut powers,
                        &mut sample_gains,
                        &mut total_weight,
                    );
                }
            }
        }
        Geometry::ThreeDimensional { .. } => {
            let (basis_u, basis_v) = ring_basis(axis);
            for ring in 1..=3 {
                let angle = theta * ring as f32 / 3.0;
                let weight = angle.sin().max(0.0);
                let points = 6 * ring;
                let phase = std::f32::consts::PI / points as f32;
                let (ring_sine, ring_cosine) = angle.sin_cos();
                for point in 0..points {
                    let azimuth = std::f32::consts::TAU * point as f32 / points as f32 + phase;
                    let (sine, cosine) = azimuth.sin_cos();
                    let tangent = add(scale(basis_u, cosine), scale(basis_v, sine));
                    let sample = add(scale(axis, ring_cosine), scale(tangent, ring_sine));
                    accumulate_spread_sample(
                        real_vectors,
                        geometry,
                        sample,
                        weight,
                        &mut powers,
                        &mut sample_gains,
                        &mut total_weight,
                    );
                }
            }
        }
        Geometry::OneDirection => unreachable!(),
    }

    for (gain, power) in out.iter_mut().zip(powers) {
        *gain = (power / total_weight).sqrt();
    }
    let _ = normalise_power(out);
}

fn accumulate_spread_sample(
    real_vectors: &[[f32; 3]],
    geometry: &Geometry,
    direction: [f32; 3],
    weight: f32,
    powers: &mut [f32; Layout::MAX_DIRECTIONS],
    sample_gains: &mut [f32; Layout::MAX_DIRECTIONS],
    total_weight: &mut f32,
) {
    if weight == 0.0 {
        return;
    }
    compute_gains(
        real_vectors,
        geometry,
        Position::new(direction[0], direction[1], direction[2]),
        &mut sample_gains[..real_vectors.len()],
    );
    for (power, gain) in powers.iter_mut().zip(sample_gains.iter()) {
        *power += weight * gain * gain;
    }
    *total_weight += weight;
}

fn ring_basis(axis: [f32; 3]) -> ([f32; 3], [f32; 3]) {
    // Rodrigues' minimal rotation maps +Z to the object axis and rotates both
    // tangent axes together. Unlike an azimuth-derived tangent this has one
    // stable limit at the zenith. The antipodal rotation is the sole ambiguity.
    let dot_from_z = axis[2];
    if dot_from_z <= -1.0 + 1.0e-9 {
        return ([1.0, 0.0, 0.0], [0.0, -1.0, 0.0]);
    }
    let denominator = 1.0 + dot_from_z;
    let basis_u = [
        1.0 - axis[0] * axis[0] / denominator,
        -axis[0] * axis[1] / denominator,
        -axis[0],
    ];
    let basis_v = [
        -axis[0] * axis[1] / denominator,
        1.0 - axis[1] * axis[1] / denominator,
        -axis[1],
    ];
    (basis_u, basis_v)
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

#[cfg(test)]
mod hull_reference_tests {
    use super::*;

    // Frozen point-VBAP entry from the pre-spread renderer. Keep this module
    // independent of `compute_spread_gains`: spread zero must continue to
    // dispatch to the legacy point algorithm bit for bit.
    mod legacy_point_reference {
        use super::*;

        pub(super) fn gains(geometry: &PanGeometry, position: Position, out: &mut [f32]) {
            assert_eq!(out.len(), geometry.real_vectors.len());
            out.fill(0.0);

            let Some(direction) = position.direction() else {
                fill_equal_power(out);
                return;
            };
            if let Some(index) = geometry
                .real_vectors
                .iter()
                .position(|&measured| squared_distance(measured, direction) <= GAIN_EPSILON.powi(2))
            {
                out[index] = 1.0;
                return;
            }

            match &geometry.geometry {
                Geometry::OneDirection => out[0] = 1.0,
                Geometry::Pairwise(pairs) => {
                    pairwise(direction, pairs, &geometry.real_vectors, out)
                }
                Geometry::ThreeDimensional {
                    triangles,
                    virtual_directions,
                } => three_dimensional(
                    direction,
                    triangles,
                    virtual_directions,
                    &geometry.real_vectors,
                    out,
                ),
            }
        }

        fn pairwise(
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
            out[nearest_horizontal_direction(projected, real_vectors)] = 1.0;
        }

        fn three_dimensional(
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
            out[nearest_direction(direction, real_vectors)] = 1.0;
        }
    }

    #[test]
    fn spread_zero_matches_point_vbap_exactly() {
        let layouts = [
            Layout::new(vec![
                ssr_types::Direction::new("FL", 30.0, 0.0),
                ssr_types::Direction::new("FR", -30.0, 0.0),
                ssr_types::Direction::new("FC", 0.0, 0.0),
                ssr_types::Direction::new("SL", 90.0, 0.0),
                ssr_types::Direction::new("SR", -90.0, 0.0),
                ssr_types::Direction::new("BL", 150.0, 0.0),
                ssr_types::Direction::new("BR", -150.0, 0.0),
                ssr_types::Direction::new("TFL", 45.0, 45.0),
                ssr_types::Direction::new("TFR", -45.0, 45.0),
                ssr_types::Direction::new("TBL", 135.0, 45.0),
                ssr_types::Direction::new("TBR", -135.0, 45.0),
            ])
            .unwrap(),
            Layout::new(vec![
                ssr_types::Direction::new("FL", 30.0, 0.0),
                ssr_types::Direction::new("FR", -30.0, 0.0),
                ssr_types::Direction::new("SL", 110.0, 0.0),
                ssr_types::Direction::new("SR", -110.0, 0.0),
            ])
            .unwrap(),
        ];
        let positions = [
            Position::new(0.0, 0.0, 0.0),
            Position::from_angles(13.0, 17.0),
            Position::from_angles(-72.0, 31.0),
            Position::from_angles(169.0, -28.0),
            Position::new(0.0, 0.0, 1.0),
            Position::new(0.0, 0.0, -1.0),
        ];
        for layout in layouts {
            let geometry = PanGeometry::new(&layout).unwrap();
            let mut expected = vec![0.0; layout.len()];
            let mut actual = vec![0.0; layout.len()];
            for position in positions {
                legacy_point_reference::gains(&geometry, position, &mut expected);
                geometry.gains(position, 0.0, &mut actual);
                assert_eq!(actual, expected, "position {position:?}");
            }
        }
    }

    // The hull search as it was before planes were dropped early. It is kept
    // as the reference the search must reproduce: the same faces, in the same
    // order, with the same normals, so every triangle and every gain stays bit
    // for bit what it was.
    fn reference_find_hull_faces(points: &[HullPoint]) -> Result<Vec<HullFace>, RenderError> {
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

    fn vector(azimuth_deg: f32, elevation_deg: f32) -> [f32; 3] {
        let (azimuth, elevation) = (azimuth_deg.to_radians(), elevation_deg.to_radians());
        [
            elevation.cos() * azimuth.cos(),
            elevation.cos() * azimuth.sin(),
            elevation.sin(),
        ]
    }

    fn points(vectors: &[[f32; 3]]) -> Vec<HullPoint> {
        vectors
            .iter()
            .enumerate()
            .map(|(index, &vector)| HullPoint {
                vector,
                vertex: Vertex::Real(index),
            })
            .collect()
    }

    fn assert_same_hull(label: &str, vectors: &[[f32; 3]]) {
        let points = points(vectors);
        let expected = reference_find_hull_faces(&points);
        let actual = find_hull_faces(&points);
        match (expected, actual) {
            (Ok(expected), Ok(actual)) => {
                assert_eq!(expected.len(), actual.len(), "{label}: face count");
                for (index, (expected, actual)) in expected.iter().zip(&actual).enumerate() {
                    assert_eq!(
                        expected.point_indices, actual.point_indices,
                        "{label}: face {index} points"
                    );
                    assert_eq!(
                        expected.outward_normal.map(f32::to_bits),
                        actual.outward_normal.map(f32::to_bits),
                        "{label}: face {index} normal"
                    );
                }
            }
            (Err(_), Err(_)) => {}
            (expected, actual) => panic!("{label}: reference {expected:?}, search {actual:?}"),
        }
    }

    fn angles(list: &[(f32, f32)], nadir: bool, zenith: bool) -> Vec<[f32; 3]> {
        let mut vectors: Vec<_> = list.iter().map(|&(a, e)| vector(a, e)).collect();
        if nadir {
            vectors.push([0.0, 0.0, -1.0]);
        }
        if zenith {
            vectors.push([0.0, 0.0, 1.0]);
        }
        vectors
    }

    #[test]
    fn hull_search_matches_the_brute_force_reference() {
        let seven_one_four = [
            (30.0, 0.0),
            (-30.0, 0.0),
            (0.0, 0.0),
            (90.0, 0.0),
            (-90.0, 0.0),
            (150.0, 0.0),
            (-150.0, 0.0),
            (45.0, 45.0),
            (-45.0, 45.0),
            (135.0, 45.0),
            (-135.0, 45.0),
        ];
        assert_same_hull("7.1.4 + nadir", &angles(&seven_one_four, true, false));
        assert_same_hull(
            "7.1.4 + nadir + zenith",
            &angles(&seven_one_four, true, true),
        );
        let twenty_two = [
            (30.0, 0.0),
            (-30.0, 0.0),
            (0.0, 0.0),
            (90.0, 0.0),
            (-90.0, 0.0),
            (150.0, 0.0),
            (-150.0, 0.0),
            (180.0, 0.0),
            (60.0, 0.0),
            (-60.0, 0.0),
            (45.0, 45.0),
            (-45.0, 45.0),
            (135.0, 45.0),
            (-135.0, 45.0),
            (90.0, 45.0),
            (-90.0, 45.0),
            (0.0, 45.0),
            (180.0, 45.0),
            (22.5, 45.0),
            (-22.5, 45.0),
            (157.5, 45.0),
            (-157.5, 45.0),
        ];
        assert_same_hull("22 + nadir", &angles(&twenty_two, true, false));
        assert_same_hull("22 + nadir + zenith", &angles(&twenty_two, true, true));
        let mut cube = Vec::new();
        for x in [-1.0_f32, 1.0] {
            for y in [-1.0_f32, 1.0] {
                for z in [-1.0_f32, 1.0] {
                    cube.push(scale([x, y, z], 3.0_f32.sqrt().recip()));
                }
            }
        }
        assert_same_hull("cube", &cube);
        let octahedron = [
            [1.0, 0.0, 0.0],
            [-1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, -1.0, 0.0],
            [0.0, 0.0, 1.0],
            [0.0, 0.0, -1.0],
        ];
        assert_same_hull("octahedron", &octahedron);
        let golden = (1.0 + 5.0_f32.sqrt()) / 2.0;
        let mut icosahedron = Vec::new();
        for a in [-1.0_f32, 1.0] {
            for b in [-golden, golden] {
                for v in [[0.0, a, b], [a, b, 0.0], [b, 0.0, a]] {
                    icosahedron.push(scale(v, length(v).recip()));
                }
            }
        }
        assert_same_hull("icosahedron", &icosahedron);
        // Ring layouts at fixed elevations, as speaker sets are built.
        for (rings, per_ring) in [(2, 8), (3, 8), (3, 12), (4, 16)] {
            let mut ring_layout = Vec::new();
            for ring in 0..rings {
                let elevation = -30.0 + 30.0 * ring as f32;
                for step in 0..per_ring {
                    let azimuth = 360.0 * step as f32 / per_ring as f32 - 180.0;
                    ring_layout.push((azimuth, elevation));
                }
            }
            assert_same_hull(
                &format!("rings {rings}x{per_ring}"),
                &angles(&ring_layout, false, true),
            );
        }
        // Seeded uniform points on the sphere, up to the 64-direction limit.
        let mut state = 0x0028_5eed_u64;
        let mut next = || {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 40) as f32 / (1_u64 << 24) as f32
        };
        for case in 0..24 {
            let count = 5 + (case * 59) / 23;
            let vectors: Vec<_> = (0..count)
                .map(|_| {
                    let z = 2.0 * next() - 1.0;
                    let angle = std::f32::consts::TAU * next();
                    let radius = (1.0 - z * z).max(0.0).sqrt();
                    [radius * angle.cos(), radius * angle.sin(), z]
                })
                .collect();
            assert_same_hull(&format!("random {case} ({count} points)"), &vectors);
        }
    }
}

#[cfg(test)]
mod ramp_tests {
    use super::Ramp;

    const ENDS: [(f32, f32); 5] = [
        (0.0, 1.0),
        (1.0, 0.0),
        (-0.0, 1.0),
        (0.25, 0.75),
        (1.0, 1.0),
    ];

    fn written(from: f32, to: f32, step: usize, frames: usize) -> f32 {
        from + (to - from) * step as f32 / frames as f32
    }

    #[test]
    fn gain_is_the_written_step_one_past_the_index_then_the_target() {
        for (from, to) in ENDS {
            for frames in [0, 1, 2, 7, 64, 480] {
                let ramp = Ramp { from, to, frames };
                for index in 0..frames + 3 {
                    let expected = if index < frames {
                        written(from, to, index + 1, frames)
                    } else {
                        to
                    };
                    assert_eq!(
                        ramp.gain(index).to_bits(),
                        expected.to_bits(),
                        "{from}->{to} over {frames}, index {index}"
                    );
                }
            }
        }
    }

    #[test]
    fn gain_with_start_is_the_written_step_at_the_frame_then_the_target() {
        for (from, to) in ENDS {
            for frames in [0, 1, 2, 7, 64, 480] {
                let ramp = Ramp { from, to, frames };
                for frame in 0..frames + 3 {
                    let expected = if frame < frames {
                        written(from, to, frame, frames)
                    } else {
                        to
                    };
                    assert_eq!(
                        ramp.gain_with_start(frame).to_bits(),
                        expected.to_bits(),
                        "{from}->{to} over {frames}, frame {frame}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_ramp_from_negative_zero_starts_at_positive_zero() {
        // The step is evaluated even on the first frame, so a ramp that
        // starts at -0.0 plays +0.0 there, as the gain expressions it
        // replaced did.
        let ramp = Ramp {
            from: -0.0,
            to: 1.0,
            frames: 480,
        };
        assert_eq!(ramp.gain_with_start(0).to_bits(), 0.0_f32.to_bits());
    }

    #[test]
    fn the_last_ramp_frame_is_computed_not_forced_to_the_target() {
        // 0.1 + (1.0 - 0.1) * 3 / 3 rounds to just under 1.0 in f32.
        let ramp = Ramp {
            from: 0.1,
            to: 1.0,
            frames: 3,
        };
        let last = written(0.1, 1.0, 3, 3);
        assert_ne!(last.to_bits(), 1.0_f32.to_bits());
        assert_eq!(ramp.gain(2).to_bits(), last.to_bits());
        assert_eq!(ramp.gain(3).to_bits(), 1.0_f32.to_bits());
    }
}
