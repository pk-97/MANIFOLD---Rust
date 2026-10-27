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
    if !(1.0..=MAX_POOL_TILES as f32).contains(&pool_tiles) {
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
    if value.fract() == 0.0 && (1.0..=MAX_POOL_TILES as f32).contains(&value) {
        Ok(value as u32)
    } else {
        Err(format!(
            "pool_tiles is {value}; it must be a whole tile count from 1 to {MAX_POOL_TILES}"
        ))
    }
}

/// Pools are counted on f32 wires, which hold whole numbers exactly up to 2²⁴.
pub const MAX_POOL_TILES: u32 = 16_777_216;

/// A pool that follows its water grows in whole grains of tiles.
const POOL_GRAIN: u32 = 256;
/// It grows once the held tiles fill this share of it, to `POOL_HEADROOM`
/// times them: they are read a frame or two late.
const GROW_AT: f64 = 2.0 / 3.0;
const POOL_HEADROOM: u32 = 2;

/// The pool that fits `held` tiles: `POOL_HEADROOM` times them in whole
/// grains, at most what a wire counts.
pub fn fitted_pool(held: u32) -> u32 {
    held.max(1)
        .saturating_mul(POOL_HEADROOM)
        .div_ceil(POOL_GRAIN)
        .saturating_mul(POOL_GRAIN)
        .min(MAX_POOL_TILES)
}

/// The pool to grow to from a table of `pool` tiles that held `held` of them
/// and found its free stack empty when `short`: `None` while it fits.
pub fn grown_pool(pool: u32, held: u32, short: bool) -> Option<u32> {
    let crowded = f64::from(held) > f64::from(pool) * GROW_AT;
    if !short && !crowded {
        return None;
    }
    let wanted = fitted_pool(held.max(if short { pool } else { 0 }));
    Some(wanted.max(pool)).filter(|&grown| grown > pool)
}

/// Bytes the pooled faces and the world-tile table must hold for a pool of
/// `pool_tiles` slots.
pub fn pooled_bytes(pool_tiles: u32) -> (u64, u64) {
    (
        pool_len(pool_tiles) * size_of::<FaceSample>() as u64,
        table_words(pool_tiles) * 4,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pool_grows_ahead_of_the_held_tiles() {
        // Two thirds full or less: it fits.
        assert_eq!(grown_pool(1024, 0, false), None);
        assert_eq!(grown_pool(1024, 682, false), None);
        // Past two thirds: twice the held tiles in grains of 256.
        assert_eq!(grown_pool(1024, 700, false), Some(1536));
        assert_eq!(grown_pool(1536, 1100, false), Some(2304));
        // Short of a slot: at least twice the whole pool.
        assert_eq!(grown_pool(1024, 1024, true), Some(2048));
        assert_eq!(grown_pool(1024, 10, true), Some(2048));
        // Never past what a wire counts.
        assert_eq!(
            grown_pool(MAX_POOL_TILES - 1, MAX_POOL_TILES - 1, true),
            Some(MAX_POOL_TILES)
        );
        assert_eq!(grown_pool(MAX_POOL_TILES, MAX_POOL_TILES, true), None);
    }

    #[test]
    fn a_fitted_pool_is_twice_the_held_tiles_in_grains() {
        assert_eq!(fitted_pool(0), 256);
        assert_eq!(fitted_pool(128), 256);
        assert_eq!(fitted_pool(129), 512);
        assert_eq!(fitted_pool(640), 1280);
        assert_eq!(fitted_pool(1632), 3328);
        assert_eq!(fitted_pool(u32::MAX), MAX_POOL_TILES);
    }
}
