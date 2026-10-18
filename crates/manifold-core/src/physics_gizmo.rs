//! The physics setup shapes a simulation used on a frame, for the Scene
//! panel's gizmo overlay (`docs/REALTIME_3D_DESIGN.md` D7a). Read-only
//! observations in world space; they never feed the render.

/// What a shape belongs to; each draws in its own color.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhysicsGizmoFamily {
    Force,
    Collider,
    /// A rigid body coupled to the liquid.
    Body,
    Source,
    Drain,
    /// The starting water: the pool under the fill height and the initial
    /// column.
    Fill,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PhysicsGizmoShape {
    /// An oriented box: centre, half extents, and the unit quaternion (xyzw)
    /// that turns its local axes into world ones.
    Box { center: [f32; 3], half: [f32; 3], rotation: [f32; 4] },
    /// A sphere around a position marker.
    Sphere { center: [f32; 3], radius: f32 },
    Arrow { from: [f32; 3], to: [f32; 3] },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PhysicsGizmo {
    pub family: PhysicsGizmoFamily,
    pub shape: PhysicsGizmoShape,
}

impl PhysicsGizmo {
    /// An axis-aligned box from its corners.
    pub fn aabb(family: PhysicsGizmoFamily, min: [f32; 3], max: [f32; 3]) -> Self {
        let center = std::array::from_fn(|i| 0.5 * (min[i] + max[i]));
        let half = std::array::from_fn(|i| 0.5 * (max[i] - min[i]));
        Self { family, shape: PhysicsGizmoShape::Box { center, half, rotation: [0.0, 0.0, 0.0, 1.0] } }
    }
}
