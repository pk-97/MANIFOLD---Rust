//! Where each copy of a copy group rests, for the world: the layout maths
//! is `manifold_core::copy_layout`; this adds what needs the body itself,
//! its box width from the installed hull and the rest box the Scene Loop
//! measures. Body copies keep their motion through a Count change; only
//! their rest places move. The card and the gizmo show the group, never a
//! copy.

use manifold_physics::BodyPose;

use std::sync::Arc;

pub(crate) use manifold_core::copy_layout::{Arrangement, CopyLayout};

use super::{CopySpawn, MAX_COPIES, RigidBody, Solid};

/// The copies `spawn` asks for, before the world's copy pool is shared.
pub(crate) fn requested_count(spawn: CopySpawn) -> usize {
    spawn.count.round().clamp(0.0, MAX_COPIES as f32) as usize
}

/// `prototype`'s arrangement of `count` copies.
pub(crate) fn arrangement_of(prototype: &RigidBody, count: usize) -> Arrangement {
    let spawn = prototype.spawn;
    Arrangement {
        layout: CopyLayout::from_value(spawn.layout),
        count,
        columns: spawn.columns.round().clamp(1.0, 64.0) as usize,
        spacing: spawn.spacing,
        turn: spawn.turn,
        origin: prototype.transform.pos,
        rotation: prototype.transform.rot_euler,
    }
}

/// Copy `index`'s rest pose in `arrangement` for a copy whose box is
/// `width` metres, as the solver takes it.
pub(crate) fn rest_pose(arrangement: &Arrangement, width: [f32; 3], index: usize) -> BodyPose {
    let rest = arrangement.rest(width, index);
    BodyPose {
        position: rest.position,
        rotation: rest.rotation,
    }
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
            let half = 0.5 * scale[axis].abs();
            (minimum[axis], maximum[axis]) = (-half, half);
        }
    }
    (minimum, maximum)
}

/// A box as its lowest and highest corner, metres.
type Corners = ([f32; 3], [f32; 3]);

/// One copy's box width and the box the whole group rests in, in the
/// group's own frame (about its origin, unturned): every copy's box turned
/// and placed at its rest pose, never where physics has moved it. The Scene
/// Loop sizes its cell by the group box placed by the group's frame; Copy
/// Blend lays its target out with the copy's. The copy's box is measured
/// again only when the shape or scale changes, the group box when the
/// layout does, so a group that moves or turns costs two compares a frame.
#[derive(Default)]
pub(crate) struct RestBox {
    shape: Option<Arc<Solid>>,
    scale: Option<[u32; 3]>,
    bounds: Corners,
    measured: Option<Arrangement>,
    value: Option<Corners>,
    #[cfg(test)]
    pub(crate) measures: usize,
}

impl RestBox {
    /// `prototype`'s copy box width, and the box its copies rest in, in the
    /// group's own frame (`None` with no copies).
    pub fn of(&mut self, prototype: &RigidBody) -> ([f32; 3], Option<Corners>) {
        let scale = prototype.transform.scale.map(f32::to_bits);
        let same_shape = match (&self.shape, &prototype.collider) {
            (None, None) => true,
            (Some(held), Some(now)) => Arc::ptr_eq(held, now),
            _ => false,
        };
        if !same_shape || self.scale != Some(scale) {
            self.bounds = copy_bounds(prototype);
            self.scale = Some(scale);
            self.shape.clone_from(&prototype.collider);
            self.measured = None;
        }
        let arrangement = Arrangement {
            origin: [0.0; 3],
            rotation: [0.0; 3],
            ..arrangement_of(prototype, requested_count(prototype.spawn))
        };
        if self.measured != Some(arrangement) {
            self.value = arrangement.rest_box(self.bounds);
            self.measured = Some(arrangement);
            #[cfg(test)]
            {
                self.measures += 1;
            }
        }
        let (lo, hi) = self.bounds;
        (std::array::from_fn(|axis| hi[axis] - lo[axis]), self.value)
    }
}
