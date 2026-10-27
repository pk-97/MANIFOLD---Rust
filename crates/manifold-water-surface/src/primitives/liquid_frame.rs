//! `node.liquid_frame` — publish a particle liquid on the particle-frame seam
//!Every frame that ends
//! on a new tick publishes it, with the whitewater classes when wired, into
//! a retained history slot of its own.
//! Frames A and B are the retired pair bracketing the display time. Exempt
//! from the codegen mandate as cross-frame state (ADDING_PRIMITIVES.md
//! exclusion 2): it owns the publication history.

use manifold_gpu::GpuBuffer;

use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::parameters::ParamValue;
use manifold_node_engine::particles::FluidParticle;
use manifold_node_engine::primitive::Primitive;
use manifold_water_liquid::display_cursor::{CursorFrame, DisplayCursor};
use manifold_water_liquid::frame_history::{FIELD_WHITEWATER, FIELDS, FrameHistory, Layout};
use manifold_water_liquid::primitives::liquid_stats::LIQUID_STATS_WORDS;
use manifold_water_liquid::primitives::particle_identity::IDENTITY_BYTES;
use manifold_water_liquid::primitives::particle_publication::{ParticlePublication, Publication};

/// Whitewater class inputs and the selected frame's copies, by class.
pub const WHITEWATER_INPUTS: [&str; 4] = ["foam_in", "bubble_in", "spray_in", "dust_in"];
pub const WHITEWATER_OUTPUTS: [&str; 4] = ["foam_b", "bubble_b", "spray_b", "dust_b"];

#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
thread_local! {
    /// Proof hook: the next publication on this thread fails to encode.
    pub static FAIL_NEXT_PUBLICATION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Every provided array output, in [`LiquidFrame`]'s binding table order.
const PROVIDED_PORTS: usize = 6;
const PROVIDED: [&str; PROVIDED_PORTS] = [
    "particles_a",
    "particles_b",
    "foam_b",
    "bubble_b",
    "spray_b",
    "dust_b",
];

manifold_node_engine::primitive! {
    name: LiquidFrame,
    type_id: "node.liquid_frame",
    purpose: "Publish a particle liquid's state as particle frames for the liquid surface: every frame that ends on a new simulated tick publishes a compact copy of the slots in use, sorted by persistent nonzero birth id, into a retained history slot, with the retired live count and a cleared tail; frames A and B are the retired pair bracketing the display time, with its blend and span. The whitewater classes foam_in, bubble_in, spray_in and dust_in per frame as frame B's foam_b, bubble_b, spray_b and dust_b. A tick whose stats flag a non-finite record is never shown.",
    inputs: {
        particles: Array(FluidParticle) required,
        stats: Array(u32) required,
        identity: Array(u32) required,
        foam_in: Array(FluidParticle) optional, bubble_in: Array(FluidParticle) optional,
        spray_in: Array(FluidParticle) optional, dust_in: Array(FluidParticle) optional,
        simulation_time: ScalarF32 optional,
        display_time: ScalarF32 optional,
        epoch: ScalarF32 optional,
        display_cursor: ScalarF32 optional,
        dropped_seconds: ScalarF32 optional,
    },
    outputs: {
        particles_a: Array(FluidParticle), particles_b: Array(FluidParticle),
        count_a: ScalarF32, count_b: ScalarF32, identity_a: ScalarF32, identity_b: ScalarF32,
        blend: ScalarF32, span: ScalarF32,
        foam_b: Array(FluidParticle), bubble_b: Array(FluidParticle),
        spray_b: Array(FluidParticle), dust_b: Array(FluidParticle),
        presented_time: ScalarF32, publications_skipped: ScalarF32,
    },
    params: [],
    depth_rule: Terminal,
    composition_notes: "Reads node.liquid_state's out, stats and identity after the region; the identity's record says how many of out's slots are in use, and simulation_time, display_time and epoch from the liquid's domain; the whitewater inputs from liquid_state's foam/bubble/spray/dust_particles, read back through foam_b..dust_b by node.interpolate_particle_frames as particles_b with this node's blend and span. Its outputs are the particle-frame seam node.fluid_surface and node.matter_frame also publish, so the Liquid Surface atoms and whitewater read any solver unchanged. identity_a/b carry each accepted frame identity epoch. Domain reset, identity renumbering and growth start a new run; a pair never straddles one. presented_time is the time the pair samples (t_A + blend × span); publications_skipped counts endpoints that found no history slot. Publication preserves solver storage order.",
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
        cursor: DisplayCursor = DisplayCursor::default(),
        wired: [bool; FIELDS] = [false; FIELDS],
        bound: [Option<GpuBuffer>; PROVIDED_PORTS] = std::array::from_fn(|_| None),
        placeholders: [Option<GpuBuffer>; PROVIDED_PORTS] = std::array::from_fn(|_| None),
    },
}

impl LiquidFrame {
    /// What `port` shows now: the pinned slot's storage, or None.
    fn wanted(&self, port: &str) -> Option<&GpuBuffer> {
        let pin = self.history.core.pinned();
        match port {
            "particles_a" => pin.and_then(|p| self.history.slot(p.a).particles.as_ref()),
            "particles_b" => pin.and_then(|p| self.history.slot(p.b).particles.as_ref()),
            _ => WHITEWATER_OUTPUTS
                .iter()
                .position(|&p| p == port)
                .map(|k| FIELD_WHITEWATER + k)
                .and_then(|field| self.pinned_field(field, true)),
        }
    }

    /// Rebind every provided output. An output with nothing to show gets a
    /// zeroed node-owned placeholder of the size this frame's wiring gives
    /// it (`sizes`, in [`PROVIDED`] order), so the executor never
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
                    self.placeholders[i]
                        .as_ref()
                        .expect("placeholder made")
                        .identity_key()
                }
            };
            if self.bound[i]
                .as_ref()
                .is_some_and(|b| b.identity_key() == key)
            {
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
        self.wired[field]
            .then(|| self.history.slot(slot).fields[field].as_ref())
            .flatten()
    }
}

impl Primitive for LiquidFrame {
    // The history shows a slot only once the frame clock retires it, so the
    // tick must be ordered before the frame that reads it.
    fn tick_publisher(&self) -> bool {
        true
    }

    fn prepare_pipelines(&mut self, device: &manifold_gpu::GpuDevice) {
        self.publication.prepare(device);
    }

    fn provides_array_output(&self, port: &str) -> bool {
        matches!(port, "particles_a" | "particles_b") || WHITEWATER_OUTPUTS.contains(&port)
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        PROVIDED
            .iter()
            .position(|&p| p == port)
            .and_then(|i| self.bound[i].as_ref())
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
        // The bound outputs are read this frame whatever happens below, so
        // the pin they show is stamped before any exit.
        let stamp = ctx
            .gpu_encoder()
            .device
            .frame_clock()
            .map_or(0, |c| c.stamp());
        self.history.core.stamp_readers(stamp);
        let simulation_time = f64::from(ctx.scalar_or_param("simulation_time", 0.0));
        let display_time = f64::from(ctx.scalar_or_param("display_time", 0.0));
        let epoch = ctx.scalar_or_param("epoch", 0.0).round().max(0.0) as u32;
        // Live uncoupled water presents at the cursor (section 3.4); export
        // presents exactly at the request whatever the wire says (D6).
        let offline = ctx.sim_step.offline();
        let cursor_frame =
            (!offline && ctx.scalar_or_param("display_cursor", 0.0) >= 0.5).then(|| CursorFrame {
                epoch,
                requested: display_time,
                transport: ctx.played(),
                dropped: f64::from(ctx.scalar_or_param("dropped_seconds", 0.0)),
            });
        let record = std::mem::size_of::<FluidParticle>() as u64;
        let particles = ctx.inputs.array("particles");
        let stats = ctx.inputs.array("stats");
        let identity = ctx.inputs.array("identity");
        // Every slot the state holds: the history keeps room for all of them.
        let slots = particles.map_or(0, |particles| (particles.size / record) as u32);
        let whitewater_in = WHITEWATER_INPUTS.map(|port| ctx.inputs.array(port));
        let mut refused = None;

        // Every wired field's exact size; a mismatched one blocks publication.
        let mut fields = [0u64; FIELDS];
        for (k, input) in whitewater_in.iter().enumerate() {
            if let Some(input) = input {
                fields[FIELD_WHITEWATER + k] = input.size.max(4);
            }
        }
        self.wired = fields.map(|bytes| bytes > 0);

        let layout = Layout { epoch, fields };
        // Placeholders take the size a slot would give the port.
        let particle_bytes = u64::from(self.history.core.capacity_for(slots))
            * std::mem::size_of::<FluidParticle>() as u64;
        let field = |f: usize| if fields[f] > 0 { fields[f] } else { 4 };
        let sizes = [
            particle_bytes,
            particle_bytes,
            field(FIELD_WHITEWATER),
            field(FIELD_WHITEWATER + 1),
            field(FIELD_WHITEWATER + 2),
            field(FIELD_WHITEWATER + 3),
        ];
        self.history.core.set_layout(layout);

        let gpu = ctx.gpu_encoder();
        let clock = gpu.device.frame_clock();
        let complete = |stamp: u64| clock.as_ref().is_none_or(|c| c.is_complete(stamp));
        self.history.retire(complete);
        match cursor_frame {
            Some(frame) => {
                self.cursor.present(&mut self.history.core, frame);
            }
            None => {
                self.cursor.clear();
                self.history.core.select(display_time);
            }
        }
        self.history.core.stamp_readers(stamp);
        self.history.core.reclaim(complete);

        let ready = particles
            .zip(stats)
            .filter(|(_, stats)| stats.size >= u64::from(LIQUID_STATS_WORDS) * 4)
            .filter(|_| identity.is_some_and(|b| b.size >= IDENTITY_BYTES));
        if let Some((particles, stats)) = ready
            && self.history.core.wants_publication(simulation_time)
        {
            match self
                .history
                .acquire(gpu.device, simulation_time, particle_bytes, &fields)
            {
                Ok(None) => {}
                Err(error) => {
                    refused = Some(format!("Liquid Frame: {error}. Raise Cell Size."));
                }
                Ok(Some(slot)) => {
                    let target = self.history.slot(slot);
                    let encoded = self.publication.encode(
                        gpu.device,
                        gpu.native_enc,
                        Publication {
                            source: particles,
                            target: target.particles.as_ref().expect("acquired slot"),
                            stats,
                            identity: identity.expect("identity checked"),
                            metadata: target.metadata.as_ref().expect("acquired slot"),
                        },
                    );
                    #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
                    let encoded = if FAIL_NEXT_PUBLICATION.take() {
                        Err("injected publication failure".to_string())
                    } else {
                        encoded
                    };
                    if let Err(error) = encoded {
                        // Never selectable; the frame still publishes its
                        // outputs below, so arrays and scalars agree.
                        self.history.core.begin(slot, simulation_time, stamp);
                        self.history.core.fail(slot, stamp);
                        refused = Some(format!("Liquid Frame: {error}"));
                    } else {
                        // Every wired field is written whole by this publication;
                        // a rejected tick's slot is never shown, so nothing is gated.
                        for (k, source) in whitewater_in.into_iter().enumerate() {
                            let field = FIELD_WHITEWATER + k;
                            if let (Some(source), Some(target)) =
                                (source, target.fields[field].as_ref())
                            {
                                let bytes = fields[field].min(source.size);
                                gpu.native_enc.copy_buffer_to_buffer(source, target, bytes);
                            }
                        }
                        self.history.core.begin(slot, simulation_time, stamp);
                        if offline {
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
        let skipped = self.history.core.publications_skipped() as f32;
        for (name, value) in [
            ("count_a", pin.map_or(0.0, |p| p.count_a as f32)),
            ("count_b", pin.map_or(0.0, |p| p.count_b as f32)),
            ("identity_a", pin.map_or(0.0, |p| p.identity_a as f32)),
            ("identity_b", pin.map_or(0.0, |p| p.identity_b as f32)),
            ("blend", pin.map_or(1.0, |p| p.blend)),
            ("span", pin.map_or(0.0, |p| p.span)),
            (
                "presented_time",
                pin.map_or(0.0, |p| p.presented_time() as f32),
            ),
            ("publications_skipped", skipped),
        ] {
            ctx.outputs.set_scalar(name, ParamValue::Float(value));
        }
        if pin.is_none() {
            // No accepted frame exists yet (startup or a new run).
            // Consumers must not interpret preallocated storage as a frame.
            ctx.mark_outputs_pending();
        }
        if let Some(error) = refused {
            ctx.error(error);
        }
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
