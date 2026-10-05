//! `node.liquid_frame` — publish a particle liquid on the particle-frame seam
//! (`docs/LIQUID_SOLVER_SEAM_DESIGN.md` section 3.1): every frame that ends
//! on a new tick publishes it, with the solid lattice, the face grid, the
//! interior distance and the whitewater classes when wired, into a retained
//! history slot of its own (`docs/GPU_FLIP_DISPLAY_HISTORY_DESIGN.md`).
//! Frames A and B are the retired pair bracketing the display time. Exempt
//! from the codegen mandate as cross-frame state (ADDING_PRIMITIVES.md
//! exclusion 2): it owns the publication history.
//!
//! Its optional interior distance carries the Ferstl et al. (2016)
//! narrow-band field beside the particle frames.

use manifold_gpu::GpuBuffer;

use super::liquid_stats::LIQUID_STATS_WORDS;
use super::particle_publication::{ParticlePublication, Publication};
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::fluid_particles::FluidParticle;
use crate::node_graph::liquid::frame_history::{
    FIELD_FACES, FIELD_INTERIOR, FIELD_SOLID, FIELD_WHITEWATER, FIELDS, FrameHistory, Layout,
};
use crate::node_graph::liquid::grid::{interior_bytes, face_len, FACE_GRID_PORTS, FACE_INPUT_PORTS};
use crate::node_graph::liquid::lattice::{FlipSolverGrid, LiquidLattice};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Whitewater class inputs and the selected frame's copies, by class.
pub const WHITEWATER_INPUTS: [&str; 4] = ["foam_in", "bubble_in", "spray_in", "dust_in"];
pub const WHITEWATER_OUTPUTS: [&str; 4] = ["foam_b", "bubble_b", "spray_b", "dust_b"];

#[cfg(feature = "gpu-proofs")]
thread_local! {
    /// Proof hook: the next publication on this thread fails to encode.
    pub(crate) static FAIL_NEXT_PUBLICATION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Every provided array output, in [`LiquidFrame`]'s binding table order.
const PROVIDED_PORTS: usize = 13;
const PROVIDED: [&str; PROVIDED_PORTS] = [
    "particles_a", "particles_b", "interior_a", "interior_b", "solid_a", "solid_b",
    "face_u", "face_v", "face_w", "foam_b", "bubble_b", "spray_b", "dust_b",
];

crate::primitive! {
    name: LiquidFrame,
    type_id: "node.liquid_frame",
    purpose: "Publish a particle liquid's state as particle frames for the liquid surface: every frame that ends on a new simulated tick publishes a compact copy sorted by persistent nonzero birth id into a retained history slot, with the retired live count and a cleared tail; frames A and B are the retired pair bracketing the display time, with its blend and span. Publishes the native FLIP mesh lattice (1.5-cell padding) when Native Mesh Grid is enabled, otherwise the legacy simulation lattice, and the solid lattice: node.liquid_solid_distance's walls and bodies when `solid` is wired, each tick's copy kept beside its frame, otherwise the walls alone. With face_u_in, face_v_in and face_w_in wired, each tick's face grid is kept beside its frame and published for frame B as face_u, face_v and face_w over the domain's cells, with face_valid_layers from the param. An optional cell-centred `interior` distance is kept per frame as interior_a/b, and the whitewater classes foam_in, bubble_in, spray_in and dust_in per frame as frame B's foam_b, bubble_b, spray_b and dust_b. A tick whose stats flag a non-finite record or narrow-band capacity shortage is never shown.",
    inputs: {
        particles: Array(FluidParticle) required,
        stats: Array(u32) required,
        identity: Array(u32) required,
        interior: Array(f32) optional,
        solid: Array(f32) optional,
        face_u_in: Array(f32) optional, face_v_in: Array(f32) optional, face_w_in: Array(f32) optional,
        foam_in: Array(FluidParticle) optional, bubble_in: Array(FluidParticle) optional,
        spray_in: Array(FluidParticle) optional, dust_in: Array(FluidParticle) optional,
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
        foam_b: Array(FluidParticle), bubble_b: Array(FluidParticle),
        spray_b: Array(FluidParticle), dust_b: Array(FluidParticle),
        presented_time: ScalarF32, publications_skipped: ScalarF32,
    },
    params: [
        ParamDef { name: std::borrow::Cow::Borrowed("closed_faces"), label: "Closed Faces (bits −X +X −Y +Y −Z +Z)", ty: ParamType::Int, default: ParamValue::Float(63.0), range: Some((0.0, 63.0)), enum_values: &[] },
        ParamDef { name: std::borrow::Cow::Borrowed("face_valid_layers"), label: "Face Valid Layers", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 8.0)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "Reads node.liquid_state's out and stats after the region; count comes from the fill, and the lattice, closed faces, simulation_time, display_time and epoch from the liquid's domain; GPU FLIP always publishes the native padded grid and takes solid from node.liquid_solid_distance sampled on the domain mesh_min/mesh_nodes outputs with mesh_wall_inset; the lattice inputs retain the authored scalar contract; the face inputs from three node.face_sample_component on liquid_state's faces; the whitewater inputs from liquid_state's foam/bubble/spray/dust_particles, read back through foam_b..dust_b by node.interpolate_particle_frames as particles_b with this node's blend and span. Its outputs are the particle-frame seam node.fluid_surface and node.matter_frame also publish, so the Liquid Surface atoms and whitewater read any solver unchanged. face_valid_layers is how many face layers past the liquid the solver extended its velocity into; it reads 0 unless all three axes are wired. identity_a/b carry each accepted frame identity epoch. Domain reset, identity renumbering and growth start a new run; a pair never straddles one. presented_time is the time the pair samples (t_A + blend × span); publications_skipped counts endpoints that found no history slot. Publication preserves solver storage order.",
    examples: [],
    picker: { label: "Liquid Frame", category: Atom },
    summary: "Hands a simulated particle liquid to the liquid surface, one frame per simulation tick.",
    category: Particles3D,
    role: Filter,
    aliases: ["liquid frame", "particle frame", "publish particles"],
    boundary_reason: CrossFrameState,
    extra_fields: {
        publication: ParticlePublication = ParticlePublication::default(),
        history: FrameHistory = FrameHistory::default(),
        solid: Option<GpuBuffer> = None,
        solid_key: Option<([u32; 7], u32)> = None,
        wired: [bool; FIELDS] = [false; FIELDS],
        bound: [Option<GpuBuffer>; PROVIDED_PORTS] = std::array::from_fn(|_| None),
        placeholders: [Option<GpuBuffer>; PROVIDED_PORTS] = std::array::from_fn(|_| None),
    },
}

impl LiquidFrame {
    /// What `port` shows now: the pinned slot's storage, the walls, or None.
    fn wanted(&self, port: &str) -> Option<&GpuBuffer> {
        let pin = self.history.core.pinned();
        match port {
            "particles_a" => pin.and_then(|p| self.history.slot(p.a).particles.as_ref()),
            "particles_b" => pin.and_then(|p| self.history.slot(p.b).particles.as_ref()),
            "interior_a" => self.pinned_field(FIELD_INTERIOR, false),
            "interior_b" => self.pinned_field(FIELD_INTERIOR, true),
            "solid_a" if self.wired[FIELD_SOLID] => self.pinned_field(FIELD_SOLID, false),
            "solid_b" if self.wired[FIELD_SOLID] => self.pinned_field(FIELD_SOLID, true),
            "solid_a" | "solid_b" => self.solid.as_ref(),
            _ => FACE_GRID_PORTS[..3].iter().position(|&p| p == port).map(|axis| FIELD_FACES + axis)
                .or_else(|| WHITEWATER_OUTPUTS.iter().position(|&p| p == port).map(|k| FIELD_WHITEWATER + k))
                .and_then(|field| self.pinned_field(field, true)),
        }
    }

    /// Rebind every provided output. An output with nothing to show gets a
    /// zeroed node-owned placeholder of the size this frame's lattice and
    /// wiring give it (`sizes`, in [`PROVIDED`] order), so the executor never
    /// keeps a slot bound that has lost its pin and reader tracking.
    /// Placeholders allocate only when a port's size changes.
    fn rebind(&mut self, device: &manifold_gpu::GpuDevice, sizes: [u64; PROVIDED_PORTS]) {
        for (i, port) in PROVIDED.iter().enumerate() {
            // Compare before cloning: an unchanged binding is left alone, so
            // held frames neither clone nor retire a buffer.
            let wanted = self.wanted(port).map(|b| b.identity_key());
            let key = match wanted {
                Some(key) => key,
                None => {
                    let size = sizes[i].max(4);
                    if self.placeholders[i].as_ref().is_none_or(|b| b.size != size) {
                        let buffer = device.create_buffer_shared(size);
                        buffer.zero_fill();
                        self.placeholders[i] = Some(buffer);
                    }
                    self.placeholders[i].as_ref().expect("placeholder made").identity_key()
                }
            };
            if self.bound[i].as_ref().is_some_and(|b| b.identity_key() == key) {
                continue;
            }
            let next = match wanted {
                Some(_) => self.wanted(port).cloned(),
                None => self.placeholders[i].clone(),
            };
            self.bound[i] = next;
        }
    }

    fn pinned_field(&self, field: usize, frame_b: bool) -> Option<&GpuBuffer> {
        let pin = self.history.core.pinned()?;
        let slot = if frame_b { pin.b } else { pin.a };
        self.wired[field].then(|| self.history.slot(slot).fields[field].as_ref()).flatten()
    }
}

impl Primitive for LiquidFrame {
    fn prepare_pipelines(&mut self, device: &manifold_gpu::GpuDevice) {
        self.publication.prepare(device);
    }

    fn provides_array_output(&self, port: &str) -> bool {
        matches!(port, "particles_a" | "particles_b" | "interior_a" | "interior_b" | "solid_a" | "solid_b")
            || FACE_GRID_PORTS[..3].contains(&port)
            || WHITEWATER_OUTPUTS.contains(&port)
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        PROVIDED.iter().position(|&p| p == port).and_then(|i| self.bound[i].as_ref())
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
        // The bound outputs are read this frame whatever happens below, so
        // the pin they show is stamped before any exit.
        let stamp = ctx.gpu_encoder().device.frame_clock().map_or(0, |c| c.stamp());
        self.history.core.stamp_readers(stamp);
        let Some(lattice) = LiquidLattice::from_wires(ctx, "Liquid Frame") else {
            return;
        };
        let surface = lattice.surface();
        let cells = FlipSolverGrid::from_lattice(lattice).cells();
        let count = ctx.scalar_or_param("count", 0.0).round().max(0.0) as u32;
        let closed_faces = ctx.scalar_or_param("closed_faces", 63.0).round().clamp(0.0, 63.0) as u32;
        let simulation_time = f64::from(ctx.scalar_or_param("simulation_time", 0.0));
        let display_time = f64::from(ctx.scalar_or_param("display_time", 0.0));
        let epoch = ctx.scalar_or_param("epoch", 0.0).round().max(0.0) as u32;
        let particles = ctx.inputs.array("particles");
        let stats = ctx.inputs.array("stats");
        let identity = ctx.inputs.array("identity");
        let interior = ctx.inputs.array("interior");
        let solid_in = ctx.inputs.array("solid");
        if let Some(solid) = solid_in && solid.size != surface.solid_bytes() {
            ctx.error(format!("Liquid Frame: solid holds {} bytes; native FLIP grid {:?} requires exactly {}. Sample the solid at gpu_flip_domain.mesh_min/mesh_nodes with mesh_wall_inset", solid.size, surface.nodes(), surface.solid_bytes()));
            return;
        }
        let faces_in = FACE_INPUT_PORTS.map(|port| ctx.inputs.array(port));
        let whitewater_in = WHITEWATER_INPUTS.map(|port| ctx.inputs.array(port));
        let face_valid_layers = ctx.scalar_or_param("face_valid_layers", 0.0).round().clamp(0.0, 8.0);
        let mut refused = None;

        // Every wired field's exact size; a mismatched one blocks publication.
        let mut fields = [0u64; FIELDS];
        let mut fields_ready = true;
        if solid_in.is_some() {
            fields[FIELD_SOLID] = surface.solid_bytes().max(4);
        }
        let interior_size = interior_bytes(cells);
        if let Some(field) = interior {
            if interior_size == 0 {
                refused = Some("Liquid Frame: interior distance has zero cells".to_string());
                fields_ready = false;
            } else if field.size != interior_size {
                refused = Some(format!(
                    "Liquid Frame: interior distance holds {} bytes; the lattice needs exactly {interior_size} bytes ({})",
                    field.size,
                    cells.iter().product::<u32>()
                ));
                fields_ready = false;
            } else {
                fields[FIELD_INTERIOR] = interior_size;
            }
        }
        for (axis, input) in faces_in.iter().enumerate() {
            let Some(input) = input else { continue };
            let bytes = face_len(cells, axis) * 4;
            if input.size < bytes {
                refused = Some(format!("Liquid Frame: face axis {axis} needs {bytes} bytes"));
                fields_ready = false;
            } else {
                fields[FIELD_FACES + axis] = bytes.max(4);
            }
        }
        for (k, input) in whitewater_in.iter().enumerate() {
            if let Some(input) = input {
                fields[FIELD_WHITEWATER + k] = input.size.max(4);
            }
        }
        self.wired = fields.map(|bytes| bytes > 0);

        let solid_key = (
            [
                surface.min()[0].to_bits(), surface.min()[1].to_bits(), surface.min()[2].to_bits(),
                lattice.cell_size().to_bits(), surface.nodes()[0], surface.nodes()[1], surface.nodes()[2],
            ],
            closed_faces,
        );
        let layout = Layout { epoch, lattice: solid_key.0, fields };
        // Placeholders take the size a slot would give the port.
        let particle_bytes = u64::from(self.history.core.capacity_for(count)) * std::mem::size_of::<FluidParticle>() as u64;
        let field = |f: usize| if fields[f] > 0 { fields[f] } else { 4 };
        let sizes = [
            particle_bytes, particle_bytes, field(FIELD_INTERIOR), field(FIELD_INTERIOR),
            surface.solid_bytes().max(self.history.field_capacity(FIELD_SOLID)), surface.solid_bytes().max(self.history.field_capacity(FIELD_SOLID)),
            field(FIELD_FACES), field(FIELD_FACES + 1), field(FIELD_FACES + 2),
            field(FIELD_WHITEWATER), field(FIELD_WHITEWATER + 1), field(FIELD_WHITEWATER + 2), field(FIELD_WHITEWATER + 3),
        ];
        self.history.core.set_layout(layout);

        let gpu = ctx.gpu_encoder();
        let clock = gpu.device.frame_clock();
        let complete = |stamp: u64| clock.as_ref().is_none_or(|c| c.is_complete(stamp));
        self.history.retire(complete);
        self.history.core.select(display_time);
        self.history.core.stamp_readers(stamp);
        self.history.core.reclaim(complete);

        if !self.wired[FIELD_SOLID] && self.solid_key != Some(solid_key) {
            let distances = surface.flip_wall_distance(closed_faces);
            // A fresh buffer per setup: the previous one may still be read by
            // an in-flight frame; its drop is fence-retired. Grow-only, as
            // the solid slots: the solid mix's output never shrinks, so the
            // walls keep the largest size and a zero tail.
            let bytes = ((distances.len() * 4).max(4) as u64).max(self.solid.as_ref().map_or(0, |b| b.size));
            let buffer = gpu.device.create_buffer_shared(bytes);
            buffer.zero_fill();
            // SAFETY: new shared buffer, not yet visible to the GPU.
            unsafe { buffer.write(0, bytemuck::cast_slice(&distances)) };
            self.solid = Some(buffer);
            self.solid_key = Some(solid_key);
        }

        let record = std::mem::size_of::<FluidParticle>() as u64;
        let ready = particles.zip(stats).filter(|(_, stats)| stats.size >= u64::from(LIQUID_STATS_WORDS) * 4)
            .filter(|_| identity.is_some_and(|b| b.size >= 16))
            .filter(|_| fields_ready);
        if let Some((particles, stats)) = ready
            && self.history.core.wants_publication(simulation_time)
        {
            match self.history.acquire(gpu.device, simulation_time, particle_bytes, &fields) {
                Ok(None) => {}
                Err(error) => {
                    refused = Some(format!("Liquid Frame: {error}. Lower Resolution."));
                }
                Ok(Some(slot)) => {
                    let count = count.min((particles.size / record) as u32);
                    let target = self.history.slot(slot);
                    let encoded = self.publication.encode(gpu.device, gpu.native_enc, Publication {
                        source: particles,
                        target: target.particles.as_ref().expect("acquired slot"),
                        stats,
                        identity: identity.expect("identity checked"),
                        metadata: target.metadata.as_ref().expect("acquired slot"),
                        count,
                    });
                    #[cfg(feature = "gpu-proofs")]
                    let encoded = if FAIL_NEXT_PUBLICATION.take() { Err("injected publication failure".to_string()) } else { encoded };
                    if let Err(error) = encoded {
                        // Never selectable; the frame still publishes its
                        // outputs below, so arrays and scalars agree.
                        self.history.core.begin(slot, simulation_time, stamp);
                        self.history.core.fail(slot, stamp);
                        refused = Some(format!("Liquid Frame: {error}"));
                    } else {
                        // Every wired field is written whole by this publication;
                        // a rejected tick's slot is never shown, so nothing is gated.
                        let sources: [Option<&GpuBuffer>; FIELDS] = [
                            solid_in, interior, faces_in[0], faces_in[1], faces_in[2],
                            whitewater_in[0], whitewater_in[1], whitewater_in[2], whitewater_in[3],
                        ];
                        for (field, source) in sources.into_iter().enumerate() {
                            if let (Some(source), Some(target)) = (source, target.fields[field].as_ref()) {
                                let bytes = fields[field].min(source.size);
                                if target.size > bytes {
                                    // Grow-only solid storage: past this lattice's size is
                                    // zero, so a shrunk lattice reads no stale tail.
                                    gpu.native_enc.clear_buffer(target);
                                }
                                gpu.native_enc.copy_buffer_to_buffer(source, target, bytes);
                            }
                        }
                        self.history.core.begin(slot, simulation_time, stamp);
                        if crate::node_graph::physics::offline_simulation() {
                            // Export presents this sample, independent of output
                            // fps: complete it now, then select again.
                            gpu.native_enc.commit_wait_and_continue(gpu.device);
                            self.history.core.release_writer(slot);
                            self.history.retire(complete);
                            self.history.core.select(display_time);
                            self.history.core.stamp_readers(stamp);
                        }
                    }
                }
            }
        }

        self.rebind(gpu.device, sizes);
        let pin = self.history.core.pinned().copied();
        let faces_published = (0..3).all(|axis| self.pinned_field(FIELD_FACES + axis, true).is_some());
        let skipped = self.history.core.publications_skipped() as f32;
        for (name, value) in [
            ("count_a", pin.map_or(0.0, |p| p.count_a as f32)),
            ("count_b", pin.map_or(0.0, |p| p.count_b as f32)),
            ("identity_a", pin.map_or(0.0, |p| p.identity_a as f32)),
            ("identity_b", pin.map_or(0.0, |p| p.identity_b as f32)),
            ("grid_nodes_x", surface.nodes()[0] as f32),
            ("grid_nodes_y", surface.nodes()[1] as f32),
            ("grid_nodes_z", surface.nodes()[2] as f32),
            ("blend", pin.map_or(1.0, |p| p.blend)),
            ("span", pin.map_or(0.0, |p| p.span)),
            ("face_cells_x", cells[0] as f32),
            ("face_cells_y", cells[1] as f32),
            ("face_cells_z", cells[2] as f32),
            ("face_valid_layers", if faces_published { face_valid_layers } else { 0.0 }),
            ("presented_time", pin.map_or(0.0, |p| p.presented_time() as f32)),
            ("publications_skipped", skipped),
        ] {
            ctx.outputs.set_scalar(name, ParamValue::Float(value));
        }
        ctx.outputs.set_transform("grid_bounds", surface.bounds());
        if pin.is_none() {
            // No accepted frame exists yet (startup or resized lattice).
            // Consumers must not interpret preallocated storage as a frame.
            ctx.mark_outputs_pending();
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
    fn liquid_frame_params_match_the_shader() {
        let frame = LiquidFrame::new();
        assert!(frame.provides_array_output("interior_a"));
        assert!(frame.provides_array_output("interior_b"));

    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::{ParticlePublication, Publication, LIQUID_STATS_WORDS};
    use super::super::liquid_stats::NARROW_BAND_SHORTAGE_WORD;
    use crate::node_graph::fluid_particles::FluidParticle;
    use super::super::liquid_surface_tests::read;
    use manifold_gpu::GpuBuffer;

    fn shared<T: bytemuck::Pod>(device: &crate::TestDevice, values: &[T]) -> GpuBuffer {
        let buffer = device.create_buffer_shared((std::mem::size_of_val(values) as u64).max(16));
        buffer.zero_fill();
        if !values.is_empty() {
            // SAFETY: shared buffer is sized for `values`; no work is in flight.
            unsafe { buffer.write(0, bytemuck::cast_slice(values)) };
        }
        buffer
    }

    #[test]
    fn gpu_flip_narrow_band_publication_repeats_failed_ticks() {
        let device = crate::test_device();
        let state = [
            FluidParticle { position_radius: [1.0, 2.0, 3.0, 0.5], velocity: [4.0, 5.0, 6.0], id: 1 },
            FluidParticle { position_radius: [7.0, 8.0, 9.0, 0.5], velocity: [1.0, 2.0, 3.0], id: 2 },
        ];
        for shortage in [false, true] {
            let state_buffer = shared(&device, &state);
            let stats = shared(&device, &[0u32; LIQUID_STATS_WORDS as usize]);
            if shortage {
                unsafe { stats.write(u64::from(NARROW_BAND_SHORTAGE_WORD) * 4, &1u32.to_ne_bytes()) };
            }
            let frame = shared(&device, &[FluidParticle::default(); 2]);
            let identity = shared(&device, &[3u32, 7, 0, 0]);
            let metadata = shared(&device, &[0u32; 4]);
            let mut publisher = ParticlePublication::default();
            publisher.prepare(&device);
            let mut encoder = device.create_encoder("liquid-frame-particle-proof");
            publisher.encode(&device, &mut encoder, Publication {
                source: &state_buffer, target: &frame, identity: &identity, stats: &stats, metadata: &metadata, count: 2,
            }).unwrap();
            encoder.commit_and_wait_completed();
            let words: Vec<u32> = read(&metadata, 4);
            assert_eq!(words, [2, 7, u32::from(!shortage), 0]);
            if !shortage {
                // A rejected tick's slot is never shown, so only an accepted
                // one is read.
                let got: Vec<FluidParticle> = read(&frame, 2);
                assert_eq!(got, state, "particle publication");
            }
        }
    }
}
