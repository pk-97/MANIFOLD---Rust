//! Where each copy of a copy group rests. A layout (Grid, Scatter, Line,
//! Ring) is centred on the group's origin, spaced in multiples of the copy's
//! own box width, and placed as one by the group's rotation. Rest poses are
//! computed only here; the card and the gizmo show the group, never a copy.

use manifold_core::copies_body::{spacing_unit, SCATTER_JITTER};
use manifold_node_engine::platonic::platonic_points;
use manifold_node_engine::scene::transform::Transform;
use manifold_physics::BodyPose;

use super::{pose_from_transform, CopySpawn, RigidBody, MAX_COPIES};

/// Stored values: Grid 0, Scatter 1 (once Pile), Line 2, Ring 3.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CopyLayout {
    #[default]
    Grid,
    Scatter,
    Line,
    Ring,
}

impl CopyLayout {
    pub(crate) fn from_value(value: f32) -> Self {
        match value.round().clamp(0.0, 3.0) as u32 {
            1 => Self::Scatter,
            2 => Self::Line,
            3 => Self::Ring,
            _ => Self::Grid,
        }
    }
}

/// Everything a group's rest poses depend on besides the copy's size: the
/// spawner, the made count and the group transform.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Arrangement {
    pub layout: CopyLayout,
    pub count: usize,
    pub columns: usize,
    /// A multiple of the copy's box width along each layout axis.
    pub spacing: f32,
    /// Each copy's own turn inside the layout, Euler radians.
    pub turn: [f32; 3],
    pub origin: [f32; 3],
    pub rotation: [f32; 3],
}

/// The copies `spawn` asks for, before the world's copy pool is shared.
pub(crate) fn requested_count(spawn: CopySpawn) -> usize {
    spawn.count.round().clamp(0.0, MAX_COPIES as f32) as usize
}

impl Arrangement {
    /// `prototype`'s arrangement of `count` copies.
    pub fn of(prototype: &RigidBody, count: usize) -> Self {
        let spawn = prototype.spawn;
        Self {
            layout: CopyLayout::from_value(spawn.layout),
            count,
            columns: spawn.columns.round().clamp(1.0, 64.0) as usize,
            spacing: spawn.spacing,
            turn: spawn.turn,
            origin: prototype.transform.pos,
            rotation: prototype.transform.rot_euler,
        }
    }

    /// Whether the two lay out the same shape, wherever the group stands.
    pub fn same_shape(&self, other: &Self) -> bool {
        Self { origin: other.origin, rotation: other.rotation, ..*self } == *other
    }

    /// Copy `index`'s rest pose for a copy whose box is `width` metres.
    pub fn rest(&self, width: [f32; 3], index: usize) -> BodyPose {
        let unit = spacing_unit(self.layout == CopyLayout::Scatter, width);
        let step = unit.map(|unit| self.spacing * unit);
        let group = euler_quat(self.rotation);
        match self.layout {
            CopyLayout::Grid => {
                let columns = self.columns.min(self.count).max(1);
                let rows = self.count.div_ceil(columns).min(columns);
                let (column, row, layer) = (index % columns, (index / columns) % columns, index / (columns * columns));
                let offset = [
                    (column as f32 - (columns as f32 - 1.0) * 0.5) * step[0],
                    layer as f32 * step[1],
                    (row as f32 - (rows as f32 - 1.0) * 0.5) * step[2],
                ];
                BodyPose { position: place(self.origin, group, offset), rotation: compose(group, euler_quat(self.turn)) }
            }
            CopyLayout::Scatter => {
                let columns = ceil_cuberoot(self.count).min(self.columns);
                let (column, row, layer) = (index % columns, (index / columns) % columns, index / (columns * columns));
                let center = (columns as f32 - 1.0) * 0.5;
                let offset = [(column as f32 - center) * step[0], layer as f32 * step[1], (row as f32 - center) * step[2]];
                let jitter = [0, 1, 2].map(|axis| bounded_jitter(index, axis, step[axis as usize]));
                let mut turn = self.turn;
                for (axis, value) in turn.iter_mut().enumerate() {
                    *value += full_turn_hash(index, 3 + axis as u32);
                }
                BodyPose {
                    position: place(place(self.origin, group, offset), group, jitter),
                    rotation: compose(group, euler_quat(turn)),
                }
            }
            CopyLayout::Line => {
                let offset = [(index as f32 - (self.count as f32 - 1.0) * 0.5) * step[0], 0.0, 0.0];
                BodyPose { position: place(self.origin, group, offset), rotation: compose(group, euler_quat(self.turn)) }
            }
            CopyLayout::Ring => {
                let angle = std::f32::consts::TAU * index as f32 / self.count.max(1) as f32;
                let radius = ring_radius(self.count, step[0], width[2]);
                let offset = [radius * angle.cos(), 0.0, radius * angle.sin()];
                // Each copy's X runs along the ring, so neighbours meet side to side.
                let (sin, cos) = (-(angle + std::f32::consts::FRAC_PI_2) * 0.5).sin_cos();
                let yaw = [0.0, sin, 0.0, cos];
                BodyPose {
                    position: place(self.origin, group, offset),
                    rotation: compose(group, compose(yaw, euler_quat(self.turn))),
                }
            }
        }
    }
}

/// Neighbours sit `step` apart around the ring, or further when the copies'
/// inner corners would otherwise meet: at Spacing 1.0 they touch, never overlap.
fn ring_radius(count: usize, step: f32, depth: f32) -> f32 {
    if count < 2 {
        return 0.0;
    }
    let half_angle = std::f32::consts::PI / count as f32;
    let chord = step / (2.0 * half_angle.sin());
    let inner = 0.5 * depth + 0.5 * step * half_angle.cos() / half_angle.sin();
    chord.max(inner)
}

/// A copy's box width per axis at its scale, metres, measured from the
/// points the world installs as its hull (a hull's extreme points are among
/// its inputs, so the box is the installed hull's).
pub(crate) fn copy_width(prototype: &RigidBody) -> [f32; 3] {
    let scale = prototype.transform.scale;
    let mut minimum = [f32::INFINITY; 3];
    let mut maximum = [f32::NEG_INFINITY; 3];
    let mut grow = |point: &[f32; 3]| {
        for axis in 0..3 {
            let scaled = point[axis] * scale[axis];
            minimum[axis] = minimum[axis].min(scaled);
            maximum[axis] = maximum[axis].max(scaled);
        }
    };
    match prototype.collider.as_ref() {
        Some(geometry) => geometry.hulls.iter().flatten().for_each(&mut grow),
        None => platonic_points(prototype.shape).iter().for_each(&mut grow),
    }
    std::array::from_fn(|axis| {
        let width = maximum[axis] - minimum[axis];
        if width.is_finite() && width > 0.0 { width } else { scale[axis] }
    })
}

/// `origin + R * offset`, exactly `origin + offset` when the group is unturned.
fn place(origin: [f32; 3], group: [f32; 4], offset: [f32; 3]) -> [f32; 3] {
    let offset = if is_identity(group) { offset } else { rotate(group, offset) };
    [origin[0] + offset[0], origin[1] + offset[1], origin[2] + offset[2]]
}

fn euler_quat(euler: [f32; 3]) -> [f32; 4] {
    pose_from_transform(Transform { rot_euler: euler, ..Transform::default() }).rotation
}

fn is_identity(q: [f32; 4]) -> bool {
    q == [0.0, 0.0, 0.0, 1.0]
}

/// `a * b`, xyzw; an identity factor returns the other exactly.
fn compose(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    if is_identity(a) {
        return b;
    }
    if is_identity(b) {
        return a;
    }
    let [ax, ay, az, aw] = a;
    let [bx, by, bz, bw] = b;
    [
        aw * bx + ax * bw + ay * bz - az * by,
        aw * by - ax * bz + ay * bw + az * bx,
        aw * bz + ax * by - ay * bx + az * bw,
        aw * bw - ax * bx - ay * by - az * bz,
    ]
}

fn rotate(q: [f32; 4], v: [f32; 3]) -> [f32; 3] {
    let [x, y, z, w] = q;
    let t = [2.0 * (y * v[2] - z * v[1]), 2.0 * (z * v[0] - x * v[2]), 2.0 * (x * v[1] - y * v[0])];
    [
        v[0] + w * t[0] + (y * t[2] - z * t[1]),
        v[1] + w * t[1] + (z * t[0] - x * t[2]),
        v[2] + w * t[2] + (x * t[1] - y * t[0]),
    ]
}

pub(crate) fn ceil_cuberoot(count: usize) -> usize {
    let mut columns: usize = 1;
    while columns.saturating_mul(columns).saturating_mul(columns) < count {
        columns += 1;
    }
    columns
}

fn bounded_jitter(index: usize, axis: u32, spacing: f32) -> f32 {
    (index_hash_unit(index, axis) * 2.0 - 1.0) * SCATTER_JITTER * spacing
}

fn full_turn_hash(index: usize, salt: u32) -> f32 {
    index_hash_unit(index, salt) * std::f32::consts::TAU
}

fn index_hash_unit(index: usize, salt: u32) -> f32 {
    let mut value = (index as u32).wrapping_add(salt.wrapping_mul(0x9e37_79b9));
    value ^= value >> 16;
    value = value.wrapping_mul(0x85eb_ca6b);
    value ^= value >> 13;
    value = value.wrapping_mul(0xc2b2_ae35);
    value ^= value >> 16;
    value as f32 / u32::MAX as f32
}
