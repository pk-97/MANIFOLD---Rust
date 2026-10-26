//! The step's per-lattice scratch arrays and what they cost.

use super::*;
use manifold_water_liquid::tile_storage::pool_len;

/// Bytes of a pooled cell field in a pool of `pool_tiles` tiles.
pub(super) fn pool_cell_bytes(pool_tiles: u32) -> u64 {
    pool_len(pool_tiles) * 4
}

/// Bytes of a pooled face grid in a pool of `pool_tiles` tiles.
pub(super) fn pool_face_bytes(pool_tiles: u32) -> u64 {
    pool_len(pool_tiles) * size_of::<FaceSample>() as u64
}

/// Bytes the step holds for itself at `cells` with `slots` particle slots,
/// besides the sort's ranges and the solver's scratch: the sorted particles,
/// [`LATTICE_CELL_ARRAYS`] pooled cell arrays, the dense solid corners,
/// [`LATTICE_FACE_GRIDS`] pooled face grids, the pocket gate, the pocket sums
/// (dense), the coarse pockets and the tile table. The pooled faces and φ
/// are among them and are the node's `faces` and `distance` outputs.
#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
pub(crate) fn scratch_bytes(cells: [u32; 3], slots: u64, ring_max: u32) -> u64 {
    let corners = cells.iter().map(|&n| u64::from(n) + 1).product::<u64>() * 4;
    let pool = pool_tiles(cells);
    slots.max(1) * size_of::<FluidParticle>() as u64
        + LATTICE_CELL_ARRAYS * pool_cell_bytes(pool)
        + corners
        + LATTICE_FACE_GRIDS * pool_face_bytes(pool)
        + POCKET_GATE_WORDS * 4
        + pocket_sum_bytes(cells)
        + pocket_coarse_bytes(cells)
        + tile_scratch_bytes(cells, ring_max)
        + mask_saved_bytes(cells, slots.max(1))
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
pub(super) fn mask_saved_bytes(cells: [u32; 3], slots: u64) -> u64 {
    slots * size_of::<FluidParticle>() as u64
        + pool_cell_bytes(pool_tiles(cells))
        + (slots * 8 + u64::from(SOLVER_WORDS) * 4)
}

/// The lattice's pooled cell arrays (`LatticeBuffers`): water, φ, the
/// right-hand side, the pressure, the pocket state and label, the solve
/// mask and the pocket leaders.
#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
pub(super) const LATTICE_CELL_ARRAYS: u64 = 8;

/// The lattice's pooled face grids: the gathered, saved, scratch, forced,
/// open-fraction and solid-velocity grids and the step's internal faces.
#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
pub(super) const LATTICE_FACE_GRIDS: u64 = 7;

/// Scratch for one lattice.
///
/// Cell and face fields are tile-pool storage of `pool_tiles` tiles
/// (`pool_len` elements, the canonical block last); `corners` and the pocket
/// sums are dense.
pub(super) struct LatticeBuffers {
    pub(super) cells: [u32; 3],
    pub(super) water: GpuBuffer,
    /// The `distance` output: shared, so it starts at zero and host proofs
    /// read it as the dense outputs were read.
    pub(super) phi: GpuBuffer,
    pub(super) rhs: GpuBuffer,
    pub(super) pressure: GpuBuffer,
    pub(super) corners: GpuBuffer,
    /// The particles' faces as gathered over C; canonical elsewhere.
    pub(super) g: GpuBuffer,
    /// The saved (old) faces: `g` extended.
    pub(super) a: GpuBuffer,
    /// Extension scratch.
    pub(super) b: GpuBuffer,
    /// The forced, projected, constrained faces.
    pub(super) f: GpuBuffer,
    /// Open fractions.
    pub(super) s: GpuBuffer,
    /// The solids' face velocity and friction.
    pub(super) v: GpuBuffer,
    /// The step's own face grid: the projected, extended, constrained faces
    /// the move reads; the `faces` output, shared like `phi`.
    pub(super) faces: GpuBuffer,
    /// Each cell's sealed-pocket state.
    pub(super) pocket: GpuBuffer,
    /// The pocket spread's indirect sizes and flags.
    pub(super) pocket_gate: GpuBuffer,
    /// Each water cell's pocket label.
    pub(super) pocket_label: GpuBuffer,
    /// [`pocket_sum_bytes`].
    pub(super) pocket_sum: GpuBuffer,
    /// The solves' water: `water` less each sealed pocket's leader cell.
    pub(super) solve_water: GpuBuffer,
    /// The pockets at the solve level: state and label per level cell,
    /// sized for level 1 ([`pocket_coarse_bytes`]).
    pub(super) pocket_coarse: GpuBuffer,
    pub(super) pocket_coarse_label: GpuBuffer,
    /// By fine label (a box cell index), the lowest level cell of that
    /// pocket; a pooled cell field.
    pub(super) pocket_leader: GpuBuffer,
}

pub(super) fn allocate(device: &GpuDevice, bytes: u64) -> Result<GpuBuffer, String> {
    manifold_node_engine::load::expand::admit_candidate_bytes(
        device.modifier_memory_snapshot(),
        bytes,
    )
    .map_err(|error| error.to_string())
    .and_then(|()| device.try_create_buffer(bytes))
}

/// A buffer that carries state from step to step: shared, so it starts at
/// zero without an encoder.
pub(super) fn allocate_zeroed(device: &GpuDevice, bytes: u64) -> Result<GpuBuffer, String> {
    let buffer = manifold_node_engine::load::expand::admit_candidate_bytes(
        device.modifier_memory_snapshot(),
        bytes,
    )
    .map_err(|error| error.to_string())
    .and_then(|()| device.try_create_buffer_shared(bytes))?;
    buffer.zero_fill();
    Ok(buffer)
}

impl LatticeBuffers {
    pub(super) fn new(
        device: &GpuDevice,
        cells: [u32; 3],
        pool_tiles: u32,
    ) -> Result<Self, String> {
        let cell = pool_cell_bytes(pool_tiles);
        let face = pool_face_bytes(pool_tiles);
        let corners = cells.iter().map(|&n| u64::from(n) + 1).product::<u64>() * 4;
        let coarse = pocket_coarse_bytes(cells) / 2;
        Ok(Self {
            cells,
            water: allocate(device, cell)?,
            phi: allocate_zeroed(device, cell)?,
            rhs: allocate(device, cell)?,
            pressure: allocate(device, cell)?,
            corners: allocate(device, corners)?,
            g: allocate(device, face)?,
            a: allocate(device, face)?,
            b: allocate(device, face)?,
            f: allocate(device, face)?,
            s: allocate(device, face)?,
            v: allocate(device, face)?,
            faces: allocate_zeroed(device, face)?,
            pocket: allocate(device, cell)?,
            pocket_gate: allocate(device, POCKET_GATE_WORDS * 4)?,
            pocket_label: allocate(device, cell)?,
            pocket_sum: allocate(device, pocket_sum_bytes(cells))?,
            solve_water: allocate(device, cell)?,
            pocket_coarse: allocate(device, coarse.max(4))?,
            pocket_coarse_label: allocate(device, coarse.max(4))?,
            pocket_leader: allocate(device, cell)?,
        })
    }
}
