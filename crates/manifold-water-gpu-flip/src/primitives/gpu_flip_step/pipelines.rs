//! The step's pipelines and the shader text they build from.

use super::*;
use manifold_water_liquid::tile_storage::TILE_STORAGE;

pub(super) struct Pipelines {
    pub(super) gather: GpuComputePipeline,
    pub(super) extend: GpuComputePipeline,
    pub(super) gravity: GpuComputePipeline,
    pub(super) open: GpuComputePipeline,
    pub(super) solid_velocity: GpuComputePipeline,
    pub(super) solid_extrapolate: GpuComputePipeline,
    pub(super) phi_into_solids: GpuComputePipeline,
    pub(super) water_from_phi: GpuComputePipeline,
    pub(super) divergence: GpuComputePipeline,
    pub(super) distance: GpuComputePipeline,
    pub(super) subtract: GpuComputePipeline,
    pub(super) constrain: GpuComputePipeline,
    pub(super) density: GpuComputePipeline,
    pub(super) advect: GpuComputePipeline,
    pub(super) band_distance: GpuComputePipeline,
    pub(super) band_gather: GpuComputePipeline,
    pub(super) band_density: GpuComputePipeline,
    pub(super) narrow_move: GpuComputePipeline,
    pub(super) narrow_tally: GpuComputePipeline,
    pub(super) narrow_latch: GpuComputePipeline,
    pub(super) narrow_disabled: GpuComputePipeline,
    pub(super) pocket_seed: GpuComputePipeline,
    pub(super) pocket_start: GpuComputePipeline,
    pub(super) pocket_round: GpuComputePipeline,
    pub(super) pocket_sweep: [GpuComputePipeline; 3],
    pub(super) pocket_check: GpuComputePipeline,
    pub(super) pocket_condition: GpuComputePipeline,
    pub(super) pocket_tally: GpuComputePipeline,
    pub(super) pocket_clear: GpuComputePipeline,
    pub(super) pocket_accumulate: GpuComputePipeline,
    pub(super) pocket_remove: GpuComputePipeline,
    pub(super) pocket_pin: GpuComputePipeline,
    /// The removed flux into the pressure, then the density, solver word.
    pub(super) pocket_flux: [GpuComputePipeline; 2],
    /// The pockets at the solve level: leaders cleared, cells coarsened,
    /// labels moved to the level.
    pub(super) pocket_coarse: [GpuComputePipeline; 3],
    pub(super) remove_crowded: GpuComputePipeline,
    pub(super) pose_bodies: GpuComputePipeline,
    pub(super) emit_flags: GpuComputePipeline,
    pub(super) emit_write: GpuComputePipeline,
    pub(super) emit_tally: GpuComputePipeline,
    pub(super) birth_tally_reset: GpuComputePipeline,
    pub(super) tiles_classify: GpuComputePipeline,
    pub(super) tiles_rings: GpuComputePipeline,
    pub(super) tiles_lists: GpuComputePipeline,
    pub(super) tiles_fill: GpuComputePipeline,
    pub(super) tiles_retire: GpuComputePipeline,
    /// The proofs' poison entry, dispatched under [`POISON`].
    #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
    pub(super) poison: GpuComputePipeline,
}

pub(super) fn step_source() -> String {
    let source = STEP_SHADER.replace("return narrow_mask[index] != 0u;", "return true;");
    with_stats_layout(&format!(
        "{LIQUID_POSE}\n{LIQUID_COLLIDER}\n{FORCE_LATTICE}\n{TILE_STORAGE}\n{source}"
    ))
}

pub(super) fn band_source() -> String {
    with_stats_layout(&format!(
        "{LIQUID_POSE}\n{LIQUID_COLLIDER}\n{FORCE_LATTICE}\n{TILE_STORAGE}\n{STEP_SHADER}"
    ))
}

impl Pipelines {
    pub(super) fn new(device: &GpuDevice) -> Self {
        let source = step_source();
        let band = band_source();
        let pipe =
            |entry: &str| device.create_compute_pipeline(&source, entry, "node.gpu_flip_step");
        let band_pipe = |entry: &str| {
            device.create_compute_pipeline(&band, entry, "node.gpu_flip_step.narrow_band")
        };
        Self {
            gather: pipe("particles_to_faces"),
            extend: pipe("extend_faces"),
            gravity: pipe("face_gravity"),
            open: pipe("open_fractions"),
            solid_velocity: pipe("solid_face_velocity"),
            pose_bodies: pipe("pose_bodies"),
            solid_extrapolate: pipe("solid_extrapolate"),
            phi_into_solids: pipe("phi_into_solids"),
            water_from_phi: pipe("water_from_phi"),
            divergence: pipe("divergence"),
            distance: pipe("particle_distance"),
            subtract: pipe("subtract_pressure"),
            constrain: pipe("constrain_solid_faces"),
            density: pipe("density_source"),
            advect: pipe("faces_to_particles"),
            band_distance: band_pipe("particle_distance"),
            band_gather: band_pipe("particles_to_faces"),
            band_density: band_pipe("density_source"),
            narrow_move: band_pipe("narrow_move"),
            narrow_tally: band_pipe("narrow_tally"),
            narrow_latch: band_pipe("narrow_latch"),
            narrow_disabled: band_pipe("narrow_disabled"),
            pocket_seed: pipe("pocket_seed"),
            pocket_start: pipe("pocket_start"),
            pocket_round: pipe("pocket_round"),
            pocket_sweep: [
                pipe("pocket_sweep_x"),
                pipe("pocket_sweep_y"),
                pipe("pocket_sweep_z"),
            ],
            pocket_check: pipe("pocket_check"),
            pocket_condition: pipe("pocket_condition"),
            pocket_tally: pipe("pocket_tally"),
            pocket_clear: pipe("pocket_clear"),
            pocket_accumulate: pipe("pocket_accumulate"),
            pocket_remove: pipe("pocket_remove"),
            pocket_pin: pipe("pocket_pin"),
            pocket_flux: [pipe("pocket_flux_pressure"), pipe("pocket_flux_density")],
            pocket_coarse: [
                pipe("pocket_leader_clear"),
                pipe("pocket_coarsen"),
                pipe("pocket_relabel"),
            ],
            remove_crowded: pipe("remove_crowded_markers"),
            emit_flags: pipe("emit_flags"),
            emit_write: pipe("emit_write"),
            emit_tally: pipe("emit_tally"),
            birth_tally_reset: pipe("birth_tally_reset"),
            tiles_classify: pipe("tiles_classify"),
            tiles_rings: pipe("tiles_rings"),
            tiles_lists: pipe("tiles_lists"),
            tiles_fill: pipe("tiles_fill"),
            tiles_retire: pipe("tiles_retire"),
            #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
            poison: pipe(super::super::gpu_flip_tile_tests::POISON_ENTRY),
        }
    }
}
