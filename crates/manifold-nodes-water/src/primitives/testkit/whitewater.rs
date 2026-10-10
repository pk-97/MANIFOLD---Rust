//! Whitewater node constructors and spec observations for family proofs.
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::freeze::codegen::{InputSource, RegionNode};
#[cfg(feature = "whitewater-oracle")]
pub fn energy_potential() -> impl Primitive { crate::primitives::energy_potential::EnergyPotential::new() }
pub fn turbulence_field() -> impl Primitive { crate::primitives::turbulence_field::TurbulenceField::new() }
pub fn whitewater_influence() -> impl Primitive { crate::primitives::whitewater_influence::WhitewaterInfluence::new() }
pub fn member(kind: &str, id: u32, inputs: Vec<InputSource>) -> RegionNode<'static> {
    match kind {
        "energy_potential" => crate::testkit::water_codegen::member::<crate::primitives::energy_potential::EnergyPotential>(id, inputs),
        "turbulence_field" => crate::testkit::water_codegen::member::<crate::primitives::turbulence_field::TurbulenceField>(id, inputs),
        "whitewater_obstacle_source" => crate::testkit::water_codegen::member::<crate::primitives::whitewater_obstacle_source::WhitewaterObstacleSource>(id, inputs),
        "whitewater_influence" => crate::testkit::water_codegen::member::<crate::primitives::whitewater_influence::WhitewaterInfluence>(id, inputs),
        "dust_potential" => crate::testkit::water_codegen::member::<crate::primitives::dust_potential::DustPotential>(id, inputs),
        "whitewater_emitter_velocity" => crate::testkit::water_codegen::member::<crate::primitives::whitewater_emitter_velocity::WhitewaterEmitterVelocity>(id, inputs),
        "inside_turbulence_potential" => crate::testkit::water_codegen::member::<crate::primitives::inside_turbulence_potential::InsideTurbulencePotential>(id, inputs),
        "turbulence_emission_count" => crate::testkit::water_codegen::member::<crate::primitives::turbulence_emission_count::TurbulenceEmissionCount>(id, inputs),
        _ => panic!("no water spec fixture for {kind}"),
    }
}
#[derive(Clone, Copy)]
pub struct GridBox {
    pub(crate) cells: [u32; 3],
    #[cfg(test)]
    pub(crate) center: [f32; 3],
    pub(crate) size: [f32; 3],
}
impl GridBox {
    pub fn new(cells: [u32; 3], _center: [f32; 3], size: [f32; 3]) -> Self {
        Self { cells, #[cfg(test)] center: _center, size }
    }
}
pub fn turbulence(faces: [&[f32]; 3], face_cells: [u32; 3], distance: &[f32], grid: GridBox) -> Vec<f32> {
    crate::primitives::whitewater_emitter_cpu::turbulence(faces, face_cells, distance, crate::primitives::whitewater_particle_cpu::Box3 { cells: grid.cells, #[cfg(test)] center: grid.center, size: grid.size })
}

pub fn source(influence: f32, dust_strength: f32, kind: u32, pad: u32) -> impl manifold_node_engine::ports::KnownItem {
    crate::primitives::whitewater_obstacle_source::WhitewaterSource { influence, dust_strength, kind, pad }
}
