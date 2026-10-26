//! The liquid bricks layout, validated through every consumer that includes it.

use manifold_node_engine::testkit::shader_source::dense_source;

#[test]
fn fluid_bricks_generated_consumers_validate_on_cpu() {
    use manifold_node_engine::freeze::codegen::standalone_for_spec;
    use manifold_water_liquid::primitives::smooth_lattice::{self, SmoothLattice};
    use manifold_water_surface::primitives::particle_volume::ParticleVolume;
    for source in [
        standalone_for_spec::<ParticleVolume>().unwrap(),
        standalone_for_spec::<SmoothLattice>().unwrap(),
        dense_source::<ParticleVolume>(
            manifold_water_surface::primitives::particle_volume::DENSE_REFERENCE,
        ),
        dense_source::<SmoothLattice>(smooth_lattice::DENSE_REFERENCE),
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
