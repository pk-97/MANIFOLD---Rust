//! Where each copy of a copy group rests. A layout (Grid, Scatter, Line, Ring,
//! Spiral, Random) grows out from the group's origin, spaced in multiples of
//! the copy's own box width, and is placed as one by the group's rotation.
//! Line and Grid grow one way from the first copy, which sits on the origin,
//! as Blender's Array does. Ring spreads its copies evenly round the whole
//! circle, as Cinema 4D's radial Cloner does, so its copies slide round as
//! Count changes. Every other layout places a copy by its index and the
//! layout rows alone, so a Count change adds or removes copies at the tail.
//! Body copies keep their motion through a Count change either way; only
//! their rest places move. Rest poses are computed only here; the card and
//! the gizmo show the group, never a copy.

use manifold_core::copies_body::{spacing_unit, SCATTER_JITTER};
use manifold_node_engine::scene::transform::Transform;
use manifold_physics::BodyPose;

use std::sync::Arc;

use super::{pose_from_transform, CopySpawn, RigidBody, Solid, MAX_COPIES};

/// Stored values: Grid 0, Scatter 1 (once Pile), Line 2, Ring 3, Spiral 4,
/// Random 5.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CopyLayout {
    #[default]
    Grid,
    Scatter,
    Line,
    Ring,
    Spiral,
    Random,
}

impl CopyLayout {
    pub(crate) fn from_value(value: f32) -> Self {
        match value.round().clamp(0.0, 5.0) as u32 {
            1 => Self::Scatter,
            2 => Self::Line,
            3 => Self::Ring,
            4 => Self::Spiral,
            5 => Self::Random,
            _ => Self::Grid,
        }
    }
}

/// A group's layout and how many copies it makes: with the copy's size,
/// everything its rest poses depend on, the made count aside.
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

    /// Whether the two differ in Count alone: the bodies that stay keep
    /// their motion, and only the copies that arrive take a rest place.
    pub fn same_places(&self, other: &Self) -> bool {
        Self { count: other.count, ..*self } == *other
    }

    /// Copy `index`'s rest pose for a copy whose box is `width` metres. Only
    /// a Ring's depends on Count (the module doc says why).
    pub fn rest(&self, width: [f32; 3], index: usize) -> BodyPose {
        let unit = spacing_unit(self.layout == CopyLayout::Scatter, width);
        let step = unit.map(|unit| self.spacing * unit);
        let group = euler_quat(self.rotation);
        let columns = self.columns.max(1);
        let laid = |offset: [f32; 3], turn: [f32; 3]| BodyPose {
            position: place(self.origin, group, offset),
            rotation: compose(group, euler_quat(turn)),
        };
        match self.layout {
            CopyLayout::Grid => {
                let (column, row, layer) = (index % columns, (index / columns) % columns, index / (columns * columns));
                laid([column as f32 * step[0], layer as f32 * step[1], row as f32 * step[2]], self.turn)
            }
            CopyLayout::Scatter => {
                let [column, layer, row] = pile_cell(index, columns);
                let offset = [centred(column) * step[0], layer as f32 * step[1], centred(row) * step[2]];
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
            CopyLayout::Line => laid([index as f32 * step[0], 0.0, 0.0], self.turn),
            CopyLayout::Ring => {
                // Up to Columns copies round each ring, spread evenly round
                // the whole circle; further rings stack up. The radius is the
                // full ring's, so a part-filled ring is the same size with
                // its copies further apart.
                let (slot, ring) = (index % columns, index / columns);
                let on_ring = self.count.saturating_sub(ring * columns).clamp(1, columns);
                let angle = std::f32::consts::TAU * slot as f32 / on_ring as f32;
                let radius = ring_radius(columns, step[0], width[2]);
                let offset = [radius * angle.cos(), ring as f32 * step[1], radius * angle.sin()];
                // Each copy's X runs along the ring, so neighbours meet side to side.
                let (sin, cos) = (-(angle + std::f32::consts::FRAC_PI_2) * 0.5).sin_cos();
                let yaw = [0.0, sin, 0.0, cos];
                BodyPose {
                    position: place(self.origin, group, offset),
                    rotation: compose(group, compose(yaw, euler_quat(self.turn))),
                }
            }
            CopyLayout::Spiral => {
                // An Archimedean arm whose turns sit two steps apart: each copy
                // is a step further along it (arc length ≈ arm·angle²/2), and the
                // arm rises a step each turn.
                let arm = 2.0 * step[0] / std::f32::consts::TAU;
                let along = (index as f32 + 0.5) * step[0];
                let angle = (2.0 * along / arm).sqrt();
                let offset = [arm * angle * angle.cos(), angle / std::f32::consts::TAU * step[1], arm * angle * angle.sin()];
                laid(offset, self.turn)
            }
            CopyLayout::Random => {
                // Anywhere in the cell Scatter's pile gives the copy.
                let [column, layer, row] = pile_cell(index, columns);
                let offset = [
                    (centred(column) + index_hash_unit(index, 6) - 0.5) * step[0],
                    (layer as f32 + index_hash_unit(index, 7)) * step[1],
                    (centred(row) + index_hash_unit(index, 8) - 0.5) * step[2],
                ];
                laid(offset, self.turn)
            }
        }
    }
}

impl Arrangement {
    /// The world-aligned box holding every copy whose own box is `bounds`
    /// (about its origin), each at its rest pose; `None` with no copies.
    pub fn rest_box(&self, bounds: ([f32; 3], [f32; 3])) -> Option<([f32; 3], [f32; 3])> {
        let (lo, hi) = bounds;
        let width: [f32; 3] = std::array::from_fn(|axis| hi[axis] - lo[axis]);
        let centre: [f32; 3] = std::array::from_fn(|axis| 0.5 * (lo[axis] + hi[axis]));
        let half: [f32; 3] = width.map(|w| 0.5 * w.abs());
        let mut found: Option<([f32; 3], [f32; 3])> = None;
        for index in 0..self.count {
            let pose = self.rest(width, index);
            let at = place(pose.position, pose.rotation, centre);
            let axes = [[half[0], 0.0, 0.0], [0.0, half[1], 0.0], [0.0, 0.0, half[2]]]
                .map(|axis| if is_identity(pose.rotation) { axis } else { rotate(pose.rotation, axis) });
            let reach: [f32; 3] = std::array::from_fn(|k| axes.iter().map(|axis| axis[k].abs()).sum());
            let (min, max) = found.get_or_insert((at, at));
            for k in 0..3 {
                min[k] = min[k].min(at[k] - reach[k]);
                max[k] = max[k].max(at[k] + reach[k]);
            }
        }
        found
    }
}

/// Slot `k`'s offset, in steps, on a line filled from the centre out: 0, +1,
/// -1, +2, ... A pile's footprint grows that way round the group's origin.
fn centred(k: usize) -> f32 {
    let reach = k.div_ceil(2) as f32;
    if k.is_multiple_of(2) { -reach } else { reach }
}

/// Copy `index`'s cell, [column, layer, row], in a pile that grows as a cube:
/// each shell wraps the cube before it, so the first n³ copies fill an
/// n-cube whatever Count is. Past `side` the pile keeps its footprint and
/// stacks whole layers.
fn pile_cell(index: usize, side: usize) -> [usize; 3] {
    let cube = side * side * side;
    if index >= cube {
        let rest = index - cube;
        let within = rest % (side * side);
        return [within % side, side + rest / (side * side), within / side];
    }
    let mut shell = (index as f64).cbrt() as usize;
    while (shell + 1).pow(3) <= index {
        shell += 1;
    }
    while shell.pow(3) > index {
        shell -= 1;
    }
    // Shell k: the cells of the (k+1)-cube whose largest coordinate is k.
    let k = shell;
    let mut j = index - k * k * k;
    if j < (k + 1) * (k + 1) {
        return [k, j / (k + 1), j % (k + 1)];
    }
    j -= (k + 1) * (k + 1);
    if j < k * (k + 1) {
        return [j % k, k, j / k];
    }
    j -= k * (k + 1);
    [j % k, j / k, k]
}

/// Neighbours sit `step` apart round a ring of `slots`, or further when the
/// copies' inner corners would otherwise meet: at Spacing 1.0 they touch,
/// never overlap.
fn ring_radius(slots: usize, step: f32, depth: f32) -> f32 {
    if slots < 2 {
        return 0.0;
    }
    let half_angle = std::f32::consts::PI / slots as f32;
    let chord = step / (2.0 * half_angle.sin());
    let inner = 0.5 * depth + 0.5 * step * half_angle.cos() / half_angle.sin();
    chord.max(inner)
}

/// A copy's box width per axis at its scale, metres, measured from the
/// points the world installs as its hull (a hull's extreme points are among
/// its inputs, so the box is the installed hull's).
pub(crate) fn copy_width(prototype: &RigidBody) -> [f32; 3] {
    let (lo, hi) = copy_bounds(prototype);
    std::array::from_fn(|axis| hi[axis] - lo[axis])
}

/// A copy's box about its own origin at its scale, metres: the lowest and
/// highest corner of the installed hull's points. An axis with no extent
/// yet (no shape, or a flat one) is the scale's width centred on the origin.
fn copy_bounds(prototype: &RigidBody) -> ([f32; 3], [f32; 3]) {
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
    if let Some(solid) = prototype.collider.as_deref() {
        solid.for_each_point(&mut grow);
    }
    for axis in 0..3 {
        let width = maximum[axis] - minimum[axis];
        if !(width.is_finite() && width > 0.0) {
            (minimum[axis], maximum[axis]) = (-0.5 * scale[axis], 0.5 * scale[axis]);
        }
    }
    (minimum, maximum)
}

/// The world-aligned box a copy group rests in: every copy's box turned and
/// placed at its rest pose, never where physics has moved it. The Scene
/// Loop sizes its cell by it. Measured again only when the layout or the
/// shape changes, so a still group costs one compare a frame.
#[derive(Default)]
pub(crate) struct RestBox {
    measured: Option<(Arrangement, [u32; 3])>,
    shape: Option<Arc<Solid>>,
    value: Option<([f32; 3], [f32; 3])>,
}

impl RestBox {
    /// The rest box of `prototype`'s copies, `None` with none.
    pub fn of(&mut self, prototype: &RigidBody) -> Option<([f32; 3], [f32; 3])> {
        let arrangement = Arrangement::of(prototype, requested_count(prototype.spawn));
        let key = (arrangement, prototype.transform.scale.map(f32::to_bits));
        let same_shape = match (&self.shape, &prototype.collider) {
            (None, None) => true,
            (Some(held), Some(now)) => Arc::ptr_eq(held, now),
            _ => false,
        };
        if !same_shape || self.measured != Some(key) {
            self.value = arrangement.rest_box(copy_bounds(prototype));
            self.measured = Some(key);
            self.shape.clone_from(&prototype.collider);
        }
        self.value
    }
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
