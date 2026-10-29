//! `node.matter_grains` — Sand drawn as grains: several small turned copies
//! of a grain mesh per point of a matter frame, in place of the surface, for
//! the points at the surface only. Hand WGSL under exclusion 1: the points
//! are counted into an occupancy table with atomics, then each point reads
//! its neighbours' counts, so it is two passes over one table.

use std::borrow::Cow;

use manifold_gpu::{BufferUse, GpuBinding, GpuBuffer, GpuComputePipeline};

use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::mesh::InstanceTransform;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::particles::FluidParticle;
use manifold_node_engine::primitive::Primitive;

/// The most grains one point can carry.
const MAX_GRAINS: f32 = 27.0;

const SHADER: &str = include_str!("shaders/matter_grains.wgsl");

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GrainParams {
    live_count: f32,
    grains: u32,
    size: f32,
    spread: f32,
    points: u32,
    entries: u32,
    _pad0: u32,
    _pad1: u32,
}

pub struct GrainPipelines {
    count: GpuComputePipeline,
    emit: GpuComputePipeline,
}

manifold_node_engine::primitive! {
    name: MatterGrains,
    type_id: "node.matter_grains",
    purpose: "Draw a matter frame as sand grains at its surface: each point carries Grains copy transforms, grain j at a hashed offset within Spread times the point's radius, turned by hashed angles and its rolling, scaled to Size times the radius (varied 0.7 to 1.3), all keyed on the point's id so a grain stays with its point. Occupancy cells are two point spacings: a point with an empty cell one step along an axis draws all its grains, one with an empty cell two steps away only its first, and a deeper point only holes, as do points past live_count, with radius 0 or with no id. count publishes live_count × Grains, the instance count to draw.",
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
    composition_notes: "Wire particles and live_count from node.matter_frame (or the display's pushed-out particles and the frame's count_b); wire copies into a scene object's instances and count into its instance_count, with a small grain mesh as the vertices. A point's grains keep their slots, so the previous frame's copies line up for motion; the draw skips the holes. Output capacity is the particles' capacity times Grains, so changing Grains rebuilds the plan.",
    examples: [],
    picker: { label: "Matter Grains", category: Atom },
    summary: "Turns each sand point at the surface into a handful of small turned grains to draw.",
    category: Particles3D,
    role: Map,
    aliases: ["sand grains", "grains", "particle grains"],
    boundary_reason: BarrieredReduction,
    extra_fields: {
        pipelines: Option<GrainPipelines> = None,
        // The occupancy table: (cell key, count) pairs, cleared every frame.
        table: Option<GpuBuffer> = None,
    },
}

fn grains(params: &ParamValues) -> u32 {
    match params.get("grains") {
        Some(ParamValue::Float(n)) => n.round().clamp(1.0, MAX_GRAINS) as u32,
        _ => 4,
    }
}

/// Table pairs for `points`: a power of two at least twice the points, so
/// probes stay short.
fn table_entries(points: u32) -> u32 {
    points.saturating_mul(2).max(1024).next_power_of_two()
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

    fn prepare_pipelines(&mut self, device: &manifold_gpu::GpuDevice) {
        if self.pipelines.is_none() {
            self.pipelines = Some(GrainPipelines {
                count: device.create_compute_pipeline(
                    SHADER,
                    "count_main",
                    "node.matter_grains.count",
                ),
                emit: device.create_compute_pipeline(
                    SHADER,
                    "emit_main",
                    "node.matter_grains.emit",
                ),
            });
        }
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
        let slots = (copies.size / std::mem::size_of::<InstanceTransform>() as u64) as u32;
        let live_points = if live >= 0.0 {
            (live as u32).min(points)
        } else {
            points
        }
        .min(slots / k);
        ctx.outputs
            .set_scalar("count", ParamValue::Float((live_points * k) as f32));
        if live_points == 0 {
            return;
        }
        let entries = table_entries(points);
        let bytes = u64::from(entries) * 8;
        let gpu = ctx.gpu_encoder();
        if self.table.as_ref().is_none_or(|t| t.size != bytes) {
            self.table = None;
            match gpu.device.try_create_buffer(bytes, BufferUse::Gpu) {
                Ok(table) => self.table = Some(table),
                Err(error) => {
                    ctx.error(format!(
                        "Matter Grains: an occupancy table for {points} points needs storage the device cannot give: {error}"
                    ));
                    return;
                }
            }
        }
        let table = self.table.as_ref().expect("allocated above");
        let pipelines = self
            .pipelines
            .as_ref()
            .expect("grain kernels built by prepare_pipelines");
        let params = GrainParams {
            live_count: live,
            grains: k,
            size,
            spread,
            points: live_points,
            entries,
            _pad0: 0,
            _pad1: 0,
        };
        let bindings = [
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&params),
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
            GpuBinding::Buffer {
                binding: 3,
                buffer: table,
                offset: 0,
            },
        ];
        let groups = [live_points.div_ceil(256), 1, 1];
        gpu.native_enc.clear_buffer(table);
        gpu.native_enc.dispatch_compute(
            &pipelines.count,
            &bindings,
            groups,
            "node.matter_grains.count",
        );
        gpu.native_enc.compute_memory_barrier_buffers();
        gpu.native_enc
            .dispatch_compute(&pipelines.emit, &bindings, groups, "node.matter_grains.emit");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matter_grains_kernels_validate_and_match_their_params() {
        assert_eq!(std::mem::size_of::<GrainParams>(), 32);
        let module = naga::front::wgsl::parse_str(SHADER)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(SHADER)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("{}", e.emit_to_string(SHADER)));
    }

    #[test]
    fn occupancy_tables_are_powers_of_two_with_room() {
        for points in [0, 1, 1000, 1025, 300_000] {
            let entries = table_entries(points);
            assert!(entries.is_power_of_two());
            assert!(entries >= points * 2 && entries >= 1024, "{points} → {entries}");
        }
    }
}
