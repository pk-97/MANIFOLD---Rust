//! `node.matter_frame` — publish a matter domain on the particle-frame seam
//! (`docs/GPU_FLUID_SURFACE_DESIGN.md` section 3.2 outputs; the MPM design's
//! D9, D14 and the surface design's D10 display clock). Exempt from the
//! codegen mandate as cross-frame state (ADDING_PRIMITIVES.md exclusion 2):
//! it owns the A/B frame ring.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::fluid::display_blend;
use crate::node_graph::fluid_particles::FluidParticle;
use crate::node_graph::matter::{MatterLattice, MatterPoint, PADDING_NODES};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use super::matter_common::read_lattice;

const SHADER: &str = include_str!("shaders/matter_frame.wgsl");
/// Frames in the ring: A, B and the one being written.
const RING: usize = 3;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FrameParams {
    count: u32,
    previous_count: u32,
    radius_scale: f32,
    _pad0: u32,
}

/// Signed distance from each lattice node to the nearest closed wall of the
/// authored box (positive inside, negative past a closed face), the seam's
/// solid lattice for a domain whose only solids are its walls. Open faces
/// contribute nothing; with none closed, every node reads the lattice
/// diagonal.
pub(crate) fn wall_distance_lattice(lattice: &MatterLattice, closed_faces: u32) -> Vec<f32> {
    let dx = lattice.cell_size;
    let low: [f32; 3] = std::array::from_fn(|d| lattice.min[d] + PADDING_NODES as f32 * dx);
    let high: [f32; 3] = std::array::from_fn(|d| low[d] + lattice.cells[d] as f32 * dx);
    let far = lattice.nodes.iter().map(|&n| (n as f32 * dx).powi(2)).sum::<f32>().sqrt();
    let [nx, ny, nz] = lattice.nodes;
    let mut out = Vec::with_capacity(lattice.node_count() as usize);
    for k in 0..nz {
        for j in 0..ny {
            for i in 0..nx {
                let p = [i, j, k].map(|c| c as f32 * dx);
                let mut distance = far;
                for d in 0..3 {
                    let x = lattice.min[d] + p[d];
                    if closed_faces & (1 << (2 * d)) != 0 {
                        distance = distance.min(x - low[d]);
                    }
                    if closed_faces & (1 << (2 * d + 1)) != 0 {
                        distance = distance.min(high[d] - x);
                    }
                }
                out.push(distance);
            }
        }
    }
    out
}

crate::primitive! {
    name: MatterFrame,
    type_id: "node.matter_frame",
    purpose: "Publish a matter domain as particle frames for the liquid surface: after every simulated tick, write the points as an id-sorted Array(FluidParticle) frame B (the previous one becomes A), with the frame lattice, blend and span of the one-tick-behind display clock, and the walls as the solid lattice. A tick with non-finite values is never published.",
    inputs: {
        points: Array(MatterPoint) required,
        stats: Array(u32) required,
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
    },
    params: [
        ParamDef { name: Cow::Borrowed("closed_faces"), label: "Closed Faces (bits −X +X −Y +Y −Z +Z)", ty: ParamType::Int, default: ParamValue::Float(63.0), range: Some((0.0, 63.0)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "Reads node.matter_state's out and stats after the region; count, lattice, closed faces, simulation_time, display_time and epoch come from node.matter_fill and node.matter_domain. Its outputs are the particle-frame seam node.fluid_surface also publishes, so the Liquid Surface atoms and node.particles_to_copies read either solver unchanged. identity_a/b carry the domain epoch.",
    examples: ["WaterDamBreakMatter", "WaterStillPoolMatter"],
    picker: { label: "Matter Frame", category: Atom },
    summary: "Hands the simulated liquid particles to the liquid surface, one frame per simulation tick.",
    category: Particles3D,
    role: Filter,
    aliases: ["matter frame", "particle frame", "publish particles"],
    boundary_reason: CrossFrameState,
    extra_fields: {
        convert: Option<GpuComputePipeline> = None,
        slots: Vec<GpuBuffer> = Vec::new(),
        slot_count: [u32; 3] = [0; 3],
        a: usize = 0,
        b: usize = 0,
        t_a: f64 = 0.0,
        t_b: f64 = 0.0,
        epoch: Option<u32> = None,
        solid: Option<GpuBuffer> = None,
        solid_key: Option<([u32; 7], u32)> = None,
    },
}

impl Primitive for MatterFrame {
    fn provides_array_output(&self, port: &str) -> bool {
        matches!(port, "particles_a" | "particles_b" | "solid_a" | "solid_b")
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        match port {
            "particles_a" => self.slots.get(self.a),
            "particles_b" => self.slots.get(self.b),
            "solid_a" | "solid_b" => self.solid.as_ref(),
            _ => None,
        }
    }

    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        // Provided storage: a one-record hint, grown at run time.
        matches!(port_name, "particles_a" | "particles_b" | "solid_a" | "solid_b").then_some(1)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let lattice = read_lattice(ctx);
        let count = ctx.scalar_or_param("count", 0.0).round().max(0.0) as u32;
        let closed_faces = ctx.scalar_or_param("closed_faces", 63.0).round().clamp(0.0, 63.0) as u32;
        let simulation_time = f64::from(ctx.scalar_or_param("simulation_time", 0.0));
        let display_time = f64::from(ctx.scalar_or_param("display_time", 0.0));
        let epoch = ctx.scalar_or_param("epoch", 0.0).round().max(0.0) as u32;
        let points = ctx.inputs.array("points");
        let stats = ctx.inputs.array("stats");

        let solid_key = (
            [
                lattice.min[0].to_bits(), lattice.min[1].to_bits(), lattice.min[2].to_bits(),
                lattice.cell_size.to_bits(), lattice.nodes[0], lattice.nodes[1], lattice.nodes[2],
            ],
            closed_faces,
        );
        let gpu = ctx.gpu_encoder();
        if self.solid_key != Some(solid_key) {
            let distances = wall_distance_lattice(&lattice, closed_faces);
            // A fresh buffer per setup: the previous one may still be read by
            // an in-flight frame; its drop is fence-retired.
            let buffer = gpu.device.create_buffer_shared((distances.len() * 4).max(4) as u64);
            // SAFETY: new shared buffer, not yet visible to the GPU.
            unsafe { buffer.write(0, bytemuck::cast_slice(&distances)) };
            self.solid = Some(buffer);
            self.solid_key = Some(solid_key);
        }

        let restarted = self.epoch != Some(epoch);
        let new_tick = restarted || simulation_time > self.t_b;
        if new_tick && let (Some(points), Some(stats)) = (points, stats) {
            let bytes = u64::from(count.max(1)) * std::mem::size_of::<FluidParticle>() as u64;
            let mut grown = false;
            if self.slots.len() < RING || self.slots.iter().any(|s| s.size < bytes) {
                // Shared storage: capture and look metrics read frames back.
                self.slots = (0..RING).map(|_| gpu.device.create_buffer_shared(bytes)).collect();
                self.slot_count = [0; RING];
                grown = true;
            }
            let write = (0..RING).find(|&i| i != self.a && i != self.b).unwrap_or(0);
            let write = if self.a == self.b { (self.b + 1) % RING } else { write };
            let previous = self.b;
            let pipeline = self.convert.get_or_insert_with(|| {
                gpu.device.create_compute_pipeline(SHADER, "cs_main", "node.matter_frame")
            });
            let params = FrameParams {
                count: count.min((points.size / std::mem::size_of::<MatterPoint>() as u64) as u32),
                previous_count: if grown { 0 } else { self.slot_count[previous] },
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
                        GpuBinding::Buffer { binding: 3, buffer: &self.slots[previous], offset: 0 },
                        GpuBinding::Buffer { binding: 4, buffer: &self.slots[write], offset: 0 },
                    ],
                    [params.count.div_ceil(256), 1, 1],
                    "node.matter_frame",
                );
            }
            self.slot_count[write] = params.count;
            if restarted || grown {
                self.a = write;
                self.t_a = simulation_time;
            } else {
                self.a = self.b;
                self.t_a = self.t_b;
            }
            self.b = write;
            self.t_b = simulation_time;
            self.epoch = Some(epoch);
        }

        let (blend, span) = display_blend(display_time, self.t_a, self.t_b);
        for (name, value) in [
            ("count_a", self.slot_count[self.a] as f32),
            ("count_b", self.slot_count[self.b] as f32),
            ("identity_a", epoch as f32),
            ("identity_b", epoch as f32),
            ("grid_nodes_x", lattice.nodes[0] as f32),
            ("grid_nodes_y", lattice.nodes[1] as f32),
            ("grid_nodes_z", lattice.nodes[2] as f32),
            ("blend", blend),
            ("span", span),
        ] {
            ctx.outputs.set_scalar(name, ParamValue::Float(value));
        }
        ctx.outputs.set_transform("grid_bounds", lattice.bounds());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matter_frame_wall_lattice_is_signed_distance_to_closed_faces() {
        let layout = crate::node_graph::fluid::domain_layout(None, 1.0, 8).unwrap();
        let lattice = MatterLattice::from_layout(&layout);
        let dx = lattice.cell_size;
        let all = wall_distance_lattice(&lattice, 63);
        let n = lattice.nodes;
        let at = |i: u32, j: u32, k: u32| all[((k * n[1] + j) * n[0] + i) as usize];
        // The authored floor sits on node 3; one node below reads −dx.
        assert!((at(7, 3, 7)).abs() < 1e-6);
        assert!((at(7, 2, 7) + dx).abs() < 1e-6);
        assert!((at(7, 5, 7) - 2.0 * dx).abs() < 1e-5);
        // An open top: the node above the ceiling is not inside a solid.
        let open_top = wall_distance_lattice(&lattice, 63 & !(1 << 3));
        let top = n[1] - 1;
        assert!(open_top[((7 * n[1] + top) * n[0] + 7) as usize] > 0.0);
        assert_eq!(std::mem::size_of::<FrameParams>(), 16);
    }
}
