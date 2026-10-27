//! Whitewater node constructors and spec observations for family proofs.
use manifold_node_engine::freeze::codegen::{InputSource, RegionNode};
use manifold_node_engine::primitive::Primitive;

pub fn whitewater_influence() -> impl Primitive {
    crate::primitives::whitewater_influence::WhitewaterInfluence::new()
}
pub fn member(kind: &str, id: u32, inputs: Vec<InputSource>) -> RegionNode<'static> {
    match kind {
        "energy_potential" => manifold_node_engine::testkit::fused_atoms::member::<
            crate::primitives::energy_potential::EnergyPotential,
        >(id, inputs),
        "turbulence_field" => manifold_node_engine::testkit::fused_atoms::member::<
            crate::primitives::turbulence_field::TurbulenceField,
        >(id, inputs),
        "whitewater_influence" => manifold_node_engine::testkit::fused_atoms::member::<
            crate::primitives::whitewater_influence::WhitewaterInfluence,
        >(id, inputs),
        "dust_potential" => manifold_node_engine::testkit::fused_atoms::member::<
            crate::primitives::dust_potential::DustPotential,
        >(id, inputs),
        "whitewater_emitter_velocity" => manifold_node_engine::testkit::fused_atoms::member::<
            crate::primitives::whitewater_emitter_velocity::WhitewaterEmitterVelocity,
        >(id, inputs),
        "inside_turbulence_potential" => manifold_node_engine::testkit::fused_atoms::member::<
            crate::primitives::inside_turbulence_potential::InsideTurbulencePotential,
        >(id, inputs),
        "turbulence_emission_count" => manifold_node_engine::testkit::fused_atoms::member::<
            crate::primitives::turbulence_emission_count::TurbulenceEmissionCount,
        >(id, inputs),
        _ => panic!("no water spec fixture for {kind}"),
    }
}
pub fn source(
    influence: f32,
    dust_strength: f32,
    kind: u32,
    pad: u32,
) -> impl manifold_node_engine::ports::KnownItem {
    crate::primitives::whitewater_obstacle_source::WhitewaterSource {
        influence,
        dust_strength,
        kind,
        pad,
    }
}
