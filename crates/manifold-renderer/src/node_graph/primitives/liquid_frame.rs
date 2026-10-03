//! `node.liquid_frame` — publish a particle liquid on the particle-frame seam
//! (`docs/LIQUID_SOLVER_SEAM_DESIGN.md` section 3.1): after every frame that
//! ran a tick, the state becomes frame B and the previous B becomes A, with
//! the frame lattice, the display blend and the solid lattice, and the tick's
//! face grid (section 3.2) when it is wired. Exempt from the codegen mandate
//! as cross-frame state (ADDING_PRIMITIVES.md exclusion 2): it owns the A/B
//! frame ring and the published faces.
//!
//! Its optional interior-distance ring carries the Ferstl et al. (2016)
//! narrow-band field beside the particle frames.

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline};

use super::liquid_stats::{with_stats_layout, LIQUID_STATS_WORDS};
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::fluid_particles::FluidParticle;
use crate::node_graph::liquid::frame_ring::{FrameRing, RING};
use crate::node_graph::liquid::grid::{interior_bytes, FACE_INPUT_PORTS, PublishedFaces};
use crate::node_graph::liquid::lattice::LiquidLattice;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

const SHADER_SOURCE: &str = include_str!("shaders/liquid_frame.wgsl");

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FrameParams {
    count: u32,
    previous_count: u32,
    slots: u32,
    _pad0: u32,
}

crate::primitive! {
    name: LiquidFrame,
    type_id: "node.liquid_frame",
    purpose: "Publish a particle liquid's state as particle frames for the liquid surface: after every simulated tick, write the first `count` records as frame B with id 0 (the previous B becomes A), slots past count at radius 0, with the frame lattice, the blend and span of the one-tick-behind display clock, and the solid lattice: node.liquid_solid_distance's walls and bodies when `solid` is wired, each tick's copy kept beside its frame, otherwise the walls alone. With face_u_in, face_v_in and face_w_in wired, each tick's face grid is published beside frame B as face_u, face_v and face_w over the domain's cells, with face_valid_layers from the param. An optional cell-centred `interior` distance is copied into matching A/B slots. A tick whose stats flag a non-finite record or narrow-band capacity shortage is never published.",
    inputs: {
        particles: Array(FluidParticle) required,
        stats: Array(u32) required,
        interior: Array(f32) optional,
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
        interior_a: Array(f32), interior_b: Array(f32),
        count_a: ScalarF32, count_b: ScalarF32, identity_a: ScalarF32, identity_b: ScalarF32,
        solid_a: Array(f32), solid_b: Array(f32), grid_bounds: Transform,
        grid_nodes_x: ScalarF32, grid_nodes_y: ScalarF32, grid_nodes_z: ScalarF32,
        blend: ScalarF32, span: ScalarF32,
        face_u: Array(f32), face_v: Array(f32), face_w: Array(f32),
        face_cells_x: ScalarF32, face_cells_y: ScalarF32, face_cells_z: ScalarF32, face_valid_layers: ScalarF32,
    },
    params: [
        ParamDef { name: std::borrow::Cow::Borrowed("closed_faces"), label: "Closed Faces (bits −X +X −Y +Y −Z +Z)", ty: ParamType::Int, default: ParamValue::Float(63.0), range: Some((0.0, 63.0)), enum_values: &[] },
        ParamDef { name: std::borrow::Cow::Borrowed("face_valid_layers"), label: "Face Valid Layers", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 8.0)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "Reads node.liquid_state's out and stats after the region; count comes from the fill, and the lattice, closed faces, simulation_time, display_time and epoch from the liquid's domain; solid from node.liquid_solid_distance; the face inputs from three node.face_sample_component on liquid_state's faces. Its outputs are the particle-frame seam node.fluid_surface and node.matter_frame also publish, so the Liquid Surface atoms and whitewater read any solver unchanged. face_valid_layers is how many face layers past the liquid the solver extended its velocity into; it reads 0 unless all three axes are wired. identity_a/b carry the domain epoch. Ids are 0: a particle solver may reorder its state every tick.",
    examples: [],
    picker: { label: "Liquid Frame", category: Atom },
    summary: "Hands a simulated particle liquid to the liquid surface, one frame per simulation tick.",
    category: Particles3D,
    role: Filter,
    aliases: ["liquid frame", "particle frame", "publish particles"],
    boundary_reason: CrossFrameState,
    extra_fields: {
        convert: Option<GpuComputePipeline> = None,
        ring: FrameRing = FrameRing::default(),
        solid: Option<GpuBuffer> = None,
        solid_key: Option<([u32; 7], u32)> = None,
        solid_slots: Vec<GpuBuffer> = Vec::new(),
        solid_wired: bool = false,
        interior_slots: Vec<GpuBuffer> = Vec::new(),
        interior_key: Option<([u32; 3], u32)> = None,
        interior_epoch: Option<u32> = None,
        lattice_key: Option<[u32; 7]> = None,
        faces: PublishedFaces = PublishedFaces::default(),
    },
}

impl Primitive for LiquidFrame {
    fn prepare_pipelines(&mut self, device: &manifold_gpu::GpuDevice) {
        if self.convert.is_none() {
            let shader = with_stats_layout(SHADER_SOURCE);
            self.convert = Some(device.create_compute_pipeline(&shader, "cs_main", "node.liquid_frame"));
        }
        self.faces.prepare(device);
    }

    fn provides_array_output(&self, port: &str) -> bool {
        matches!(port, "particles_a" | "particles_b" | "interior_a" | "interior_b" | "solid_a" | "solid_b")
            || PublishedFaces::provides(port)
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        match port {
            "particles_a" => self.ring.buffer_a(),
            "particles_b" => self.ring.buffer_b(),
            "interior_a" => self.interior_slots.get(self.ring.a()),
            "interior_b" => self.interior_slots.get(self.ring.b()),
            "solid_a" if self.solid_wired => self.solid_slots.get(self.ring.a()),
            "solid_b" if self.solid_wired => self.solid_slots.get(self.ring.b()),
            "solid_a" | "solid_b" => self.solid.as_ref(),
            _ => self.faces.buffer(port),
        }
    }

    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        // Provided storage: a one-record hint, grown at run time.
        self.provides_array_output(port_name).then_some(1)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(lattice) = LiquidLattice::from_wires(ctx, "Liquid Frame") else {
            return;
        };
        let count = ctx.scalar_or_param("count", 0.0).round().max(0.0) as u32;
        let closed_faces = ctx.scalar_or_param("closed_faces", 63.0).round().clamp(0.0, 63.0) as u32;
        let simulation_time = f64::from(ctx.scalar_or_param("simulation_time", 0.0));
        let display_time = f64::from(ctx.scalar_or_param("display_time", 0.0));
        let epoch = ctx.scalar_or_param("epoch", 0.0).round().max(0.0) as u32;
        let particles = ctx.inputs.array("particles");
        let stats = ctx.inputs.array("stats");
        let interior = ctx.inputs.array("interior");
        let solid_in = ctx.inputs.array("solid");
        self.solid_wired = solid_in.is_some();
        let faces_in = FACE_INPUT_PORTS.map(|port| ctx.inputs.array(port));
        let face_valid_layers = ctx.scalar_or_param("face_valid_layers", 0.0).round().clamp(0.0, 8.0);

        let solid_key = (
            [
                lattice.min()[0].to_bits(), lattice.min()[1].to_bits(), lattice.min()[2].to_bits(),
                lattice.cell_size().to_bits(), lattice.nodes()[0], lattice.nodes()[1], lattice.nodes()[2],
            ],
            closed_faces,
        );
        let lattice_key = [
            lattice.min()[0].to_bits(), lattice.min()[1].to_bits(), lattice.min()[2].to_bits(),
            lattice.cell_size().to_bits(), lattice.nodes()[0], lattice.nodes()[1], lattice.nodes()[2],
        ];
        let lattice_changed = self.lattice_key.is_some_and(|key| key != lattice_key);
        if lattice_changed {
            self.ring.invalidate();
        }
        let mut refused = None;
        let gpu = ctx.gpu_encoder();
        let interior_size = interior_bytes(lattice.cells());
        let interior_key = (lattice.cells(), lattice.cell_size().to_bits());
        let interior_valid = if interior_size == 0 {
            if interior.is_some() {
                refused = Some("Liquid Frame: interior distance has zero cells".to_string());
            }
            false
        } else {
            match interior {
                None => false,
                Some(field) if field.size == interior_size => true,
                Some(field) => {
                    refused = Some(format!(
                        "Liquid Frame: interior distance holds {} bytes; the lattice needs exactly {interior_size} bytes ({})",
                        field.size,
                        lattice.cells().iter().product::<u32>()
                    ));
                    false
                }
            }
        };
        if interior.is_none() {
            // An unwired optional field cannot keep exposing a previous
            // lattice's storage through the provided outputs.
            self.interior_slots.clear();
            self.interior_key = None;
            self.interior_epoch = None;
        } else if interior_valid {
            let fresh = self.interior_key != Some(interior_key)
                || self.interior_slots.len() < RING
                || self.interior_slots.iter().any(|slot| slot.size < interior_size);
            if fresh {
                self.interior_slots = crate::node_graph::scene_modifier_expand::admit_candidate_bytes(
                    gpu.device.modifier_memory_snapshot(),
                    RING as u64 * interior_size.max(4),
                )
                .map_err(|error| error.to_string())
                .and_then(|()| {
                    (0..RING)
                        .map(|_| gpu.device.try_create_buffer_shared(interior_size.max(4)))
                        .collect::<Result<_, _>>()
                })
                .unwrap_or_else(|error| {
                    refused = Some(format!(
                        "Liquid Frame: the interior distance needs {RING} × {interior_size} bytes the device cannot give: {error}. Lower Resolution."
                    ));
                    Vec::new()
                });
                self.interior_key = (self.interior_slots.len() == RING).then_some(interior_key);
                self.interior_epoch = None;
                for slot in &self.interior_slots {
                    if let Err(error) = self.faces.clear_interior(gpu, slot) {
                        refused = Some(format!("Liquid Frame: {error}"));
                    }
                }
            }
            if self.interior_epoch != Some(epoch) {
                for slot in &self.interior_slots {
                    if let Err(error) = self.faces.clear_interior(gpu, slot) {
                        refused = Some(format!("Liquid Frame: {error}"));
                    }
                }
                self.interior_epoch = Some(epoch);
            }
            if lattice_changed {
                for slot in &self.interior_slots {
                    if let Err(error) = self.faces.clear_interior(gpu, slot) {
                        refused = Some(format!("Liquid Frame: {error}"));
                    }
                }
            }
        } else {
            self.interior_slots.clear();
            self.interior_key = None;
            self.interior_epoch = None;
        }
        if !self.solid_wired && self.solid_key != Some(solid_key) {
            let distances = lattice.wall_distance(closed_faces);
            // A fresh buffer per setup: the previous one may still be read by
            // an in-flight frame; its drop is fence-retired.
            let buffer = gpu.device.create_buffer_shared((distances.len() * 4).max(4) as u64);
            // SAFETY: new shared buffer, not yet visible to the GPU.
            unsafe { buffer.write(0, bytemuck::cast_slice(&distances)) };
            self.solid = Some(buffer);
            self.solid_key = Some(solid_key);
        }

        let record = std::mem::size_of::<FluidParticle>() as u64;
        let ready = particles.zip(stats).filter(|(_, stats)| stats.size >= u64::from(LIQUID_STATS_WORDS) * 4);
        let bytes = u64::from(count.max(1)) * record;
        let interior_ready = interior.is_none() || (interior_valid && self.interior_slots.len() == RING);
        let ring = if self.ring.wants_tick(epoch, simulation_time) && ready.is_some() && interior_ready {
            self.ring.begin(gpu.device, bytes, epoch).map(Some).unwrap_or_else(|error| {
                refused = Some(format!(
                    "Liquid Frame: the particle frames need {RING} × {bytes} bytes the device cannot give: {error}. Lower Resolution."
                ));
                None
            })
        } else {
            None
        };
        let published = ring.is_some();
        if let (Some(slot), Some((particles, stats))) = (ring, ready) {
            let write = slot.write;
            if slot.grown {
                for field in &self.interior_slots {
                    if let Err(error) = self.faces.clear_interior(gpu, field) {
                        refused = Some(format!("Liquid Frame: {error}"));
                    }
                }
            }
            let pipeline = self.convert.as_ref().expect("liquid frame pipeline prepared at install");
            let count = count.min((particles.size / record) as u32);
            let target = self.ring.slot(write);
            let params = FrameParams {
                count,
                previous_count: if lattice_changed {
                    0
                } else {
                    slot.previous_count.min((self.ring.slot(slot.previous).size / record) as u32)
                },
                slots: (target.size / record) as u32,
                _pad0: 0,
            };
            gpu.native_enc.dispatch_compute(
                pipeline,
                &[
                    GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&params) },
                    GpuBinding::Buffer { binding: 1, buffer: particles, offset: 0 },
                    GpuBinding::Buffer { binding: 2, buffer: stats, offset: 0 },
                    GpuBinding::Buffer { binding: 3, buffer: self.ring.slot(slot.previous), offset: 0 },
                    GpuBinding::Buffer { binding: 4, buffer: target, offset: 0 },
                ],
                [params.slots.div_ceil(256), 1, 1],
                "node.liquid_frame",
            );
            if let Some(solid_in) = solid_in {
                // Each tick's solid lattice sits beside its frame, so A and B
                // each carry the solids where their particles were.
                let bytes = lattice.solid_bytes();
                let fresh = self.solid_slots.len() < RING || self.solid_slots.iter().any(|s| s.size < bytes);
                if fresh {
                    // A ring the device cannot give leaves none: this node
                    // names it, and the surface atoms draw nothing without it.
                    let device = gpu.device;
                    self.solid_slots = crate::node_graph::scene_modifier_expand::admit_candidate_bytes(
                        device.modifier_memory_snapshot(),
                        RING as u64 * bytes.max(4),
                    )
                    .map_err(|error| error.to_string())
                    .and_then(|()| (0..RING).map(|_| device.try_create_buffer_shared(bytes.max(4))).collect::<Result<_, _>>())
                    .unwrap_or_else(|error| {
                        refused = Some(format!(
                            "Liquid Frame: the solid lattice needs {RING} × {bytes} bytes the device cannot give: {error}. Lower Resolution."
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
            if let Some(interior) = interior
                && let Err(error) = self.faces.copy_gated(
                    gpu,
                    interior,
                    &self.interior_slots[slot.previous],
                    stats,
                    &self.interior_slots[write],
                    interior_size,
                    "node.liquid_frame.interior",
                )
            {
                refused = Some(format!("Liquid Frame: {error}"));
            }
            self.ring.finish(slot, count, epoch, simulation_time);
            self.lattice_key = Some(lattice_key);
        }
        let faces_refused =
            self.faces.publish(gpu, lattice.cells(), faces_in, ready.map(|(_, stats)| stats), published, "Liquid Frame");

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
            ("face_valid_layers", if faces_published { face_valid_layers } else { 0.0 }),
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
    fn liquid_frame_params_match_the_shader() {
        let shader = with_stats_layout(SHADER_SOURCE);
        assert_eq!(std::mem::size_of::<FrameParams>(), 16);
        assert!(shader.contains("struct FrameParams"));
        assert!(shader.contains("NARROW_BAND_SHORTAGE_WORD"), "capacity-short ticks repeat particle frames");
        let frame = LiquidFrame::new();
        assert!(frame.provides_array_output("interior_a"));
        assert!(frame.provides_array_output("interior_b"));
        let module = naga::front::wgsl::parse_str(&shader).expect("liquid_frame.wgsl parses");
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
            .validate(&module)
            .expect("liquid_frame.wgsl validates");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::{with_stats_layout, FrameParams, LIQUID_STATS_WORDS, SHADER_SOURCE};
    use super::super::liquid_stats::NARROW_BAND_SHORTAGE_WORD;
    use crate::node_graph::fluid_particles::FluidParticle;
    use super::super::liquid_surface_tests::read;
    use manifold_gpu::{GpuBinding, GpuBuffer};

    #[repr(C)]
    #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
    struct FaceParams {
        len: u32,
        _pad0: u32,
        _pad1: u32,
        has_shortage: u32,
    }

    fn shared<T: bytemuck::Pod>(device: &crate::TestDevice, values: &[T]) -> GpuBuffer {
        let buffer = device.create_buffer_shared((std::mem::size_of_val(values) as u64).max(16));
        buffer.zero_fill();
        if !values.is_empty() {
            // SAFETY: shared buffer is sized for `values`; no work is in flight.
            unsafe { buffer.write(0, bytemuck::cast_slice(values)) };
        }
        buffer
    }

    fn bind(binding: u32, buffer: &GpuBuffer) -> GpuBinding<'_> {
        GpuBinding::Buffer { binding, buffer, offset: 0 }
    }

    #[test]
    fn gpu_flip_narrow_band_publication_repeats_failed_ticks() {
        let device = crate::test_device();
        let state = [
            FluidParticle { position_radius: [1.0, 2.0, 3.0, 0.5], velocity: [4.0, 5.0, 6.0], id: 0 },
            FluidParticle { position_radius: [7.0, 8.0, 9.0, 0.5], velocity: [1.0, 2.0, 3.0], id: 0 },
        ];
        let previous = [
            FluidParticle { position_radius: [-1.0, -2.0, -3.0, 0.5], velocity: [-4.0, -5.0, -6.0], id: 2 },
            FluidParticle { position_radius: [-7.0, -8.0, -9.0, 0.5], velocity: [-1.0, -2.0, -3.0], id: 3 },
        ];
        let frame_params = FrameParams { count: 2, previous_count: 2, slots: 2, _pad0: 0 };
        for shortage in [false, true] {
            let state_buffer = shared(&device, &state);
            let previous_buffer = shared(&device, &previous);
            let stats = shared(&device, &[0u32; LIQUID_STATS_WORDS as usize]);
            if shortage {
                unsafe { stats.write(u64::from(NARROW_BAND_SHORTAGE_WORD) * 4, &1u32.to_ne_bytes()) };
            }
            let frame = shared(&device, &[FluidParticle::default(); 2]);
            let shader = with_stats_layout(SHADER_SOURCE);
            let pipeline = device.create_compute_pipeline(&shader, "cs_main", "liquid-frame-proof");
            let mut encoder = device.create_encoder("liquid-frame-particle-proof");
            encoder.dispatch_compute(
                &pipeline,
                &[
                    GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&frame_params) },
                    bind(1, &state_buffer), bind(2, &stats), bind(3, &previous_buffer), bind(4, &frame),
                ],
                [1, 1, 1],
                "liquid-frame-particle-proof",
            );
            encoder.commit_and_wait_completed();
            let got: Vec<FluidParticle> = read(&frame, 2);
            let expected = if shortage { previous } else { state };
            assert_eq!(got, expected, "particle publication shortage={shortage}");

            let source = shared(&device, &[1.0f32, 2.0, 3.0, 4.0]);
            let fallback = shared(&device, &[9.0f32, 8.0, 7.0, 6.0]);
            let interior = shared(&device, &[0.0f32; 4]);
            let face_params = FaceParams { len: 4, _pad0: 0, _pad1: 0, has_shortage: u32::from(shortage) };
            let face_shader = with_stats_layout(include_str!("shaders/liquid_frame_faces.wgsl"));
            let face_pipeline = device.create_compute_pipeline(
                &face_shader,
                "cs_main",
                "liquid-frame-interior-proof",
            );
            let mut encoder = device.create_encoder("liquid-frame-interior-proof");
            encoder.copy_buffer_to_buffer(&fallback, &interior, 16);
            encoder.dispatch_compute(
                &face_pipeline,
                &[
                    GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&face_params) },
                    bind(1, &source), bind(2, &stats), bind(3, &interior),
                ],
                [1, 1, 1],
                "liquid-frame-interior-proof",
            );
            encoder.commit_and_wait_completed();
            let got: Vec<f32> = read(&interior, 4);
            let expected = if shortage { [9.0, 8.0, 7.0, 6.0] } else { [1.0, 2.0, 3.0, 4.0] };
            assert_eq!(got, expected, "interior publication shortage={shortage}");
        }
    }
}
