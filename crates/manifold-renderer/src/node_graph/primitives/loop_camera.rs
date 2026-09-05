//! `node.loop_camera` — beat-locked flythrough camera for scene looping
//! (SCENE_LOOP_DESIGN.md D3, SCENE_MODIFIER_FRAMEWORK P4 controls).
//!
//! Emits a single [`Camera`] on `out`. Position advances along the chosen
//! axis over the loop phase: travel = home + d(phase)·stride·cell_size,
//! where d(p) = p − flow·sin(2πp)/(2π) eases the flight with equal slope at
//! both seams. Sway drifts the lateral/height offsets, the look sweep
//! weaves the target laterally, and the zoom pulse breathes the fov — every
//! one a function of the loop phase alone, so the frame at phase 0 is
//! identical to the frame at phase 1 (INV-3 wrap purity).
//!
//! `phase` (0..1) comes from a `beat_ramp` at attack=1, rate=1/bars.
//!
//! CPU-only — no GPU dispatch. Same convention as `node.orbit_camera`.

use std::borrow::Cow;

use crate::node_graph::camera::Camera;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

pub const LOOP_CAMERA_AXIS_LABELS: &[&str] = &["+X", "-X", "+Y", "-Y", "+Z", "-Z"];

crate::primitive! {
    name: LoopCamera,
    type_id: "node.loop_camera",
    purpose: "Beat-locked flythrough camera for scene looping. Emits one Camera on `out` from phase (0..1, wired from beat_ramp at attack=1 rate=1/bars), cell_size, axis (+X/-X/+Y/-Y/+Z/-Z), lateral/height offsets, and fov. Travel = home + d(phase)·stride·cell_size along axis, where d(p) = p − flow·sin(2πp)/(2π) eases the flight (equal seam slope, INV-3 wrap purity); sway drifts the lateral/height offsets, the look sweep weaves the target laterally, and the zoom pulse breathes the fov — all phase-periodic. The frame at phase 0 equals the frame at phase 1 by construction. CPU-only, no GPU dispatch.",
    inputs: {
        phase: ScalarF32 optional,
    },
    outputs: {
        out: Camera,
        pos_x: ScalarF32,
        pos_y: ScalarF32,
        pos_z: ScalarF32,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("cell_size"),
            label: "Cell Size",
            ty: ParamType::Float,
            default: ParamValue::Float(10.0),
            range: Some((0.01, 1000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("axis"),
            label: "Axis",
            ty: ParamType::Enum,
            default: ParamValue::Enum(4), // +Z
            range: None,
            enum_values: LOOP_CAMERA_AXIS_LABELS,
        },
        ParamDef {
            name: Cow::Borrowed("lateral"),
            label: "Lateral",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-100.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("height"),
            label: "Height",
            ty: ParamType::Float,
            default: ParamValue::Float(1.5),
            range: Some((-100.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("home"),
            label: "Home",
            // Phase-0 start along the travel axis (SCENE_LOOP_DESIGN D10 +
            // the BUG-70wo gap rule): the plan builder's cell is two
            // object-depths and home = -cell/2 puts the camera mid-gap, one
            // half-depth of air in front of copy 0's near face — the phase-0
            // frame is an approach shot, wrap-identical to phase 1.
            // Default 0 keeps the primitive's standalone behaviour unchanged.
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-100000.0, 100000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("fov_y"),
            label: "FOV Y",
            ty: ParamType::Angle,
            default: ParamValue::Float(0.9),
            range: Some((0.05, 2.5)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("near"),
            label: "Near",
            ty: ParamType::Float,
            default: ParamValue::Float(0.05),
            range: Some((0.001, 10000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("far"),
            label: "Far",
            ty: ParamType::Float,
            default: ParamValue::Float(200.0),
            range: Some((1.0, 10000.0)),
            enum_values: &[],
        },
        // ── Framing offsets (BUG-gsql): static rotation constants applied
        // after the look_at basis is built, in camera-local space via
        // Camera::rotate_local (yaw → pitch → roll). STATIC is what keeps
        // INV-3: they read no phase, so phase 0 and phase 1 frames stay
        // identical by construction — the wrap purity proof only has to
        // cover the time-varying terms above.
        ParamDef {
            name: Cow::Borrowed("roll"),
            label: "Roll",
            ty: ParamType::Angle,
            default: ParamValue::Float(0.0),
            range: Some((-3.2, 3.2)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("pitch"),
            label: "Pitch",
            ty: ParamType::Angle,
            default: ParamValue::Float(0.0),
            range: Some((-3.2, 3.2)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("yaw"),
            label: "Yaw",
            ty: ParamType::Angle,
            default: ParamValue::Float(0.0),
            range: Some((-3.2, 3.2)),
            enum_values: &[],
        },
        // ── SCENE_MODIFIER_FRAMEWORK P4 loop controls. Every time-varying
        // term below is a function of the loop PHASE alone (INV-3): the
        // frame at phase 0 equals the frame at phase 1 by construction.
        // Nothing here may read the frame clock — a non-phased driver is
        // the one-frame wrap jump (SCENE_LOOP_DESIGN D8).
        //
        // Flow: travel = d(phase) where d(p) = p − A·sin(2πp)/(2π).
        // d(0)=0, d(1)=1 and d'(0)=d'(1)=1−A — equal slope at both seams
        // by construction, so the flight eases through the wrap instead of
        // kinking. A ≥ 1 reverses seam velocity (position purity survives,
        // the motion kinks) — the 0.95 ceiling pins below that.
        ParamDef {
            name: Cow::Borrowed("flow"),
            label: "Flow",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 0.95)),
            enum_values: &[],
        },
        // Stride: whole cells travelled per loop (travel = K·cell). The
        // instance array must scale with it — the Stride card row is a
        // coupled write that also sets scene_array.count = K+2 (behind +
        // current + ahead) in one undo unit. scene_array.count clamps at
        // its own ceiling of 8, so K ≥ 7 outruns the array by one cell
        // (the clamp, not a wrap concern).
        ParamDef {
            name: Cow::Borrowed("stride"),
            label: "Stride",
            ty: ParamType::Int,
            default: ParamValue::Float(1.0),
            range: Some((1.0, 8.0)),
            enum_values: &[],
        },
        // Sway: lateral AND height offsets += amp·sin(2π·cycles·phase) —
        // a diagonal drift through the cross-section. cycles is whole
        // (1..8) so phase 0 and phase 1 land on the same sine value; a
        // non-integer cycle count would break the wrap.
        ParamDef {
            name: Cow::Borrowed("sway_amp"),
            label: "Sway",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("sway_cycles"),
            label: "Sway Rate",
            ty: ParamType::Int,
            default: ParamValue::Float(1.0),
            range: Some((1.0, 8.0)),
            enum_values: &[],
        },
        // Look sweep: the look target drifts laterally
        // (amp·sin(2π·cycles·phase)) while the position holds its path —
        // the camera weaves without leaving the corridor. Integer cycles
        // for the same wrap reason as sway.
        ParamDef {
            name: Cow::Borrowed("look_sweep_amp"),
            label: "Look Sway",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("look_sweep_cycles"),
            label: "Look Sweep Rate",
            ty: ParamType::Int,
            default: ParamValue::Float(1.0),
            range: Some((1.0, 8.0)),
            enum_values: &[],
        },
        // Zoom pulse: fov_y += amp·sin(π·phase) — the window is zero at
        // BOTH seams, so the pulse breathes once per loop and lands back
        // on the exact base fov at the wrap.
        ParamDef {
            name: Cow::Borrowed("zoom_pulse_amp"),
            label: "Zoom Pulse",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 0.5)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "phase is port-shadowed: wire from beat_ramp (attack=1, rate=1/bars) for beat-locked looping. Unwired, reads FrameTime.beats mod 1. The SAME cell_size feeds both this node and node.scene_array — the plan builder computes it once from scene_bounds (D4). lateral/height offset the camera within the cross-section perpendicular to travel. axis enum matches scene_array's axis enum. Camera looks down the travel axis (fwd = axis direction); lateral offsets the camera perpendicular to travel. pos_x/pos_y/pos_z outputs for PBR material atoms.",
    examples: [],
    picker: { label: "Loop Camera", category: Driver },
    summary: "A camera that flies through a scene in a perfect loop, locked to the beat.",
    category: Geometry3D,
    role: Source,
    aliases: ["loop camera", "flythrough camera", "travel camera"],
    boundary_reason: NonGpu,
}

impl Primitive for LoopCamera {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let cell_size = match ctx.params.get("cell_size") {
            Some(ParamValue::Float(f)) => *f,
            _ => 10.0,
        };
        let axis = match ctx.params.get("axis") {
            Some(ParamValue::Enum(n)) => *n,
            _ => 4, // +Z
        };
        let lateral = ctx.scalar_or_param("lateral", 0.0);
        let height = ctx.scalar_or_param("height", 1.5);
        // home comes from the plan builder (-cell/2 = mid-gap start under the
        // D4 gap rule); standalone default 0 starts at the origin.
        let home = ctx.scalar_or_param("home", 0.0);
        let fov_y = ctx.scalar_or_param("fov_y", 0.9).max(0.01);
        let near = match ctx.params.get("near") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.05,
        };
        let far = match ctx.params.get("far") {
            Some(ParamValue::Float(f)) => *f,
            _ => 200.0,
        };

        // P4 loop controls — all phase-periodic (see the ParamDef block
        // above). Unset/absent reads as "off": flow 0 = linear travel,
        // stride 1 = one cell, sway/look/zoom 0 = no effect.
        let flow = ctx.scalar_or_param("flow", 0.0).clamp(0.0, 0.95);
        let stride = ctx.scalar_or_param("stride", 1.0).round().clamp(1.0, 8.0);
        let sway_amp = ctx.scalar_or_param("sway_amp", 0.0);
        let sway_cycles = ctx.scalar_or_param("sway_cycles", 1.0).round().clamp(1.0, 8.0);
        let look_sweep_amp = ctx.scalar_or_param("look_sweep_amp", 0.0);
        let look_sweep_cycles =
            ctx.scalar_or_param("look_sweep_cycles", 1.0).round().clamp(1.0, 8.0);
        let zoom_pulse_amp = ctx.scalar_or_param("zoom_pulse_amp", 0.0).clamp(0.0, 0.5);

        // Framing offsets — static constants (BUG-gsql). No phase read, so
        // INV-3 (phase 0 frame == phase 1 frame) survives by construction.
        let roll = ctx.scalar_or_param("roll", 0.0);
        let pitch = ctx.scalar_or_param("pitch", 0.0);
        let yaw = ctx.scalar_or_param("yaw", 0.0);

        // Phase: wired beat_ramp or fallback to fract(beats).
        let phase = match ctx.inputs.scalar("phase") {
            Some(ParamValue::Float(f)) => f.fract().clamp(0.0, 1.0),
            _ => {
                let b = ctx.time.beats.0 as f32;
                b.fract().clamp(0.0, 1.0)
            }
        };

        // Travel distance along the axis, from the corridor entry `home`.
        // Flow eases the flight: d(0)=0, d(1)=1, equal seam slopes (the
        // sin term vanishes at both seams). Stride walks K cells per loop;
        // integer stride keeps phase 0 == phase 1 in POSITION (K cells
        // ahead = the identical scene, D4).
        let two_pi = std::f32::consts::TAU;
        let eased = phase - flow * (two_pi * phase).sin() / two_pi;
        let travel = home + eased * stride * cell_size;

        // Sway: the SAME sine adds to lateral and height (the committed
        // P4 math — a diagonal drift through the cross-section).
        let sway = sway_amp * (two_pi * sway_cycles * phase).sin();
        let lateral = lateral + sway;
        let height = height + sway;

        // Build position: travel along axis + lateral/height offsets.
        // Lateral offsets perpendicular to travel; height is Y offset.
        let (pos, fwd): ([f32; 3], [f32; 3]) = match axis {
            0 => ([travel, height, lateral], [1.0, 0.0, 0.0]),       // +X
            1 => ([-travel, height, lateral], [-1.0, 0.0, 0.0]),     // -X
            2 => ([lateral, travel, height], [0.0, 1.0, 0.0]),       // +Y
            3 => ([lateral, -travel, height], [0.0, -1.0, 0.0]),     // -Y
            4 => ([lateral, height, travel], [0.0, 0.0, 1.0]),       // +Z
            _ => ([lateral, height, -travel], [0.0, 0.0, -1.0]),     // -Z
        };

        // The lateral direction of each axis arm — the look sweep drifts
        // the target along it while the position holds its path.
        let lateral_dir: [f32; 3] = match axis {
            0 | 1 => [0.0, 0.0, 1.0], // ±X travel → lateral is Z
            _ => [1.0, 0.0, 0.0],     // ±Y/±Z travel → lateral is X
        };

        // Target is one unit ahead along the travel axis, drifted by the
        // look sweep (integer cycles → wrap-pure like sway).
        let look_sweep = look_sweep_amp * (two_pi * look_sweep_cycles * phase).sin();
        let target = [
            pos[0] + fwd[0] + lateral_dir[0] * look_sweep,
            pos[1] + fwd[1] + lateral_dir[1] * look_sweep,
            pos[2] + fwd[2] + lateral_dir[2] * look_sweep,
        ];

        // Zoom pulse: sin(π·phase) is zero at both seams — the fov breathes
        // once per loop and lands back on the base fov at the wrap.
        let fov = (fov_y + zoom_pulse_amp * (std::f32::consts::PI * phase).sin()).max(0.01);
        let mut cam = Camera::look_at(pos, target, [0.0, 1.0, 0.0], fov, near, far);
        cam.rotate_local(yaw, pitch, roll);

        ctx.outputs.set_camera("out", cam);
        ctx.outputs.set_scalar("pos_x", ParamValue::Float(pos[0]));
        ctx.outputs.set_scalar("pos_y", ParamValue::Float(pos[1]));
        ctx.outputs.set_scalar("pos_z", ParamValue::Float(pos[2]));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn loop_camera_declares_one_scalar_input_and_camera_output() {
        use crate::node_graph::ports::{PortType, ScalarType};
        assert_eq!(LoopCamera::TYPE_ID, "node.loop_camera");
        let in_names: Vec<&str> = LoopCamera::INPUTS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(in_names, vec!["phase"]);
        for input in LoopCamera::INPUTS {
            assert!(!input.required, "{} should be optional (port-shadow)", input.name);
            assert_eq!(input.ty, PortType::Scalar(ScalarType::F32));
        }
        assert_eq!(LoopCamera::OUTPUTS.len(), 4);
        assert_eq!(LoopCamera::OUTPUTS[0].name, "out");
        assert_eq!(LoopCamera::OUTPUTS[0].ty, PortType::Camera);
        assert_eq!(LoopCamera::OUTPUTS[1].name, "pos_x");
        assert_eq!(LoopCamera::OUTPUTS[2].name, "pos_y");
        assert_eq!(LoopCamera::OUTPUTS[3].name, "pos_z");
    }

    #[test]
    fn loop_camera_has_eighteen_params() {
        let names: Vec<&str> = LoopCamera::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(
            names,
            vec![
                "cell_size", "axis", "lateral", "height", "home", "fov_y", "near", "far",
                "roll", "pitch", "yaw", "flow", "stride", "sway_amp", "sway_cycles",
                "look_sweep_amp", "look_sweep_cycles", "zoom_pulse_amp",
            ]
        );
    }

    /// INV-3 (P4 extension), CPU half. The loop's phase input is fract()'d,
    /// so the wrap lands on phase 0.0 EXACTLY (fract(1.0) = 0.0) — and at
    /// phase 0 every offset term is exactly 0 (sin(0) = 0, no 2π residue).
    /// That exact-seam property is what the pixel gate in
    /// tests/scene_loop_wrap_parity.rs exercises (beat 0 vs beat 8). Note
    /// sin(2πc·1.0) is NOT bit-zero in f32 (~1e-7 residue) — the seam must
    /// come through fract, never through a literal phase of 1.0.
    #[test]
    fn movement_controls_vanish_exactly_at_the_seam() {
        let two_pi = std::f32::consts::TAU;
        let sway = |phase: f32, cycles: f32| 0.5 * (two_pi * cycles * phase).sin();
        let look = |phase: f32, cycles: f32| 0.5 * (two_pi * cycles * phase).sin();
        let zoom = |phase: f32| 0.25 * (std::f32::consts::PI * phase).sin();

        assert_eq!(sway(0.0, 2.0), 0.0, "sway must be exactly 0 at the seam");
        assert_eq!(look(0.0, 1.0), 0.0, "look sweep must be exactly 0 at the seam");
        assert_eq!(zoom(0.0), 0.0, "zoom pulse must be exactly 0 at the seam");

        // Wrap mechanics: the phase reader fract()'s its input, so the seam
        // is phase 0.0 on both sides — a literal 1.0 never survives.
        let phase_from_beats = |beats: f32, bars: f32| (beats / bars).fract().clamp(0.0, 1.0);
        assert_eq!(phase_from_beats(0.0, 8.0), phase_from_beats(8.0, 8.0));

        // Flow ease: d(0) = 0 exactly; d(1) within 1 ulp (the f32 sin(2π)
        // residue, unreachable through fract — documented, not hidden).
        let d = |phase: f32| phase - 0.8 * (two_pi * phase).sin() / two_pi;
        assert_eq!(d(0.0), 0.0);
        assert!(
            (d(1.0) - 1.0).abs() <= f32::EPSILON,
            "eased travel end within 1 ulp of 1 (got {})",
            d(1.0)
        );
        // Travel per loop is stride·cell to within 1 ulp — an integer
        // number of scene periods, so the rendered frame wraps pure.
        let cell = 10.0f32;
        let stride = 3.0f32;
        assert!(
            ((d(1.0) - d(0.0)) * stride * cell - stride * cell).abs() <= f32::EPSILON * 100.0,
            "travel per loop must be stride·cell within a hair"
        );
    }

    /// Flow ease shape: d(0)=0, d(1)=1 (position purity) and equal seam
    /// slopes d'(0)==d'(1)==1−A (no velocity kink at the wrap).
    #[test]
    fn flow_ease_hits_seams_with_equal_slope() {
        let two_pi = std::f32::consts::TAU;
        let d = |p: f32, a: f32| p - a * (two_pi * p).sin() / two_pi;
        for a in [0.0, 0.25, 0.5, 0.8, 0.95] {
            assert_eq!(d(0.0, a), 0.0, "d(0) must be exactly 0 (A={a})");
            assert_eq!(d(1.0, a), 1.0, "d(1) must be exactly 1 (A={a})");
            let slope = |p: f32| 1.0 - a * (two_pi * p).cos();
            assert!(
                (slope(0.0) - slope(1.0)).abs() < 1e-6,
                "seam slopes must match (A={a})"
            );
        }
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = LoopCamera::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.loop_camera");
    }

    // The run() tests below build a real EffectNodeContext over a MockBackend
    // (same harness as transform_shake's tests): the phase is wired as a
    // scalar input so both the fract()'d seam path and the movement terms
    // are exercised end to end.

    use crate::node_graph::backend::Backend;
    use crate::node_graph::bindings::{NodeInputs, NodeOutputs};
    use crate::node_graph::effect_node::{EffectNodeContext, FrameTime, ParamValues};
    use crate::node_graph::execution_plan::ResourceId;
    use crate::node_graph::{MockBackend, ports::ScalarType};
    use manifold_core::{Beats, Seconds};

    fn frame_time() -> FrameTime {
        FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        }
    }

    /// Run the primitive once with `phase` wired to the given value and the
    /// named params overridden (everything else at the manifest default).
    fn run_camera(phase: f32, overrides: &[(&'static str, f32)]) -> Camera {
        let defaults: &[(&str, f32)] = &[
            ("cell_size", 10.0),
            ("lateral", 0.0),
            ("height", 1.5),
            ("home", 0.0),
            ("fov_y", 0.9),
            ("flow", 0.0),
            ("stride", 1.0),
            ("sway_amp", 0.0),
            ("sway_cycles", 1.0),
            ("look_sweep_amp", 0.0),
            ("look_sweep_cycles", 1.0),
            ("zoom_pulse_amp", 0.0),
            ("roll", 0.0),
            ("pitch", 0.0),
            ("yaw", 0.0),
        ];

        let mut backend = MockBackend::new();
        let phase_slot = backend.acquire(
            ResourceId(0),
            crate::node_graph::ports::PortType::Scalar(ScalarType::F32),
            None,
            (0, 0),
        );
        backend.set_scalar(phase_slot, ParamValue::Float(phase));
        let out_slot = backend.acquire(ResourceId(1), crate::node_graph::ports::PortType::Camera, None, (0, 0));

        let mut params = ParamValues::default();
        for &(name, default) in defaults {
            let value = overrides
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, v)| *v)
                .unwrap_or(default);
            params.insert(Cow::Owned(name.to_string()), ParamValue::Float(value));
        }
        // axis is an Enum param; the test harness only overrides floats, so
        // +Z (the standalone default) applies unless a test wires otherwise.
        params.insert(Cow::Borrowed("axis"), ParamValue::Enum(4)); // +Z

        let wire_slots: &[(&'static str, crate::node_graph::bindings::Slot)] =
            &[("phase", phase_slot)];
        let outputs_bindings: &[(&'static str, crate::node_graph::bindings::Slot)] =
            &[("out", out_slot)];
        let mut scalar_scratch = Vec::new();
        let mut camera_scratch = Vec::new();
        let mut light_scratch = Vec::new();
        let mut material_scratch = Vec::new();
        let mut transform_scratch = Vec::new();
        let mut atmosphere_scratch = Vec::new();
        let mut object_scratch = Vec::new();
        let inputs = NodeInputs::new(wire_slots, &backend, &[]);
        let outputs = NodeOutputs::new(
            outputs_bindings,
            &backend,
            &mut scalar_scratch,
            &mut camera_scratch,
            &mut light_scratch,
            &mut material_scratch,
            &mut transform_scratch,
            &mut atmosphere_scratch,
            &mut object_scratch,
        );
        let time = frame_time();
        let mut ctx = EffectNodeContext::new(time, &params, inputs, outputs, None);
        let mut prim = LoopCamera::new();
        Primitive::run(&mut prim, &mut ctx);

        for (slot, value) in camera_scratch.drain(..) {
            backend.set_camera(slot, value);
        }
        backend.camera(out_slot).expect("camera output should be set")
    }

    /// INV-3 with the framing offsets live: nonzero roll/pitch/yaw plus every
    /// movement control at a nonzero phase-varying value, and the frame at
    /// wired phase 1.0 (fract()'d to exactly 0.0) must equal the phase-0
    /// frame BIT FOR BIT — the offsets are static, so nothing about the wrap
    /// can depend on them.
    #[test]
    fn framing_offsets_keep_the_wrap_bit_exact() {
        let overrides: &[(&'static str, f32)] = &[
            ("roll", 0.7),
            ("pitch", -0.4),
            ("yaw", 1.1),
            ("flow", 0.8),
            ("stride", 3.0),
            ("sway_amp", 0.5),
            ("sway_cycles", 2.0),
            ("look_sweep_amp", 0.5),
            ("zoom_pulse_amp", 0.25),
        ];
        let at_zero = run_camera(0.0, overrides);
        let at_wrap = run_camera(1.0, overrides);
        assert_eq!(
            at_zero, at_wrap,
            "phase 1.0 fract()'s to 0.0 — with static framing offsets the frames must be bit-identical"
        );
        // Sanity: the offsets actually changed the frame (a zero-angle bug
        // would pass the equality above trivially).
        let unrotated = run_camera(0.0, &[]);
        assert_ne!(
            at_zero.fwd, unrotated.fwd,
            "nonzero roll/pitch/yaw must rotate the basis away from the unrotated frame"
        );
    }

    /// Concrete numeric case: +Z travel with yaw = π/2 looks down +X — the
    /// camera-local positive-yaw convention (left about camera Y) applied to
    /// the look_at basis (fwd +Z, up +Y).
    #[test]
    fn yaw_quarter_turn_on_z_travel_faces_positive_x() {
        let cam = run_camera(0.0, &[("yaw", std::f32::consts::FRAC_PI_2)]);
        assert!(
            (cam.fwd[0] - 1.0).abs() < 1e-5,
            "yaw π/2 must face +X, got fwd {:?}",
            cam.fwd
        );
        assert!(cam.fwd[1].abs() < 1e-5 && cam.fwd[2].abs() < 1e-5, "fwd {:?}", cam.fwd);
    }

    /// Zero offsets are byte-identical to the pre-offset camera: existing
    /// saved loops render exactly as before (the rotate_local no-op).
    #[test]
    fn zero_framing_offsets_match_the_unrotated_camera() {
        let base = run_camera(0.25, &[]);
        // Reference basis straight from look_at (no rotate_local).
        let cell = 10.0;
        let phase = 0.25f32;
        let travel = phase * cell;
        let pos = [0.0f32, 1.5, travel];
        let target = [pos[0], pos[1], pos[2] + 1.0];
        let expected = Camera::look_at(pos, target, [0.0, 1.0, 0.0], 0.9, 0.05, 200.0);
        assert_eq!(base, expected, "zero roll/pitch/yaw must not perturb the camera at all");
    }
}
