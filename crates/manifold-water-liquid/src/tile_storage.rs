//! The storage helpers every water kernel indexes the lattice through
//! (`tile_storage.wgsl`). Cells are the solver's cells; records are one past
//! the cells on every axis. Today both are dense boxes, x fastest, then y,
//! then z; the tile pool later replaces the helpers' bodies, not their
//! callers. The including shader defines `ts_cells()`, the cell counts.

/// Cells per tile side (GPU_FLIP_SPARSE_BLOCKS_DESIGN.md section 3 (The tile
/// table)). Partial edge tiles are allowed: no lattice side rule.
pub const TILE: u32 = 8;

/// The dense grid helpers (`ts_grid_*`); they need nothing from the includer.
pub const TILE_GRID: &str = include_str!("primitives/shaders/tile_grid.wgsl");

/// The grid helpers plus the lattice helpers; the including shader defines
/// `ts_cells()`.
pub const TILE_STORAGE: &str = concat!(
    include_str!("primitives/shaders/tile_grid.wgsl"),
    "\n",
    include_str!("primitives/shaders/tile_storage.wgsl")
);

/// Tiles per axis, the last one partial when the side is not a multiple of
/// [`TILE`].
pub fn tile_counts(cells: [u32; 3]) -> [u32; 3] {
    cells.map(|n| n.div_ceil(TILE))
}

pub fn tile_total(cells: [u32; 3]) -> u64 {
    tile_counts(cells).iter().map(|&t| u64::from(t)).product()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_edge_tiles_count() {
        assert_eq!(tile_counts([17, 8, 1]), [3, 1, 1]);
        assert_eq!(tile_total([17, 8, 1]), 3);
    }

    #[test]
    fn storage_helpers_validate() {
        let shader = format!(
            "{TILE_STORAGE}\nfn ts_cells() -> vec3<i32> {{ return vec3<i32>(17, 8, 3); }}\n\
             @compute @workgroup_size(1) fn main() {{ _ = ts_list_cell(0u, 0u); }}\n"
        );
        let module = naga::front::wgsl::parse_str(&shader).expect("tile_storage.wgsl parses");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("tile_storage.wgsl validates");
    }

    #[test]
    fn grid_helpers_validate_and_index() {
        let shader = format!(
            "{TILE_GRID}\n@compute @workgroup_size(1) fn main() {{ \
             _ = ts_grid_index(vec3<i32>(1, 2, 3), vec3<i32>(4, 5, 6)); }}\n"
        );
        let module = naga::front::wgsl::parse_str(&shader).expect("tile_grid.wgsl parses");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("tile_grid.wgsl validates");

        fn grid_index(p: [i32; 3], d: [i32; 3]) -> u32 {
            (p[0] + d[0] * (p[1] + d[1] * p[2])) as u32
        }
        assert_eq!(grid_index([1, 2, 3], [4, 5, 6]), 69);
        assert_eq!(grid_index([3, 4, 5], [4, 5, 6]), 119);
    }
}
