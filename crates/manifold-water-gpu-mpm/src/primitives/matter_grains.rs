//! `node.matter_grains` — Sand drawn as grains: several small turned copies
//! of a grain mesh per point of a matter frame, in place of the surface.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::mesh::InstanceTransform;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::particles::FluidParticle;
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;

/// The most grains one point can carry.
const MAX_GRAINS: f32 = 27.0;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GrainUniforms {
    live_count: f32,
    grains: i32,
    size: f32,
    spread: f32,
    dispatch_count: u32,
    _pad: [u32; 3],
}

manifold_node_engine::primitive! {
    name: MatterGrains,
    type_id: "node.matter_grains",
    purpose: "Draw a matter frame as sand grains: each point carries Grains copy transforms, grain j at a hashed offset within Spread times the point's radius, turned by hashed angles and scaled to Size times the radius (varied 0.7 to 1.3), all keyed on the point's id so a grain stays with its point. Points past live_count, with radius 0 or with no id are holes. count publishes live_count × Grains, the instance count to draw.",
    inputs: {
        particles: Array(FluidParticle) required,
        live_count: ScalarF32 optional,
    },
    outputs: {
        copies: Array(InstanceTransform),
        count: ScalarF32,
    },
    params: [
        ParamDef { name: Cow::Borrowed("live_count"), label: "Live Count", ty: ParamType::Float, default: ParamValue::Float(-1.0), range: Some((-1.0, 1.0e9)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("grains"), label: "Grains", ty: ParamType::Int, default: ParamValue::Float(4.0), range: Some((1.0, MAX_GRAINS)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("size"), label: "Size", ty: ParamType::Float, default: ParamValue::Float(0.65), range: Some((0.05, 2.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("spread"), label: "Spread", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.0, 2.0)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "Wire particles and live_count from node.matter_frame (or the display's pushed-out particles and the frame's count_b); wire copies into a scene object's instances and count into its instance_count, with a small grain mesh as the vertices. Output capacity is the particles' capacity times Grains, so changing Grains rebuilds the plan.",
    examples: [],
    picker: { label: "Matter Grains", category: Atom },
    summary: "Turns each sand point into a handful of small turned grains to draw.",
    category: Particles3D,
    role: Map,
    aliases: ["sand grains", "grains", "particle grains"],
    fusion_kind: Boundary,
    boundary_reason: Blocked,
    wgsl_body: include_str!("shaders/matter_grains_body.wgsl"),
    input_access: [BufferGather],
}

fn grains(params: &ParamValues) -> u32 {
    match params.get("grains") {
        Some(ParamValue::Float(n)) => n.round().clamp(1.0, MAX_GRAINS) as u32,
        _ => 4,
    }
}

impl Primitive for MatterGrains {
    fn array_output_capacity(
        &self,
        port_name: &str,
        params: &ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        (port_name == "copies").then(|| {
            input_capacities
                .iter()
                .find(|(port, _)| *port == "particles")
                .map_or(1, |&(_, n)| n.max(1).saturating_mul(grains(params)))
        })
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let k = grains(ctx.params);
        let size = ctx.scalar_or_param("size", 0.5);
        let spread = ctx.scalar_or_param("spread", 1.0);
        let (Some(particles), Some(copies)) =
            (ctx.inputs.array("particles"), ctx.outputs.array("copies"))
        else {
            ctx.outputs.set_scalar("count", ParamValue::Float(0.0));
            return;
        };
        let points = (particles.size / std::mem::size_of::<FluidParticle>() as u64) as u32;
        let live = ctx.scalar_or_param("live_count", -1.0);
        let live_points = if live >= 0.0 {
            (live as u32).min(points)
        } else {
            points
        };
        let slots = (copies.size / std::mem::size_of::<InstanceTransform>() as u64) as u32;
        let total = live_points.saturating_mul(k).min(slots);
        ctx.outputs
            .set_scalar("count", ParamValue::Float(total as f32));
        if total == 0 {
            return;
        }
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let uniforms = GrainUniforms {
            live_count: live,
            grains: k as i32,
            size,
            spread,
            dispatch_count: total,
            _pad: [0; 3],
        };
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: particles,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: copies,
                    offset: 0,
                },
            ],
            [total.div_ceil(256), 1, 1],
            "node.matter_grains",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matter_grains_generates_a_valid_kernel() {
        let wgsl = manifold_node_engine::freeze::codegen::standalone_for_spec::<MatterGrains>()
            .expect("matter_grains codegen");
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
