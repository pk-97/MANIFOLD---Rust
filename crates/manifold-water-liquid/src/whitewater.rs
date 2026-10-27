//! Uses the particle id limit (256) from FLIP Fluids diffuseparticlesimulation.h `_diffuseParticleIDLimit` (MIT); see THIRD_PARTY_NOTICES.md.
//! The whitewater grid and its records. The grid atoms run on the liquid
//! step's world tiles (`TilePool`). The lifecycle still reads a dense
//! grid, the frame's solid lattice read as cells: `nodes − 1` cells a side
//! from the solid lattice's first node, the solid on the cell corners.

use manifold_node_engine::channel_names::well_known;
use manifold_node_engine::ports::{ChannelElementType, ChannelSpec, KnownItem};

/// Helpers every whitewater grid atom's body includes.
pub const WHITEWATER_COMMON: &str = include_str!("primitives/shaders/whitewater_common.wgsl");

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

/// Cell `index` of a grid of `cells`.
pub fn cell_coords(cells: [u32; 3], index: usize) -> [u32; 3] {
    let [nx, ny, _] = cells.map(|n| n as usize);
    [index % nx, (index / nx) % ny, index / (nx * ny)].map(|n| n as u32)
}
