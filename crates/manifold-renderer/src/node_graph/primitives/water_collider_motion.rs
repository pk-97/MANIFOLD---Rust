//! `node.water_collider_motion` — S5: interpolate the cube collider over
//! the frame's accepted substeps.
//!
//! CPU-only driver (no GPU allocation, no independent clock): reads the
//! authored target `Transform` and the boundary's step clock wires, retains
//! the previous accepted translation in per-owner state, and emits the
//! interpolated `Transform` plus the frame-constant collider velocity
//! `(target - previous) / simulated_seconds`. The emitted `transform` wire
//! feeds `node.water_state`'s `collider_in` capture, so the displayed cube
//! consumes exactly the accepted collider transform — one authoring model,
//! no copied position sliders.
//!
//! Contract: docs/WATER_SIMULATION_DESIGN.md section 6;
//! docs/WATER_IMPLEMENTATION_PLAN.md sections 2.1 and 2.2. Reset
//! (epoch change or sim-time regression, which is what a boundary reset
//! looks like from inside the region) initialises both translations to the
//! current target — no artificial launch. Translation speed above
//! [`VELOCITY_BOUND`] m/s faults visibly via `ctx.error`, once per frame.
//! Rotation and scale pass through to the displayed object untouched; the
//! collision geometry is the fixed-half-extents AABB (node.water_collide_box).

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::ParamValue;
use crate::node_graph::primitive::Primitive;
use crate::node_graph::state_store::NodeState;
use crate::node_graph::transform::Transform;
use crate::node_graph::water::VELOCITY_BOUND;

/// Result of one [`ColliderMotion::sample`] call.
pub struct ColliderSample {
    /// Interpolated collider transform for this substep. Rotation, scale
    /// and billboard pass through from the authored target.
    pub transform: Transform,
    /// Frame-constant collider velocity in m/s
    /// `(target - previous) / (accepted_substeps * step_dt)`.
    pub velocity: [f32; 3],
    /// True when the frame's translation speed exceeds [`VELOCITY_BOUND`].
    /// Latched per frame so `run` reports the fault once, not per substep.
    pub speed_fault: bool,
}

/// Per-owner collider interpolation state. Lives in the `StateStore` so it
/// survives across frames keyed by (node, owner); reset is detected from
/// the clock wires, not from host messages.
#[derive(Clone, Copy)]
pub struct ColliderMotion {
    initialized: bool,
    epoch: u64,
    frame_id: u64,
    /// Translation accepted at the end of the previous advancing frame.
    last_pos: [f32; 3],
    /// Previous/target snapshot for the frame currently being interpolated.
    previous: [f32; 3],
    target: [f32; 3],
    n_ticks: u32,
    last_step_time: f32,
    speed_faulted: bool,
}

impl Default for ColliderMotion {
    fn default() -> Self {
        Self {
            initialized: false,
            epoch: 0,
            frame_id: u64::MAX,
            last_pos: [0.0; 3],
            previous: [0.0; 3],
            target: [0.0; 3],
            n_ticks: 1,
            last_step_time: 0.0,
            speed_faulted: false,
        }
    }
}

impl NodeState for ColliderMotion {}

impl ColliderMotion {
    /// Advance the interpolation one substep. `run` calls this once per
    /// region iteration; the caller passes exactly what the step clock
    /// wires and `SimulationFrame` carry.
    #[allow(clippy::too_many_arguments)]
    pub fn sample(
        &mut self,
        epoch: u64,
        frame_id: u64,
        advancing: bool,
        step_time: f32,
        step_index: u32,
        step_count: u32,
        step_dt: f32,
        target: Transform,
    ) -> ColliderSample {
        // Reset / reappearance (design section 6): first observation, epoch
        // change, or the boundary clock restarted (a reset frame schedules
        // zero ticks, so the regression is first visible on the next
        // advancing substep). Initialise both translations to the current
        // target — no artificial launch velocity.
        let reset = !self.initialized
            || epoch != self.epoch
            || step_time < self.last_step_time;
        if reset {
            self.initialized = true;
            self.epoch = epoch;
            self.previous = target.pos;
            self.last_pos = target.pos;
            self.target = target.pos;
            self.speed_faulted = false;
        }

        // Hold on a non-advancing frame: frozen water must not have
        // collision geometry moved through it (defensive — the region
        // iterates zero times while paused, so run() is not called).
        if !advancing {
            // Hold on a non-advancing frame: frozen water must not have
            // collision geometry moved through it (defensive — the region
            // iterates zero times while paused, so run() is not called).
            return ColliderSample {
                transform: Transform {
                    pos: self.last_pos,
                    rot_euler: target.rot_euler,
                    scale: target.scale,
                    billboard: target.billboard,
                },
                velocity: [0.0; 3],
                speed_fault: false,
            };
        }

        if self.frame_id != frame_id {
            // First substep of a new frame: snapshot the interpolation
            // endpoints. The authored target is evaluated once per frame,
            // so the snapshot is frame-constant.
            self.frame_id = frame_id;
            self.previous = self.last_pos;
            self.target = target.pos;
            self.n_ticks = step_count.max(1);
            self.speed_faulted = false;
        }

        let n = self.n_ticks as f32;
        let frac = (step_index.min(self.n_ticks - 1) as f32 + 1.0) / n;
        let pos = [
            self.previous[0] + (self.target[0] - self.previous[0]) * frac,
            self.previous[1] + (self.target[1] - self.previous[1]) * frac,
            self.previous[2] + (self.target[2] - self.previous[2]) * frac,
        ];
        let sim_seconds = n * step_dt;
        let velocity = if sim_seconds > 0.0 {
            [
                (self.target[0] - self.previous[0]) / sim_seconds,
                (self.target[1] - self.previous[1]) / sim_seconds,
                (self.target[2] - self.previous[2]) / sim_seconds,
            ]
        } else {
            [0.0; 3]
        };
        let speed = (velocity[0] * velocity[0]
            + velocity[1] * velocity[1]
            + velocity[2] * velocity[2])
            .sqrt();
        let speed_fault = speed > VELOCITY_BOUND;
        if speed_fault {
            self.speed_faulted = true;
        }

        self.last_pos = pos;
        self.last_step_time = step_time;

        let transform = Transform {
            pos,
            rot_euler: target.rot_euler,
            scale: target.scale,
            billboard: target.billboard,
        };
        ColliderSample {
            transform,
            velocity,
            speed_fault,
        }
    }
}

crate::primitive! {
    name: WaterColliderMotion,
    type_id: "node.water_collider_motion",
    purpose: "Interpolate the Live Water cube collider across the frame's accepted substeps (design section 6). Retains the previous accepted translation per owner, snapshots the authored target Transform once per frame, and emits transform = previous + (target-previous)*(i+1)/N plus the frame-constant collider velocity (target-previous)/(N*step_dt) for the translating-AABB boundary in node.water_collide_box and (later) node.mpm_grid_velocity. The emitted transform feeds node.water_state's collider_in capture, so the displayed cube IS the accepted collider — the authored target remains the only authoring model. At reset/reappearance both translations initialise to the target (no artificial launch); while paused the last accepted transform holds. Translation speed above the proof bound (4 m/s) faults visibly, once per frame. Rotation/scale pass through to the display; the collision geometry is the fixed-half-extents AABB.",
    inputs: {
        target: Transform required,
        step_dt: ScalarF32 optional,
        step_time: ScalarF32 optional,
        step_index: ScalarF32 optional,
        step_count: ScalarF32 optional,
    },
    outputs: {
        transform: Transform,
        velocity: ScalarVec3,
    },
    params: [],
    depth_rule: Terminal,
    composition_notes: "First stage of the repeated water region body that touches the collider: `water_collider_motion -> water_collide_box` (and the S7 grid boundary). Wire `target` from the authored cube object (e.g. node.transform_3d), and the four step clock wires from node.water_state's step_dt/step_time/step_index/step_count outputs. The transform output goes to node.water_state's collider_in capture port — that back-edge is what makes the displayed cube consume the accepted collider.",
    examples: [],
    picker: { label: "Water Collider Motion", category: Atom },
    summary: "Moves the water collider smoothly between its last accepted position and the authored target over the frame's substeps, and reports how fast it is going.",
    category: Particles3D,
    role: Filter,
    aliases: ["water collider", "collider motion", "cube motion", "water cube"],
    boundary_reason: NonGpu,
}

impl Primitive for WaterColliderMotion {
    fn requires(&self) -> crate::node_graph::effect_node::NodeRequires {
        crate::node_graph::effect_node::NodeRequires {
            state_store: true,
            gpu_encoder: false,
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(target) = ctx.inputs.transform("target") else {
            return;
        };
        let frame = ctx.simulation_frame;
        let (epoch, frame_id, advancing) = match frame {
            Some(f) => (f.epoch, f.frame_id, f.advancing),
            None => (0, u64::MAX, false),
        };
        let step_dt = ctx.scalar_or_param("step_dt", 0.0);
        let step_time = ctx.scalar_or_param("step_time", 0.0);
        let step_index = ctx.scalar_or_param("step_index", 0.0).round().max(0.0) as u32;
        let step_count = ctx
            .scalar_or_param("step_count", 1.0)
            .round()
            .max(1.0) as u32;

        let node_id = ctx.node_id;
        let owner_key = ctx.owner_key;
        let mut motion = {
            let store = ctx
                .state
                .as_deref_mut()
                .expect("WaterColliderMotion requires a StateStore");
            match store.get::<ColliderMotion>(node_id, owner_key) {
                Some(existing) => *existing,
                None => ColliderMotion::default(),
            }
        };

        let sample =
            motion.sample(epoch, frame_id, advancing, step_time, step_index, step_count, step_dt, target);

        {
            let store = ctx
                .state
                .as_deref_mut()
                .expect("WaterColliderMotion requires a StateStore");
            store.insert(node_id, owner_key, motion);
        }
        ctx.outputs.set_transform("transform", sample.transform);
        ctx.outputs
            .set_scalar("velocity", ParamValue::Vec3(sample.velocity));
        if sample.speed_fault {
            ctx.error(format!(
                "WaterColliderMotion: collider translation speed exceeds the {VELOCITY_BOUND} m/s \
                 proof bound — slow the cube stroke; the sweep is not supported"
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    fn target_at(x: f32, y: f32, z: f32) -> Transform {
        Transform {
            pos: [x, y, z],
            ..Transform::default()
        }
    }

    const DT: f32 = 1.0 / 960.0;

    /// Run one 4-substep advancing frame with the target parked at `to_y`.
    /// Step times are monotone per frame and across frames.
    fn stroke_frame(motion: &mut ColliderMotion, epoch: u64, frame: u64, to_y: f32) -> Vec<ColliderSample> {
        let mut out = Vec::new();
        for i in 0..4u32 {
            let step_time = frame as f32 * 10.0 + (i as f32 + 1.0) * DT;
            out.push(motion.sample(
                epoch,
                frame,
                true,
                step_time,
                i,
                4,
                DT,
                target_at(0.0, to_y, 0.0),
            ));
        }
        out
    }

    #[test]
    fn collider_motion_interpolates_linearly_over_substeps() {
        let mut motion = ColliderMotion::default();
        // Frame 1: establish the collider at rest (target == initial).
        let rest = stroke_frame(&mut motion, 0, 1, 1.0);
        for (i, s) in rest.iter().enumerate() {
            assert_eq!(s.transform.pos[1], 1.0, "rest frame moved at substep {i}");
            assert_eq!(s.velocity, [0.0; 3]);
            assert!(!s.speed_fault);
        }

        // Frame 2: stroke 0.2 m over 4 substeps of dt = 1/960 → frame speed
        // 0.2/(4*DT) = 48 m/s, well above the 4 m/s bound (fault asserted in
        // the speed test below; here we check the interpolation shape).
        let stroke = stroke_frame(&mut motion, 0, 2, 1.2);
        let expected_frac = [0.25f32, 0.5, 0.75, 1.0];
        let expected_v = 0.2 / (4.0 * DT);
        for (i, s) in stroke.iter().enumerate() {
            let expected_y = 1.0 + 0.2 * expected_frac[i];
            assert!(
                (s.transform.pos[1] - expected_y).abs() < 1.0e-6,
                "substep {i}: pos {} != {expected_y}",
                s.transform.pos[1]
            );
            // Frame-constant velocity across every substep.
            assert!(
                (s.velocity[1] - expected_v).abs() < 1.0e-3,
                "substep {i}: velocity {} != {expected_v}",
                s.velocity[1]
            );
            assert_eq!(s.velocity[0], 0.0);
            assert_eq!(s.velocity[2], 0.0);
        }
        // The final accepted transform IS the target: the displayed cube and
        // the collision geometry share one wire.
        assert_eq!(stroke[3].transform.pos[1], 1.2);
    }

    #[test]
    fn collider_motion_resets_without_artificial_launch() {
        let mut motion = ColliderMotion::default();
        let _ = stroke_frame(&mut motion, 0, 1, 2.0);
        assert!(motion.last_pos[1] > 1.9);
        // Reset: sim time regresses (a boundary reset frame schedules zero
        // ticks, so the regression is first visible on the next advancing
        // substep). The target has also moved (reappearance elsewhere).
        let s = motion.sample(0, 2, true, DT, 0, 4, DT, target_at(0.0, 0.5, 0.0));
        assert_eq!(s.transform.pos[1], 0.5, "reset must land exactly on the target");
        assert_eq!(s.velocity, [0.0; 3], "no artificial launch velocity");
        assert!(!s.speed_fault);
        // And the next substep continues from there without a jump.
        let s2 = motion.sample(0, 2, true, 2.0 * DT, 1, 4, DT, target_at(0.0, 0.5, 0.0));
        assert_eq!(s2.transform.pos[1], 0.5);
        assert_eq!(s2.velocity, [0.0; 3]);
    }

    #[test]
    fn collider_motion_epoch_change_reinitialises() {
        let mut motion = ColliderMotion::default();
        let _ = stroke_frame(&mut motion, 0, 1, 2.0);
        // Seek: epoch bumps and the sim clock restarts below the last
        // observation. Both signals are present; either alone must suffice.
        let s = motion.sample(7, 100, true, DT, 0, 4, DT, target_at(1.0, 1.0, 1.0));
        assert_eq!(s.transform.pos, [1.0, 1.0, 1.0]);
        assert_eq!(s.velocity, [0.0; 3]);
        assert!(!s.speed_fault);
    }

    #[test]
    fn collider_motion_speed_faults_above_the_proof_bound() {
        let mut motion = ColliderMotion::default();
        // Establish rest at y = 1.0 first: the first-ever sample initialises
        // both translations to the target (no artificial launch).
        let _ = stroke_frame(&mut motion, 0, 1, 1.0);
        // Boundary-speed stroke: displacement of exactly 4 m/s * 4*DT.
        let max_d = 4.0 * 4.0 * DT;
        let ok = stroke_frame(&mut motion, 0, 2, 1.0 + max_d);
        assert!(
            !ok[0].speed_fault,
            "stroke at exactly the 4 m/s bound must not fault"
        );
        assert!(!ok[3].speed_fault);
        // Fast stroke: 2 m in one frame → ~470 m/s.
        let bad = stroke_frame(&mut motion, 0, 3, 3.0);
        assert!(bad[0].speed_fault, "fast stroke must fault");
        assert!(bad[3].speed_fault);
    }

    #[test]
    fn collider_motion_holds_when_not_advancing() {
        let mut motion = ColliderMotion::default();
        let s = motion.sample(0, 1, true, DT, 0, 4, DT, target_at(0.0, 1.0, 0.0));
        assert_eq!(s.transform.pos[1], 1.0);
        // A non-advancing sample (defensive: the region does not iterate
        // while paused, so run() is not called at all) holds the last
        // accepted transform with zero velocity.
        let hold = motion.sample(0, 2, false, DT, 0, 4, DT, target_at(0.0, 2.0, 0.0));
        assert_eq!(hold.transform.pos[1], 1.0);
        assert_eq!(hold.velocity, [0.0; 3]);
        assert!(!hold.speed_fault);
    }

    #[test]
    fn collider_motion_passes_through_rotation_and_scale() {
        let mut motion = ColliderMotion::default();
        let mut t = target_at(0.0, 1.0, 0.0);
        t.rot_euler = [0.1, 0.2, 0.3];
        t.scale = [2.0, 2.0, 2.0];
        let s = motion.sample(0, 1, true, DT, 0, 4, DT, t);
        assert_eq!(s.transform.rot_euler, t.rot_euler);
        assert_eq!(s.transform.scale, t.scale);
        assert!(!s.transform.billboard);
    }

    #[test]
    fn collider_motion_declares_ports() {
        use crate::node_graph::ports::{PortType, ScalarType};
        let prim = WaterColliderMotion::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.water_collider_motion");
        assert_eq!(WaterColliderMotion::INPUTS.len(), 5);
        assert_eq!(WaterColliderMotion::INPUTS[0].name, "target");
        assert_eq!(WaterColliderMotion::INPUTS[0].ty, PortType::Transform);
        assert_eq!(WaterColliderMotion::OUTPUTS.len(), 2);
        assert_eq!(WaterColliderMotion::OUTPUTS[0].name, "transform");
        assert_eq!(WaterColliderMotion::OUTPUTS[0].ty, PortType::Transform);
        assert_eq!(WaterColliderMotion::OUTPUTS[1].name, "velocity");
        assert_eq!(
            WaterColliderMotion::OUTPUTS[1].ty,
            PortType::Scalar(ScalarType::Vec3)
        );
        let req = node.requires();
        assert!(req.state_store);
        assert!(!req.gpu_encoder);
    }

    #[test]
    fn collider_motion_is_registered() {
        let registry = crate::node_graph::PrimitiveRegistry::with_builtin();
        assert!(
            registry.construct("node.water_collider_motion").is_some(),
            "node.water_collider_motion must be constructible from the builtin registry"
        );
    }
}
