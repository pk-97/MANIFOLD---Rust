//! Uses the particle id limit (256) from FLIP Fluids diffuseparticlesimulation.h `_diffuseParticleIDLimit` (MIT); see THIRD_PARTY_NOTICES.md.
//! The whitewater grid and its records. The grid atoms run on the liquid
//! step's world tiles (`WhitewaterPool`). The lifecycle still reads a dense
//! grid, the frame's solid lattice read as cells: `nodes − 1` cells a side
//! from the solid lattice's first node, the solid on the cell corners.

use manifold_node_engine::channel_names::well_known;
use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::ports::{ChannelElementType, ChannelSpec, KnownItem};

/// Helpers every whitewater grid atom's body includes.
pub const WHITEWATER_COMMON: &str = include_str!("primitives/shaders/whitewater_common.wgsl");

/// Where a whitewater atom's grid arrays live: the liquid step's world tiles
/// (`tiles`, `pool_tiles`) at `cell_size`. Every grid array holds
/// `pool_len(P)` elements, world cell p's at its pool index, the last one
/// canonical. A cell array holds cell p's value at its centre p·h; a node
/// array (solid, influence, obstacle source) holds the value at cell p's low
/// corner (p − ½)·h, so node n of a grid is element n as cell n of it is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WhitewaterPool {
    pub pool_tiles: u32,
    pub cell_size: f32,
}

impl WhitewaterPool {
    /// Elements of every grid array.
    pub fn elements(self) -> u64 {
        manifold_node_engine::scene::world_tiles::pool_len(self.pool_tiles)
    }

    /// Bytes of the table the arrays are read through.
    pub fn table_bytes(self) -> u64 {
        manifold_node_engine::scene::world_tiles::table_words(self.pool_tiles) * 4
    }
}

/// A pooled whitewater atom's pool: `pool_tiles` and `cell_size`, wire else
/// param, with the `tiles` input holding its table. `node` names the node in
/// the refusal.
pub fn whitewater_pool(
    ctx: &EffectNodeContext<'_, '_>,
    node: &str,
) -> Result<WhitewaterPool, String> {
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
    let pool = WhitewaterPool {
        pool_tiles: pool_tiles as u32,
        cell_size,
    };
    match ctx.inputs.array("tiles") {
        Some(tiles) if tiles.size >= pool.table_bytes() => Ok(pool),
        Some(_) => Err(format!(
            "{node}: the tiles hold less than a pool of {} tiles' table",
            pool.pool_tiles
        )),
        None => Err(format!("{node}: needs the liquid step's tiles")),
    }
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
    ChannelSpec {
        name: well_known::VALUE,
        ty: ChannelElementType::F32,
    },
    ChannelSpec {
        name: well_known::KNOWN,
        ty: ChannelElementType::F32,
    },
];

impl KnownItem for KnownValue {
    const SPECS: &'static [ChannelSpec] = KNOWN_VALUE_SPECS;
}

/// One slot of the GPU whitewater pool: a spawn record plus the id FLIP keeps per particle. A slot
/// of kind [`WHITEWATER_EMPTY`] holds no particle; one with lifetime ≤ 0 holds
/// a dead particle, still counted and retyped until the tick's removal, as
/// in FLIP.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct WhitewaterParticle {
    /// Scene metres, and seconds left.
    pub position_lifetime: [f32; 4],
    pub velocity: [f32; 3],
    /// Bubble 0, foam 1, spray 2, dust 4, empty [`WHITEWATER_EMPTY`].
    pub kind: u32,
    /// FLIP's per-particle id, 0 to [`WHITEWATER_ID_LIMIT`] − 1.
    pub id: u32,
    pub pad0: u32,
    pub pad1: u32,
    pub pad2: u32,
}

const _: () = assert!(std::mem::size_of::<WhitewaterParticle>() == 48);

/// The kind of a pool slot that holds no particle.
pub const WHITEWATER_EMPTY: u32 = 3;

/// FLIP's `_diffuseParticleIDLimit`: ids cycle through this many values.
pub const WHITEWATER_ID_LIMIT: u32 = 256;

/// Std430: position_lifetime Vec4F at 0, velocity Vec3F at 16, kind U32 at
/// 28, id and padding at 32; stride 48.
pub const WHITEWATER_PARTICLE_SPECS: &[ChannelSpec] = &[
    ChannelSpec {
        name: well_known::POSITION_LIFETIME,
        ty: ChannelElementType::Vec4F,
    },
    ChannelSpec {
        name: well_known::VELOCITY,
        ty: ChannelElementType::Vec3F,
    },
    ChannelSpec {
        name: well_known::KIND,
        ty: ChannelElementType::U32,
    },
    ChannelSpec {
        name: well_known::ID,
        ty: ChannelElementType::U32,
    },
    ChannelSpec {
        name: well_known::PAD0,
        ty: ChannelElementType::U32,
    },
    ChannelSpec {
        name: well_known::PAD1,
        ty: ChannelElementType::U32,
    },
    ChannelSpec {
        name: well_known::PAD2,
        ty: ChannelElementType::U32,
    },
];

impl KnownItem for WhitewaterParticle {
    const SPECS: &'static [ChannelSpec] = WHITEWATER_PARTICLE_SPECS;
}

/// FLIP's own default whitewater pool budget, and the most a pool may hold.
pub const DEFAULT_CAPACITY: u32 = 100_000;
pub const MAX_CAPACITY: u32 = 250_000;

/// FLIP's material kinds, one u32 per whitewater cell.
pub const CELL_AIR: u32 = 0;
pub const CELL_LIQUID: u32 = 1;
pub const CELL_SOLID: u32 = 2;

/// Largest solid lattice side the atoms take.
pub const MAX_GRID_NODES: u32 = 4096;

/// Cells per axis of a whitewater grid over a solid lattice of `nodes`.
pub fn grid_cells(nodes: [u32; 3]) -> Option<[u32; 3]> {
    nodes
        .iter()
        .all(|&n| (3..=MAX_GRID_NODES).contains(&n))
        .then(|| nodes.map(|n| n - 1))
}

/// Whitewater cells from the grid's first cell to the face grid's, on each
/// axis: the face grid sits centred by a whole number of cells, the same on
/// every axis (section 3.1), or the placement is a named refusal.
pub fn face_offset(nodes: [u32; 3], face_cells: [u32; 3]) -> Result<[u32; 3], String> {
    let cells = grid_cells(nodes)
        .ok_or_else(|| format!("a {nodes:?} solid lattice has too few or too many nodes"))?;
    let pad: [Option<u32>; 3] = std::array::from_fn(|a| {
        let spare = cells[a].checked_sub(face_cells[a])?;
        (face_cells[a] > 0 && spare % 2 == 0).then_some(spare / 2)
    });
    match pad {
        [Some(x), Some(y), Some(z)] if x == y && y == z => Ok([x; 3]),
        _ => Err(format!(
            "a {face_cells:?}-cell face grid does not sit centred on the {cells:?}-cell whitewater grid by one whole number of cells on every axis"
        )),
    }
}

/// The whitewater reads face velocity at least one layer past the liquid.
pub fn require_extended_faces(layers: f32) -> Result<(), String> {
    if layers >= 1.0 {
        Ok(())
    } else {
        Err(format!(
            "the face grid carries velocity {layers} layers past the liquid; whitewater needs at least 1"
        ))
    }
}

/// The grid's first node and cell size, from the frame's `grid_bounds` (the
/// scene box of the solid lattice's nodes) and its node counts. Cells that
/// aren't cubes are a named refusal.
pub fn grid_box(
    bounds: manifold_node_engine::scene::transform::Transform,
    nodes: [u32; 3],
) -> Result<([f32; 3], f32), String> {
    let cells = grid_cells(nodes)
        .ok_or_else(|| format!("a {nodes:?} solid lattice has too few or too many nodes"))?;
    let sizes: [f32; 3] = std::array::from_fn(|a| bounds.scale[a] / cells[a] as f32);
    let size = sizes[0];
    if !(size.is_finite() && size > 0.0) || sizes.iter().any(|s| (s - size).abs() > 1e-4 * size) {
        return Err(format!(
            "a {:?} m grid box over {nodes:?} nodes does not make cube cells",
            bounds.scale
        ));
    }
    Ok((
        std::array::from_fn(|a| bounds.pos[a] - 0.5 * bounds.scale[a]),
        size,
    ))
}

/// Cell `index` of a grid of `cells`.
pub fn cell_coords(cells: [u32; 3], index: usize) -> [u32; 3] {
    let [nx, ny, _] = cells.map(|n| n as usize);
    [index % nx, (index / nx) % ny, index / (nx * ny)].map(|n| n as u32)
}
