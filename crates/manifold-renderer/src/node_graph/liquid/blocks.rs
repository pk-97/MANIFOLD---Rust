//! The block occupancy map (`docs/LIQUID_SOLVER_SEAM_DESIGN.md` section 3.10
//! (block occupancy map)): one u32 per 4³ block of a liquid's cells. A set
//! bit means the block may hold that thing; a clear bit guarantees it does
//! not. Consumers skip work whose result a clear bit already tells them.

/// Cells per block side.
pub const LIQUID_BLOCK_CELLS: u32 = 4;
/// Some cell in the block is liquid under the solver's own rule.
pub const BLOCK_LIQUID: u32 = 1;
/// The level set has a node `< 0` and a node `>= 0` in the block's closed footprint.
pub const BLOCK_SURFACE: u32 = 2;
/// Some solid-lattice node in the block's closed footprint is `< 0`.
pub const BLOCK_SOLID: u32 = 4;

/// WGSL twin of the constants above, for every atom that reads or writes the map.
pub(crate) const LIQUID_BLOCKS_WGSL: &str = include_str!("../primitives/shaders/liquid_blocks.wgsl");

/// Blocks per axis over `cells` cells per axis; edge blocks are partial.
pub fn block_lattice(cells: [u32; 3]) -> [u32; 3] {
    cells.map(|n| n.div_ceil(LIQUID_BLOCK_CELLS))
}

/// Blocks over `cells`, in u64 so no size wraps.
pub fn block_total(cells: [u32; 3]) -> u64 {
    block_lattice(cells).iter().map(|&n| u64::from(n)).product()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wgsl_constants_match_rust() {
        for (name, value) in [
            ("LB_CELLS", LIQUID_BLOCK_CELLS),
            ("LB_LIQUID", BLOCK_LIQUID),
            ("LB_SURFACE", BLOCK_SURFACE),
            ("LB_SOLID", BLOCK_SOLID),
        ] {
            assert!(LIQUID_BLOCKS_WGSL.contains(&format!("const {name}: u32 = {value}u;")), "{name} drifted");
        }
    }

    #[test]
    fn edge_blocks_are_partial() {
        assert_eq!(block_lattice([8, 7, 6]), [2, 2, 2]);
        assert_eq!(block_total([64, 64, 64]), 4096);
    }
}
