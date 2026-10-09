//! FLIP Fluids trianglemesh.cpp smooth(value, iterations), using the shared
//! generated relaxation pass. Iteration dependencies materialize between passes.
use crate::primitives::relax_surface_mesh::{RelaxSurfaceMesh, SurfaceMeshPass};
use manifold_node_engine::float_param;
use manifold_node_engine::mesh::MeshVertex;
use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use std::borrow::Cow;
manifold_node_engine::primitive! {
    name: SmoothSurfaceMesh,
    type_id: "node.smooth_surface_mesh",
    purpose: "FLIP Fluids Jacobi mesh smoothing: strength times the incident-triangle neighbour mean displacement, repeated iterations times on a welded marching-cubes mesh. Each pass reads the previous pass. Zero iterations copies the input.",
    inputs: {
        vertices: Array(MeshVertex) required,
        levelset: Array(f32) required,
        scan: Array(u32) required,
        extent: Array(u32) optional,
        bricks: Array(u32) optional,
        edge_scan: Array(u32) optional,
        nodes_x: ScalarF32 optional, nodes_y: ScalarF32 optional, nodes_z: ScalarF32 optional,
        strength: ScalarF32 optional, iterations: ScalarF32 optional,
    },
    outputs: {
        relaxed: Array(MeshVertex),
    },
    params: [
        float_param!("nodes_x", "Nodes X", 2.0, 2.0, 4096.0),
        float_param!("nodes_y", "Nodes Y", 2.0, 2.0, 4096.0),
        float_param!("nodes_z", "Nodes Z", 2.0, 2.0, 4096.0),
        float_param!("strength", "Smoothing Value", 0.5, 0.0, 10.0),
        ParamDef {
            name: Cow::Borrowed("iterations"), label: "Smoothing Iterations", ty: ParamType::Int,
            default: ParamValue::Float(2.0), range: Some((0.0, 10.0)), enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Wire the same levelset, triangle scan, edge scan and lattice as volume_surface_mesh. The stage owns the iteration loop and reuses the generated relax_surface_mesh pass. Follow with surface_mesh_normals to rebuild normals from the final triangles. Values match FLIP Fluids defaults: strength 0.5, iterations 2. No iteration cap.",
    examples: [],
    picker: { label: "Smooth Surface Mesh", category: Atom },
    summary: "Smooths a liquid's surface mesh by easing each point toward its neighbours, rounding off the small facets and steps.",
    category: Geometry3D,
    role: Filter,
    aliases: ["mesh smoothing", "laplacian smooth", "relax mesh", "smooth liquid mesh", "umbrella smoothing"],
    fusion_kind: Boundary,
    boundary_reason: BarrieredReduction,
    extra_fields: {
        pass: SurfaceMeshPass = SurfaceMeshPass::default(),
    },
}

impl Primitive for SmoothSurfaceMesh {
    fn array_output_capacity(
        &self,
        port: &str,
        _params: &ParamValues,
        inputs: &[(&str, u32)],
    ) -> Option<u32> {
        (port == "relaxed")
            .then(|| {
                inputs
                    .iter()
                    .find(|(name, _)| *name == "vertices")
                    .map(|(_, n)| *n)
            })
            .flatten()
    }
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let strength = ctx.scalar_or_param("strength", 0.5);
        let iterations = ctx.scalar_or_param("iterations", 2.0);
        if !iterations.is_finite()
            || iterations < 0.0
            || iterations.fract() != 0.0
            || f64::from(iterations) > f64::from(u32::MAX)
        {
            ctx.error(
                "Smooth Surface Mesh: iterations must be a representable nonnegative integer",
            );
            return;
        }
        if !strength.is_finite() {
            ctx.error("Smooth Surface Mesh: smoothing value must be finite");
            return;
        }
        self.pass
            .run::<RelaxSurfaceMesh>(ctx, strength, iterations as u32, "relaxed");
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
