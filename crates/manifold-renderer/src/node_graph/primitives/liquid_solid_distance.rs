//! `node.liquid_solid_distance` — a liquid domain's solid lattice for the
//! particle-frame seam: walls and bodies as one signed distance per lattice
//! node (`docs/LIQUID_SOLVER_SEAM_DESIGN.md` section 3.1, amendment 3;
//! GPU_MPM_SOLVER_DESIGN.md D11).

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuBuffer};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::fluid::TICK;
use crate::node_graph::fluid_role::MAX_FLUID_ROLES;
use crate::node_graph::liquid::bodies::{LIQUID_COLLIDER, LIQUID_POSE, LiquidBody, LiquidShape};
use crate::node_graph::matter::solid_bytes;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use super::matter_common::read_lattice;
use super::standalone_pipeline::standalone_pipeline;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SolidDistanceUniforms {
    lattice_min_x: f32,
    lattice_min_y: f32,
    lattice_min_z: f32,
    cell_size: f32,
    nodes_x: i32,
    nodes_y: i32,
    nodes_z: i32,
    closed_faces: i32,
    body_count: i32,
    rows: i32,
    tick_seconds: f32,
    dispatch_count: u32,
}

crate::primitive! {
    name: LiquidSolidDistance,
    type_id: "node.liquid_solid_distance",
    purpose: "Write a liquid domain's solid lattice: per lattice node, the smaller of the distance to the nearest closed wall and every enabled body's signed distance at the end of this frame's last tick (positive in free space, negative inside a solid). Bodies are sampled from their shapes' lattices in the atlas through their pose, scaled by each shape's smallest scale.",
    inputs: {
        bodies: Array(LiquidBody) required,
        shapes: Array(LiquidShape) required,
        atlas: Array(u32) required,
        lattice_min_x: ScalarF32 optional, lattice_min_y: ScalarF32 optional, lattice_min_z: ScalarF32 optional,
        cell_size: ScalarF32 optional,
        nodes_x: ScalarF32 optional, nodes_y: ScalarF32 optional, nodes_z: ScalarF32 optional,
        closed_faces: ScalarF32 optional,
        body_count: ScalarF32 optional,
        rows: ScalarF32 optional,
        tick_seconds: ScalarF32 optional,
    },
    outputs: {
        solid: Array(f32),
    },
    params: [
        ParamDef { name: Cow::Borrowed("lattice_min_x"), label: "Lattice Min X", ty: ParamType::Float, default: ParamValue::Float(-2.1875), range: Some((-1000.0, 1000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("lattice_min_y"), label: "Lattice Min Y", ty: ParamType::Float, default: ParamValue::Float(-0.1875), range: Some((-1000.0, 1000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("lattice_min_z"), label: "Lattice Min Z", ty: ParamType::Float, default: ParamValue::Float(-2.1875), range: Some((-1000.0, 1000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("cell_size"), label: "Cell Size", ty: ParamType::Float, default: ParamValue::Float(0.0625), range: Some((1.0e-4, 100.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("nodes_x"), label: "Nodes X", ty: ParamType::Int, default: ParamValue::Float(71.0), range: Some((1.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("nodes_y"), label: "Nodes Y", ty: ParamType::Int, default: ParamValue::Float(71.0), range: Some((1.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("nodes_z"), label: "Nodes Z", ty: ParamType::Int, default: ParamValue::Float(71.0), range: Some((1.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("closed_faces"), label: "Closed Faces (bits −X +X −Y +Y −Z +Z)", ty: ParamType::Int, default: ParamValue::Float(63.0), range: Some((0.0, 63.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("body_count"), label: "Bodies", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, MAX_FLUID_ROLES as f32)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("rows"), label: "Rows", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 16_777_216.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("tick_seconds"), label: "Tick (s)", ty: ParamType::Float, default: ParamValue::Float(TICK as f32), range: Some((0.0, 1.0)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "Once per frame, after the Live Matter region. bodies, shapes, atlas, body_count, rows, the lattice and closed faces come from node.matter_domain; solid feeds node.matter_frame's solid input, which publishes it as the seam's solid_a/solid_b. solid holds exactly one value per lattice node, sized every frame from the same node count the dispatch covers; a lattice the device cannot hold is a named error.",
    examples: ["WaterDamBreakMatter"],
    picker: { label: "Liquid Solid Distance", category: Atom },
    summary: "Marks where the walls and solid objects are around a liquid, so its surface stops at them.",
    category: Particles3D,
    role: Filter,
    aliases: ["solid lattice", "collider distance", "solid field"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/liquid_solid_distance_body.wgsl"),
    input_access: [BufferGather, BufferGather, BufferGather],
    wgsl_includes: [LIQUID_POSE, LIQUID_COLLIDER],
    extra_fields: {
        solid: Option<GpuBuffer> = None,
    },
}

impl Primitive for LiquidSolidDistance {
    fn provides_array_output(&self, port: &str) -> bool {
        port == "solid"
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        (port == "solid").then_some(self.solid.as_ref()).flatten()
    }

    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        // Provided storage: a one-value hint, sized to the lattice at run time.
        (port_name == "solid").then_some(1)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let lattice = read_lattice(ctx);
        let closed_faces = ctx.scalar_or_param("closed_faces", 63.0).round().clamp(0.0, 63.0) as i32;
        let body_count = ctx.scalar_or_param("body_count", 0.0).round().clamp(0.0, MAX_FLUID_ROLES as f32) as i32;
        let rows = ctx.scalar_or_param("rows", 0.0).round().max(0.0) as i32;
        let tick_seconds = ctx.scalar_or_param("tick_seconds", TICK as f32);
        let inputs = (ctx.inputs.array("bodies"), ctx.inputs.array("shapes"), ctx.inputs.array("atlas"));
        let nodes = lattice.node_count();
        // The storage follows the node count the dispatch covers, before it
        // is encoded.
        let bytes = solid_bytes(lattice.nodes);
        if self.solid.as_ref().is_none_or(|solid| solid.size < bytes) {
            let device = ctx.gpu_encoder().device;
            let created = crate::node_graph::scene_modifier_expand::admit_candidate_bytes(
                device.modifier_memory_snapshot(),
                bytes,
            )
            .map_err(|error| error.to_string())
            .and_then(|()| device.try_create_buffer_shared(bytes));
            match created {
                Ok(buffer) => {
                    buffer.zero_fill();
                    self.solid = Some(buffer);
                }
                Err(error) => {
                    ctx.error(format!(
                        "Liquid Solid Distance: the lattice's {nodes} nodes need {bytes} bytes the device cannot give: {error}. Lower Resolution."
                    ));
                    return;
                }
            }
        }
        let gpu = ctx.gpu_encoder();
        let ((Some(bodies), Some(shapes), Some(atlas)), Some(solid)) = (inputs, self.solid.as_ref()) else {
            return;
        };
        let rows = rows.min((bodies.size / std::mem::size_of::<LiquidBody>() as u64).min(i32::MAX as u64) as i32);
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let uniforms = SolidDistanceUniforms {
            lattice_min_x: lattice.min[0],
            lattice_min_y: lattice.min[1],
            lattice_min_z: lattice.min[2],
            cell_size: lattice.cell_size,
            nodes_x: lattice.nodes[0] as i32,
            nodes_y: lattice.nodes[1] as i32,
            nodes_z: lattice.nodes[2] as i32,
            closed_faces,
            body_count,
            rows,
            tick_seconds,
            dispatch_count: nodes,
        };
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: bodies, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: shapes, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: atlas, offset: 0 },
                GpuBinding::Buffer { binding: 4, buffer: solid, offset: 0 },
            ],
            [nodes.div_ceil(256), 1, 1],
            "node.liquid_solid_distance",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn liquid_solid_distance_generates_a_gathering_node_kernel() {
        let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<LiquidSolidDistance>()
            .expect("liquid_solid_distance codegen");
        let module = naga::front::wgsl::parse_str(&wgsl).unwrap_or_else(|e| panic!("{}", e.emit_to_string(&wgsl)));
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
            .validate(&module)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&wgsl)));
        for binding in ["buf_bodies", "buf_shapes", "buf_atlas: array<u32>", "buf_solid[idx] = body(idx, params.dispatch_count,"] {
            assert!(wgsl.contains(binding), "{binding}: {wgsl}");
        }
        assert_eq!(std::mem::size_of::<SolidDistanceUniforms>(), 48);
    }

    /// The solid atom poses bodies the way node.matter_move_bodies does:
    /// both turn through the shared pose library.
    #[test]
    fn liquid_solid_distance_poses_bodies_as_move_bodies() {
        let solid = include_str!("shaders/liquid_solid_distance_body.wgsl");
        let moving = include_str!("shaders/matter_move_bodies_body.wgsl");
        assert!(solid.contains("liquid_turn(bd.rotation, bd.angular_velocity.xyz, tick_seconds)"));
        assert!(moving.contains("liquid_turn(b.rotation, b.angular_velocity.xyz, t)"));
        assert!(!solid.contains("sin(0.5 * angle)") && !moving.contains("sin(0.5 * angle)"));
    }
}
