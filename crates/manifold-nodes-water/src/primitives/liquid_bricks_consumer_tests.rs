//! The liquid bricks layout, validated through every consumer that includes it.

use manifold_node_engine::testkit::shader_source::dense_source;

#[test]
fn fluid_bricks_generated_consumers_validate_on_cpu() {
    use manifold_node_engine::freeze::codegen::standalone_for_spec;
    use crate::primitives::{clamp_liquid_to_solids::ClampLiquidToSolids, count_surface_triangles::CountSurfaceTriangles, particle_volume::ParticleVolume, relax_surface_mesh::RelaxSurfaceMesh, volume_surface_mesh::VolumeSurfaceMesh};
    use manifold_water_liquid::primitives::smooth_lattice::{self, SmoothLattice};
    for source in [
        standalone_for_spec::<ParticleVolume>().unwrap(),
        standalone_for_spec::<SmoothLattice>().unwrap(),
        standalone_for_spec::<ClampLiquidToSolids>().unwrap(),
        standalone_for_spec::<CountSurfaceTriangles>().unwrap(),
        standalone_for_spec::<VolumeSurfaceMesh>().unwrap(),
        dense_source::<VolumeSurfaceMesh>(include_str!(
            "shaders/volume_surface_mesh_dense_reference.wgsl"
        )),
        dense_source::<RelaxSurfaceMesh>(include_str!(
            "shaders/relax_surface_mesh_dense_reference.wgsl"
        )),
        dense_source::<ParticleVolume>(include_str!(
            "shaders/particle_volume_dense_reference.wgsl"
        )),
        dense_source::<SmoothLattice>(smooth_lattice::DENSE_REFERENCE),
        dense_source::<ClampLiquidToSolids>(include_str!(
            "shaders/clamp_liquid_to_solids_dense_reference.wgsl"
        )),
        dense_source::<CountSurfaceTriangles>(include_str!(
            "shaders/count_surface_triangles_dense_reference.wgsl"
        )),
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

