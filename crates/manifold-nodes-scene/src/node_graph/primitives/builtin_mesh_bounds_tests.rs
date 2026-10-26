//! The cube, sphere and plane nodes publish the box their vertices fit in on
//! `vertices`, each run, from their parameters alone.

use std::borrow::Cow;

use manifold_core::{Beats, Seconds};
use manifold_node_engine::bindings::{NodeInputs, NodeOutputs};
use manifold_node_engine::exec::backend::{Backend, MockBackend};
use manifold_node_engine::exec::cpu_values::CpuWireWrites;
use manifold_node_engine::exec::effect_node::{EffectNodeContext, FrameTime, ParamValues};
use manifold_node_engine::exec::execution_plan::ResourceId;
use manifold_node_engine::parameters::ParamValue;
use manifold_node_engine::ports::PortType;
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::scene::mesh_bounds::MeshBounds;

use super::generate_cube_mesh::GenerateCubeMesh;
use super::plane_mesh::GeneratePlaneMesh;
use super::sphere_mesh::SphereMesh;

/// Run `node` once with `vertices` bound to a slot that has no buffer behind
/// it, and read back what it published there (`None`: nothing at all).
fn published_box(
    node: &mut impl Primitive,
    params: &[(&'static str, f32)],
) -> Option<Option<MeshBounds>> {
    let params: ParamValues = params
        .iter()
        .map(|&(name, value)| (Cow::Borrowed(name), ParamValue::Float(value)))
        .collect();
    let mut backend = MockBackend::new();
    let slot = backend.acquire(ResourceId(0), PortType::MeshSource, None, (0, 0));
    let bindings = [("vertices", slot)];
    let (mut scalar, mut camera, mut light, mut material) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let (mut transform, mut atmosphere, mut render_mode, mut object) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut cpu_values = CpuWireWrites::default();
    let mut mesh_sources = Vec::new();
    {
        let backend_ref: &dyn Backend = &backend;
        let inputs = NodeInputs::new(&[], backend_ref, &[]);
        let outputs = NodeOutputs::new(
            &bindings,
            backend_ref,
            &mut scalar,
            &mut camera,
            &mut light,
            &mut material,
            &mut transform,
            &mut atmosphere,
            &mut render_mode,
            &mut object,
        )
        .with_cpu_value_writes(&mut cpu_values)
        .with_mesh_source_writes(&mut mesh_sources);
        let time = FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        };
        let mut ctx = EffectNodeContext::new(time, &params, inputs, outputs, None);
        node.run(&mut ctx);
    }
    cpu_values.commit(backend.cpu_values_mut());
    backend.cpu_values().get::<Option<MeshBounds>>(slot)
}

#[test]
fn the_built_in_solids_publish_their_parameter_box() {
    let cube = published_box(&mut GenerateCubeMesh::new(), &[("size", 3.0)])
        .flatten()
        .expect("cube");
    assert_eq!((cube.min, cube.max), ([-1.5; 3], [1.5; 3]));

    let sphere = published_box(&mut SphereMesh::new(), &[("radius", 2.0)])
        .flatten()
        .expect("sphere");
    assert_eq!((sphere.min, sphere.max), ([-2.0; 3], [2.0; 3]));

    let plane = published_box(
        &mut GeneratePlaneMesh::new(),
        &[("width", 4.0), ("height", 1.5)],
    )
    .flatten()
    .expect("plane");
    assert_eq!(
        (plane.min, plane.max),
        ([-2.0, -0.75, 0.0], [2.0, 0.75, 0.0])
    );

    // A box that cannot be trusted is published as unknown, replacing the
    // last good one rather than leaving it standing.
    assert_eq!(
        published_box(&mut GenerateCubeMesh::new(), &[("size", f32::NAN)]),
        Some(None)
    );
}
