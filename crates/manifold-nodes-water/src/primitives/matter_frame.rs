//! `node.matter_frame` — publish a matter domain on the particle-frame seam
//! (`docs/GPU_FLUID_SURFACE_DESIGN.md` section 3.2 outputs; the MPM design's
//! D9, D14 and the surface design's D10 display clock). Exempt from the
//! codegen mandate as cross-frame state (ADDING_PRIMITIVES.md exclusion 2):
//! it owns the A/B frame ring.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline};

use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::particles::FluidParticle;
use crate::liquid::frame_ring::{FrameRing, RING};
use crate::liquid::grid::{FACE_INPUT_PORTS, PublishedFaces};
use crate::liquid::lattice::LiquidLattice;
use crate::matter::{MatterPoint, solid_bytes};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;

use super::matter_face_component::MATTER_FACE_VALID_LAYERS;

const SHADER: &str = include_str!("shaders/matter_frame.wgsl");

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FrameParams {
    count: u32,
    previous_count: u32,
    radius_scale: f32,
    _pad0: u32,
}

manifold_node_engine::primitive! {
    name: MatterFrame,
    type_id: "node.matter_frame",
    purpose: "Publish a matter domain as particle frames for the liquid surface: after every simulated tick, write the points as an id-sorted Array(FluidParticle) frame B (the previous one becomes A), with the frame lattice, blend and span of the one-tick-behind display clock, and the solid lattice: node.liquid_solid_distance's walls and bodies when `solid` is wired, each tick's copy kept beside its frame, otherwise the walls alone. With face_u_in, face_v_in and face_w_in wired, it also publishes frame B's face grid (face_u, face_v, face_w over face_cells_x/y/z, and face_valid_layers, 0: only the liquid's own faces and those sharing an edge with them are sure to carry velocity), copied each tick and held between ticks. A tick with non-finite values is never published, as particles or as faces.",
    inputs: {
        points: Array(MatterPoint) required,
        stats: Array(u32) required,
        solid: Array(f32) optional,
        face_u_in: Array(f32) optional, face_v_in: Array(f32) optional, face_w_in: Array(f32) optional,
        count: ScalarF32 optional,
        lattice_min_x: ScalarF32 optional, lattice_min_y: ScalarF32 optional, lattice_min_z: ScalarF32 optional,
        cell_size: ScalarF32 optional,
        nodes_x: ScalarF32 optional, nodes_y: ScalarF32 optional, nodes_z: ScalarF32 optional,
        closed_faces: ScalarF32 optional,
        simulation_time: ScalarF32 optional,
        display_time: ScalarF32 optional,
        epoch: ScalarF32 optional,
    },
    outputs: {
        particles_a: Array(FluidParticle), particles_b: Array(FluidParticle),
        count_a: ScalarF32, count_b: ScalarF32, identity_a: ScalarF32, identity_b: ScalarF32,
        solid_a: Array(f32), solid_b: Array(f32), grid_bounds: Transform,
        grid_nodes_x: ScalarF32, grid_nodes_y: ScalarF32, grid_nodes_z: ScalarF32,
        blend: ScalarF32, span: ScalarF32,
        face_u: Array(f32), face_v: Array(f32), face_w: Array(f32),
        face_cells_x: ScalarF32, face_cells_y: ScalarF32, face_cells_z: ScalarF32, face_valid_layers: ScalarF32,
    },
    params: [
        ParamDef { name: Cow::Borrowed("closed_faces"), label: "Closed Faces (bits −X +X −Y +Y −Z +Z)", ty: ParamType::Int, default: ParamValue::Float(63.0), range: Some((0.0, 63.0)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "Reads node.matter_state's out and stats after the region; count, lattice, closed faces, simulation_time, display_time and epoch come from node.matter_fill and node.matter_domain. Its outputs are the particle-frame seam node.fluid_surface also publishes, so the Liquid Surface atoms and node.particles_to_copies read either solver unchanged. identity_a/b carry the domain epoch. The face grid inputs come from three node.matter_face_component on node.matter_state's grid; leave them unwired unless something reads the face grid, since each costs a copy per tick.",
    examples: ["WaterDamBreakMatter", "WaterStillPoolMatter"],
    picker: { label: "Matter Frame", category: Atom },
    summary: "Hands the simulated liquid particles to the liquid surface, one frame per simulation tick.",
    category: Particles3D,
    role: Filter,
    aliases: ["matter frame", "particle frame", "publish particles"],
    boundary_reason: CrossFrameState,
    extra_fields: {
        convert: Option<GpuComputePipeline> = None,
        ring: FrameRing = FrameRing::default(),
        solid: Option<GpuBuffer> = None,
        solid_key: Option<([u32; 7], u32)> = None,
        solid_slots: Vec<GpuBuffer> = Vec::new(),
        solid_wired: bool = false,
        faces: PublishedFaces = PublishedFaces::default(),
    },
}

impl Primitive for MatterFrame {
    fn prepare_pipelines(&mut self, device: &manifold_gpu::GpuDevice) {
        if self.convert.is_none() {
            self.convert = Some(device.create_compute_pipeline(SHADER, "cs_main", "node.matter_frame"));
        }
        self.faces.prepare(device);
    }

    fn provides_array_output(&self, port: &str) -> bool {
        matches!(port, "particles_a" | "particles_b" | "solid_a" | "solid_b") || PublishedFaces::provides(port)
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        match port {
            "particles_a" => self.ring.buffer_a(),
            "particles_b" => self.ring.buffer_b(),
            "solid_a" if self.solid_wired => self.solid_slots.get(self.ring.a()),
            "solid_b" if self.solid_wired => self.solid_slots.get(self.ring.b()),
            "solid_a" | "solid_b" => self.solid.as_ref(),
            _ => self.faces.buffer(port),
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
        let Some(lattice) = LiquidLattice::from_wires(ctx, "Matter Frame") else {
            return;
        };
        let count = ctx.scalar_or_param("count", 0.0).round().max(0.0) as u32;
        let closed_faces = ctx.scalar_or_param("closed_faces", 63.0).round().clamp(0.0, 63.0) as u32;
        let simulation_time = f64::from(ctx.scalar_or_param("simulation_time", 0.0));
        let display_time = f64::from(ctx.scalar_or_param("display_time", 0.0));
        let epoch = ctx.scalar_or_param("epoch", 0.0).round().max(0.0) as u32;
        let points = ctx.inputs.array("points");
        let stats = ctx.inputs.array("stats");
        let solid_in = ctx.inputs.array("solid");
        self.solid_wired = solid_in.is_some();
        let faces_in = FACE_INPUT_PORTS.map(|port| ctx.inputs.array(port));

        let solid_key = (
            [
                lattice.min()[0].to_bits(), lattice.min()[1].to_bits(), lattice.min()[2].to_bits(),
                lattice.cell_size().to_bits(), lattice.nodes()[0], lattice.nodes()[1], lattice.nodes()[2],
            ],
            closed_faces,
        );
        let mut refused = None;
        let gpu = ctx.gpu_encoder();
        if self.solid_key != Some(solid_key) {
            let distances = lattice.wall_distance(closed_faces);
            // A fresh buffer per setup: the previous one may still be read by
            // an in-flight frame; its drop is fence-retired.
            let buffer = gpu.device.create_buffer_shared((distances.len() * 4).max(4) as u64);
            // SAFETY: new shared buffer, not yet visible to the GPU.
            unsafe { buffer.write(0, bytemuck::cast_slice(&distances)) };
            self.solid = Some(buffer);
            self.solid_key = Some(solid_key);
        }

        let wants_tick = self.ring.wants_tick(epoch, simulation_time);
        let bytes = u64::from(count.max(1)) * std::mem::size_of::<FluidParticle>() as u64;
        let ring = if wants_tick && points.is_some() && stats.is_some() {
            self.ring.begin(gpu.device, bytes, epoch).map(Some).unwrap_or_else(|error| {
                refused = Some(format!(
                    "Matter Frame: the particle frames need 3 × {bytes} bytes the device cannot give: {error}. Lower Resolution."
                ));
                None
            })
        } else {
            None
        };
        let published = ring.is_some();
        if let (Some(slot), Some(points), Some(stats)) = (ring, points, stats) {
            let write = slot.write;
            let pipeline = self.convert.as_ref().expect("matter frame pipeline prepared at install");
            let params = FrameParams {
                count: count.min((points.size / std::mem::size_of::<MatterPoint>() as u64) as u32),
                previous_count: slot.previous_count,
                radius_scale: (3.0 / (4.0 * std::f32::consts::PI)).cbrt(),
                _pad0: 0,
            };
            if params.count > 0 {
                gpu.native_enc.dispatch_compute(
                    pipeline,
                    &[
                        GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&params) },
                        GpuBinding::Buffer { binding: 1, buffer: points, offset: 0 },
                        GpuBinding::Buffer { binding: 2, buffer: stats, offset: 0 },
                        GpuBinding::Buffer { binding: 3, buffer: self.ring.slot(slot.previous), offset: 0 },
                        GpuBinding::Buffer { binding: 4, buffer: self.ring.slot(write), offset: 0 },
                    ],
                    [params.count.div_ceil(256), 1, 1],
                    "node.matter_frame",
                );
            }
            if let Some(solid_in) = solid_in {
                // Each tick's solid lattice sits beside its frame, so A and B
                // each carry the bodies where their particles were.
                let bytes = solid_bytes(lattice.nodes());
                let fresh = self.solid_slots.len() < RING || self.solid_slots.iter().any(|s| s.size < bytes);
                if fresh {
                    // A ring the device cannot give leaves none: this node
                    // names it, and the surface atoms draw nothing without it.
                    let device = gpu.device;
                    self.solid_slots = manifold_node_engine::load::expand::admit_candidate_bytes(
                        device.modifier_memory_snapshot(),
                        RING as u64 * bytes.max(4),
                    )
                    .map_err(|error| error.to_string())
                    .and_then(|()| (0..RING).map(|_| device.try_create_buffer_shared(bytes.max(4))).collect::<Result<_, _>>())
                        .unwrap_or_else(|error| {
                            refused = Some(format!(
                                "Matter Frame: the solid lattice needs 3 × {bytes} bytes the device cannot give: {error}. Lower Resolution."
                            ));
                            Vec::new()
                        });
                }
                let bytes = bytes.min(solid_in.size);
                let targets = if fresh { 0..self.solid_slots.len() } else { write..write + 1 };
                for slot in targets {
                    gpu.native_enc.copy_buffer_to_buffer(solid_in, &self.solid_slots[slot], bytes);
                }
            }
            self.ring.finish(slot, params.count, epoch, simulation_time);
        }
        let faces_refused = self.faces.publish(gpu, lattice.cells(), faces_in, stats, published, "Matter Frame");

        let (blend, span) = self.ring.blend(display_time);
        let faces_published = self.faces.complete();
        for (name, value) in [
            ("count_a", self.ring.count_a() as f32),
            ("count_b", self.ring.count_b() as f32),
            ("identity_a", epoch as f32),
            ("identity_b", epoch as f32),
            ("grid_nodes_x", lattice.nodes()[0] as f32),
            ("grid_nodes_y", lattice.nodes()[1] as f32),
            ("grid_nodes_z", lattice.nodes()[2] as f32),
            ("blend", blend),
            ("span", span),
            ("face_cells_x", lattice.cells()[0] as f32),
            ("face_cells_y", lattice.cells()[1] as f32),
            ("face_cells_z", lattice.cells()[2] as f32),
            ("face_valid_layers", if faces_published { MATTER_FACE_VALID_LAYERS as f32 } else { 0.0 }),
        ] {
            ctx.outputs.set_scalar(name, ParamValue::Float(value));
        }
        ctx.outputs.set_transform("grid_bounds", lattice.bounds());
        for error in [refused, faces_refused].into_iter().flatten() {
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
