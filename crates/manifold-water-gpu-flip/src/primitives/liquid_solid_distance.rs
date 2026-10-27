//! `node.liquid_solid_distance` — a liquid domain's solid lattice for the
//! particle-frame seam: walls and bodies as one signed distance per lattice
//! node.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice, GpuEncoder};

use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;

use manifold_core::fluid_domain::MAX_LIQUID_BODIES;
use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_physics::clock::TICK;
use manifold_water_liquid::bodies::{LIQUID_COLLIDER, LIQUID_POSE, LiquidBody, LiquidShape};
use manifold_water_liquid::lattice::LiquidLattice;

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
    bounds_lo_x: f32,
    bounds_lo_y: f32,
    bounds_lo_z: f32,
    bounds_hi_x: f32,
    bounds_hi_y: f32,
    bounds_hi_z: f32,
    body_count: i32,
    rows: i32,
    tick_seconds: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
}

manifold_node_engine::primitive! {
    name: LiquidSolidDistance,
    type_id: "node.liquid_solid_distance",
    purpose: "Write a liquid domain's solid lattice: per lattice node, the smaller of the distance to the nearest Wall face of the Bounds box (bounds_lo/hi in metres, closed_faces the Wall faces; 1e30 with none) and every enabled body's signed distance at the end of this frame's last tick (positive in free space, negative inside a solid). Bodies are sampled from their shapes' lattices in the atlas through their pose, in metres along each shape's stretch at the surface.",
    inputs: {
        bodies: Array(LiquidBody) required,
        shapes: Array(LiquidShape) required,
        atlas: Array(u32) required,
        lattice_min_x: ScalarF32 optional, lattice_min_y: ScalarF32 optional, lattice_min_z: ScalarF32 optional,
        cell_size: ScalarF32 optional,
        nodes_x: ScalarF32 optional, nodes_y: ScalarF32 optional, nodes_z: ScalarF32 optional,
        closed_faces: ScalarF32 optional,
        bounds_lo_x: ScalarF32 optional, bounds_lo_y: ScalarF32 optional, bounds_lo_z: ScalarF32 optional,
        bounds_hi_x: ScalarF32 optional, bounds_hi_y: ScalarF32 optional, bounds_hi_z: ScalarF32 optional,
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
        ParamDef { name: Cow::Borrowed("closed_faces"), label: "Closed Faces (bits −X +X −Y +Y −Z +Z)", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 63.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("bounds_lo_x"), label: "Bounds Min X", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-1.0e4, 1.0e4)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("bounds_lo_y"), label: "Bounds Min Y", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-1.0e4, 1.0e4)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("bounds_lo_z"), label: "Bounds Min Z", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-1.0e4, 1.0e4)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("bounds_hi_x"), label: "Bounds Max X", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-1.0e4, 1.0e4)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("bounds_hi_y"), label: "Bounds Max Y", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-1.0e4, 1.0e4)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("bounds_hi_z"), label: "Bounds Max Z", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-1.0e4, 1.0e4)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("body_count"), label: "Bodies", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, MAX_LIQUID_BODIES as f32)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("rows"), label: "Rows", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 16_777_216.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("tick_seconds"), label: "Tick (s)", ty: ParamType::Float, default: ParamValue::Float(TICK as f32), range: Some((0.0, 1.0)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "Once per frame, after the liquid's tick region. bodies, shapes, atlas, body_count, rows and the lattice come from the liquid's domain (node.matter_domain, node.gpu_flip_domain); the Bounds box and the Wall faces come from node.gpu_flip_domain; solid feeds its particle frame's (node.matter_frame, node.liquid_frame) solid input, which publishes it as the seam's solid_a/solid_b. solid holds exactly one value per lattice node, sized every frame from the same node count the dispatch covers; a lattice the device cannot hold is a named error.",
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
        clock_plan: Option<GpuBuffer> = None,
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
        _params: &manifold_node_engine::exec::effect_node::ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        // Provided storage: a one-value hint, sized to the lattice at run time.
        (port_name == "solid").then_some(1)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(lattice) = LiquidLattice::from_wires(ctx, "Liquid Solid Distance") else {
            return;
        };
        let closed_faces = ctx
            .scalar_or_param("closed_faces", 0.0)
            .round()
            .clamp(0.0, 63.0) as i32;
        let bounds = [
            ctx.scalar_or_param("bounds_lo_x", 0.0),
            ctx.scalar_or_param("bounds_lo_y", 0.0),
            ctx.scalar_or_param("bounds_lo_z", 0.0),
            ctx.scalar_or_param("bounds_hi_x", 0.0),
            ctx.scalar_or_param("bounds_hi_y", 0.0),
            ctx.scalar_or_param("bounds_hi_z", 0.0),
        ];
        let body_count = ctx
            .scalar_or_param("body_count", 0.0)
            .round()
            .clamp(0.0, MAX_LIQUID_BODIES as f32) as i32;
        let rows = ctx.scalar_or_param("rows", 0.0).round().max(0.0) as i32;
        let tick_seconds = ctx.scalar_or_param("tick_seconds", TICK as f32);
        let inputs = (
            ctx.inputs.array("bodies"),
            ctx.inputs.array("shapes"),
            ctx.inputs.array("atlas"),
        );
        let nodes = lattice.node_count();
        // The storage follows the node count the dispatch covers, before it
        // is encoded.
        let bytes = lattice.solid_bytes();
        if self.solid.as_ref().is_none_or(|solid| solid.size != bytes) {
            let device = ctx.gpu_encoder().device;
            let created = manifold_node_engine::load::expand::admit_candidate_bytes(
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
                        "Liquid Solid Distance: the lattice's {nodes} nodes need {bytes} bytes the device cannot give: {error}. Raise Cell Size."
                    ));
                    return;
                }
            }
        }
        if self.clock_plan.is_none() {
            let plan = ctx.gpu_encoder().device.try_create_buffer_shared(48);
            match plan {
                Ok(plan) => {
                    plan.zero_fill();
                    self.clock_plan = Some(plan);
                }
                Err(error) => {
                    ctx.error(format!(
                        "Liquid Solid Distance: could not allocate its clock plan: {error}"
                    ));
                    return;
                }
            }
        }
        let gpu = ctx.gpu_encoder();
        let ((Some(bodies), Some(shapes), Some(atlas)), Some(solid)) =
            (inputs, self.solid.as_ref())
        else {
            return;
        };
        let job = SolidDistanceJob {
            min: lattice.min(),
            cell_size: lattice.cell_size(),
            nodes: lattice.nodes(),
            closed_faces,
            bounds_lo: [bounds[0], bounds[1], bounds[2]],
            bounds_hi: [bounds[3], bounds[4], bounds[5]],
            body_count,
            rows,
            tick_seconds,
            bodies,
            shapes,
            atlas,
            out: solid,
            clock_plan: self.clock_plan.as_ref().expect("clock plan prepared"),
        };
        encode_solid_distance(
            &mut self.pipeline,
            gpu.device,
            gpu.native_enc,
            &job,
            "node.liquid_solid_distance",
        );
    }
}

/// One solid distance lattice: `nodes` lattice nodes from `min`, `cell_size` apart.
pub(crate) struct SolidDistanceJob<'a> {
    pub min: [f32; 3],
    pub cell_size: f32,
    pub nodes: [u32; 3],
    /// Faces of the Bounds box that are solid Walls (bits −X +X −Y +Y −Z +Z).
    pub closed_faces: i32,
    /// The Bounds box in metres, as placed.
    pub bounds_lo: [f32; 3],
    pub bounds_hi: [f32; 3],
    pub body_count: i32,
    pub rows: i32,
    pub tick_seconds: f32,
    pub bodies: &'a GpuBuffer,
    pub shapes: &'a GpuBuffer,
    pub atlas: &'a GpuBuffer,
    /// At least one f32 per node.
    pub out: &'a GpuBuffer,
    pub clock_plan: &'a GpuBuffer,
}

/// The node's kernel, for a stage that writes a solid lattice inside its own
/// dispatch chain (`node.gpu_flip_step`). `slot` holds the codegen pipeline.
pub(crate) fn encode_solid_distance(
    slot: &mut Option<GpuComputePipeline>,
    device: &GpuDevice,
    encoder: &mut GpuEncoder,
    job: &SolidDistanceJob<'_>,
    label: &str,
) {
    let nodes = job.nodes.iter().map(|&n| u64::from(n)).product::<u64>() as u32;
    let rows = job.rows.min(
        (job.bodies.size / std::mem::size_of::<LiquidBody>() as u64).min(i32::MAX as u64) as i32,
    );
    let pipeline = standalone_pipeline::<LiquidSolidDistance>(slot, device);
    let uniforms = SolidDistanceUniforms {
        lattice_min_x: job.min[0],
        lattice_min_y: job.min[1],
        lattice_min_z: job.min[2],
        cell_size: job.cell_size,
        nodes_x: job.nodes[0] as i32,
        nodes_y: job.nodes[1] as i32,
        nodes_z: job.nodes[2] as i32,
        closed_faces: job.closed_faces,
        bounds_lo_x: job.bounds_lo[0],
        bounds_lo_y: job.bounds_lo[1],
        bounds_lo_z: job.bounds_lo[2],
        bounds_hi_x: job.bounds_hi[0],
        bounds_hi_y: job.bounds_hi[1],
        bounds_hi_z: job.bounds_hi[2],
        body_count: job.body_count,
        rows,
        tick_seconds: job.tick_seconds,
        dispatch_count: nodes,
        _pad0: 0,
        _pad1: 0,
    };
    encoder.dispatch_compute(
        pipeline,
        &[
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&uniforms),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: job.bodies,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: job.shapes,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 3,
                buffer: job.atlas,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 4,
                buffer: job.out,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 5,
                buffer: job.clock_plan,
                offset: 0,
            },
        ],
        [nodes.div_ceil(256), 1, 1],
        label,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn liquid_solid_distance_generates_a_gathering_node_kernel() {
        let wgsl =
            manifold_node_engine::freeze::codegen::standalone_for_spec::<LiquidSolidDistance>()
                .expect("liquid_solid_distance codegen");
        let module = naga::front::wgsl::parse_str(&wgsl)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&wgsl)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&wgsl)));
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests;

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
