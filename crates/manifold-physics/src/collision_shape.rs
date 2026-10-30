//! The one route from a mesh to every solver (SCENE_DOCUMENT_DESIGN.md
//! section 7.8 (Physics and collision shapes)). An item's mesh becomes a
//! [`CollisionShape`]; each solver asks it for the form it reads:
//!
//! - A static collider in Box3D: the exact triangle mesh ([`Solid::Mesh`],
//!   installed through `b3CreateMesh`).
//! - A body in Box3D: a spatial split into convex hulls with automatic detail
//!   ([`CollisionShape::body_hulls`]), open surfaces and flat pieces given a
//!   0.1% shell.
//! - Water: the exact triangle mesh through a mesh tree (`mesh_bvh`), read by
//!   the GPU for distance and winding.
//! - A Terrain's ground: a [`HeightField`] ([`Solid::HeightField`]), a Box3D
//!   height field for bodies and the same heights on the GPU for water.
//!
//! Its [`ShapeKey`] is the cache key: the mesh content that was actually
//! loaded (so a mesh arriving late changes it), the item's scale and the
//! water cell size.
//!
//! The gate into Box3D lives here too. [`Solid::install`] and the checks it
//! runs ([`validate_scale`], [`validate_position`], [`validate_hull`]) are
//! how a body reaches the solver: anything non-finite, out of the world's
//! range or degenerate is refused with a reason before Box3D sees it, so a
//! bad scale can never grow a broad-phase tree past its depth limit
//! (BUG-k787p (Box3D tree query assertion)). Any finite non-zero scale is a
//! scale, mirrored included, as in Blender and Unity: the shape it makes is
//! what the gate checks, against [`MAX_COORDINATE`] and Box3D's own hull
//! builder.
//! A mirrored scale reflects the shape: Box3D rebuilds each hull from its
//! reflected points, so its faces point out, and a mirrored mesh has its
//! triangles turned ([`scaled_mesh`]).

use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Arc, OnceLock};

use manifold_foundation::mesh_partition::partition;

use crate::{
    BodyConfig, BodyHandle, CookedSolid, PhysicsError, PhysicsWorld, TriangleMesh,
    closed_triangles, cook_hull, cook_hull_mesh,
};

/// The farthest any position or hull point may sit from the origin, in
/// metres. Box3D treats bounds past 100 km as corrupt; this keeps every
/// installed bound a factor of ten inside that.
pub const MAX_COORDINATE: f32 = 10_000.0;

/// Why a hull Box3D's builder could not make is refused: its points are
/// flat, a line or a point for their size.
pub const TOO_THIN: &str = "part of the shape is too thin to collide";

/// An open surface becomes a solid this fraction of its longest side thick.
pub const SHELL_FRACTION: f32 = 0.001;

/// Steps per octave a scale is rounded to before it enters a cache key, so a
/// scale moving within one step (about 9%) rebuilds nothing.
pub const SCALE_STEPS_PER_OCTAVE: f32 = 8.0;

/// A body's longest side over this many metres earns one more hull.
const METRES_PER_HULL: f32 = 0.05;

/// A closed part whose convex hull holds no more than this much more volume
/// than the part itself is convex enough for one hull.
const CONVEX_VOLUME_RATIO: f64 = 1.02;

fn step(value: f32) -> i32 {
    (value.abs().log2() * SCALE_STEPS_PER_OCTAVE).round() as i32
}

/// A scale's smallest and largest axis in eighth-octave steps, the only part
/// of the scale a lattice's spacing reads.
pub fn scale_steps(scale: [f32; 3]) -> [i32; 2] {
    let [x, y, z] = scale.map(f32::abs);
    [step(x.min(y).min(z)), step(x.max(y).max(z))]
}

/// What a prepared shape was made from. Equal keys give equal shapes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ShapeKey {
    /// A hash of the mesh content that was loaded: present only once the
    /// mesh is, so an asynchronous mesh arriving changes the key.
    content: u64,
    /// Each scale axis in eighth-octave steps.
    scale: [i32; 3],
    /// The water cell size's bits; zero for a solver without cells.
    cell: u32,
}

impl ShapeKey {
    /// The key for `shape` at `scale`, for a solver with cells `cell_size`
    /// metres across (water) or none (Box3D).
    pub fn new(
        shape: &CollisionShape,
        scale: [f32; 3],
        cell_size: Option<f32>,
    ) -> Result<Self, PhysicsError> {
        validate_scale(scale)?;
        let cell = match cell_size {
            None => 0,
            Some(cell) if cell.is_finite() && cell > 0.0 => cell.to_bits(),
            Some(_) => {
                return Err(PhysicsError::InvalidInput(
                    "a water cell size must be finite and positive",
                ));
            }
        };
        Ok(Self {
            content: shape.content,
            scale: scale.map(step),
            cell,
        })
    }

    /// The key for a height field, which is installed at its exact scale.
    pub fn of_height_field(field: &HeightField) -> Self {
        let mut hasher = DefaultHasher::new();
        field.points.hash(&mut hasher);
        for value in [field.spacing]
            .into_iter()
            .chain(field.origin)
            .chain(field.heights.iter().copied())
        {
            value.to_bits().hash(&mut hasher);
        }
        Self {
            content: hasher.finish(),
            scale: [0; 3],
            cell: 0,
        }
    }
}

/// An item's mesh, welded and checked, in its local frame. Built off the
/// content thread: welding and the checks cost one pass over the triangles.
#[derive(Debug)]
pub struct CollisionShape {
    parts: Vec<TriangleMesh>,
    content: u64,
    min: [f32; 3],
    max: [f32; 3],
    open: OnceLock<bool>,
}

impl CollisionShape {
    /// A shape from unindexed triangle lists, one per part (a compound body's
    /// parts each split on their own). Exactly equal positions weld.
    pub fn from_triangle_lists<'a>(
        parts: impl IntoIterator<Item = &'a [[f32; 3]]>,
    ) -> Result<Self, PhysicsError> {
        let welded = parts.into_iter().map(weld).collect::<Result<Vec<_>, _>>()?;
        Self::new(welded)
    }

    /// A shape from indexed parts. Zero-area triangles are dropped (a scan's
    /// slivers); a part with none left is dropped. Refused when a vertex is
    /// non-finite or past [`MAX_COORDINATE`], an index is out of range, or
    /// nothing with extent remains.
    pub fn new(parts: Vec<TriangleMesh>) -> Result<Self, PhysicsError> {
        let mut kept = Vec::with_capacity(parts.len());
        let mut min = [f32::INFINITY; 3];
        let mut max = [f32::NEG_INFINITY; 3];
        for mut part in parts {
            for vertex in &part.vertices {
                validate_position(*vertex)?;
            }
            if part
                .triangles
                .iter()
                .flatten()
                .any(|&index| index as usize >= part.vertices.len())
            {
                return Err(PhysicsError::InvalidInput(
                    "a mesh triangle index is out of range",
                ));
            }
            let floor = zero_area_floor(&part.vertices);
            part.triangles.retain(|triangle| {
                let [a, b, c] = triangle.map(|i| part.vertices[i as usize].map(f64::from));
                triangle[0] != triangle[1]
                    && triangle[1] != triangle[2]
                    && triangle[2] != triangle[0]
                    && norm2(cross(sub(b, a), sub(c, a))) > floor
            });
            if part.triangles.is_empty() {
                continue;
            }
            for triangle in &part.triangles {
                for &index in triangle {
                    let vertex = part.vertices[index as usize];
                    for axis in 0..3 {
                        min[axis] = min[axis].min(vertex[axis]);
                        max[axis] = max[axis].max(vertex[axis]);
                    }
                }
            }
            kept.push(part);
        }
        if kept.is_empty() {
            return Err(PhysicsError::InvalidInput(
                "the mesh has no triangle with area",
            ));
        }
        let longest = (0..3)
            .map(|axis| max[axis] - min[axis])
            .fold(0.0f32, f32::max);
        if longest.is_nan() || longest <= 0.0 {
            return Err(PhysicsError::InvalidInput("the mesh has no extent"));
        }
        let mut hasher = DefaultHasher::new();
        for part in &kept {
            part.triangles.hash(&mut hasher);
            for vertex in &part.vertices {
                vertex.map(f32::to_bits).hash(&mut hasher);
            }
        }
        Ok(Self {
            parts: kept,
            content: hasher.finish(),
            min,
            max,
            open: OnceLock::new(),
        })
    }

    /// The hash of the loaded content [`ShapeKey`] carries.
    pub fn content(&self) -> u64 {
        self.content
    }

    pub fn parts(&self) -> &[TriangleMesh] {
        &self.parts
    }

    /// Local bounds: min then max.
    pub fn bounds(&self) -> [[f32; 3]; 2] {
        [self.min, self.max]
    }

    fn extent(&self) -> [f32; 3] {
        std::array::from_fn(|axis| self.max[axis] - self.min[axis])
    }

    /// Whether any part is an open surface (a ground plane, a scan).
    pub fn has_open_parts(&self) -> bool {
        *self.open.get_or_init(|| any_open(&self.parts))
    }

    /// Every part in one indexed mesh: the exact surface a static collider is.
    pub fn exact_mesh(&self) -> TriangleMesh {
        let mut vertices =
            Vec::with_capacity(self.parts.iter().map(|part| part.vertices.len()).sum());
        let mut triangles =
            Vec::with_capacity(self.parts.iter().map(|part| part.triangles.len()).sum());
        for part in &self.parts {
            let base = vertices.len() as u32;
            vertices.extend_from_slice(&part.vertices);
            triangles.extend(
                part.triangles
                    .iter()
                    .map(|triangle| triangle.map(|index| index + base)),
            );
        }
        TriangleMesh {
            vertices,
            triangles,
        }
    }

    /// Convex hulls for a moving body at `scale`, in the local frame: each
    /// part split in space into pieces, the count chosen from its triangles
    /// and its size at this scale (one hull for a convex closed part), with
    /// no cap on the total. Open surfaces get a shell
    /// [`SHELL_FRACTION`] of the shape's longest side thick so every hull has
    /// volume. Costs a hull cook per piece; never on the content thread.
    pub fn body_hulls(&self, scale: [f32; 3]) -> Result<Vec<Vec<[f32; 3]>>, PhysicsError> {
        validate_scale(scale)?;
        let thickness = self.extent().into_iter().fold(0.0f32, f32::max) * SHELL_FRACTION;
        let counts: Vec<usize> = self
            .parts
            .iter()
            .map(|part| automatic_pieces(part, scale))
            .collect::<Result<_, _>>()?;
        let mut hulls = Vec::with_capacity(counts.iter().sum());
        for (part, count) in self.parts.iter().zip(counts) {
            let closed = closed_triangles(part).is_ok_and(|flags| !flags.contains(&false));
            for piece in partition(&part.vertices, &part.triangles, count)
                .map_err(|_| PhysicsError::InvalidInput("the mesh could not be split into hulls"))?
            {
                // A piece of a closed part keeps its exact points unless it is
                // flat; an open surface always gets the shell.
                let exact = closed
                    .then(|| piece_hull(part, &piece.triangle_ids, 0.0, scale).ok())
                    .flatten();
                let hull = match exact {
                    Some(hull) => hull,
                    None => piece_hull(part, &piece.triangle_ids, thickness, scale)?,
                };
                hulls.push(hull);
            }
        }
        Ok(hulls)
    }
}

/// The cooked hull of `triangle_ids` of `part`, each corner pushed half of
/// `thickness` out and in along its triangle's normal, checked at `scale`.
fn piece_hull(
    part: &TriangleMesh,
    triangle_ids: &[usize],
    thickness: f32,
    scale: [f32; 3],
) -> Result<Vec<[f32; 3]>, PhysicsError> {
    let mut points = Vec::with_capacity(triangle_ids.len() * 6);
    for &triangle in triangle_ids {
        let corners = part.triangles[triangle].map(|index| part.vertices[index as usize]);
        let normal = cross(sub64(corners[1], corners[0]), sub64(corners[2], corners[0]));
        let length = norm2(normal).sqrt();
        let shell: [f32; 3] = std::array::from_fn(|axis| {
            if length > 0.0 {
                (normal[axis] / length) as f32 * thickness * 0.5
            } else {
                0.0
            }
        });
        for corner in corners {
            points.push(std::array::from_fn(|axis| corner[axis] + shell[axis]));
            points.push(std::array::from_fn(|axis| corner[axis] - shell[axis]));
        }
    }
    points.sort_unstable_by(|a, b| {
        a[0].total_cmp(&b[0])
            .then(a[1].total_cmp(&b[1]))
            .then(a[2].total_cmp(&b[2]))
    });
    points.dedup();
    let hull = cook_hull(&points)?;
    validate_hull(&scaled(&hull, scale))?;
    Ok(hull)
}

/// How many hulls a part asks for at `scale`.
fn automatic_pieces(part: &TriangleMesh, scale: [f32; 3]) -> Result<usize, PhysicsError> {
    let triangles = part.triangles.len();
    if triangles < 4 || convex_closed(part) {
        return Ok(1);
    }
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for vertex in &part.vertices {
        for axis in 0..3 {
            min[axis] = min[axis].min(vertex[axis]);
            max[axis] = max[axis].max(vertex[axis]);
        }
    }
    let longest = (0..3)
        .map(|axis| (max[axis] - min[axis]) * scale[axis].abs())
        .fold(0.0f32, f32::max);
    let by_size = (longest / METRES_PER_HULL).ceil().max(1.0) as usize;
    let by_detail = (triangles as f32).sqrt().ceil() as usize;
    Ok(by_detail.min(by_size).max(1).min(triangles))
}

/// Whether `part` is one closed volume its convex hull barely exceeds.
fn convex_closed(part: &TriangleMesh) -> bool {
    let Ok(closed) = closed_triangles(part) else {
        return false;
    };
    if closed.contains(&false) {
        return false;
    }
    let volume = mesh_volume(part).abs();
    let Ok(hull) = cook_hull_mesh(&part.vertices) else {
        return false;
    };
    volume > 0.0 && mesh_volume(&hull).abs() <= volume * CONVEX_VOLUME_RATIO
}

fn mesh_volume(mesh: &TriangleMesh) -> f64 {
    mesh.triangles
        .iter()
        .map(|triangle| {
            let [a, b, c] = triangle.map(|index| mesh.vertices[index as usize].map(f64::from));
            dot(a, cross(b, c))
        })
        .sum::<f64>()
        / 6.0
}

fn any_open(meshes: &[TriangleMesh]) -> bool {
    meshes
        .iter()
        .any(|mesh| closed_triangles(mesh).is_ok_and(|closed| closed.contains(&false)))
}

/// Ground as a grid of heights: `points[0]` along x and `points[1]` along z,
/// `spacing` apart, row by row in z, point (0, 0) at `origin`, solid below.
/// Local and unscaled, as every solid is.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeightField {
    pub points: [u32; 2],
    pub spacing: f32,
    pub origin: [f32; 3],
    pub heights: Vec<f32>,
}

impl HeightField {
    /// The lowest and highest height.
    pub fn height_range(&self) -> [f32; 2] {
        self.heights
            .iter()
            .fold([f32::INFINITY, f32::NEG_INFINITY], |[low, high], &h| {
                [low.min(h), high.max(h)]
            })
    }

    /// Its box's corners, local and unscaled.
    fn corners(&self) -> [[f32; 3]; 8] {
        let [low, high] = self.height_range();
        let extent = |axis: usize| (self.points[axis].max(1) - 1) as f32 * self.spacing;
        std::array::from_fn(|k| {
            [
                self.origin[0] + if k & 1 == 0 { 0.0 } else { extent(0) },
                self.origin[1] + if k & 2 == 0 { low } else { high },
                self.origin[2] + if k & 4 == 0 { 0.0 } else { extent(1) },
            ]
        })
    }
}

/// The form a body takes in Box3D.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Solid {
    /// Convex hulls on one moving body, local and unscaled.
    Hulls(Vec<Vec<[f32; 3]>>),
    /// The exact surface of a static collider, local and unscaled.
    Mesh(TriangleMesh),
    /// A static collider's ground, local and unscaled.
    HeightField(HeightField),
}

impl Solid {
    /// The hulls, when this is a hull solid.
    pub fn hulls(&self) -> Option<&[Vec<[f32; 3]>]> {
        match self {
            Self::Hulls(hulls) => Some(hulls),
            Self::Mesh(_) | Self::HeightField(_) => None,
        }
    }

    /// Every local point: the hulls' points, the mesh's vertices, or the
    /// corners of a height field's box.
    pub fn for_each_point(&self, mut visit: impl FnMut(&[f32; 3])) {
        match self {
            Self::Hulls(hulls) => hulls.iter().flatten().for_each(&mut visit),
            Self::Mesh(mesh) => mesh.vertices.iter().for_each(&mut visit),
            Self::HeightField(field) => field.corners().iter().for_each(&mut visit),
        }
    }

    /// Whether this solid at `scale` is one the solver can hold, refused in
    /// the scale's terms: every point within [`MAX_COORDINATE`] of the
    /// origin, and every hull one Box3D's hull builder makes at this scale.
    pub fn validate_at(&self, scale: [f32; 3]) -> Result<(), PhysicsError> {
        validate_scale(scale)?;
        let mut far = false;
        self.for_each_point(|point| {
            far |= validate_position(std::array::from_fn(|axis| point[axis] * scale[axis])).is_err()
        });
        if far {
            return Err(PhysicsError::InvalidInput(
                "at this scale the shape reaches past 10 km from the origin, outside the solver's world",
            ));
        }
        if let Some(hulls) = self.hulls() {
            let scaled = scaled_hulls(hulls, scale);
            let refs: Vec<&[[f32; 3]]> = scaled.iter().map(Vec::as_slice).collect();
            CookedSolid::hulls(&refs, 128)?;
        }
        Ok(())
    }

    /// Cook this solid at `scale` once, for any number of bodies to share
    /// ([`PhysicsWorld::add_cooked`]): the gate into Box3D. Refuses a bad
    /// scale, hull or mesh before the solver sees it.
    pub fn cook(&self, scale: [f32; 3]) -> Result<Arc<CookedSolid>, PhysicsError> {
        validate_scale(scale)?;
        match self {
            Self::Hulls(hulls) => {
                let scaled = scaled_hulls(hulls, scale);
                let refs: Vec<&[[f32; 3]]> = scaled.iter().map(Vec::as_slice).collect();
                CookedSolid::hulls(&refs, 128)
            }
            Self::Mesh(mesh) => {
                let mesh = scaled_mesh(mesh, scale);
                CookedSolid::mesh(&mesh.vertices, &mesh.triangles)
            }
            Self::HeightField(field) => CookedSolid::height_field(field, scale),
        }
    }

    /// Install this solid at `scale` as one body, through [`Self::cook`].
    pub fn install(
        &self,
        world: &mut PhysicsWorld,
        scale: [f32; 3],
        config: BodyConfig,
    ) -> Result<BodyHandle, PhysicsError> {
        world.add_cooked(&self.cook(scale)?, config)
    }

    /// Swap an installed body's shape for this solid at `scale` in place,
    /// through the same gate: its pose, its velocity and every other body
    /// stay as they are. The body must already be the same form (hulls or
    /// exact mesh).
    pub fn replace(
        &self,
        world: &mut PhysicsWorld,
        handle: BodyHandle,
        scale: [f32; 3],
        config: BodyConfig,
    ) -> Result<(), PhysicsError> {
        world.replace_cooked(handle, &self.cook(scale)?, config)
    }

    /// Whether `self` and `other` install as the same form, so one can
    /// replace the other in place ([`Self::replace`]). A height field never
    /// does: its body sits at its grid's origin, which moves with it.
    pub fn same_form(&self, other: &Self) -> bool {
        matches!(
            (self, other),
            (Self::Hulls(_), Self::Hulls(_)) | (Self::Mesh(_), Self::Mesh(_))
        )
    }
}

fn scaled(points: &[[f32; 3]], scale: [f32; 3]) -> Vec<[f32; 3]> {
    points
        .iter()
        .map(|point| std::array::from_fn(|axis| point[axis] * scale[axis]))
        .collect()
}

fn scaled_hulls(hulls: &[Vec<[f32; 3]>], scale: [f32; 3]) -> Vec<Vec<[f32; 3]>> {
    hulls.iter().map(|hull| scaled(hull, scale)).collect()
}

/// Whether `scale` mirrors: an odd number of its axes are negative.
pub fn mirrors(scale: [f32; 3]) -> bool {
    scale.iter().filter(|s| s.is_sign_negative()).count() % 2 == 1
}

/// `mesh` at `scale`. A mirroring scale also turns every triangle, so each
/// face still winds outward.
pub fn scaled_mesh(mesh: &TriangleMesh, scale: [f32; 3]) -> TriangleMesh {
    let triangles = if mirrors(scale) {
        mesh.triangles.iter().map(|&[a, b, c]| [a, c, b]).collect()
    } else {
        mesh.triangles.clone()
    };
    TriangleMesh {
        vertices: scaled(&mesh.vertices, scale),
        triangles,
    }
}

/// A scale is finite and not zero on any axis. Mirrored is a scale; what
/// the scale makes of a shape is checked on the shape ([`validate_hull`],
/// [`validate_position`]).
pub fn validate_scale(scale: [f32; 3]) -> Result<(), PhysicsError> {
    if scale.iter().all(|s| s.is_finite() && *s != 0.0) {
        Ok(())
    } else {
        Err(PhysicsError::InvalidInput(
            "scale must be finite and not zero on any axis",
        ))
    }
}

/// A position is finite and within [`MAX_COORDINATE`] of the origin.
pub fn validate_position(position: [f32; 3]) -> Result<(), PhysicsError> {
    if position
        .iter()
        .all(|value| value.is_finite() && value.abs() <= MAX_COORDINATE)
    {
        Ok(())
    } else {
        Err(PhysicsError::InvalidInput(
            "positions must be finite and within 10 km of the origin",
        ))
    }
}

/// A hull is at least four points, each a valid position. Whether they span
/// a volume is Box3D's hull builder's call: its tolerance is relative to the
/// points' size, so a small hull is a hull and only a flat, line or point
/// one fails ([`TOO_THIN`]).
pub fn validate_hull(points: &[[f32; 3]]) -> Result<(), PhysicsError> {
    if points.len() < 4 {
        return Err(PhysicsError::InvalidInput("a hull needs at least 4 points"));
    }
    points
        .iter()
        .try_for_each(|point| validate_position(*point))
}

/// Weld an unindexed triangle list by exactly equal coordinates (signed zero
/// is one coordinate); no tolerance merges distinct points.
pub fn weld(triangles: &[[f32; 3]]) -> Result<TriangleMesh, PhysicsError> {
    if triangles.is_empty() || !triangles.len().is_multiple_of(3) {
        return Err(PhysicsError::InvalidInput(
            "a mesh must be a nonempty triangle list",
        ));
    }
    let mut indices = std::collections::HashMap::<[u32; 3], u32>::new();
    let mut vertices = Vec::new();
    let mut indexed = Vec::with_capacity(triangles.len() / 3);
    for corners in triangles.chunks_exact(3) {
        let mut triangle = [0u32; 3];
        for (slot, position) in corners.iter().enumerate() {
            let key = position.map(|value| if value == 0.0 { 0 } else { value.to_bits() });
            triangle[slot] = *indices.entry(key).or_insert_with(|| {
                vertices.push(*position);
                (vertices.len() - 1) as u32
            });
        }
        indexed.push(triangle);
    }
    if vertices.len() > u32::MAX as usize {
        return Err(PhysicsError::InvalidInput("a mesh has too many vertices"));
    }
    Ok(TriangleMesh {
        vertices,
        triangles: indexed,
    })
}

/// The squared doubled area under which a triangle counts as zero area,
/// relative to the mesh's size (the threshold `closed_triangles` refuses).
fn zero_area_floor(vertices: &[[f32; 3]]) -> f64 {
    let mut min = [f64::INFINITY; 3];
    let mut max = [f64::NEG_INFINITY; 3];
    for vertex in vertices {
        for axis in 0..3 {
            min[axis] = min[axis].min(f64::from(vertex[axis]));
            max[axis] = max[axis].max(f64::from(vertex[axis]));
        }
    }
    let size = (0..3)
        .map(|axis| max[axis] - min[axis])
        .fold(1.0f64, f64::max);
    1.0e-24 * size.powi(4)
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn sub64(a: [f32; 3], b: [f32; 3]) -> [f64; 3] {
    sub(a.map(f64::from), b.map(f64::from))
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn norm2(a: [f64; 3]) -> f64 {
    dot(a, a)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube_list(half: f32) -> Vec<[f32; 3]> {
        let corners: [[f32; 3]; 8] = std::array::from_fn(|i| {
            [
                if i & 1 == 0 { -half } else { half },
                if i & 2 == 0 { -half } else { half },
                if i & 4 == 0 { -half } else { half },
            ]
        });
        const FACES: [[usize; 4]; 6] = [
            [0, 2, 3, 1],
            [4, 5, 7, 6],
            [0, 1, 5, 4],
            [2, 6, 7, 3],
            [0, 4, 6, 2],
            [1, 3, 7, 5],
        ];
        FACES
            .iter()
            .flat_map(|f| [f[0], f[1], f[2], f[0], f[2], f[3]])
            .map(|i| corners[i])
            .collect()
    }

    /// Fundamental 7 (SCENE_DOCUMENT_DESIGN.md section 10 (Testing)): the
    /// cache key follows the loaded content, the scale and the water cell,
    /// so scaling an item refreshes its collision shape.
    #[test]
    fn scaling_an_item_changes_its_shape_key() {
        let shape = CollisionShape::from_triangle_lists([cube_list(0.5).as_slice()]).unwrap();
        let key = |scale, cell| ShapeKey::new(&shape, scale, cell).unwrap();
        assert_eq!(key([1.0; 3], None), key([1.0; 3], None));
        assert_ne!(
            key([1.0; 3], None),
            key([2.0, 1.0, 1.0], None),
            "a scale edit must refresh the shape"
        );
        assert_ne!(
            key([1.0; 3], Some(0.1)),
            key([1.0; 3], Some(0.05)),
            "a finer water must refresh its lattice"
        );
        let other = CollisionShape::from_triangle_lists([cube_list(0.6).as_slice()]).unwrap();
        assert_ne!(
            key([1.0; 3], None),
            ShapeKey::new(&other, [1.0; 3], None).unwrap(),
            "new content is a new key"
        );
        assert!(ShapeKey::new(&shape, [f32::NAN, 1.0, 1.0], None).is_err());
    }
}
