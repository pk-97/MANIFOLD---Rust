//! A curve swept into a solid, on the CPU: the mirror of `node.extrude_curve`,
//! `node.revolve_curve` and `node.tube_from_path` followed by
//! `node.make_triangles` with Keep Normals. Each grid function computes the
//! same positions and normals as the sweep's WGSL body, so a collision shape
//! or a copy layout reads the solid the GPU draws.
//!
//! The sweeps write exact normals. Finite differences of the grid flip where
//! a tube is thicker than its path's tightest turn (the inner wall folds
//! through itself), and vanish at a pole or a cap centre. A displacement
//! along flipped normals pulls the fold apart into spikes.
//!
//! Normals keep the orientation finite differences give a grid
//! (`cross(along rows, along columns)`), so a sweep's winding and normals
//! agree as they did before.
//!
//! Caps (Blender's Fill Caps) are four extra grid rows at each end of the
//! sweep: the cap centre, the rim with the cap's normal, then the rim again
//! with the side's normal as the first side row. The doubled rim is a hard
//! edge; the quad between the two rim rows has no area.

use crate::mesh::MeshVertex;

/// How a curve becomes a solid. Counts are the sweep node's params.
#[derive(Clone, Copy, Debug)]
pub enum Sweep {
    /// `node.extrude_curve`: the outline pushed along +Z by `depth`, or
    /// centred on its plane when `symmetric`.
    Extrude { depth: f32, steps: u32, close: bool, symmetric: bool, caps: bool },
    /// `node.revolve_curve`: the profile spun `sweep` radians about +Y.
    Revolve { segments: u32, sweep: f32 },
    /// `node.tube_from_path`: a ring of `radius` around the path (XZ plane,
    /// `lift` along +Y).
    Tube { radius: f32, sides: u32, caps: bool },
}

impl Sweep {
    fn bits(&self) -> [u32; 6] {
        match *self {
            Self::Extrude { depth, steps, close, symmetric, caps } => {
                [0, depth.to_bits(), steps, u32::from(close), u32::from(symmetric), u32::from(caps)]
            }
            Self::Revolve { segments, sweep } => [1, segments, sweep.to_bits(), 0, 0, 0],
            Self::Tube { radius, sides, caps } => [2, radius.to_bits(), sides, u32::from(caps), 0, 0],
        }
    }
}

/// A curve and the sweep that makes it a solid.
#[derive(Clone, Debug)]
pub struct SweptCurve {
    /// The curve the sweep reads: an outline, a profile or a path.
    pub points: Vec<[f32; 2]>,
    /// A tube's rise along +Y per path point; empty is none.
    pub lift: Vec<f32>,
    pub sweep: Sweep,
}

/// Bit for bit, so a curve with a NaN in it is one unchanged solid rather
/// than a new one every frame.
impl PartialEq for SweptCurve {
    fn eq(&self, other: &Self) -> bool {
        self.is(&other.points, &other.lift, other.sweep)
    }
}

impl SweptCurve {
    /// Whether this is the solid `points`, `lift` and `sweep` make, bit for
    /// bit, without building it.
    pub fn is(&self, points: &[[f32; 2]], lift: &[f32], sweep: Sweep) -> bool {
        self.sweep.bits() == sweep.bits()
            && self.points.len() == points.len()
            && self.points.iter().zip(points).all(|(a, b)| a.map(f32::to_bits) == b.map(f32::to_bits))
            && self.lift.len() == lift.len()
            && self.lift.iter().zip(lift).all(|(a, b)| a.to_bits() == b.to_bits())
    }
}

/// A grid of positions and normals, row-major (`row * cols + col`), as a
/// sweep writes it.
pub struct Grid {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub cols: usize,
    pub rows: usize,
}

/// What a grid row is when the sweep has caps.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CapRow {
    Side,
    /// The start cap's centre or rim.
    Start { centre: bool },
    /// The end cap's rim or centre.
    End { centre: bool },
}

/// Grid row `row`'s index along the sweep (`0..=last`) and its cap role.
/// Without caps, every row is a side row.
pub fn capped_row(row: usize, last: usize, caps: bool) -> (usize, CapRow) {
    if !caps {
        return (row, CapRow::Side);
    }
    match row {
        0 => (0, CapRow::Start { centre: true }),
        1 => (0, CapRow::Start { centre: false }),
        r if r <= last + 2 => (r - 2, CapRow::Side),
        r if r == last + 3 => (last, CapRow::End { centre: false }),
        _ => (last, CapRow::End { centre: true }),
    }
}

/// The rows a sweep with `side_rows` rows has, with caps.
pub fn capped_rows(side_rows: usize, caps: bool) -> usize {
    side_rows + if caps { 4 } else { 0 }
}

/// Whether a curve ends where it starts (a circle sampled end to end).
pub fn is_loop(points: &[[f32; 2]]) -> bool {
    let (Some(first), Some(last)) = (points.first(), points.last()) else {
        return false;
    };
    let reach = 1e-5 * (first[0] * first[0] + first[1] * first[1]).sqrt().max(1.0);
    points.len() > 2 && ((last[0] - first[0]).powi(2) + (last[1] - first[1]).powi(2)).sqrt() <= reach
}

/// The curve's direction at point `i`: a central difference, wrapping round
/// a loop (the end point repeats the start, or `close` adds the start again)
/// and one-sided at an open end.
pub fn curve_tangent(points: &[[f32; 2]], i: usize, looped: bool) -> [f32; 2] {
    let len = points.len();
    if len < 2 {
        return [0.0, 0.0];
    }
    let (prev, next) = if looped {
        let distinct = if is_loop(points) { len - 1 } else { len };
        let i = i % distinct;
        ((i + distinct - 1) % distinct, (i + 1) % distinct)
    } else {
        (i.saturating_sub(1), (i + 1).min(len - 1))
    };
    [points[next][0] - points[prev][0], points[next][1] - points[prev][1]]
}

fn signum(v: f32) -> f32 {
    if v < 0.0 { -1.0 } else { 1.0 }
}

fn unit_or(v: [f32; 3], fallback: [f32; 3]) -> [f32; 3] {
    let len = length(v);
    if len < 1e-8 { fallback } else { scale(v, 1.0 / len) }
}

/// Twice the signed area a closed outline encloses (shoelace): positive when
/// it turns counter-clockwise about +Z.
pub fn outline_area2(points: &[[f32; 2]]) -> f32 {
    let len = points.len();
    (0..len)
        .map(|i| {
            let ([ax, ay], [bx, by]) = (points[i], points[(i + 1) % len]);
            ax * by - bx * ay
        })
        .sum()
}

/// Whether an extrusion reads its outline backwards so the solid faces out.
/// Read forwards, a counter-clockwise outline pushed along +Z (or a clockwise
/// one along -Z) has its winding and normals facing in. An open outline has
/// no inside and keeps its order; a self-crossing one follows its net area.
pub fn extrude_reversed(points: &[[f32; 2]], close: bool, depth: f32) -> bool {
    (close || is_loop(points)) && outline_area2(points) * signum(depth) > 0.0
}

/// `node.extrude_curve`'s grid. A cap fans from the curve's origin, so it
/// fills an outline drawn around its origin (a circle, a polygon, a star).
pub fn extrude_grid(points: &[[f32; 2]], depth: f32, steps: u32, close: bool, symmetric: bool, caps: bool) -> Grid {
    let p_len = points.len().max(1);
    let cols = p_len + usize::from(close);
    let steps = steps.max(1) as usize;
    let rows = capped_rows(steps + 1, caps);
    let shift = if symmetric { depth * 0.5 } else { 0.0 };
    let looped = close || is_loop(points);
    let reversed = extrude_reversed(points, close, depth);
    let mut grid = Grid { positions: Vec::with_capacity(cols * rows), normals: Vec::with_capacity(cols * rows), cols, rows };
    for row in 0..rows {
        let (step, role) = capped_row(row, steps, caps);
        let z = depth * step as f32 / steps as f32 - shift;
        for col in 0..cols {
            let at = if reversed { cols - 1 - col } else { col } % p_len;
            let [x, y] = points.get(at).copied().unwrap_or_default();
            let [tx, ty] = curve_tangent(points, at, looped).map(|t| if reversed { -t } else { t });
            // A cap faces along Z, the way the outline runs round its
            // origin; the far cap faces the other way.
            let around = signum(x * ty - y * tx);
            let rim = |centre: bool| if centre { [0.0, 0.0, z] } else { [x, y, z] };
            let (position, normal) = match role {
                CapRow::Side => ([x, y, z], unit_or([-signum(depth) * ty, signum(depth) * tx, 0.0], [0.0, 1.0, 0.0])),
                CapRow::Start { centre } => (rim(centre), [0.0, 0.0, around]),
                CapRow::End { centre } => (rim(centre), [0.0, 0.0, -around]),
            };
            grid.positions.push(position);
            grid.normals.push(normal);
        }
    }
    grid
}

/// `node.revolve_curve`'s grid: profile point `i` (x radius, y height) at
/// angle `sweep * j / segments` about +Y.
pub fn revolve_grid(points: &[[f32; 2]], segments: u32, sweep: f32) -> Grid {
    let segments = segments.max(1) as usize;
    let cols = segments + 1;
    let rows = points.len();
    let looped = is_loop(points);
    let mut grid = Grid { positions: Vec::with_capacity(cols * rows), normals: Vec::with_capacity(cols * rows), cols, rows };
    for (i, &[radius, height]) in points.iter().enumerate() {
        let [tr, th] = curve_tangent(points, i, looped);
        let turn = signum(radius) * signum(sweep);
        for col in 0..cols {
            let phi = sweep * col as f32 / segments as f32;
            let (s, c) = phi.sin_cos();
            grid.positions.push([radius * c, height, radius * s]);
            grid.normals.push(unit_or(scale([th * c, -tr, th * s], turn), [0.0, 1.0, 0.0]));
        }
    }
    grid
}

/// `node.tube_from_path`'s grid: a ring around each path point, framed by
/// the path's central difference and +Y. A cap closes each path end at the
/// path point.
pub fn tube_grid(points: &[[f32; 2]], lift: &[f32], radius: f32, sides: u32, caps: bool) -> Grid {
    let p_len = points.len().max(1);
    let sides = sides.max(3) as usize;
    let cols = sides + 1;
    let rows = capped_rows(p_len, caps);
    let at = |k: usize| {
        let [x, z] = points.get(k).copied().unwrap_or_default();
        [x, lift.get(k).copied().unwrap_or(0.0), z]
    };
    let mut grid = Grid { positions: Vec::with_capacity(cols * rows), normals: Vec::with_capacity(cols * rows), cols, rows };
    for row in 0..rows {
        let (k, role) = capped_row(row, p_len - 1, caps);
        let center = at(k);
        let tangent = unit_or(sub(at((k + 1).min(p_len - 1)), at(k.saturating_sub(1))), [0.0, 0.0, 1.0]);
        let right = unit_or(cross([0.0, 1.0, 0.0], tangent), [1.0, 0.0, 0.0]);
        let ring_up = cross(tangent, right);
        for col in 0..cols {
            // The ring turns clockwise seen down the path, so the grid's
            // winding faces out of the tube as its normals do.
            let theta = -std::f32::consts::TAU * col as f32 / sides as f32;
            let ring = add(scale(right, theta.cos()), scale(ring_up, theta.sin()));
            let rim = |centre: bool| if centre { center } else { add(center, scale(ring, radius)) };
            let (position, normal) = match role {
                CapRow::Side => (rim(false), scale(ring, signum(radius))),
                CapRow::Start { centre } => (rim(centre), scale(tangent, -1.0)),
                CapRow::End { centre } => (rim(centre), tangent),
            };
            grid.positions.push(position);
            grid.normals.push(normal);
        }
    }
    grid
}

/// `node.make_triangles` with Keep Normals on `grid`: two triangles per quad,
/// each corner with the grid's own normal.
pub fn triangulate(grid: &Grid) -> Vec<MeshVertex> {
    let (cols, rows) = (grid.cols, grid.rows);
    if cols < 2 || rows < 2 || grid.positions.len() < cols * rows || grid.normals.len() < cols * rows {
        return Vec::new();
    }
    let mut out = Vec::with_capacity((cols - 1) * (rows - 1) * 6);
    for qy in 0..rows - 1 {
        for qx in 0..cols - 1 {
            for (dx, dy) in [(0, 0), (0, 1), (1, 0), (0, 1), (1, 1), (1, 0)] {
                let at = (qy + dy) * cols + qx + dx;
                let mut vertex: MeshVertex = bytemuck::Zeroable::zeroed();
                vertex.position = grid.positions[at];
                vertex.normal = grid.normals[at];
                vertex.color = [1.0; 4];
                out.push(vertex);
            }
        }
    }
    out
}

impl SweptCurve {
    /// The grid this sweep makes.
    pub fn grid(&self) -> Grid {
        match self.sweep {
            Sweep::Extrude { depth, steps, close, symmetric, caps } => {
                extrude_grid(&self.points, depth, steps, close, symmetric, caps)
            }
            Sweep::Revolve { segments, sweep } => revolve_grid(&self.points, segments, sweep),
            Sweep::Tube { radius, sides, caps } => tube_grid(&self.points, &self.lift, radius, sides, caps),
        }
    }

    /// The solid's triangles, local space, without the zero-area ones the
    /// grid has at a doubled rim or a cap centre.
    pub fn load_vertices(&self) -> Result<Vec<MeshVertex>, String> {
        if self.points.len() < 2 {
            return Err("a swept curve needs at least two points".into());
        }
        let finite = |v: &f32| v.is_finite();
        if !self.points.iter().flatten().all(finite) || !self.lift.iter().all(finite) {
            return Err("a swept curve's points must be finite".into());
        }
        let size = match self.sweep {
            Sweep::Extrude { depth, .. } => depth,
            Sweep::Revolve { sweep, .. } => sweep,
            Sweep::Tube { radius, .. } => radius,
        };
        if !size.is_finite() {
            return Err("a swept curve's size must be finite".into());
        }
        let mut vertices = triangulate(&self.grid());
        let mut kept = 0;
        for tri in 0..vertices.len() / 3 {
            let [a, b, c] = [0, 1, 2].map(|k| vertices[tri * 3 + k].position);
            if length(cross(sub(b, a), sub(c, a))) > 1e-12 {
                vertices.copy_within(tri * 3..tri * 3 + 3, kept * 3);
                kept += 1;
            }
        }
        vertices.truncate(kept * 3);
        if vertices.is_empty() {
            return Err("a swept curve has no area".into());
        }
        Ok(vertices)
    }
}

fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn scale(a: [f32; 3], k: f32) -> [f32; 3] {
    [a[0] * k, a[1] * k, a[2] * k]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn length(a: [f32; 3]) -> f32 {
    (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn circle(clockwise: bool) -> Vec<[f32; 2]> {
        let turn = if clockwise { -1.0 } else { 1.0 };
        (0..=24).map(|i| (turn * std::f32::consts::TAU * i as f32 / 24.0).sin_cos()).map(|(s, c)| [c, s]).collect()
    }

    /// The volume a triangle list encloses, by the divergence theorem:
    /// positive when every face winds outward.
    fn signed_volume(vertices: &[MeshVertex]) -> f32 {
        vertices
            .chunks(3)
            .map(|tri| {
                let [a, b, c] = [0, 1, 2].map(|k| tri[k].position);
                a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0]) + a[2] * (b[0] * c[1] - b[1] * c[0])
            })
            .sum::<f32>()
            / 6.0
    }

    /// A push along the normals swells a sweep, as in Blender: its winding
    /// encloses positive volume and every face normal agrees with it,
    /// whichever way the outline turns and the depth points.
    #[test]
    fn swept_solids_face_out() {
        let mut sweeps = Vec::new();
        for clockwise in [false, true] {
            for depth in [2.0, -2.0] {
                for (close, symmetric) in [(false, false), (true, true)] {
                    let mut points = circle(clockwise);
                    if close {
                        points.pop();
                    }
                    sweeps.push(SweptCurve { points, lift: Vec::new(), sweep: Sweep::Extrude { depth, steps: 2, close, symmetric, caps: true } });
                }
            }
        }
        let path = (0..8).map(|i| [i as f32 * 0.5, (i as f32 * 0.7).sin()]).collect();
        sweeps.push(SweptCurve { points: path, lift: Vec::new(), sweep: Sweep::Tube { radius: 0.3, sides: 12, caps: true } });
        for swept in &sweeps {
            let vertices = swept.load_vertices().expect("the sweep has area");
            assert!(signed_volume(&vertices) > 0.0, "{:?} encloses negative volume", swept.sweep);
            for tri in vertices.chunks(3) {
                let [a, b, c] = [0, 1, 2].map(|k| tri[k].position);
                let face = cross(sub(b, a), sub(c, a));
                let normal = add(add(tri[0].normal, tri[1].normal), tri[2].normal);
                let along = face[0] * normal[0] + face[1] * normal[1] + face[2] * normal[2];
                assert!(along > 0.0, "{:?}: a face's normals point against its winding", swept.sweep);
            }
        }
    }
}

