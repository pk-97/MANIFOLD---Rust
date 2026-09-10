//! `node.water_impulse` — S5 solver stage: latched radial impulse events.
//!
//! First-stage event semantics (design step 1 and section 6): an integer
//! change of the `trigger_count` wire supplies the event multiplicity; the
//! per-owner latch queues events across frames with zero due substeps and
//! consumes exactly one per actual substep — one event is a velocity change
//! applied once, never once-per-substep, never force·dt. Pending multiplicity
//! caps at 32 with a reported overflow; a trigger decrease (rollback) rearms
//! the baseline and never creates negative events; reset (epoch change or a
//! regressed boundary clock) clears the queue; an explicit non-advancing
//! sample discards incoming events and rearms so a pause can never burst on
//! resume. Transport stop clears the latch through the existing
//! `is_trigger_latch` host path.
//!
//! `run` dispatches the radial-falloff kernel only on substeps that consume
//! an event; otherwise the aliased output already carries the input.
//!
//! Contract: docs/WATER_SIMULATION_DESIGN.md section 6;
//! docs/WATER_IMPLEMENTATION_PLAN.md section 2.2 (stage port table).

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::{EffectNodeContext, SubstepFrameContext};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use crate::node_graph::state_store::NodeState;
use crate::node_graph::water::{DEFAULT_STEP_DT, WaterParticle};

/// Maximum queued event multiplicity (design section 6). Excess arrivals
/// are dropped and reported, never deferred.
pub const MAX_PENDING_IMPULSES: u32 = 32;

/// The latch decision for one substep, returned by
/// [`ImpulseEventLatch::sample`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImpulseDecision {
    /// Consume one queued event this substep: dispatch the velocity change.
    pub apply: bool,
    /// Events dropped by the cap on the sampling substep (`Some(dropped)`),
    /// reported once by `run`.
    pub overflow: Option<u32>,
}

/// Per-owner impulse event latch. Lives in the `StateStore` so transport
/// stop / project load clear it through the existing
/// `PresetRuntime::clear_trigger_state` path (the node flags
/// `is_trigger_latch`).
#[derive(Clone, Copy, Debug)]
pub struct ImpulseEventLatch {
    baseline: Option<i64>,
    pending: u32,
    last_sample_frame: Option<u64>,
    last_step_time: f32,
    epoch: u64,
    initialized: bool,
    dropped_total: u32,
}

impl Default for ImpulseEventLatch {
    fn default() -> Self {
        Self {
            baseline: None,
            pending: 0,
            last_sample_frame: None,
            epoch: 0,
            initialized: false,
            last_step_time: 0.0,
            dropped_total: 0,
        }
    }
}

impl NodeState for ImpulseEventLatch {}

impl ImpulseEventLatch {
    /// Advance the latch one substep. `run` calls this once per region
    /// iteration with the frame's own ids; the GPU tests drive it directly
    /// with the same call sequence. The trigger count is sampled at most
    /// once per frame — on the first substep that runs — so a trigger
    /// change over a frame with N substeps queues N events' worth of
    /// multiplicity, applied one per substep, never N× per substep.
    pub fn sample(
        &mut self,
        epoch: u64,
        frame_id: u64,
        advancing: bool,
        step_time: f32,
        trigger_count: f32,
    ) -> ImpulseDecision {
        // Reset dominates: epoch change (seek / project load) or the
        // boundary clock regressed (a reset frame schedules zero ticks, so
        // the regression is first visible on the next advancing substep).
        if !self.initialized || epoch != self.epoch || step_time < self.last_step_time {
            self.pending = 0;
            self.baseline = None;
            self.initialized = true;
            self.epoch = epoch;
        }

        let mut overflow = None;
        if self.last_sample_frame != Some(frame_id) {
            self.last_sample_frame = Some(frame_id);
            let current = trigger_count.round() as i64;
            match self.baseline {
                None => {
                    // First observation arms the latch — no event.
                    self.baseline = Some(current);
                }
                Some(base) => {
                    if current > base {
                        let delta = (current - base) as u32;
                        self.baseline = Some(current);
                        if !advancing {
                            // Pause / zero time-scale discards incoming
                            // events and rearms — no burst on resume.
                            self.pending = 0;
                        } else {
                            let room = MAX_PENDING_IMPULSES - self.pending;
                            let accepted = delta.min(room);
                            self.pending += accepted;
                            let dropped = delta - accepted;
                            if dropped > 0 {
                                self.dropped_total += dropped;
                                overflow = Some(self.dropped_total);
                            }
                        }
                    } else if current < base {
                        // Rollback rearms; it never creates negative events.
                        self.baseline = Some(current);
                    }
                }
            }
        }

        // Consume on the first actual substep: one event per substep.
        let apply = self.pending > 0;
        if apply {
            self.pending -= 1;
        }
        self.last_step_time = step_time;
        ImpulseDecision { apply, overflow }
    }

    /// Queued events waiting for a substep (diagnostics and tests).
    pub fn pending(&self) -> u32 {
        self.pending
    }
}

/// Generated-codegen uniform layout: the seven scalar params in PARAMS
/// order, then the codegen-injected `dispatch_count`, padded to 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ImpulseUniforms {
    pub centre_x: f32,
    pub centre_y: f32,
    pub centre_z: f32,
    pub radius: f32,
    pub impulse_x: f32,
    pub impulse_y: f32,
    pub impulse_z: f32,
    pub dispatch_count: u32,
}

crate::primitive! {
    name: WaterImpulse,
    type_id: "node.water_impulse",
    purpose: "Apply a latched impulse event to Live Water (design section 6): when the integer trigger_count advances, queue one event per count change (cap 32, overflow reported) and consume one event per actual substep, adding max(0, 1 - distance/R)^2 * impulse_vector m/s to every live particle within radius R of the centre. The velocity change applies exactly once per event — never once per substep, never a force multiplied by dt again. Events queue across frames with zero due substeps and fire on the first actual substep; rollback rearms without negative events; reset and transport stop clear the queue; a pause discards incoming events so resume cannot burst. Inactive slots are never touched. centre/radius/impulse accept wires (impulse follows the cube) or params.",
    inputs: {
        in: Array(WaterParticle) required,
        trigger_count: ScalarF32 optional,
        centre_x: ScalarF32 optional,
        centre_y: ScalarF32 optional,
        centre_z: ScalarF32 optional,
        radius: ScalarF32 optional,
        impulse_x: ScalarF32 optional,
        impulse_y: ScalarF32 optional,
        impulse_z: ScalarF32 optional,
        step_dt: ScalarF32 optional,
        step_time: ScalarF32 optional,
    },
    outputs: {
        out: Array(WaterParticle),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("centre_x"),
            label: "Centre X",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-2.0, 2.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("centre_y"),
            label: "Centre Y",
            ty: ParamType::Float,
            default: ParamValue::Float(0.7),
            range: Some((0.0, 4.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("centre_z"),
            label: "Centre Z",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-2.0, 2.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("radius"),
            label: "Radius",
            ty: ParamType::Float,
            default: ParamValue::Float(0.5),
            range: Some((0.01, 4.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("impulse_x"),
            label: "Impulse X (m/s)",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-4.0, 4.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("impulse_y"),
            label: "Impulse Y (m/s)",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((-4.0, 4.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("impulse_z"),
            label: "Impulse Z (m/s)",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-4.0, 4.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "First stage of the repeated water region body alongside node.water_emit (design step 1): `water_state.out -> water_emit -> water_impulse -> clear_grid -> ...`. Wire `trigger_count` from the clip-trigger / MIDI source (e.g. system.generator_input.trigger_count), and centre/impulse from the collider or envelope nodes if they should move. `step_dt`/`step_time` come from node.water_state's step outputs and pin the reset detection to the boundary clock. The output aliases the input wire (pure per-element velocity change).",
    examples: [],
    picker: { label: "Water Impulse", category: Atom },
    summary: "Turns trigger hits into one radial splash of velocity per event — queued across frames, capped, and never doubled by substepping.",
    category: Particles3D,
    role: Filter,
    aliases: ["water impulse", "splash", "water hit", "impulse"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/water_impulse_body.wgsl"),
    input_access: [Coincident],
    wgsl_includes: [include_str!("shaders/water_common.wgsl")],
}

impl Primitive for WaterImpulse {
    fn requires(&self) -> crate::node_graph::effect_node::NodeRequires {
        crate::node_graph::effect_node::NodeRequires { state_store: true, gpu_encoder: true }
    }

    fn observes_substep_frame(&self) -> bool {
        true
    }

    fn observe_substep_frame(&mut self, ctx: &mut SubstepFrameContext<'_>) {
        let Some(frame) = ctx.simulation_frame else { return };
        if frame.advancing {
            return;
        }
        let trigger_count = ctx
            .inputs
            .scalar("trigger_count")
            .and_then(|v| v.as_scalar())
            .unwrap_or(0.0);
        let mut latch = ctx
            .state
            .as_deref_mut()
            .and_then(|store| store.get::<ImpulseEventLatch>(ctx.node_id, ctx.owner_key).copied())
            .unwrap_or_default();
        let _ = latch.sample(frame.epoch, frame.frame_id, false, latch.last_step_time, trigger_count);
        if let Some(store) = ctx.state.as_deref_mut() {
            if let Some(existing) = store.get::<ImpulseEventLatch>(ctx.node_id, ctx.owner_key) {
                *existing = latch;
            } else {
                store.insert(ctx.node_id, ctx.owner_key, latch);
            }
        }
    }

    /// The output is a distinct working buffer so failed candidates cannot
    /// overwrite WaterState's accepted particle buffer.
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name == "out" {
            input_capacities
                .iter()
                .find(|(p, _)| *p == "in")
                .map(|(_, n)| *n)
        } else {
            None
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(in_buf) = ctx.inputs.array("in") else {
            // Aliased out shares the input's buffer, so a no-dispatch path
            // leaves the wire coherent; mark GPU access for the executor's
            // stale-data debug_assert.
            ctx.mark_gpu_accessed();
            return;
        };
        let Some(out_buf) = ctx.outputs.array("out") else {
            ctx.mark_gpu_accessed();
            return;
        };
        let particle_size = std::mem::size_of::<WaterParticle>() as u64;
        let capacity = (in_buf.size.min(out_buf.size) / particle_size) as u32;
        if capacity == 0 {
            ctx.mark_gpu_accessed();
            return;
        }

        let node_id = ctx.node_id;
        let owner_key = ctx.owner_key;
        let mut latch = {
            let store = ctx
                .state
                .as_deref_mut()
                .expect("WaterImpulse requires a StateStore");
            match store.get::<ImpulseEventLatch>(node_id, owner_key) {
                Some(existing) => *existing,
                None => ImpulseEventLatch::default(),
            }
        };
        let frame = ctx.simulation_frame;
        let (epoch, frame_id, advancing) = match frame {
            Some(f) => (f.epoch, f.frame_id, f.advancing),
            // No SimulationFrame: the executor reports the host error and
            // the region should not have run; hold state, pass through.
            None => {
                store_latch(ctx, node_id, owner_key, latch);
                ctx.mark_gpu_accessed();
                return;
            }
        };
        let trigger_count = ctx
            .inputs
            .scalar("trigger_count")
            .and_then(|v| v.as_scalar())
            .unwrap_or(0.0);
        let step_time = ctx.scalar_or_param("step_time", 0.0);
        let _step_dt = ctx.scalar_or_param("step_dt", DEFAULT_STEP_DT);

        let decision = latch.sample(epoch, frame_id, advancing, step_time, trigger_count);
        store_latch(ctx, node_id, owner_key, latch);
        if let Some(dropped) = decision.overflow {
            ctx.error(format!(
                "WaterImpulse: impulse event backlog exceeded {MAX_PENDING_IMPULSES} — \
                 dropped {dropped} event(s) total; strike slower or raise the cap"
            ));
        }
        if !decision.apply {
            let gpu = ctx.gpu_encoder();
            gpu.native_enc.copy_buffer_to_buffer(in_buf, out_buf, in_buf.size.min(out_buf.size));
            ctx.mark_gpu_accessed();
            return;
        }

        let uniforms = ImpulseUniforms {
            centre_x: ctx.scalar_or_param("centre_x", 0.0),
            centre_y: ctx.scalar_or_param("centre_y", 0.7),
            centre_z: ctx.scalar_or_param("centre_z", 0.0),
            radius: ctx.scalar_or_param("radius", 0.5),
            impulse_x: ctx.scalar_or_param("impulse_x", 0.0),
            impulse_y: ctx.scalar_or_param("impulse_y", 1.0),
            impulse_z: ctx.scalar_or_param("impulse_z", 0.0),
            dispatch_count: capacity,
        };

        let gpu = ctx.gpu_encoder();
        gpu.native_enc.copy_buffer_to_buffer(in_buf, out_buf, in_buf.size.min(out_buf.size));
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Codegen path (mandatory for per-element GPU atoms): the kernel
            // is generated from the `wgsl_body` so the atom participates in
            // freeze fusion.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.water_impulse standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.water_impulse",
            )
        });

        // uniform(0), in(1), out(2).
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: in_buf,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: out_buf,
                    offset: 0,
                },
            ],
            [capacity.div_ceil(256), 1, 1],
            "node.water_impulse",
        );
    }

    fn clear_state(&mut self) {
        // The latch lives in the StateStore (cleared via is_trigger_latch on
        // transport stop / project load); nothing instance-level to reset.
    }

    /// Transport stop and project load must rearm the latch without a burst
    /// — the host clears StateStore buckets of `is_trigger_latch` nodes.
    /// See `PresetRuntime::clear_trigger_state`.
    fn is_trigger_latch(&self) -> bool {
        true
    }
}

fn store_latch(ctx: &mut EffectNodeContext<'_, '_>, node_id: crate::node_graph::NodeInstanceId, owner_key: crate::node_graph::OwnerKey, latch: ImpulseEventLatch) {
    let store = ctx
        .state
        .as_deref_mut()
        .expect("WaterImpulse requires a StateStore");
    if let Some(existing) = store.get::<ImpulseEventLatch>(node_id, owner_key) {
        *existing = latch;
    } else {
        store.insert(node_id, owner_key, latch);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    const DT: f32 = 1.0 / 960.0;
    fn t(frame: u64, substep: u32) -> f32 {
        frame as f32 * 10.0 + (substep as f32 + 1.0) * DT
    }

    #[test]
    fn impulse_declares_particle_in_particle_out() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let particle_layout = ArrayType::of_known::<WaterParticle>();
        assert_eq!(WaterImpulse::TYPE_ID, "node.water_impulse");
        assert_eq!(WaterImpulse::INPUTS[0].name, "in");
        assert_eq!(WaterImpulse::INPUTS[0].ty, PortType::Array(particle_layout));
        assert_eq!(WaterImpulse::OUTPUTS.len(), 1);
        assert_eq!(WaterImpulse::OUTPUTS[0].name, "out");
        assert_eq!(WaterImpulse::OUTPUTS[0].ty, PortType::Array(particle_layout));
    }

    #[test]
    fn impulse_uses_distinct_particle_buffers_and_is_a_trigger_latch() {
        let prim = WaterImpulse::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.water_impulse");
        assert!(node.aliased_array_io().is_empty());
        assert!(node.is_trigger_latch());
    }

    #[test]
    fn impulse_codegen_binds_coincident_input() {
        let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<WaterImpulse>()
            .expect("node.water_impulse standalone codegen");
        assert!(wgsl.contains("struct Element"));
        assert!(wgsl.contains("radius"));
    }

    #[test]
    fn impulse_first_observation_arms_then_one_trigger_one_apply() {
        let mut latch = ImpulseEventLatch::default();
        // Frame 1, 4 substeps: first observation arms, no events.
        for i in 0..4 {
            let d = latch.sample(0, 1, true, t(1, i), 0.0);
            assert!(!d.apply, "first observation must not fire");
            assert_eq!(d.overflow, None);
        }
        assert_eq!(latch.pending(), 0);
        // Frame 2: trigger advances by 1 on the first substep — exactly one
        // apply, on that substep, and no more for the rest of the frame.
        let mut applies = 0;
        for i in 0..4 {
            let d = latch.sample(0, 2, true, t(2, i), 1.0);
            if d.apply {
                applies += 1;
                assert_eq!(i, 0, "the event consumes on the first actual substep");
            }
        }
        assert_eq!(applies, 1, "one trigger = one total velocity change");
        assert_eq!(latch.pending(), 0);
    }

    #[test]
    fn impulse_multiplicity_consumes_one_per_substep() {
        let mut latch = ImpulseEventLatch::default();
        let _ = latch.sample(0, 1, true, t(1, 0), 0.0);
        // Trigger jumps by 3: three pending events across three substeps.
        let mut applies = 0;
        for i in 0..4 {
            let d = latch.sample(0, 2, true, t(2, i), 3.0);
            if d.apply {
                applies += 1;
            }
        }
        assert_eq!(applies, 3);
        assert_eq!(latch.pending(), 0);
    }

    #[test]
    fn impulse_pending_event_consumed_on_first_substep_after_zero_substep_frame() {
        let mut latch = ImpulseEventLatch::default();
        let _ = latch.sample(0, 1, true, t(1, 0), 0.0);
        // Frame 2: zero due substeps (fractional time scale) — the body
        // never runs, the trigger wire advances to 1 mid-frame.
        // Frame 3 has substeps: the queued event fires on its first one.
        let d = latch.sample(0, 3, true, t(3, 0), 1.0);
        assert!(d.apply, "queued event consumed on the first actual substep");
        for i in 1..4 {
            let d = latch.sample(0, 3, true, t(3, i), 1.0);
            assert!(!d.apply, "event applied exactly once");
        }
    }

    #[test]
    fn impulse_rollback_rearms_without_negative_events() {
        let mut latch = ImpulseEventLatch::default();
        let _ = latch.sample(0, 1, true, t(1, 0), 5.0);
        // Trigger count goes backwards (clip rollback / retrigger): rearms,
        // no negative events.
        let d = latch.sample(0, 2, true, t(2, 0), 2.0);
        assert!(!d.apply);
        assert_eq!(latch.pending(), 0);
        // One new advance from the rolled-back baseline is one new event —
        // the lost counts are not replayed, and the queue stays bounded.
        let d = latch.sample(0, 3, true, t(3, 0), 3.0);
        assert!(d.apply, "+1 after rollback is exactly one event");
        let d = latch.sample(0, 4, true, t(4, 0), 3.0);
        assert!(!d.apply, "an unchanged count fires nothing");
    }

    #[test]
    fn impulse_pending_caps_at_32_with_reported_overflow() {
        let mut latch = ImpulseEventLatch::default();
        let _ = latch.sample(0, 1, true, t(1, 0), 0.0);
        let d = latch.sample(0, 2, true, t(2, 0), 100.0);
        // The sampling substep is itself the first actual substep: it
        // consumes one event immediately, leaving a full-but-one queue.
        assert!(d.apply);
        assert_eq!(latch.pending(), MAX_PENDING_IMPULSES - 1);
        assert_eq!(d.overflow, Some(100 - MAX_PENDING_IMPULSES));
        // The cap keeps the queue bounded while substeps drain it.
        let mut applies = 0;
        for i in 1..40 {
            let d = latch.sample(0, 2, true, t(2, i), 100.0);
            if d.apply {
                applies += 1;
            }
            assert_eq!(d.overflow, None, "overflow reported once, at acceptance");
        }
        assert_eq!(applies, MAX_PENDING_IMPULSES - 1);
        assert_eq!(latch.pending(), 0);
    }

    #[test]
    fn impulse_pause_discards_incoming_and_rearms() {
        let mut latch = ImpulseEventLatch::default();
        let _ = latch.sample(0, 1, true, t(1, 0), 0.0);
        // Paused frame: the region does not iterate, so run() is not called;
        // a non-advancing sample (defensive path) discards and rearms.
        let d = latch.sample(0, 2, false, t(2, 0), 4.0);
        assert!(!d.apply);
        assert_eq!(latch.pending(), 0);
        // Resume: no burst — the pause-fired counts were absorbed.
        let d = latch.sample(0, 3, true, t(3, 0), 4.0);
        assert!(!d.apply, "no burst on resume");
        let d = latch.sample(0, 4, true, t(4, 0), 5.0);
        assert!(d.apply, "a genuinely new event still fires");
    }

    #[test]
    fn impulse_zero_time_scale_observation_never_replays_on_resume() {
        let mut latch = ImpulseEventLatch::default();
        let _ = latch.sample(0, 1, true, t(1, 0), 0.0);
        let d = latch.sample(0, 2, false, 0.0, 2.0);
        assert!(!d.apply);
        let d = latch.sample(0, 3, true, t(3, 0), 2.0);
        assert!(!d.apply);
    }

    #[test]
    fn impulse_fractional_advancing_observation_keeps_trigger_for_next_tick() {
        let mut latch = ImpulseEventLatch::default();
        let _ = latch.sample(0, 1, true, t(1, 0), 0.0);
        // Advancing frame with no completed tick still queues the event.
        let d = latch.sample(0, 2, true, t(2, 0), 1.0);
        assert!(d.apply, "the first later substep consumes the retained event");
        assert_eq!(latch.pending(), 0);
    }

    #[test]
    fn impulse_reset_clears_pending_events() {
        let mut latch = ImpulseEventLatch::default();
        let _ = latch.sample(0, 1, true, t(1, 0), 0.0);
        let d = latch.sample(0, 2, true, t(2, 0), 10.0);
        assert!(d.apply, "first substep consumes immediately");
        assert_eq!(latch.pending(), 9);
        // Reset (boundary clock regression) before the queue drains: the
        // pending events die with the old state — reset dominates impulses.
        let d = latch.sample(0, 3, true, t(0, 0), 10.0);
        assert!(!d.apply, "reset dominates pending impulses");
        assert_eq!(latch.pending(), 0);
    }

    #[test]
    fn impulse_epoch_change_clears_the_latch() {
        let mut latch = ImpulseEventLatch::default();
        let _ = latch.sample(0, 1, true, t(1, 0), 0.0);
        let d = latch.sample(1, 2, true, t(2, 0), 7.0);
        assert!(!d.apply, "first observation of the new epoch arms");
        assert_eq!(latch.pending(), 0);
    }
}
