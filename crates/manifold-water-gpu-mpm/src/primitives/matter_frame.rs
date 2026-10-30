//! `node.matter_frame` — publish a matter domain on the particle-frame seam
//! (`docs/GPU_FLUID_SURFACE_DESIGN.md` section 3.2 outputs; the MPM design's
//! D9, D14 and the surface design's D10 display clock). Exempt from the
//! codegen mandate as cross-frame state (ADDING_PRIMITIVES.md exclusion 2):
//! it owns the A/B frame ring.

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline};

use crate::matter::MatterPoint;
use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::parameters::ParamValue;
use manifold_node_engine::particles::FluidParticle;
use manifold_node_engine::primitive::Primitive;
use manifold_water_liquid::frame_ring::FrameRing;

const SHADER: &str = include_str!("shaders/matter_frame.wgsl");

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FrameParams {
    count: u32,
    previous_count: u32,
    radius_scale: f32,
    first: u32,
}

manifold_node_engine::primitive! {
    name: MatterFrame,
    type_id: "node.matter_frame",
    purpose: "Publish a matter domain as particle frames for the liquid surface: after every simulated tick, write the count points from first (one Matter item's run, or all of them) as an id-sorted Array(FluidParticle) frame B (the previous one becomes A), with the blend and span of the one-tick-behind display clock. A tick with non-finite values is never published as particles.",
    inputs: {
        points: Array(MatterPoint) required,
        stats: Array(u32) required,
        count: ScalarF32 optional,
        // The run's first point: node.matter_domain's first_<slot> for one
        // item's draw, 0 for every point.
        first: ScalarF32 optional,
        simulation_time: ScalarF32 optional,
        display_time: ScalarF32 optional,
        epoch: ScalarF32 optional,
    },
    outputs: {
        particles_a: Array(FluidParticle), particles_b: Array(FluidParticle),
        count_a: ScalarF32, count_b: ScalarF32, identity_a: ScalarF32, identity_b: ScalarF32,
        blend: ScalarF32, span: ScalarF32,
    },
    params: [],
    depth_rule: Terminal,
    composition_notes: "Reads node.matter_state's out and stats after the region; count, simulation_time, display_time and epoch come from node.matter_fill and node.matter_domain; with several Matter items in one solver, each item's draw takes first and count from the domain's first_<slot> and points_<slot>, and the frame the water and gas read takes every point. Its outputs are the particle-frame seam node.fluid_surface also publishes, so the Liquid Surface atoms and node.particles_to_copies read either solver unchanged. identity_a/b carry the domain epoch.",
    examples: ["SandColumn"],
    picker: { label: "Matter Frame", category: Atom },
    summary: "Hands the simulated liquid particles to the liquid surface, one frame per simulation tick.",
    category: Particles3D,
    role: Filter,
    aliases: ["matter frame", "particle frame", "publish particles"],
    boundary_reason: CrossFrameState,
    extra_fields: {
        convert: Option<GpuComputePipeline> = None,
        ring: FrameRing = FrameRing::default(),
    },
}

impl Primitive for MatterFrame {
    fn prepare_pipelines(&mut self, device: &manifold_gpu::GpuDevice) {
        if self.convert.is_none() {
            self.convert =
                Some(device.create_compute_pipeline(SHADER, "cs_main", "node.matter_frame"));
        }
    }

    fn provides_array_output(&self, port: &str) -> bool {
        matches!(port, "particles_a" | "particles_b")
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        match port {
            "particles_a" => self.ring.buffer_a(),
            "particles_b" => self.ring.buffer_b(),
            _ => None,
        }
    }

    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &manifold_node_engine::exec::effect_node::ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        // Provided storage: a one-record hint, grown at run time.
        self.provides_array_output(port_name).then_some(1)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let count = ctx.scalar_or_param("count", 0.0).round().max(0.0) as u32;
        let first = ctx.scalar_or_param("first", 0.0).round().max(0.0) as u32;
        let simulation_time = f64::from(ctx.scalar_or_param("simulation_time", 0.0));
        let display_time = f64::from(ctx.scalar_or_param("display_time", 0.0));
        let epoch = ctx.scalar_or_param("epoch", 0.0).round().max(0.0) as u32;
        let points = ctx.inputs.array("points");
        let stats = ctx.inputs.array("stats");
        let mut refused = None;
        let gpu = ctx.gpu_encoder();

        let wants_tick = self.ring.wants_tick(epoch, simulation_time);
        let bytes = u64::from(count.max(1)) * std::mem::size_of::<FluidParticle>() as u64;
        let ring = if wants_tick && points.is_some() && stats.is_some() {
            self.ring.begin(gpu.device, bytes, epoch).map(Some).unwrap_or_else(|error| {
                refused = Some(format!(
                    "Matter Frame: the particle frames need 3 × {bytes} bytes the device cannot give: {error}. Raise Cell Size."
                ));
                None
            })
        } else {
            None
        };
        if let (Some(slot), Some(points), Some(stats)) = (ring, points, stats) {
            let write = slot.write;
            let pipeline = self
                .convert
                .as_ref()
                .expect("matter frame pipeline prepared at install");
            let params = FrameParams {
                count: count.min(
                    ((points.size / std::mem::size_of::<MatterPoint>() as u64) as u32)
                        .saturating_sub(first),
                ),
                previous_count: slot.previous_count,
                radius_scale: crate::matter::RADIUS_SCALE,
                first,
            };
            if params.count > 0 {
                gpu.native_enc.dispatch_compute(
                    pipeline,
                    &[
                        GpuBinding::Bytes {
                            binding: 0,
                            data: bytemuck::bytes_of(&params),
                        },
                        GpuBinding::Buffer {
                            binding: 1,
                            buffer: points,
                            offset: 0,
                        },
                        GpuBinding::Buffer {
                            binding: 2,
                            buffer: stats,
                            offset: 0,
                        },
                        GpuBinding::Buffer {
                            binding: 3,
                            buffer: self.ring.slot(slot.previous),
                            offset: 0,
                        },
                        GpuBinding::Buffer {
                            binding: 4,
                            buffer: self.ring.slot(write),
                            offset: 0,
                        },
                    ],
                    [params.count.div_ceil(256), 1, 1],
                    "node.matter_frame",
                );
            }
            self.ring.finish(slot, params.count, epoch, simulation_time);
        }

        let (blend, span) = self.ring.blend(display_time);
        for (name, value) in [
            ("count_a", self.ring.count_a() as f32),
            ("count_b", self.ring.count_b() as f32),
            ("identity_a", epoch as f32),
            ("identity_b", epoch as f32),
            ("blend", blend),
            ("span", span),
        ] {
            ctx.outputs.set_scalar(name, ParamValue::Float(value));
        }
        if let Some(error) = refused {
            ctx.error(error);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matter_frame_params_match_the_shader() {
        assert_eq!(std::mem::size_of::<FrameParams>(), 16);
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
