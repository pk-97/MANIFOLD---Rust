//! What the matter atoms share at run time: the Bounds walls shader.

/// The Bounds box's Wall faces as placed, applied to nodes on or past each plane.
pub(super) const MATTER_BOUNDS: &str = include_str!("shaders/matter_bounds.wgsl");

pub(super) use manifold_water_liquid::bodies::COLLIDER_BVH;
/// Static-collider (tree shape) distance, gradient and projection over it.
pub(super) const MATTER_TREE: &str = include_str!("shaders/matter_tree.wgsl");
