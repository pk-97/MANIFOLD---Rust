//! What the matter atoms share at run time: the walls shader. The lattice itself is
//! every liquid's (`liquid::lattice::LiquidLattice::from_wires`).

/// The lattice's closed walls, three padding nodes deep.
pub(super) const MATTER_WALLS: &str = include_str!("shaders/matter_walls.wgsl");
