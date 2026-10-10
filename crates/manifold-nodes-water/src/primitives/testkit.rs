//! Concrete water-node constructors/spec observations for family proofs.
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::freeze::codegen::{InputSource, RegionNode};
use manifold_gpu::{GpuDevice, GpuComputePipeline};
pub fn running_total() -> impl Primitive { super::running_total::RunningTotal::new() }
pub fn shape_particle_blobs() -> impl Primitive { super::shape_particle_blobs::ShapeParticleBlobs::new() }
pub fn smooth_lattice(pipeline: Option<GpuComputePipeline>) -> impl Primitive {
    let mut node = super::smooth_lattice::SmoothLattice::new();
    node.pipeline = pipeline;
    node
}
#[cfg(feature = "whitewater-oracle")]
pub fn energy_potential() -> impl Primitive { super::energy_potential::EnergyPotential::new() }
pub fn turbulence_field() -> impl Primitive { super::turbulence_field::TurbulenceField::new() }
pub fn whitewater_influence() -> impl Primitive { super::whitewater_influence::WhitewaterInfluence::new() }
pub fn clamp_liquid_to_solids(pipeline: Option<GpuComputePipeline>) -> impl Primitive {
    let mut node = super::clamp_liquid_to_solids::ClampLiquidToSolids::new();
    node.pipeline = pipeline;
    node
}
pub fn count_surface_triangles(pipeline: Option<GpuComputePipeline>) -> impl Primitive {
    let mut node = super::count_surface_triangles::CountSurfaceTriangles::new();
    node.pipeline = pipeline;
    node
}
pub fn relax_surface_mesh() -> impl Primitive { super::relax_surface_mesh::RelaxSurfaceMesh::new() }
pub fn member(kind: &str, id: u32, inputs: Vec<InputSource>) -> RegionNode<'static> {
    match kind {
        "energy_potential" => crate::testkit::water_codegen::member::<super::energy_potential::EnergyPotential>(id, inputs),
        "turbulence_field" => crate::testkit::water_codegen::member::<super::turbulence_field::TurbulenceField>(id, inputs),
        "whitewater_obstacle_source" => crate::testkit::water_codegen::member::<super::whitewater_obstacle_source::WhitewaterObstacleSource>(id, inputs),
        "whitewater_influence" => crate::testkit::water_codegen::member::<super::whitewater_influence::WhitewaterInfluence>(id, inputs),
        "dust_potential" => crate::testkit::water_codegen::member::<super::dust_potential::DustPotential>(id, inputs),
        "whitewater_emitter_velocity" => crate::testkit::water_codegen::member::<super::whitewater_emitter_velocity::WhitewaterEmitterVelocity>(id, inputs),
        "inside_turbulence_potential" => crate::testkit::water_codegen::member::<super::inside_turbulence_potential::InsideTurbulencePotential>(id, inputs),
        "turbulence_emission_count" => crate::testkit::water_codegen::member::<super::turbulence_emission_count::TurbulenceEmissionCount>(id, inputs),
        _ => panic!("no water spec fixture for {kind}"),
    }
}
pub fn dense_pipeline(kind: &str, device: &GpuDevice, source: &str) -> GpuComputePipeline {
    let source = match kind {
        "clamp_liquid_to_solids" => manifold_node_engine::testkit::shader_source::dense_source::<super::clamp_liquid_to_solids::ClampLiquidToSolids>(source),
        "count_surface_triangles" => manifold_node_engine::testkit::shader_source::dense_source::<super::count_surface_triangles::CountSurfaceTriangles>(source),
        "smooth_lattice" => manifold_node_engine::testkit::shader_source::dense_source::<super::smooth_lattice::SmoothLattice>(source),
        "relax_surface_mesh" => manifold_node_engine::testkit::shader_source::dense_source::<super::relax_surface_mesh::RelaxSurfaceMesh>(source),
        _ => panic!("no dense water fixture for {kind}"),
    };
    device.create_compute_pipeline(&source, "cs_main", "liquid.bricks.dense_reference")
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
    super::whitewater_emitter_cpu::turbulence(faces, face_cells, distance, super::whitewater_particle_cpu::Box3 { cells: grid.cells, #[cfg(test)] center: grid.center, size: grid.size })
}

pub fn source(influence: f32, dust_strength: f32, kind: u32, pad: u32) -> impl manifold_node_engine::ports::KnownItem {
    super::whitewater_obstacle_source::WhitewaterSource { influence, dust_strength, kind, pad }
}
