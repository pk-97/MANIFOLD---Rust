//! The storage helpers every water kernel indexes the lattice through
//! (`tile_storage.wgsl`). Cells are the solver's cells; records are one past
//! the cells on every axis. A pooled field is stored per 8^3 tile in a pool of
//! `P` tiles plus a canonical block of [`CANON_RECORDS`] elements; the
//! including shader defines `ts_cells()` (the cell counts) and
//! `ts_pool_tiles()` (`P`) and declares the slot table `ts_table`.

/// Cells per tile side (GPU_FLIP_SPARSE_BLOCKS_DESIGN.md section 3 (The tile
/// table)). Partial edge tiles are allowed: no lattice side rule.
pub const TILE: u32 = 8;

/// Cells in one tile (`TILE`³): the stride of a pool slot.
pub const TILE_CELLS: u32 = 512;

/// Elements after the last slot of a pooled field: the canonical block, one
/// element per wall mask (`ts_canon`).
pub const CANON_RECORDS: u32 = 8;

/// Elements of a pooled field with room for `pool_tiles` tiles.
pub fn pool_len(pool_tiles: u32) -> u64 {
    u64::from(pool_tiles) * u64::from(TILE_CELLS) + u64::from(CANON_RECORDS)
}

/// The dense grid helpers (`ts_grid_*`); they need nothing from the includer.
pub const TILE_GRID: &str = include_str!("primitives/shaders/tile_grid.wgsl");

/// The grid helpers plus the lattice helpers; the including shader defines
/// `ts_cells()` and `ts_pool_tiles()` and declares `ts_table`.
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

    const STUB: &str = "fn ts_cells() -> vec3<i32> { return vec3<i32>(17, 8, 3); }\n\
        fn ts_pool_tiles() -> u32 { return 3u; }\n\
        @group(0) @binding(0) var<storage, read> ts_table: array<u32>;\n";

    #[test]
    fn storage_helpers_validate() {
        let shader = format!(
            "{TILE_STORAGE}\n{STUB}\
             @compute @workgroup_size(1) fn main() {{ \
             _ = ts_list_cell(0u, 0u) + ts_cell_at(vec3<i32>(1)) + ts_face(vec3<i32>(1)) \
             + ts_dense_cell(vec3<i32>(1)) + ts_corner(vec3<i32>(1)) + ts_pool_len() \
             + ts_canon(1u) + ts_wall_mask(vec3<i32>(1)); \
             _ = ts_cell_stored(vec3<i32>(1)) && ts_record_stored(vec3<i32>(1)); \
             _ = ts_cell_coord(0u) + ts_face_coord(0u) + ts_box_cell(0u) + ts_box_record(0u); }}\n"
        );
        let module = naga::front::wgsl::parse_str(&shader).expect("tile_storage.wgsl parses");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("tile_storage.wgsl validates");
    }

    const NONE: u32 = u32::MAX;
    const CELLS: [i32; 3] = [17, 8, 3];
    const POOL: u32 = 3;

    fn mirror_tile(p: [i32; 3]) -> usize {
        // Tiles per axis for 17 x 8 x 3 cells: 3 x 1 x 1.
        (p[0] >> 3) as usize
    }

    fn mirror_local(p: [i32; 3]) -> u32 {
        ((p[0] & 7) + 8 * ((p[1] & 7) + 8 * (p[2] & 7))) as u32
    }

    fn mirror_wall_mask(p: [i32; 3]) -> u32 {
        let mut mask = 0;
        for a in 0..3 {
            let mut other = p;
            other[a] = 0;
            let exists = (0..3).all(|b| other[b] < CELLS[b]);
            if exists && (p[a] == 0 || p[a] == CELLS[a]) {
                mask |= 1 << a;
            }
        }
        mask
    }

    fn mirror_cell_at(slot_of: &[u32], p: [i32; 3]) -> u32 {
        let slot = slot_of[mirror_tile(p)];
        if slot == NONE {
            return pool_len(POOL) as u32 - CANON_RECORDS;
        }
        slot * TILE_CELLS + mirror_local(p)
    }

    fn mirror_face(slot_of: &[u32], p: [i32; 3]) -> u32 {
        if (0..3).all(|a| p[a] < CELLS[a]) {
            let slot = slot_of[mirror_tile(p)];
            if slot != NONE {
                return slot * TILE_CELLS + mirror_local(p);
            }
        }
        pool_len(POOL) as u32 - CANON_RECORDS + mirror_wall_mask(p)
    }

    /// The helpers' index rules on a 17 x 8 x 3 lattice (three tiles, the
    /// last one partial) with the middle tile absent: stored cells at
    /// `slot * 512 + local`, an absent tile's cells and every record past
    /// the lattice in the canonical block.
    #[test]
    fn pool_indices_follow_the_slot_table() {
        assert_eq!(pool_len(POOL), 3 * 512 + 8);
        let canon = pool_len(POOL) as u32 - CANON_RECORDS;
        // Tile 0 in slot 2, tile 1 absent, tile 2 in slot 0.
        let slot_of = [2, NONE, 0];
        assert_eq!(
            mirror_cell_at(&slot_of, [3, 2, 1]),
            2 * 512 + 3 + 8 * (2 + 8)
        );
        assert_eq!(mirror_cell_at(&slot_of, [16, 7, 2]), 8 * (7 + 16));
        assert_eq!(mirror_cell_at(&slot_of, [9, 4, 1]), canon);
        assert_eq!(mirror_face(&slot_of, [0, 0, 0]), 2 * 512);
        assert_eq!(mirror_face(&slot_of, [9, 4, 1]), canon);
        // Record (9, 0, 0) of the absent tile: faces y and z sit on the
        // lower walls.
        assert_eq!(mirror_face(&slot_of, [9, 0, 0]), canon + 6);
        // Past the lattice on x only: the x face exists and is a wall.
        assert_eq!(mirror_face(&slot_of, [17, 3, 1]), canon + 1);
        assert_eq!(mirror_face(&slot_of, [4, 8, 1]), canon + 2);
        assert_eq!(mirror_face(&slot_of, [4, 3, 3]), canon + 4);
        // Past the lattice on two axes no face exists; the corner too.
        assert_eq!(mirror_face(&slot_of, [17, 8, 1]), canon);
        assert_eq!(mirror_face(&slot_of, [17, 8, 3]), canon);
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
