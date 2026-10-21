//! One copy group of a Physics World: the bodies it made from its copies
//! body, and the layout they started in. Every group shares the world's
//! copy pool of `MAX_COPIES`.

use manifold_node_engine::scene::transform::Transform;
use manifold_physics::BodyHandle;

use super::{CopySpawn, DeferredAnimatedEdit, RigidBody, MAX_COPIES};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CopyLayout {
    #[default]
    Grid,
    Pile,
}

impl CopyLayout {
    pub(crate) fn from_scalar(value: f32) -> Self {
        if value.round().clamp(0.0, 1.0) >= 1.0 {
            Self::Pile
        } else {
            Self::Grid
        }
    }
}

/// The layout a group's copies were installed in, read from its spawner on
/// first build, reset or a backwards seek.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct CopyLatch {
    pub count: usize,
    pub spacing: f32,
    pub columns: usize,
    pub layout: CopyLayout,
}

impl Default for CopyLatch {
    fn default() -> Self {
        Self { count: 0, spacing: 1.25, columns: 16, layout: CopyLayout::Grid }
    }
}

/// One group's installed copies. Its vectors are sized when the world is
/// built, so a steady tick never allocates.
#[derive(Default)]
pub(crate) struct CopyGroup {
    pub handles: Vec<Option<BodyHandle>>,
    pub bullet_enabled: Vec<bool>,
    pub poses: Vec<Transform>,
    pub description: Option<RigidBody>,
    /// Every copy's hull volume: copies share the prototype's shape and scale.
    pub volume: f32,
    pub deferred_edit: Option<DeferredAnimatedEdit>,
    pub latch: CopyLatch,
}

impl CopyGroup {
    pub fn active(&self) -> usize {
        self.latch.count
    }

    /// Copy `index`'s authored body under the latched layout.
    pub fn copy_at(&self, prototype: &RigidBody, index: usize) -> RigidBody {
        copy_transform_for_layout(
            prototype.clone(),
            prototype.transform.pos,
            index,
            self.latch.count,
            self.latch.columns,
            self.latch.spacing,
            self.latch.layout,
        )
    }
}

/// What a group's spawner asks for now, before the world pool is shared.
pub(crate) fn requested_latch(spawn: CopySpawn) -> CopyLatch {
    CopyLatch {
        count: spawn.count.round().clamp(0.0, MAX_COPIES as f32) as usize,
        spacing: spawn.spacing,
        columns: spawn.columns.round().clamp(1.0, 64.0) as usize,
        layout: CopyLayout::from_scalar(spawn.layout),
    }
}

pub(crate) fn validate_spawn(spawn: CopySpawn) -> Result<(), String> {
    if !spawn.count.is_finite()
        || !spawn.spacing.is_finite()
        || spawn.spacing <= 0.0
        || !spawn.columns.is_finite()
        || !spawn.layout.is_finite()
    {
        return Err("Physics: copy count, spacing, columns, and layout must be finite; spacing must be positive".into());
    }
    Ok(())
}

pub(crate) fn validate_copy_prototype(prototype: &RigidBody) -> Result<(), String> {
    if prototype.shape >= 5
        || prototype.kind > 2
        || prototype.transform.billboard
        || prototype
            .transform
            .scale
            .iter()
            .any(|s| !s.is_finite() || *s <= 0.0 || *s > 100.0)
    {
        return Err("Physics: copy prototype needs a Platonic shape, positive scale up to 100, and no billboard".into());
    }
    let scale = prototype.transform.scale[0];
    if prototype
        .transform
        .scale
        .iter()
        .any(|axis| (*axis - scale).abs() > 1.0e-5)
    {
        return Err("Physics: copy prototype scale must be uniform".into());
    }
    Ok(())
}

pub(crate) fn copy_transform_for_layout(
    mut copy: RigidBody,
    origin: [f32; 3],
    index: usize,
    count: usize,
    columns: usize,
    spacing: f32,
    layout: CopyLayout,
) -> RigidBody {
    copy.transform.pos = match layout {
        CopyLayout::Grid => copy_position(origin, index, count, columns, spacing),
        CopyLayout::Pile => pile_position(origin, index, count, columns, spacing),
    };
    if layout == CopyLayout::Pile {
        let offsets = pile_rotation(index);
        for (axis, offset) in offsets.into_iter().enumerate() {
            copy.transform.rot_euler[axis] += offset;
        }
    }
    copy
}

pub(crate) fn pile_position(
    origin: [f32; 3],
    index: usize,
    count: usize,
    copy_columns: usize,
    spacing: f32,
) -> [f32; 3] {
    let columns = ceil_cuberoot(count).min(copy_columns.max(1));
    let column = index % columns;
    let row = (index / columns) % columns;
    let layer = index / (columns * columns);
    let center = (columns as f32 - 1.0) * 0.5;
    [
        origin[0] + (column as f32 - center) * spacing + bounded_jitter(index, 0, spacing),
        origin[1] + layer as f32 * spacing + bounded_jitter(index, 1, spacing),
        origin[2] + (row as f32 - center) * spacing + bounded_jitter(index, 2, spacing),
    ]
}

pub(crate) fn ceil_cuberoot(count: usize) -> usize {
    let mut columns: usize = 1;
    while columns.saturating_mul(columns).saturating_mul(columns) < count {
        columns += 1;
    }
    columns
}

pub(crate) fn pile_rotation(index: usize) -> [f32; 3] {
    [
        full_turn_hash(index, 3),
        full_turn_hash(index, 4),
        full_turn_hash(index, 5),
    ]
}

fn bounded_jitter(index: usize, axis: u32, spacing: f32) -> f32 {
    (index_hash_unit(index, axis) * 2.0 - 1.0) * 0.04 * spacing
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

pub(crate) fn copy_position(
    origin: [f32; 3],
    index: usize,
    count: usize,
    columns: usize,
    spacing: f32,
) -> [f32; 3] {
    let rows = count.div_ceil(columns).min(columns);
    let column = index % columns;
    let row = (index / columns) % columns;
    let layer = index / (columns * columns);
    [
        origin[0] + (column as f32 - (columns as f32 - 1.0) * 0.5) * spacing,
        origin[1] + layer as f32 * spacing,
        origin[2] + (row as f32 - (rows as f32 - 1.0) * 0.5) * spacing,
    ]
}
