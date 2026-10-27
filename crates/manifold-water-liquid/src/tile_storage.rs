//! The tile sizes the water's pooled fields are laid out by, and the dense
//! grid helpers (`tile_grid.wgsl`) for arrays that stay dense boxes. A pooled
//! field is read through the world tiles' read side
//! (`manifold_node_engine::scene::world_tiles::READ_SIDE`), whose module owns
//! these sizes.

pub use manifold_node_engine::scene::world_tiles::{
    CANON_ELEMENTS, TILE, TILE_CELLS, pool_len, table_entries, table_words,
};

/// The dense grid helpers (`ts_grid_*`); they need nothing from the includer
/// and compose beside the world tiles' read side.
pub const TILE_GRID: &str = include_str!("primitives/shaders/tile_grid.wgsl");

#[cfg(test)]
mod tests {
    use super::*;

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
