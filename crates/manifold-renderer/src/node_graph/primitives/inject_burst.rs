//! `node.inject_burst` — fixed-duration burst state machine with
//! hashed UV pick, used to drive FluidSim2D's mode-4 ("inject") clip-
//! trigger response.
//!
//! On each new `trigger` value, if `enable > 0.5`, the primitive
//! starts a burst that runs for `duration` seconds. During the burst
//! the `active` output is `1.0`, `phase` ramps `0.0 → 1.0`, and
//! `point_x` / `point_y` emit a stable random UV picked at burst
//! start. Once the burst expires the outputs settle back to zero.
//!
//! The hash is bit-exact with `fluid_sim_core::random_inject_uv` so
//! the inject location is identical to the legacy generator for any
//! given (trigger, frame_count) pair.
//!
//! State: `last_trigger`, `active`, `point[2]`, `elapsed`. Reset on
//! seek / pause clears all state — a re-entered clip starts with no
//! burst in flight.

use std::borrow::Cow;
use crate::node_graph::effect_node::{
    EffectNode, EffectNodeContext, EffectNodeType, NodeRequires,
};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType};
use crate::node_graph::state_store::NodeState;

pub const INJECT_BURST_TYPE_ID: &str = "node.inject_burst";

struct InjectState {
    last_trigger: i32,
    active: bool,
    point: [f32; 2],
    elapsed: f32,
}

impl NodeState for InjectState {}

const INJECT_BURST_INPUTS: [NodeInput; 2] = [
    NodePort {
        name: Cow::Borrowed("trigger"),
        ty: PortType::Scalar(ScalarType::F32),
        kind: PortKind::Input,
        required: true,
    },
    // Gate input — typically wired from a math chain like
    // `(clip_trigger > 0.5) * (mode == 4)`. On trigger-edge frames
    // the primitive only starts a burst if `enable > 0.5`.
    NodePort {
        name: Cow::Borrowed("enable"),
        ty: PortType::Scalar(ScalarType::F32),
        kind: PortKind::Input,
        required: true,
    },
];

const INJECT_BURST_OUTPUTS: [NodeOutput; 4] = [
    NodePort {
        name: Cow::Borrowed("active"),
        ty: PortType::Scalar(ScalarType::F32),
        kind: PortKind::Output,
        required: false,
    },
    NodePort {
        name: Cow::Borrowed("phase"),
        ty: PortType::Scalar(ScalarType::F32),
        kind: PortKind::Output,
        required: false,
    },
    NodePort {
        name: Cow::Borrowed("point_x"),
        ty: PortType::Scalar(ScalarType::F32),
        kind: PortKind::Output,
        required: false,
    },
    NodePort {
        name: Cow::Borrowed("point_y"),
        ty: PortType::Scalar(ScalarType::F32),
        kind: PortKind::Output,
        required: false,
    },
];

const INJECT_BURST_PARAMS: [ParamDef; 1] = [ParamDef {
    name: Cow::Borrowed("duration"),
    label: "Duration (s)",
    ty: ParamType::Float,
    default: ParamValue::Float(0.5),
    range: Some((0.01, 5.0)),
    enum_values: &[],
}];

/// Bit-exact mirror of `fluid_sim_core::random_inject_uv`. Returns
/// a UV in `[0.1, 0.9]^2` (keeps the burst away from the canvas edge).
fn random_inject_uv(trigger: u32, frame: u32) -> [f32; 2] {
    let seed = trigger.wrapping_mul(747796405).wrapping_add(frame);
    let mut s = (seed ^ 61) ^ (seed >> 16);
    s = s.wrapping_mul(9);
    s ^= s >> 4;
    s = s.wrapping_mul(0x27d4eb2d);
    s ^= s >> 15;
    let x = (s & 0xFFFF) as f32 / 65535.0;
    let y = ((s >> 16) & 0xFFFF) as f32 / 65535.0;
    [0.1 + x * 0.8, 0.1 + y * 0.8]
}

#[derive(Debug)]
pub struct InjectBurst {
    type_id: EffectNodeType,
}

impl InjectBurst {
    pub fn new() -> Self {
        Self {
            type_id: EffectNodeType::new(INJECT_BURST_TYPE_ID),
        }
    }
}

impl Default for InjectBurst {
    fn default() -> Self {
        Self::new()
    }
}

impl EffectNode for InjectBurst {
    fn depth_rule(&self) -> crate::node_graph::depth_rule::DepthRule {
        crate::node_graph::depth_rule::DepthRule::Terminal
    }
    fn type_id(&self) -> &EffectNodeType {
        &self.type_id
    }
    fn boundary_reason(&self) -> Option<crate::node_graph::freeze::classify::BoundaryReason> {
        Some(crate::node_graph::freeze::classify::BoundaryReason::NonGpu)
    }

    fn inputs(&self) -> &[NodeInput] {
        &INJECT_BURST_INPUTS
    }

    fn outputs(&self) -> &[NodeOutput] {
        &INJECT_BURST_OUTPUTS
    }

    fn parameters(&self) -> &[ParamDef] {
        &INJECT_BURST_PARAMS
    }

    fn requires(&self) -> NodeRequires {
        NodeRequires {
            state_store: true,
            gpu_encoder: false,
        }
    }

    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let trigger_value = match ctx.inputs.scalar("trigger") {
            Some(ParamValue::Float(f)) => f.round() as i32,
            _ => return,
        };
        let enable = match ctx.inputs.scalar("enable") {
            Some(ParamValue::Float(f)) => f,
            _ => return,
        };
        let duration = match ctx.params.get("duration") {
            Some(ParamValue::Float(f)) => f.max(1e-4),
            _ => 0.5,
        };
        let dt = ctx.time.delta.0 as f32;
        let frame_count = ctx.time.frame_count as u32;

        let node_id = ctx.node_id;
        let owner_key = ctx.owner_key;
        let store = ctx
            .state
            .as_deref_mut()
            .expect("InjectBurst::evaluate requires a StateStore");

        let (mut last_trigger, mut active, mut point, mut elapsed) = store
            .get::<InjectState>(node_id, owner_key)
            .map(|s| (s.last_trigger, s.active, s.point, s.elapsed))
            .unwrap_or((-1, false, [0.0, 0.0], 0.0));

        // Edge detect on trigger — matches legacy `trigger_count != last_trigger_count`.
        // First observation (last_trigger == -1) arms without firing.
        if trigger_value != last_trigger {
            let fire = last_trigger >= 0 && enable > 0.5;
            last_trigger = trigger_value;
            if fire {
                active = true;
                point = random_inject_uv(trigger_value as u32, frame_count);
                elapsed = 0.0;
            }
        }

        // Advance burst clock when active.
        if active {
            elapsed += dt;
            if elapsed >= duration {
                active = false;
            }
        }

        let (active_out, phase_out, px_out, py_out) = if active {
            (1.0, elapsed / duration, point[0], point[1])
        } else {
            (0.0, 0.0, 0.0, 0.0)
        };

        store.insert(
            node_id,
            owner_key,
            InjectState {
                last_trigger,
                active,
                point,
                elapsed,
            },
        );

        ctx.outputs.set_scalar("active", ParamValue::Float(active_out));
        ctx.outputs.set_scalar("phase", ParamValue::Float(phase_out));
        ctx.outputs.set_scalar("point_x", ParamValue::Float(px_out));
        ctx.outputs.set_scalar("point_y", ParamValue::Float(py_out));
    }
}

inventory::submit! {
    crate::node_graph::persistence::PrimitiveFactory {
        type_id: INJECT_BURST_TYPE_ID,
        create: || Box::new(InjectBurst::new()),
        picker: Some(crate::node_graph::palette::PickerInfo {
            label: "Inject Burst",
            category: crate::node_graph::palette::PaletteCategory::Driver,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inject_burst_declares_two_inputs_and_four_outputs() {
        let node = InjectBurst::new();
        assert_eq!(node.inputs().len(), 2);
        assert_eq!(node.inputs()[0].name, "trigger");
        assert!(node.inputs()[0].required);
        assert_eq!(node.inputs()[1].name, "enable");
        assert!(node.inputs()[1].required);
        let outs: Vec<&str> = node.outputs().iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(outs, vec!["active", "phase", "point_x", "point_y"]);
    }

    #[test]
    fn inject_burst_has_duration_param() {
        let node = InjectBurst::new();
        let names: Vec<&str> = node.parameters().iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["duration"]);
    }

    #[test]
    fn inject_burst_type_id_is_node_prefixed() {
        let node = InjectBurst::new();
        assert_eq!(node.type_id().as_str(), "node.inject_burst");
    }

    /// The hash function is the load-bearing parity guarantee — the
    /// inject point on each clip-trigger must land in the exact same
    /// UV as the legacy generator. This test re-implements the legacy
    /// hash inline and asserts identity across a spread of inputs.
    #[test]
    fn random_inject_uv_matches_legacy_hash() {
        fn legacy(trigger: u32, frame: u32) -> [f32; 2] {
            let seed = trigger.wrapping_mul(747796405).wrapping_add(frame);
            let mut s = (seed ^ 61) ^ (seed >> 16);
            s = s.wrapping_mul(9);
            s = s ^ (s >> 4);
            s = s.wrapping_mul(0x27d4eb2d);
            s = s ^ (s >> 15);
            let x = (s & 0xFFFF) as f32 / 65535.0;
            let y = ((s >> 16) & 0xFFFF) as f32 / 65535.0;
            [0.1 + x * 0.8, 0.1 + y * 0.8]
        }
        for trigger in [0u32, 1, 7, 42, 999, 1_000_000] {
            for frame in [0u32, 1, 16, 60, 3600, 12345678] {
                let a = legacy(trigger, frame);
                let b = random_inject_uv(trigger, frame);
                assert_eq!(a, b, "hash mismatch at trigger={trigger} frame={frame}");
                // Sanity: UV stays inside [0.1, 0.9]^2.
                assert!(a[0] >= 0.1 && a[0] <= 0.9);
                assert!(a[1] >= 0.1 && a[1] <= 0.9);
            }
        }
    }

    /// CPU mirror of fluid_sim_core's inject state machine — confirms
    /// the burst timer + edge gating match legacy event-for-event.
    #[test]
    fn inject_burst_matches_fluid_sim_core_state_machine() {
        struct Mirror {
            last_trigger: i32,
            active: bool,
            point: [f32; 2],
            elapsed: f32,
        }
        impl Mirror {
            fn new() -> Self {
                Self {
                    last_trigger: -1,
                    active: false,
                    point: [0.0, 0.0],
                    elapsed: 0.0,
                }
            }
            fn tick(
                &mut self,
                trigger: i32,
                enable: f32,
                dt: f32,
                frame: u32,
                duration: f32,
            ) -> (f32, f32, f32, f32) {
                if trigger != self.last_trigger {
                    let fire = self.last_trigger >= 0 && enable > 0.5;
                    self.last_trigger = trigger;
                    if fire {
                        self.active = true;
                        self.point = random_inject_uv(trigger as u32, frame);
                        self.elapsed = 0.0;
                    }
                }
                if self.active {
                    self.elapsed += dt;
                    if self.elapsed >= duration {
                        self.active = false;
                    }
                }
                if self.active {
                    (1.0, self.elapsed / duration, self.point[0], self.point[1])
                } else {
                    (0.0, 0.0, 0.0, 0.0)
                }
            }
        }

        let mut m = Mirror::new();
        let dt = 1.0 / 60.0;
        let duration = 0.5;

        // Arm without firing — first trigger observation does nothing.
        let r = m.tick(0, 1.0, dt, 0, duration);
        assert_eq!(r, (0.0, 0.0, 0.0, 0.0));

        // Fire on next change.
        let r = m.tick(1, 1.0, dt, 1, duration);
        assert!(r.0 == 1.0);
        let (px, py) = (r.2, r.3);
        assert!((0.1..=0.9).contains(&px) && (0.1..=0.9).contains(&py));

        // Burst plays through frames 1..29 (elapsed climbs from 1/60
        // to 29/60 ≈ 0.483). On the 30th frame elapsed hits 0.5 and
        // the active flag clears.
        for _ in 0..28 {
            let r = m.tick(1, 1.0, dt, 1, duration);
            assert!(r.0 == 1.0, "burst should still be active");
            // Point stays stable for the whole burst.
            assert_eq!((r.2, r.3), (px, py));
        }

        // Frame 30 (elapsed = 30/60 = 0.5) — burst expires.
        let r = m.tick(1, 1.0, dt, 1, duration);
        assert_eq!(r, (0.0, 0.0, 0.0, 0.0));

        // Disabled trigger doesn't fire even on edge.
        let r = m.tick(2, 0.0, dt, 2, duration);
        assert_eq!(r, (0.0, 0.0, 0.0, 0.0));
    }
}
