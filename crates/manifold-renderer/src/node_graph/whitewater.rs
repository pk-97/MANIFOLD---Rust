//! The whitewater grid and its records (`docs/GPU_WHITEWATER_DESIGN.md`
//! section 3.1 (Grids), section 3.3 (Atoms)). The grid is the frame's solid
//! lattice read as cells: `nodes − 1` cells a side from the solid lattice's
//! first node, so the solid sits on the cell corners and the refined level
//! set on s nodes per cell. Every grid atom takes the solid lattice's node
//! counts and derives the rest here, never assuming a resolution.

use crate::node_graph::channel_names::well_known;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::ports::{ChannelElementType, ChannelSpec, KnownItem};

/// Helpers every whitewater grid atom's body includes.
pub(crate) const WHITEWATER_COMMON: &str = include_str!("primitives/shaders/whitewater_common.wgsl");

/// The solid lattice's node counts a grid atom runs over: its `nodes_x/y/z`
/// inputs, else its params.
pub(crate) fn grid_nodes(ctx: &EffectNodeContext<'_, '_>) -> [u32; 3] {
    ["nodes_x", "nodes_y", "nodes_z"].map(|name| ctx.scalar_or_param(name, 71.0).round().max(0.0) as u32)
}

/// A cell's nearest surface crossing, the surface normal there, and the
/// liquid level at the cell centre. The distance to the crossing's tangent
/// plane is second-order in how far the crossing sits to the side of the
/// true nearest point; the distance to the point itself is first-order.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SurfaceCrossing {
    /// Grid cells from the grid's first node; [`NO_CROSSING`] on every axis
    /// when none is known.
    pub crossing: [f32; 3],
    /// The level set at the cell centre, metres, negative in the liquid.
    pub level: f32,
    /// Unit gradient of the level set at the crossing, pointing out of the
    /// liquid; zero when there is no crossing.
    pub normal: [f32; 3],
    pub pad0: f32,
}

pub const SURFACE_CROSSING_BYTES: u64 = std::mem::size_of::<SurfaceCrossing>() as u64;
const _: () = assert!(SURFACE_CROSSING_BYTES == 32);

/// Std430: each vec3 + f32 pair shares one 16-byte slot.
pub const SURFACE_CROSSING_SPECS: &[ChannelSpec] = &[
    ChannelSpec { name: well_known::CROSSING, ty: ChannelElementType::Vec3F },
    ChannelSpec { name: well_known::LEVEL, ty: ChannelElementType::F32 },
    ChannelSpec { name: well_known::NORMAL, ty: ChannelElementType::Vec3F },
    ChannelSpec { name: well_known::PAD0, ty: ChannelElementType::F32 },
];

impl KnownItem for SurfaceCrossing {
    const SPECS: &'static [ChannelSpec] = SURFACE_CROSSING_SPECS;
}

/// A lattice value and whether it is known (1) or still to extrapolate (0),
/// FLIP's valid flag carried beside the value.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct KnownValue {
    pub value: f32,
    pub known: f32,
}

const _: () = assert!(std::mem::size_of::<KnownValue>() == 8);

pub const KNOWN_VALUE_SPECS: &[ChannelSpec] = &[
    ChannelSpec { name: well_known::VALUE, ty: ChannelElementType::F32 },
    ChannelSpec { name: well_known::KNOWN, ty: ChannelElementType::F32 },
];

impl KnownItem for KnownValue {
    const SPECS: &'static [ChannelSpec] = KNOWN_VALUE_SPECS;
}

/// The `step` of each node.nearest_crossing pass, in order. A first pass at
/// 2 then two at 1 find the nearest stored crossing wherever three passes at
/// 1 settle on a neighbour's (GPU_WHITEWATER_DESIGN.md D4).
pub const SPREAD_STEPS: [f32; 3] = [2.0, 1.0, 1.0];

/// Grid cells past every real crossing: the coordinate of "none".
pub const NO_CROSSING: f32 = 1e6;

/// FLIP's material kinds, one u32 per whitewater cell.
pub const CELL_AIR: u32 = 0;
pub const CELL_LIQUID: u32 = 1;
pub const CELL_SOLID: u32 = 2;

/// Refined level-set nodes per grid cell the atoms support: the surface
/// group's Surface Detail range.
pub const MAX_REFINEMENT: u32 = 4;

/// Largest solid lattice side the atoms take.
pub const MAX_GRID_NODES: u32 = 4096;

/// Cells per axis of a whitewater grid over a solid lattice of `nodes`.
pub fn grid_cells(nodes: [u32; 3]) -> Option<[u32; 3]> {
    nodes.iter().all(|&n| (3..=MAX_GRID_NODES).contains(&n)).then(|| nodes.map(|n| n - 1))
}

/// Cells in a grid of `cells`, in u64 so no size wraps.
pub fn cell_total(cells: [u32; 3]) -> u64 {
    cells.iter().map(|&n| u64::from(n)).product()
}

/// Level-set nodes per grid cell: the same whole number s on every axis,
/// with `level_nodes = cells·s + 1`, or a named refusal.
pub fn refinement(nodes: [u32; 3], level_nodes: [u32; 3]) -> Result<u32, String> {
    let cells = grid_cells(nodes).ok_or_else(|| format!("a {nodes:?} solid lattice has too few or too many nodes"))?;
    let per_axis: Vec<Option<u32>> = (0..3)
        .map(|a| {
            let spans = level_nodes[a].checked_sub(1)?;
            (spans % cells[a] == 0).then_some(spans / cells[a])
        })
        .collect();
    match per_axis[..] {
        [Some(s), Some(y), Some(z)] if s == y && s == z && (1..=MAX_REFINEMENT).contains(&s) => Ok(s),
        _ => Err(format!(
            "a {level_nodes:?} level set is not a whole refinement of 1 to {MAX_REFINEMENT} of the {cells:?}-cell grid"
        )),
    }
}

/// The grid index of cell `c`, x fastest.
pub fn cell_index(cells: [u32; 3], c: [u32; 3]) -> usize {
    let [nx, ny, _] = cells.map(|n| n as usize);
    c[0] as usize + nx * (c[1] as usize + ny * c[2] as usize)
}

/// Cell `index` of a grid of `cells`.
pub fn cell_coords(cells: [u32; 3], index: usize) -> [u32; 3] {
    let [nx, ny, _] = cells.map(|n| n as usize);
    [index % nx, (index / nx) % ny, index / (nx * ny)].map(|n| n as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whitewater_grid_refinement_is_whole_and_equal() {
        assert_eq!(grid_cells([71; 3]), Some([70; 3]));
        assert_eq!(refinement([71; 3], [211; 3]), Ok(3));
        assert_eq!(refinement([9, 8, 7], [17, 15, 13]), Ok(2));
        assert!(refinement([71; 3], [212; 3]).unwrap_err().contains("whole refinement"));
        assert!(refinement([71; 3], [211, 141, 211]).is_err(), "unequal refinement");
        assert!(refinement([71; 3], [351; 3]).is_err(), "refinement past {MAX_REFINEMENT}");
        assert!(refinement([2, 71, 71], [4, 211, 211]).is_err(), "a grid needs three nodes a side");
        let cells = [7, 6, 5];
        for index in [0, 1, 41, 209] {
            assert_eq!(cell_index(cells, cell_coords(cells, index)), index);
        }
    }
}
