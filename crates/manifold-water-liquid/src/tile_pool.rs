//! Where a pooled atom's lattice arrays live: a world-tile table (`tiles`,
//! `pool_tiles`) at `cell_size`, the liquid step's or the display's. Every
//! array holds `pool_len(P)` elements, world cell p's at its pool index, the
//! last one canonical. A cell array holds cell p's value at its centre p·h; a
//! node array (solid, influence, obstacle source, the display's level set)
//! holds the value at cell p's low corner (p − ½)·h, so node n of a grid is
//! element n as cell n of it is.

use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::scene::world_tiles::{pool_len, table_words};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TilePool {
    pub pool_tiles: u32,
    pub cell_size: f32,
}

impl TilePool {
    /// Elements of every array on the pool.
    pub fn elements(self) -> u64 {
        pool_len(self.pool_tiles)
    }

    /// Bytes of the table the arrays are read through.
    pub fn table_bytes(self) -> u64 {
        table_words(self.pool_tiles) * 4
    }
}

/// A pooled atom's pool: `pool_tiles` and `cell_size`, wire else param, with
/// the `tiles` input holding its table. `node` names the node in the refusal.
pub fn tile_pool(ctx: &EffectNodeContext<'_, '_>, node: &str) -> Result<TilePool, String> {
    let pool_tiles = ctx.scalar_or_param("pool_tiles", 1.0).round();
    if !(1.0..=16_777_216.0).contains(&pool_tiles) {
        return Err(format!(
            "{node}: a pool of {pool_tiles} tiles is not a pool"
        ));
    }
    let cell_size = ctx.scalar_or_param("cell_size", 0.0625);
    if !(cell_size > 0.0 && cell_size.is_finite()) {
        return Err(format!("{node}: cell size {cell_size} is not a length"));
    }
    let pool = TilePool {
        pool_tiles: pool_tiles as u32,
        cell_size,
    };
    match ctx.inputs.array("tiles") {
        Some(tiles) if tiles.size >= pool.table_bytes() => Ok(pool),
        Some(_) => Err(format!(
            "{node}: the tiles hold less than a pool of {} tiles' table",
            pool.pool_tiles
        )),
        None => Err(format!("{node}: needs its world tiles")),
    }
}
