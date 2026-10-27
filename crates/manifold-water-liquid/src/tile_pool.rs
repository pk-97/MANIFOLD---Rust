//! Where a pooled atom's lattice arrays live: a world-tile table (`tiles`,
//! `pool_tiles`) at `cell_size`, the liquid step's or the display's. Every
//! array holds `pool_len(P)` elements, world cell p's at its pool index, the
//! last one canonical. A cell array holds cell p's value at its centre p·h; a
//! node array (solid, influence, obstacle source, the display's level set)
//! holds the value at cell p's low corner (p − ½)·h, so node n of a grid is
//! element n as cell n of it is.

use crate::fluid_particles::FaceSample;
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

/// The world cell of a solver grid's first cell, from the grid's first node
/// (its minimum corner) and cell size. Cell p is centred on p·h (the lattice
/// origin is -h/2), so the first cell's centre, half a cell in from the first
/// node, is p·h. The grid a domain publishes sits on whole world cells; the
/// rounding only absorbs f32 noise.
pub fn solver_origin_cell(first_node: [f32; 3], cell_size: f32) -> [i32; 3] {
    first_node.map(|x| (x / cell_size + 0.5).round() as i32)
}

/// The `pool_tiles` wire, else its param: the pool's capacity in tiles.
pub fn pool_tiles_wire(read: impl Fn(&str, f32) -> f32) -> Result<u32, String> {
    let value = read("pool_tiles", 1.0);
    if value.fract() == 0.0 && (1.0..=MAX_POOL_TILES).contains(&value) {
        Ok(value as u32)
    } else {
        Err(format!(
            "pool_tiles is {value}; it must be a whole tile count from 1 to {MAX_POOL_TILES}"
        ))
    }
}

const MAX_POOL_TILES: f32 = 16_777_216.0;

/// Bytes the pooled faces and the world-tile table must hold for a pool of
/// `pool_tiles` slots.
pub fn pooled_bytes(pool_tiles: u32) -> (u64, u64) {
    (
        pool_len(pool_tiles) * size_of::<FaceSample>() as u64,
        table_words(pool_tiles) * 4,
    )
}
