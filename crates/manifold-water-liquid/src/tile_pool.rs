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
/// It shrinks once the held tiles are under this share of it.
const SHRINK_UNDER: f64 = 1.0 / 3.0;

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

/// The pool to shrink to from a table of `pool` tiles that held `held` of
/// them: [`fitted_pool`] of them, never below `floor`, and `None` while they
/// fill a third of it or more.
pub fn shrunk_pool(pool: u32, held: u32, floor: u32) -> Option<u32> {
    if f64::from(held) >= f64::from(pool) * SHRINK_UNDER {
        return None;
    }
    Some(fitted_pool(held).max(floor)).filter(|&shrunk| shrunk < pool)
}

/// Bytes a pool that follows its liquid may grow to: the D4 live budget
/// (docs/GPU_FLIP_END_GAME_DESIGN.md), 1.5 GiB.
pub const POOL_BUDGET_BYTES: u64 = 3 << 29;

/// Tiles a pool of `budget` bytes holds when a tile costs `tile_bytes`, at
/// most the `wanted` tiles.
pub fn pool_capacity(wanted: u64, tile_bytes: u64, budget: u64) -> u32 {
    wanted.min(budget / tile_bytes) as u32
}

/// The pool a tick runs on and whether it is fitted yet, from the domain's
/// pool `start`, the most it may grow to `ceiling`, the last tick's pool and
/// fit (`None` before the first tick) and its table's late read (held tiles,
/// short). The first tick runs on the domain's pool, so a fill in the box
/// fits; the first read that holds liquid fits the pool to it, and from then
/// on it only grows, so a restart's fill finds the room the first one had.
/// Never past the ceiling.
pub fn next_pool(
    start: u32,
    ceiling: u32,
    last: Option<(u32, bool)>,
    read: Option<(u32, bool)>,
) -> (u32, bool) {
    let (pool, fitted) = match (last, read) {
        (None, _) => (start, false),
        (Some((pool, true)), Some((held, short))) => {
            (grown_pool(pool, held, short).unwrap_or(pool), true)
        }
        (Some((_, false)), Some((held, _))) if held > 0 => (fitted_pool(held), true),
        (Some(last), _) => last,
    };
    (pool.min(ceiling), fitted)
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

    /// The first tick runs on the ceiling; the first read holding water fits
    /// the pool to twice it, a read with none keeps the pool; after the fit the
    /// pool only grows, past two thirds or short, and never past the ceiling.
    #[test]
    fn the_pool_fits_the_water_then_only_grows() {
        assert_eq!(next_pool(2744, 2744, None, None), (2744, false));
        assert_eq!(
            next_pool(2744, 2744, Some((2744, false)), Some((0, false))),
            (2744, false)
        );
        assert_eq!(
            next_pool(2744, 2744, Some((2744, false)), Some((640, false))),
            (1280, true)
        );
        // A tank whose fill holds most of the ceiling stays at the ceiling.
        assert_eq!(
            next_pool(2744, 2744, Some((2744, false)), Some((2000, false))),
            (2744, true)
        );
        assert_eq!(
            next_pool(2744, 2744, Some((1280, true)), Some((853, false))),
            (1280, true)
        );
        assert_eq!(
            next_pool(2744, 2744, Some((1280, true)), Some((854, false))),
            (1792, true)
        );
        // Water drains: the fitted pool keeps its room for the next fill.
        assert_eq!(
            next_pool(2744, 2744, Some((1792, true)), Some((10, false))),
            (1792, true)
        );
        assert_eq!(
            next_pool(2744, 2744, Some((1792, true)), None),
            (1792, true)
        );
        assert_eq!(
            next_pool(2744, 2744, Some((1792, true)), Some((10, true))),
            (2744, true)
        );
        // A box made smaller takes the pool down with it.
        assert_eq!(
            next_pool(1024, 1024, Some((1792, true)), Some((300, false))),
            (1024, true)
        );
        // With nothing bounding the water the first tick runs on the domain's
        // pool and the pool grows past it as the water spreads.
        assert_eq!(next_pool(2744, 5000, None, None), (2744, false));
        assert_eq!(
            next_pool(2744, 5000, Some((2744, true)), Some((2600, true))),
            (5000, true)
        );
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

    #[test]
    fn the_pool_shrinks_to_twice_the_held_tiles_once_under_a_third() {
        // A third full or more: it stays.
        assert_eq!(shrunk_pool(30_000, 10_000, 1024), None);
        // Under a third: twice the held tiles in grains, and the shrunk pool
        // does not grow again at once (half full is under two thirds).
        assert_eq!(shrunk_pool(30_000, 9_999, 1024), Some(20_224));
        assert_eq!(grown_pool(20_224, 9_999, false), None);
        assert_eq!(shrunk_pool(12_288, 700, 1024), Some(1536));
        // Never below the floor, and never "shrinks" to the same size.
        assert_eq!(shrunk_pool(4096, 0, 1024), Some(1024));
        assert_eq!(shrunk_pool(1024, 0, 1024), None);
    }
}
