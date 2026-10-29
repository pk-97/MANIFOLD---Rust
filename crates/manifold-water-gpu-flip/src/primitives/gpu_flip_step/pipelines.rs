//! The step's pipelines and the shader text they build from.

use super::*;
use manifold_node_engine::scene::world_tiles::READ_SIDE;
use manifold_water_liquid::tile_storage::TILE_GRID;

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
    /// Walls that only push.
    pub(super) release: GpuComputePipeline,
    pub(super) density: GpuComputePipeline,
    pub(super) advect: GpuComputePipeline,
    pub(super) pocket_seed: GpuComputePipeline,
    pub(super) pocket_start: GpuComputePipeline,
    pub(super) pocket_round: GpuComputePipeline,
    pub(super) pocket_sweep: [GpuComputePipeline; 3],
    pub(super) pocket_check: GpuComputePipeline,
    pub(super) pocket_condition: GpuComputePipeline,
    pub(super) pocket_tally: GpuComputePipeline,
    pub(super) pocket_clear: GpuComputePipeline,
    /// The pocket sums' accumulate and remove over a list's cells.
    pub(super) pocket_accumulate: GpuComputePipeline,
    pub(super) pocket_remove: GpuComputePipeline,
    pub(super) pocket_pin: GpuComputePipeline,
    /// The removed flux into the pressure, then the density, solver word.
    pub(super) pocket_flux: [GpuComputePipeline; 2],
    /// The pockets at the solve level: leaders cleared, cells coarsened,
    /// labels moved to the level.
    pub(super) pocket_coarse: [GpuComputePipeline; 3],
    pub(super) remove_crowded: GpuComputePipeline,
    pub(super) owed: [GpuComputePipeline; 3],
    pub(super) pose_bodies: GpuComputePipeline,
    pub(super) emit_flags: GpuComputePipeline,
    pub(super) emit_write: GpuComputePipeline,
    pub(super) emit_tally: GpuComputePipeline,
    pub(super) birth_tally_reset: GpuComputePipeline,
    /// The slot record, loaded from the identity at a tick's start and opened
    /// after each slot's clock plan (`count.wgsl`).
    pub(super) slot_load: GpuComputePipeline,
    pub(super) slot_open: GpuComputePipeline,
    pub(super) tiles_fresh: GpuComputePipeline,
    pub(super) tiles_retire: GpuComputePipeline,
    /// The static colliders' corners of the fresh slots, then of every held
    /// slot when a static body moved or the statics are rebuilt, over the
    /// triple the args pass writes.
    pub(super) static_fresh: GpuComputePipeline,
    pub(super) static_args: GpuComputePipeline,
    pub(super) static_held: GpuComputePipeline,
    /// The solid corners of every held slot.
    pub(super) solid_corners: GpuComputePipeline,
    /// A wired Sand: its particles' share at the corners, then its drag on
    /// the water inside it.
    pub(super) sand_corners: GpuComputePipeline,
    pub(super) sand_drag: GpuComputePipeline,
    /// The pool's held share and the particles with no slot.
    pub(super) tiles_tally: GpuComputePipeline,
    /// The canonical element of every pooled field, once per pool build.
    pub(super) pool_canon: GpuComputePipeline,
    /// Narrow Band's passes (`narrow_band.wgsl`).
    pub(super) nb_fresh: GpuComputePipeline,
    pub(super) nb_canon: GpuComputePipeline,
    pub(super) nb_union: GpuComputePipeline,
    pub(super) nb_combine: GpuComputePipeline,
    pub(super) nb_advect_phi: GpuComputePipeline,
    pub(super) nb_advect_faces: GpuComputePipeline,
    pub(super) nb_delete: GpuComputePipeline,
    pub(super) nb_reseed_flags: GpuComputePipeline,
    pub(super) nb_reseed_write: GpuComputePipeline,
    pub(super) nb_reseed_tally: GpuComputePipeline,
    pub(super) nb_kept_clear: GpuComputePipeline,
    pub(super) nb_keep: GpuComputePipeline,
    /// The proofs' poison entry, dispatched under [`POISON`].
    #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
    pub(super) poison: GpuComputePipeline,
    /// NaN into the free slots after `tiles_fresh`, under [`POISON`].
    #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
    pub(super) poison_free: GpuComputePipeline,
}

manifold_core::testkit_visible! {
/// The step's shader: the poses, the colliders, the force lattice, the dense
/// grid helpers and the world tiles' read side, then the step's own passes,
/// with the stats layout ahead.
pub(crate) fn step_source() -> String {
    step_source_with(STEP_SHADER)
}
}

manifold_core::testkit_visible! {
/// [`step_source`] with `body` in place of the step's own passes, for a
/// proof that composes one of them on its own.
pub(crate) fn step_source_with(body: &str) -> String {
    with_stats_layout(&format!(
        "{LIQUID_POSE}\n{LIQUID_COLLIDER}\n{COLLIDER_BVH}\n{FORCE_LATTICE}\n{TILE_GRID}\n{READ_SIDE}\n{body}"
    ))
}
}

impl Pipelines {
    pub(super) fn new(device: &GpuDevice) -> Self {
        let source = step_source();
        let pipe =
            |entry: &str| device.create_compute_pipeline(&source, entry, "node.gpu_flip_step");
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
            release: pipe("release_walls"),
            density: pipe("density_source"),
            advect: pipe("faces_to_particles"),
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
            owed: [
                pipe("owed_clear"),
                pipe("owed_count"),
                pipe("remove_owed_markers"),
            ],
            emit_flags: pipe("emit_flags"),
            emit_write: pipe("emit_write"),
            emit_tally: pipe("emit_tally"),
            birth_tally_reset: pipe("birth_tally_reset"),
            slot_load: pipe("slot_load"),
            slot_open: pipe("slot_open"),
            tiles_fresh: pipe("tiles_fresh"),
            tiles_retire: pipe("tiles_retire"),
            static_fresh: pipe("static_corners_fresh"),
            static_args: pipe("static_corners_args"),
            static_held: pipe("static_corners_held"),
            solid_corners: pipe("solid_corners"),
            sand_corners: pipe("sand_corners"),
            sand_drag: pipe("sand_drag"),
            tiles_tally: pipe("tiles_tally"),
            pool_canon: pipe("pool_canon"),
            nb_fresh: pipe("nb_fresh"),
            nb_canon: pipe("nb_canon"),
            nb_union: pipe("nb_union"),
            nb_combine: pipe("nb_combine"),
            nb_advect_phi: pipe("nb_advect_phi"),
            nb_advect_faces: pipe("nb_advect_faces"),
            nb_delete: pipe("nb_delete"),
            nb_reseed_flags: pipe("nb_reseed_flags"),
            nb_reseed_write: pipe("nb_reseed_write"),
            nb_reseed_tally: pipe("nb_reseed_tally"),
            nb_kept_clear: pipe("nb_kept_clear"),
            nb_keep: pipe("nb_keep"),
            #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
            poison: pipe(super::super::gpu_flip_tile_tests::POISON_ENTRY),
            #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
            poison_free: pipe("poison_free"),
        }
    }
}
