//! The liquid bricks layout, validated through every consumer that includes it.

use manifold_node_engine::testkit::shader_source::dense_source;

#[test]
fn fluid_bricks_generated_consumers_validate_on_cpu() {
    use manifold_node_engine::freeze::codegen::standalone_for_spec;
    use manifold_water_gpu_flip::primitives::clamp_liquid_to_solids::{self, ClampLiquidToSolids};
    use manifold_water_surface::primitives::{count_surface_triangles::CountSurfaceTriangles, particle_volume::ParticleVolume, relax_surface_mesh::RelaxSurfaceMesh, volume_surface_mesh::VolumeSurfaceMesh};
    use manifold_water_liquid::primitives::smooth_lattice::{self, SmoothLattice};
    for source in [
        standalone_for_spec::<ParticleVolume>().unwrap(),
        standalone_for_spec::<SmoothLattice>().unwrap(),
        standalone_for_spec::<ClampLiquidToSolids>().unwrap(),
        standalone_for_spec::<CountSurfaceTriangles>().unwrap(),
        standalone_for_spec::<VolumeSurfaceMesh>().unwrap(),
        dense_source::<VolumeSurfaceMesh>(manifold_water_surface::primitives::volume_surface_mesh::DENSE_REFERENCE),
        dense_source::<RelaxSurfaceMesh>(manifold_water_surface::primitives::relax_surface_mesh::DENSE_REFERENCE),
        dense_source::<ParticleVolume>(manifold_water_surface::primitives::particle_volume::DENSE_REFERENCE),
        dense_source::<SmoothLattice>(smooth_lattice::DENSE_REFERENCE),
        dense_source::<ClampLiquidToSolids>(clamp_liquid_to_solids::DENSE_REFERENCE),
        dense_source::<CountSurfaceTriangles>(manifold_water_surface::primitives::count_surface_triangles::DENSE_REFERENCE),
    ] {
        let module = naga::front::wgsl::parse_str(&source)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&source)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap();
    }
}

