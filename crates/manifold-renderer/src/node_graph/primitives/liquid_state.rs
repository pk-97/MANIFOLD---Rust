//! `node.liquid_state` — the tick boundary of a particle liquid
//! (`docs/LIQUID_SOLVER_SEAM_DESIGN.md` D10): it holds the particles across
//! frames and has the executor run its region once per liquid tick the
//! domain's clock is due. The region body is one whole tick. A non-finite
//! tick (read back one frame late through a fenced ring) halts the liquid
//! with a node error until the domain's epoch changes (Reset). The last
//! tick's face grid escapes beside the particles into storage this node owns,
//! sized from the lattice wires before the region runs, so it is whole from
//! the first frame and holds while the transport is paused.

use manifold_gpu::GpuBuffer;

use super::gpu_flip_step::face_bytes;
use super::liquid_stats::{LIQUID_STATS_WORDS, LiquidTickStats};
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::fluid_particles::{FaceSample, FluidParticle};
use crate::node_graph::liquid::lattice::LiquidLattice;
use crate::node_graph::parameters::ParamValue;
use crate::node_graph::primitive::Primitive;
use crate::node_graph::substeps::{SubstepBoundaryPorts, SubstepResultPorts};

const RESULTS: &[SubstepResultPorts] = &[
    SubstepResultPorts { capture: "stats_in", output: "stats" },
    SubstepResultPorts { capture: "faces_in", output: "faces" },
];

/// The region's contract. The one iteration scalar is the tick's index in
/// the epoch.
pub const LIQUID_STATE_PORTS: SubstepBoundaryPorts = SubstepBoundaryPorts {
    seed: "seed",
    capture: "in",
    state: "out",
    iteration_scalars: &["tick_index"],
    results: RESULTS,
    // The domain owns the clock; offline it may sync between ticks.
    clock: Some("ticks"),
};

/// Stats readbacks in flight: the GPU writes a slot at the end of a frame's
/// region, the CPU reads it once the frame clock says that frame retired.
const READBACK_SLOTS: usize = 3;

pub struct ReadbackSlot {
    buffer: GpuBuffer,
    stamp: u64,
    epoch: u32,
    pending: bool,
}

crate::primitive! {
    name: LiquidState,
    type_id: "node.liquid_state",
    purpose: "Hold a particle liquid across frames and run its tick region: seed the particles when the epoch changes, then repeat the region once per due tick with the tick's index. The region's last particles become the state, and its last stats and its last tick's face grid escape with them; a new epoch's faces are zero. A tick with non-finite values halts the liquid with an error until Reset.",
    inputs: {
        seed: Array(FluidParticle) required,
        in: Array(FluidParticle) required,
        stats_in: Array(u32) required,
        faces_in: Array(FaceSample) required,
        count: ScalarF32 optional,
        ticks: ScalarF32 optional,
        epoch: ScalarF32 optional,
        nodes_x: ScalarF32 optional,
        nodes_y: ScalarF32 optional,
        nodes_z: ScalarF32 optional,
    },
    outputs: {
        out: Array(FluidParticle),
        stats: Array(u32),
        faces: Array(FaceSample),
        tick_index: ScalarF32,
        live_count: ScalarF32,
        fault: ScalarF32,
    },
    params: [],
    depth_rule: Terminal,
    composition_notes: "The tick boundary of a particle liquid. seed and count come from the fill; ticks and epoch from the liquid's domain (the region's clock owner). The body is one tick: every step of the solver from out, then node.liquid_stats over the tick's last particles, closing back into in and stats_in; the last step's projected, extended faces close into faces_in, and the domain's nodes_x/y/z size the held faces (required with faces_in). out and stats escape to node.liquid_frame; faces to three node.face_sample_component that feed the frame's face grid.",
    examples: [],
    picker: { label: "Liquid State", category: Atom },
    summary: "Keeps a particle liquid between frames and runs one pass of its simulation per tick.",
    category: Particles3D,
    role: Filter,
    aliases: ["liquid state", "tick loop", "particle state"],
    boundary_reason: CrossFrameState,
    extra_fields: {
        epoch: Option<u32> = None,
        pending: u32 = 0,
        ticks_done: u64 = 0,
        captures: u32 = 0,
        zero_stats: Option<GpuBuffer> = None,
        readback: Vec<ReadbackSlot> = Vec::new(),
        faulted: bool = false,
        last_stats: Option<LiquidTickStats> = None,
        faces: Option<GpuBuffer> = None,
    },
}

impl LiquidState {
    /// Read every retired readback of the current epoch, newest last.
    fn poll_readbacks(&mut self, clock: Option<&manifold_gpu::FrameClock>) {
        let Some(epoch) = self.epoch else { return };
        let mut newest: Option<(u64, LiquidTickStats)> = None;
        for slot in self.readback.iter_mut().filter(|s| s.pending) {
            if !clock.is_none_or(|c| c.is_complete(slot.stamp)) {
                continue;
            }
            slot.pending = false;
            if slot.epoch != epoch {
                continue;
            }
            let Some(ptr) = slot.buffer.mapped_ptr() else { continue };
            // SAFETY: the frame that wrote this shared slot has retired.
            let words =
                unsafe { std::slice::from_raw_parts(ptr.cast::<u32>().cast_const(), LIQUID_STATS_WORDS as usize) };
            let stats = LiquidTickStats::from_words(words);
            if newest.is_none_or(|(stamp, _)| slot.stamp >= stamp) {
                newest = Some((slot.stamp, stats));
            }
        }
        if let Some((_, stats)) = newest {
            if stats.nonfinite > 0 {
                self.faulted = true;
            }
            self.last_stats = Some(stats);
        }
    }
}

impl Primitive for LiquidState {
    fn state_capture_input_ports(&self) -> &'static [&'static str] {
        &["in", "stats_in", "faces_in"]
    }

    fn persistent_output_ports(&self) -> &'static [&'static str] {
        &["out", "stats"]
    }

    fn provides_array_output(&self, port: &str) -> bool {
        port == "faces"
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        (port == "faces").then_some(self.faces.as_ref()).flatten()
    }

    fn substep_boundary(&self) -> Option<SubstepBoundaryPorts> {
        Some(LIQUID_STATE_PORTS)
    }

    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        match port_name {
            "out" => input_capacities.iter().find(|(p, _)| *p == "seed").map(|&(_, n)| n),
            "stats" => Some(LIQUID_STATS_WORDS),
            // Provided storage: a one-record hint, sized at run time from the
            // body's faces, which the plan allocates after this node.
            "faces" => Some(1),
            _ => None,
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let whole = |v: f32| v.round().max(0.0) as u32;
        let count = whole(ctx.scalar_or_param("count", 0.0));
        let ticks = whole(ctx.scalar_or_param("ticks", 0.0));
        let epoch = whole(ctx.scalar_or_param("epoch", 0.0));
        let seed = ctx.inputs.array("seed");
        let out = ctx.outputs.array("out");
        let stats = ctx.outputs.array("stats");
        let mut refused = None;
        // The face grid's bytes, from the lattice this frame's ticks run on:
        // faces_in still holds the last frame's (or no) body here.
        let face_grid = if ctx.inputs.slot("faces_in").is_none() {
            None
        } else if ["nodes_x", "nodes_y", "nodes_z"].iter().any(|port| ctx.inputs.slot(port).is_none()) {
            refused = Some("Liquid State: faces_in needs the lattice on nodes_x, nodes_y and nodes_z".to_string());
            None
        } else {
            LiquidLattice::from_wires(ctx, "Liquid State").map(|lattice| face_bytes(lattice.cells()))
        }
        .filter(|_| ctx.outputs.array("faces").is_some());
        let gpu = ctx.gpu_encoder();
        let clock = gpu.device.frame_clock();
        let stats_bytes = u64::from(LIQUID_STATS_WORDS) * 4;
        let zero_stats = self.zero_stats.get_or_insert_with(|| gpu.device.create_buffer(stats_bytes));

        // The faces are the lattice's face grid, held across frames; unwired
        // or refused, none.
        let mut fresh_faces = false;
        match face_grid {
            None => self.faces = None,
            Some(bytes) if self.faces.as_ref().is_none_or(|f| f.size != bytes) => {
                let device = gpu.device;
                self.faces = crate::node_graph::scene_modifier_expand::admit_candidate_bytes(
                    device.modifier_memory_snapshot(),
                    bytes,
                )
                .map_err(|error| error.to_string())
                .and_then(|()| device.try_create_buffer_shared(bytes))
                .map_err(|error| {
                    refused = Some(format!(
                        "Liquid State: the face grid needs {bytes} bytes the device cannot give: {error}. Lower Resolution."
                    ));
                })
                .ok();
                fresh_faces = self.faces.is_some();
            }
            Some(_) => {}
        }

        if self.epoch != Some(epoch) || fresh_faces {
            // A new epoch's faces are zero until its first tick.
            if let Some(faces) = &self.faces {
                gpu.native_enc.clear_buffer(faces);
            }
        }
        if self.epoch != Some(epoch) {
            self.epoch = Some(epoch);
            self.ticks_done = 0;
            self.faulted = false;
            self.last_stats = None;
            if let (Some(seed), Some(out)) = (seed, out) {
                let bytes = (u64::from(count) * std::mem::size_of::<FluidParticle>() as u64).min(seed.size).min(out.size);
                if bytes > 0 {
                    gpu.native_enc.copy_buffer_to_buffer(seed, out, bytes);
                }
            }
            if let Some(stats) = stats {
                gpu.native_enc.clear_buffer(zero_stats);
                gpu.native_enc.copy_buffer_to_buffer(zero_stats, stats, stats_bytes.min(stats.size));
            }
        }
        self.poll_readbacks(clock.as_ref());

        self.pending = if self.faulted { 0 } else { ticks };
        self.captures = 0;
        let live = self.last_stats.map_or(count, |s| s.live);
        ctx.outputs.set_scalar("live_count", ParamValue::Float(live as f32));
        ctx.outputs.set_scalar("fault", ParamValue::Float(if self.faulted { 1.0 } else { 0.0 }));
        if self.faulted {
            ctx.error("Liquid State: a tick produced non-finite values; the liquid is halted until Reset");
        }
        if let Some(error) = refused {
            ctx.error(error);
        }
    }

    fn substep_iteration(&mut self, iteration: u32, scalars: &mut [f32]) -> bool {
        if iteration >= self.pending {
            return false;
        }
        scalars[0] = (self.ticks_done + u64::from(iteration)) as f32;
        true
    }

    fn late_capture(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        // A body that writes fresh storage is accepted by copy.
        for (candidate, state) in [("in", "out"), ("stats_in", "stats")] {
            if let (Some(candidate), Some(state)) = (ctx.inputs.array(candidate), ctx.outputs.array(state))
                && !candidate.ptr_eq(state)
            {
                let size = candidate.size.min(state.size);
                ctx.gpu_encoder().native_enc.copy_buffer_to_buffer(candidate, state, size);
            }
        }
        self.captures += 1;
        if self.captures != self.pending {
            return;
        }
        // Only the frame's last tick reaches the faces.
        if let (Some(candidate), Some(faces)) = (ctx.inputs.array("faces_in"), self.faces.as_ref()) {
            if candidate.size == faces.size {
                ctx.gpu_encoder().native_enc.copy_buffer_to_buffer(candidate, faces, faces.size);
            } else {
                ctx.error(format!(
                    "Liquid State: the tick's faces hold {} bytes; the lattice's face grid is {}",
                    candidate.size, faces.size
                ));
            }
        }
        self.ticks_done += u64::from(self.pending);
        let Some(stats) = ctx.outputs.array("stats") else { return };
        let Some(epoch) = self.epoch else { return };
        let gpu = ctx.gpu_encoder();
        let clock = gpu.device.frame_clock();
        let stats_bytes = u64::from(LIQUID_STATS_WORDS) * 4;
        let complete = |slot: &ReadbackSlot| !slot.pending || clock.as_ref().is_none_or(|c| c.is_complete(slot.stamp));
        let index = match self.readback.iter().position(complete) {
            Some(index) => index,
            None if self.readback.len() < READBACK_SLOTS => {
                self.readback.push(ReadbackSlot {
                    buffer: gpu.device.create_buffer_shared(stats_bytes),
                    stamp: 0,
                    epoch,
                    pending: false,
                });
                self.readback.len() - 1
            }
            // Every slot is still in flight: skip this frame's readback; live
            // never waits on the GPU.
            None => return,
        };
        let slot = &mut self.readback[index];
        gpu.native_enc.copy_buffer_to_buffer(stats, &slot.buffer, stats_bytes.min(stats.size));
        slot.stamp = clock.as_ref().map_or(0, |c| c.stamp());
        slot.epoch = epoch;
        slot.pending = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;

    #[test]
    fn liquid_state_serves_the_tick_index_per_iteration() {
        let mut state = LiquidState::new();
        state.pending = 3;
        state.ticks_done = 10;
        let mut scalars = [0.0f32; 1];
        let mut seen = Vec::new();
        for i in 0.. {
            if !EffectNode::substep_iteration(&mut state, i, &mut scalars) {
                break;
            }
            seen.push(scalars[0]);
        }
        assert_eq!(seen, [10.0, 11.0, 12.0]);
        assert_eq!(LIQUID_STATE_PORTS.iteration_scalars.len(), 1);
    }
}
