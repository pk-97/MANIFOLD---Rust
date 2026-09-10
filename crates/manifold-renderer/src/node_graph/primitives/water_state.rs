//! `node.water_state` — the bounded-substep boundary for MLS-MPM water.
//!
//! Contract: `docs/WATER_SIMULATION_DESIGN.md` sections 4 and 6 and
//! `docs/WATER_IMPLEMENTATION_PLAN.md` section 2.1. This node owns the
//! simulation clock and the persistent accepted state; the solver stages
//! in its substep region are ordinary per-dispatch atoms. Per output
//! frame `run` resolves the tick count from the context's
//! [`SimulationFrame`](crate::node_graph::substeps::SimulationFrame)
//! (accumulate `delta * time_scale` in f64, consume integer ticks of
//! `1/step_hz`, drop whole ticks at the `max_substeps` cap — never
//! enlarge dt, never backlog), then the executor repeats the region body
//! and calls `late_capture` once per iteration to accept the candidate.
//!
//! `out` IS the accepted buffer (persistent output): reset/epoch re-seeds
//! it from `seed`, each capture copies the candidate into it, and outside
//! consumers read the final accepted state of the frame. A zero-tick
//! frame (pause, duplicate frame_id, exhausted clock) still exposes it.
//!
//! Reset semantics (design section 6): first observation of
//! `reset_trigger` arms; a later integer change re-seeds even while
//! paused. An epoch change (seek / project replacement) re-seeds, zeroes
//! the clock, the sticky status and diagnostics, and re-arms the collider
//! from `collider_seed`. Reset dominates anything else on the same frame.

use std::borrow::Cow;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use crate::node_graph::state_store::NodeState;
use crate::node_graph::substeps::{SubstepBoundaryPorts, SubstepResultPorts};
use crate::node_graph::transform::Transform;
use crate::node_graph::water::WaterParticle;

struct StatusReadbackSlot {
    buffer: manifold_gpu::GpuBuffer,
    ticket: u64,
    generation: u64,
    collider: Transform,
}

pub struct StatusReadbackRing {
    event: manifold_gpu::GpuEvent,
    slots: Vec<StatusReadbackSlot>,
    reported: bool,
    faulted: bool,
    next_ticket: u64,
    generation: u64,
    last_frame_id: Option<u64>,
}

crate::primitive! {
    name: WaterState,
    type_id: "node.water_state",
    purpose: "Substep boundary for MLS-MPM water: owns the fixed-rate simulation clock and the persistent accepted particle state. The executor repeats its substep region under this clock; capture accepts the candidate each iteration. time_scale [0,1] scales the clock (0 = frozen); reset_trigger re-seeds on integer change even while paused. Seek/project-load (epoch change) re-seeds from scratch. step_hz and max_substeps are install-time constants, not performance knobs.",
    inputs: {
        seed: Array(WaterParticle) required,
        in: Array(WaterParticle) optional,
        collider_seed: Transform required,
        collider_in: Transform optional,
        status_in: Array(u32) optional,
        time_scale: ScalarF32 optional,
        reset_trigger: ScalarF32 optional,
    },
    outputs: {
        out: Array(WaterParticle),
        collider_out: Transform,
        status_out: Array(u32),
        step_count: ScalarF32,
        step_dt: ScalarF32,
        step_time: ScalarF32,
        step_index: ScalarF32,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("step_hz"),
            label: "Step rate (Hz)",
            ty: ParamType::Float,
            default: ParamValue::Float(960.0),
            range: Some((1.0, 1920.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("max_substeps"),
            label: "Max substeps per frame",
            ty: ParamType::Float,
            default: ParamValue::Float(32.0),
            range: Some((1.0, 64.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("time_scale"),
            label: "Simulation speed",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Seed, collider target and controls feed this boundary once per frame; the solver stages (emit, scatter, stress, grid, gather, collide, validate, commit) sit inside the substep region and run step_count times per frame. out feeds the surface graph; status_out feeds diagnostics; collider_out feeds the displayed collider object.",
    examples: [],
    picker: { label: "Water State", category: Atom },
    summary: "Owns the water simulation clock and persistent accepted state; the executor repeats the solver region under it.",
    category: Particles3D,
    role: Filter,
    aliases: ["water state", "mpm boundary", "substep boundary"],
    boundary_reason: CrossFrameState,
    extra_fields: {
        // Mirror of the StateStore tick schedule, written by `run` and
        // read by `substep_iteration` (which gets no context). Per node
        // instance — each layer's graph instantiates its own node.
        pending_ticks: std::cell::Cell<u32> = std::cell::Cell::new(0),
        tick_base: std::cell::Cell<f64> = std::cell::Cell::new(0.0),
        last_step_hz: std::cell::Cell<f64> = std::cell::Cell::new(960.0),
        effective_advancing: std::cell::Cell<bool> = std::cell::Cell::new(false),
        current_substep_final: std::cell::Cell<bool> = std::cell::Cell::new(false),
        status_ring: Option<StatusReadbackRing> = None,
        fatal_error: Option<String> = None,
    },
}

/// Per-owner boundary state: the f64 tick clock, reset/epoch latches and
/// diagnostics. The accepted particle buffer is the persistent `out`
/// SLOT — zero-copy across frames — so keyed state here is scalar only.
#[derive(Clone, Copy)]
struct WaterBoundaryState {
    accumulator: f64,
    sim_time: f64,
    last_frame_id: Option<u64>,
    epoch: u64,
    seeded: bool,
    dropped_ticks: u32,
    overload_reported: bool,
    last_reset_trigger: Option<i32>,
    last_collider: Transform,
    verified_collider: Transform,
    verified_ticket: u64,
    capacity_bytes: u64,
}

impl NodeState for WaterBoundaryState {}

fn accept_verified_pose(state: &mut WaterBoundaryState, ticket: u64, pose: Transform) {
    if ticket > state.verified_ticket {
        state.verified_ticket = ticket;
        state.verified_collider = pose;
    }
}

const BOUNDARY_RESULTS: &[SubstepResultPorts] = &[
    SubstepResultPorts {
        capture: "collider_in",
        output: "collider_out",
    },
    SubstepResultPorts {
        capture: "status_in",
        output: "status_out",
    },
];

impl WaterState {
    fn completed_ring_error(&self) -> Option<String> {
        let ring = self.status_ring.as_ref()?;
        let completed = ring.event.signaled_value();
        ring.slots.iter().find_map(|slot| {
            if slot.ticket == 0 || slot.ticket > completed || slot.generation != ring.generation {
                return None;
            }
            let ptr = slot.buffer.mapped_ptr().expect("status readback buffer must be mapped");
            let status = unsafe { std::ptr::read_unaligned(ptr.cast::<u32>()) };
            (status != 0).then(|| format!("WaterState: solver fault status 0x{status:08x}"))
        })
    }

    fn reset_status_ring(&mut self) {
        if let Some(ring) = &mut self.status_ring {
            ring.reported = false;
            ring.faulted = false;
            ring.generation = ring.generation.wrapping_add(1);
            ring.last_frame_id = None;
        }
        self.fatal_error = None;
    }

    fn poll_status_ring(&mut self, ctx: &mut EffectNodeContext<'_, '_>, state: &mut WaterBoundaryState) {
        let Some(ring) = &mut self.status_ring else { return };
        let completed = ring.event.signaled_value();
        let mut restore_verified = false;
        for slot in &mut ring.slots {
            if slot.ticket == 0 || slot.ticket > completed {
                continue;
            }
            let ptr = slot.buffer.mapped_ptr().expect("status readback buffer must be mapped");
            let status = unsafe { std::ptr::read_unaligned(ptr.cast::<u32>()) };
            let ticket = slot.ticket;
            let collider = slot.collider;
            slot.ticket = 0;
            if slot.generation != ring.generation { continue; }
            if status != 0 && !ring.reported {
                ring.reported = true;
                ring.faulted = true;
                let message = format!("WaterState: solver fault status 0x{status:08x}");
                self.fatal_error = Some(message.clone());
                ctx.error(message);
                self.effective_advancing.set(false);
                self.pending_ticks.set(0);
                restore_verified = true;
            } else if status == 0 {
                accept_verified_pose(state, ticket, collider);
            }
        }
        if restore_verified {
            state.last_collider = state.verified_collider;
        }
    }

    fn schedule_status_readback(&mut self, ctx: &mut EffectNodeContext<'_, '_>, collider: Transform) {
        let Some(status_out) = ctx.outputs.array("status_out") else { return };
        let Some(gpu) = ctx.gpu.as_deref_mut() else { return };
        if self.status_ring.is_none() {
            let event = gpu.device.create_event();
            let mut slots = Vec::with_capacity(3);
            for _ in 0..3 {
                let buffer = gpu.device.create_buffer_shared(4);
                slots.push(StatusReadbackSlot { buffer, ticket: 0, generation: 0, collider: Transform::default() });
            }
            self.status_ring = Some(StatusReadbackRing { event, slots, reported: false, faulted: false, next_ticket: 0, generation: 0, last_frame_id: None });
        }
        let ring = self.status_ring.as_mut().expect("status ring initialized");
        let frame_id = ctx.simulation_frame.map(|f| f.frame_id);
        if ring.faulted || ring.last_frame_id == frame_id { return; }
        ring.last_frame_id = frame_id;
        let Some(slot) = ring.slots.iter_mut().find(|slot| slot.ticket == 0) else { return };
        ring.next_ticket = ring.next_ticket.saturating_add(1).max(1);
        let ticket = ring.next_ticket;
        gpu.native_enc.copy_buffer_to_buffer(status_out, &slot.buffer, 4);
        gpu.native_enc.signal_event_value(&ring.event, ticket);
        slot.ticket = ticket;
        slot.generation = ring.generation;
        slot.collider = collider;
    }

    fn clock_config(&self, ctx: &EffectNodeContext<'_, '_>) -> (f64, u32) {
        let step_hz = match ctx.params.get("step_hz") {
            Some(ParamValue::Float(f)) => f64::from(f.max(1.0)),
            _ => 960.0,
        };
        let max_substeps = match ctx.params.get("max_substeps") {
            Some(ParamValue::Float(f)) => f.max(1.0).round() as u32,
            _ => 32,
        };
        (step_hz, max_substeps)
    }
}

impl Primitive for WaterState {
    fn simulation_error(&self) -> Option<&str> { self.fatal_error.as_deref() }
    fn completed_simulation_error(&self) -> Option<String> {
        self.fatal_error.clone().or_else(|| self.completed_ring_error())
    }
    fn requires(&self) -> crate::node_graph::effect_node::NodeRequires {
        crate::node_graph::effect_node::NodeRequires {
            state_store: true,
            gpu_encoder: true,
        }
    }

    fn state_capture_input_ports(&self) -> &'static [&'static str] {
        &["in", "collider_in", "status_in"]
    }

    fn persistent_output_ports(&self) -> &[&str] {
        // `out` is the accepted buffer and `status_out` the sticky fault
        // word: both must survive frames untouched by pool recycling.
        &["out", "status_out"]
    }

    fn substep_boundary(&self) -> Option<SubstepBoundaryPorts> {
        Some(SubstepBoundaryPorts {
            seed: "seed",
            capture: "in",
            state: "out",
            count: "step_count",
            delta: "step_dt",
            time: "step_time",
            index: "step_index",
            results: BOUNDARY_RESULTS,
        })
    }

    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        match port_name {
            "out" => input_capacities
                .iter()
                .find(|(p, _)| *p == "seed")
                .map(|(_, n)| *n),
            "status_out" => Some(1),
            _ => None,
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        ctx.mark_gpu_accessed();
        let node_id = ctx.node_id;
        let owner_key = ctx.owner_key;
        let out_size = match ctx.outputs.array("out") {
            Some(b) => b.size,
            None => return,
        };

        // Load-or-init the per-owner clock state as a VALUE; the store
        // borrow ends here so GPU calls and ctx.error below are free of
        // conflicting borrows.
        let mut s: WaterBoundaryState = {
            let store = ctx
                .state
                .as_deref_mut()
                .expect("WaterState requires a StateStore");
            match store.get::<WaterBoundaryState>(node_id, owner_key) {
                Some(existing) if existing.capacity_bytes == out_size => *existing,
                _ => WaterBoundaryState {
                    accumulator: 0.0,
                    sim_time: 0.0,
                    last_frame_id: None,
                    epoch: ctx.simulation_frame.map(|f| f.epoch).unwrap_or(0),
                    seeded: false,
                    dropped_ticks: 0,
                    overload_reported: false,
                    last_reset_trigger: None,
                    last_collider: Transform::default(),
                    verified_collider: Transform::default(),
                    verified_ticket: 0,
                    capacity_bytes: out_size,
                },
            }
        };

        let frame = ctx.simulation_frame;
        let epoch_changed = frame.is_some_and(|f| f.epoch != s.epoch);
        if epoch_changed {
            s.epoch = frame.map(|f| f.epoch).unwrap_or(0);
            s.seeded = false;
        }

        // Reset trigger: first observation arms; a later integer change
        // re-seeds even while paused. Dominates everything else this frame.
        let reset_edge = match ctx.inputs.scalar("reset_trigger") {
            Some(ParamValue::Float(v)) => {
                let current = v.round() as i32;
                let edge = s.last_reset_trigger.is_some_and(|p| current != p);
                s.last_reset_trigger = Some(current);
                edge
            }
            _ => false,
        };

        let time_scale = ctx.scalar_or_param("time_scale", 1.0).clamp(0.0, 1.0);
        self.poll_status_ring(ctx, &mut s);
        self.effective_advancing.set(
            ctx.simulation_frame
                .is_some_and(|f| f.advancing && time_scale > 0.0),
        );
        let (step_hz, max_substeps) = self.clock_config(ctx);
        self.last_step_hz.set(step_hz);

        // Seed/re-seed path: first frame, epoch change, or reset edge.
        // Re-seeds the accepted `out` buffer from the seed wire, zeroes
        // the sticky status, the clock and diagnostics, and re-arms the
        // collider from collider_seed.
        let mut reset_fired = false;
        if !s.seeded || epoch_changed || reset_edge {
            if epoch_changed || reset_edge { self.reset_status_ring(); }
            if let (Some(seed_buf), Some(out_buf)) =
                (ctx.inputs.array("seed"), ctx.outputs.array("out"))
            {
                let gpu = ctx
                    .gpu
                    .as_deref_mut()
                    .expect("WaterState requires a GpuEncoder");
                let copy_size = seed_buf.size.min(out_buf.size);
                if copy_size > 0 {
                    gpu.native_enc.copy_buffer_to_buffer(seed_buf, out_buf, copy_size);
                }
                if let Some(status_buf) = ctx.outputs.array("status_out") {
                    gpu.native_enc.clear_buffer(status_buf);
                }
            }
            s.seeded = true;
            s.accumulator = 0.0;
            s.sim_time = 0.0;
            s.dropped_ticks = 0;
            s.overload_reported = false;
            s.last_frame_id = None;
            s.last_collider = ctx.inputs.transform("collider_seed").unwrap_or_default();
            s.verified_collider = s.last_collider;
            s.verified_ticket = 0;
            reset_fired = epoch_changed || reset_edge;
        }

        // Clock resolution → the frame's tick schedule.
        let mut ticks: u32 = 0;
        let mut overload_error: Option<String> = None;
        let mut host_error = false;
        match frame {
            None => {
                // Host integration error: the executor reports it too and
                // runs the region zero times. No panic; expose accepted.
                host_error = true;
            }
            Some(f) if reset_fired || s.last_frame_id == Some(f.frame_id) => {
                // Reset dominates: zero ticks this frame. A duplicate frame
                // renders the accepted state again without advancing.
            }
            Some(_) if self.fatal_error.is_some()
                || self.status_ring.as_ref().is_some_and(|ring| ring.faulted) => {}
            Some(f) if !f.advancing || time_scale <= 0.0 => {
                s.last_frame_id = Some(f.frame_id);
            }
            Some(f) => {
                s.last_frame_id = Some(f.frame_id);
                let h = 1.0 / step_hz;
                let accumulated = s.accumulator + f.delta.0 * f64::from(time_scale);
                let due_whole = (accumulated / h).floor();
                let mut whole = due_whole;
                if due_whole > f64::from(max_substeps) {
                    let dropped = (due_whole - f64::from(max_substeps)) as u32;
                    if f.exporting {
                        self.fatal_error = Some(format!(
                            "WaterState: simulation overload in export — requested {dropped} extra ticks"
                        ));
                        self.effective_advancing.set(false);
                        self.pending_ticks.set(0);
                        ctx.outputs.set_scalar("step_count", ParamValue::Float(0.0));
                        return;
                    } else if !s.overload_reported {
                        s.dropped_ticks += dropped;
                        whole = f64::from(max_substeps);
                        s.overload_reported = true;
                        overload_error = Some(format!(
                            "WaterState: simulation overload — dropping whole ticks \
                             (total dropped: {})",
                            s.dropped_ticks
                        ));
                    } else {
                        s.dropped_ticks += dropped;
                        whole = f64::from(max_substeps);
                    }
                }
                s.accumulator = accumulated - due_whole * h;
                self.tick_base.set(s.sim_time);
                s.sim_time += whole * h;
                ticks = whole as u32;
            }
        }
        if self.fatal_error.is_some()
            || self.status_ring.as_ref().is_some_and(|ring| ring.faulted)
        {
            ticks = 0;
            self.effective_advancing.set(false);
        }
        self.pending_ticks.set(ticks);
        self.current_substep_final.set(ticks == 0);
        if ticks == 0 {
            self.schedule_status_readback(ctx, s.last_collider);
        }

        // Write the clock state back, then the frame's scalar/transform
        // outputs and any deferred error.
        {
            let store = ctx
                .state
                .as_deref_mut()
                .expect("WaterState requires a StateStore");
            if let Some(existing) = store.get::<WaterBoundaryState>(node_id, owner_key) {
                *existing = s;
            } else {
                store.insert(node_id, owner_key, s);
            }
        }
        ctx.outputs
            .set_scalar("step_count", ParamValue::Float(ticks as f32));
        ctx.outputs.set_transform("collider_out", s.last_collider);
        if host_error {
            ctx.error("WaterState: no SimulationFrame installed".to_string());
        }
        if let Some(msg) = overload_error {
            ctx.error(msg);
        }
    }

    fn clear_state(&mut self) {
        self.reset_status_ring();
    }

    fn substep_iteration(&mut self, iteration: u32) -> Option<[f32; 3]> {
        if iteration >= self.pending_ticks.get() {
            return None;
        }
        self.current_substep_final
            .set(iteration + 1 == self.pending_ticks.get());
        // The schedule mirrors `run`'s: dt = 1/step_hz, time = base +
        // (i+1)*dt — both pinned to cells when `run` resolved the clock.
        let dt = 1.0 / self.last_step_hz.get();
        Some([
            dt as f32,
            (self.tick_base.get() + (f64::from(iteration) + 1.0) * dt) as f32,
            iteration as f32,
        ])
    }

    fn substep_effective_advancing(&self) -> Option<bool> {
        Some(self.effective_advancing.get())
    }

    fn late_capture(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        // Per-iteration accept: the body's final candidate sits in the
        // persistent `in` slot; copy it into the accepted `out` buffer.
        // Status and collider results land the same way — sticky status
        // word, accepted collider transform for the displayed object.
        ctx.mark_gpu_accessed();
        let gpu = ctx
            .gpu
            .as_deref_mut()
            .expect("WaterState::late_capture requires a GpuEncoder");
        if let (Some(in_buf), Some(out_buf)) =
            (ctx.inputs.array("in"), ctx.outputs.array("out"))
        {
            let copy_size = in_buf.size.min(out_buf.size);
            if copy_size > 0 {
                gpu.native_enc.copy_buffer_to_buffer(in_buf, out_buf, copy_size);
            }
        }
        if let (Some(status_in), Some(status_out)) =
            (ctx.inputs.array("status_in"), ctx.outputs.array("status_out"))
        {
            let copy_size = status_in.size.min(status_out.size).min(4);
            if copy_size > 0 {
                gpu.native_enc
                    .copy_buffer_to_buffer(status_in, status_out, copy_size);
            }
        }
        if let Some(collider) = ctx.inputs.transform("collider_in") {
            let store = ctx
                .state
                .as_deref_mut()
                .expect("WaterState::late_capture requires a StateStore");
            if let Some(s) = store.get::<WaterBoundaryState>(ctx.node_id, ctx.owner_key) {
                s.last_collider = collider;
            }
            ctx.outputs.set_transform("collider_out", collider);
            if self.current_substep_final.get() {
                self.schedule_status_readback(ctx, collider);
            }
        } else if self.current_substep_final.get() {
            let collider = ctx.state.as_deref_mut()
                .expect("WaterState::late_capture requires a StateStore")
                .get::<WaterBoundaryState>(ctx.node_id, ctx.owner_key)
                .expect("WaterState clock must be initialized before capture")
                .last_collider;
            self.schedule_status_readback(ctx, collider);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::ports::{ArrayType, PortType};

    #[test]
    fn verified_collider_accepts_highest_ticket_only() {
        let pose2 = Transform { pos: [2.0, 0.0, 0.0], ..Transform::default() };
        let pose4 = Transform { pos: [4.0, 0.0, 0.0], ..Transform::default() };
        let mut state = WaterBoundaryState {
            accumulator: 0.0, sim_time: 0.0, last_frame_id: None, epoch: 0,
            seeded: true, dropped_ticks: 0, overload_reported: false,
            last_reset_trigger: None, last_collider: pose4,
            verified_collider: Transform::default(), verified_ticket: 0,
            capacity_bytes: 0,
        };
        accept_verified_pose(&mut state, 4, pose4);
        accept_verified_pose(&mut state, 2, pose2);
        assert_eq!(state.verified_ticket, 4);
        assert_eq!(state.verified_collider.pos, pose4.pos);
    }

    #[test]
    fn water_state_declares_boundary_contract() {
        let prim = WaterState::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.water_state");

        // Capture ports and persistence per design section 4: the primary
        // particle back-edge plus collider/status results; out and
        // status_out persist as the accepted state across frames.
        assert_eq!(
            node.state_capture_input_ports(),
            &["in", "collider_in", "status_in"]
        );
        assert_eq!(node.persistent_output_ports(), &["out", "status_out"]);

        let ports = node.substep_boundary().expect("WaterState is a boundary");
        assert_eq!(ports.seed, "seed");
        assert_eq!(ports.capture, "in");
        assert_eq!(ports.state, "out");
        assert_eq!(ports.count, "step_count");
        assert_eq!(ports.delta, "step_dt");
        assert_eq!(ports.time, "step_time");
        assert_eq!(ports.index, "step_index");
        assert_eq!(ports.results.len(), 2);
        assert_eq!(ports.results[0].capture, "collider_in");
        assert_eq!(ports.results[0].output, "collider_out");
        assert_eq!(ports.results[1].capture, "status_in");
        assert_eq!(ports.results[1].output, "status_out");

        // The particle wire carries the 96-byte WaterParticle record, not
        // the ordinary 64-byte Particle.
        let particle_layout = ArrayType::of_known::<WaterParticle>();
        let out = node
            .outputs()
            .iter()
            .find(|p| p.name == "out")
            .expect("out port");
        assert_eq!(out.ty, PortType::Array(particle_layout));
        let status = node
            .outputs()
            .iter()
            .find(|p| p.name == "status_out")
            .expect("status_out port");
        assert_eq!(status.ty, PortType::Array(ArrayType::of_known::<u32>()));
    }

    #[test]
    fn water_state_requires_state_store_and_gpu() {
        let prim = WaterState::new();
        let node: &dyn EffectNode = &prim;
        let req = node.requires();
        assert!(req.state_store);
        assert!(req.gpu_encoder);
    }

    #[test]
    fn water_state_is_registered() {
        let registry = crate::node_graph::PrimitiveRegistry::with_builtin();
        assert!(
            registry.construct("node.water_state").is_some(),
            "node.water_state must be constructible from the builtin registry"
        );
    }
}
