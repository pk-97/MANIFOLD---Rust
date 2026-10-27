//! What the matter atoms share at run time: the Bounds walls shader.

/// The Bounds box's Wall faces as placed, applied to nodes on or past each plane.
pub(super) const MATTER_BOUNDS: &str = include_str!("shaders/matter_bounds.wgsl");
