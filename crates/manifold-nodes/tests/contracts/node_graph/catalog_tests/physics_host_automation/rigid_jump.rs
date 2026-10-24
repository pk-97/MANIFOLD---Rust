//! A real rigid world through the host's jumps. A forward seek, a seek back,
//! and a placement held over paused frames before Play each land a falling
//! body on the same bits at every rate, live and exported. A frame whose
//! input knot the world refuses loses its ticks, and how many depends on the
//! rate, so a refusal shows here as a pose mismatch. With Speed on an
//! automation lane, the ticks between the jump's target and its landed frame
//! must also take the lane's Speed at their own starts.
use super::*;
use manifold_node_engine::scene::transform::Transform;
use std::cell::Cell;

thread_local! {
    static POSE: Cell<Option<Transform>> = const { Cell::new(None) };
}

/// A 20 Hz Sim Rate, whose boundaries miss [`UNTIL`].
const INTERVAL: Seconds = Seconds(0.05);
/// A frame at every rate, a third of a tick past a boundary: the replay that
/// closes it takes knots past the last boundary the world accepted.
const UNTIL: f64 = 2.0 / 3.0;

struct PoseProbe(EffectNodeType);

impl EffectNode for PoseProbe {
    fn is_liveness_root(&self) -> bool {
        true
    }
    fn runs_with_pending_inputs(&self) -> bool {
        true
    }
    fn type_id(&self) -> &EffectNodeType {
        &self.0
    }
    fn depth_rule(&self) -> manifold_node_engine::scene::depth_rule::DepthRule {
        manifold_node_engine::scene::depth_rule::DepthRule::Terminal
    }
    fn inputs(&self) -> &[NodeInput] {
        static INPUTS: [NodeInput; 1] = [NodePort {
            name: Cow::Borrowed("pose"),
            ty: PortType::Transform,
            kind: PortKind::Input,
            required: true,
        }];
        &INPUTS
    }
    fn outputs(&self) -> &[NodeOutput] {
        &[]
    }
    fn parameters(&self) -> &[ParamDef] {
        &[]
    }
    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        POSE.set(ctx.inputs.transform("pose"));
    }
}

/// A body falling in a native rigid world whose Speed is the host's `speed`
/// param.
fn rigid_runtime() -> PresetRuntime {
    let mut registry = PrimitiveRegistry::with_builtin();
    manifold_water_rigid::testkit::physics_fixtures::register(&mut registry);
    registry.register("test.rigid_pose", || {
        Box::new(PoseProbe(EffectNodeType::new("test.rigid_pose")))
    });
    let def = serde_json::json!({
        "version": 2, "name": "Rigid world",
        "presetMetadata": {
            "id": "AutomatedSpeed", "displayName": "Rigid world", "category": "Test",
            "oscPrefix": "rigid_world", "available": true,
            "params": [{"id": "speed", "name": "Speed", "min": 0.0, "max": 4.0, "defaultValue": 1.0}],
            "bindings": [{"id": "speed", "label": "Speed", "defaultValue": 1.0,
                "target": {"kind": "node", "nodeId": "world", "param": "speed"}}]
        },
        "nodes": [
            {"id": 0, "nodeId": "input", "typeId": "system.generator_input"},
            {"id": 1, "nodeId": "start", "typeId": "test.physics_transform", "params": {"pos_y": {"type": "Float", "value": 50.0}}},
            {"id": 2, "nodeId": "body", "typeId": "test.physics_body"},
            {"id": 3, "nodeId": "world", "typeId": "node.physics_world"},
            {"id": 4, "nodeId": "observe", "typeId": "test.rigid_pose"},
            {"id": 5, "nodeId": "source", "typeId": "system.source"},
            {"id": 6, "nodeId": "output", "typeId": "system.final_output"}
        ],
        "wires": [
            {"fromNode": 1, "fromPort": "transform", "toNode": 2, "toPort": "transform"},
            {"fromNode": 2, "fromPort": "body", "toNode": 3, "toPort": "body_0"},
            {"fromNode": 3, "fromPort": "pose_0", "toNode": 4, "toPort": "pose"},
            {"fromNode": 5, "fromPort": "out", "toNode": 6, "toPort": "in"}
        ]
    });
    PresetRuntime::from_json_str(&def.to_string(), &registry).unwrap()
}

/// Plays to [`UNTIL`], jumps to `target`, holds the landed frame for
/// `paused` more frames, then plays to `end`, a frame at every rate. Speed
/// follows `case`'s project: 1 throughout for [`Case::InGraphSteps`] (no
/// lane), the rising lane for [`Case::Lane`]. Returns the body's last pose,
/// its bits.
fn rigid_jump(case: Case, fps: u32, offline: bool, target: f64, paused: u32, end: f64) -> [u32; 3] {
    let fps_f = f64::from(fps);
    let landed = (target * fps_f - 2e-7).ceil() as u32;
    let at = |frame: u32| f64::from(frame) / fps_f;
    let before = (0..=(UNTIL * fps_f).round() as u32).map(|frame| (at(frame), None));
    let jump = std::iter::once((at(landed), Some(target)));
    let held = (0..paused).map(|_| (at(landed), None));
    let after = (landed + 1..=(end * fps_f).round() as u32).map(|frame| (at(frame), None));
    let frames = before
        .chain(jump)
        .chain(held)
        .chain(after)
        .map(|(seconds, target)| Frame {
            seconds,
            target,
            bpm: BPM,
            tempo_shift: 0.0,
        });
    POSE.set(None);
    play_frames_on(rigid_runtime(), case, INTERVAL, fps, offline, frames);
    POSE.get()
        .expect("the world publishes its pose")
        .pos
        .map(f32::to_bits)
}

/// Every rate, live and exported, lands the reference's pose.
fn assert_same_pose(run: impl Fn(u32, bool) -> [u32; 3]) {
    let reference = run(60, true);
    assert!(
        f32::from_bits(reference[1]) < 49.0,
        "the body must fall: {reference:?}"
    );
    for fps in [30, 60, 120, 144] {
        for offline in [false, true] {
            assert_eq!(run(fps, offline), reference, "{fps} fps, offline {offline}");
        }
    }
}

#[test]
fn a_rigid_world_after_a_forward_seek_matches_at_30_60_120_144_fps_live_and_export() {
    assert_same_pose(|fps, offline| rigid_jump(Case::InGraphSteps, fps, offline, 0.9137, 0, 2.0));
}

#[test]
fn a_rigid_world_after_a_seek_back_matches_at_30_60_120_144_fps_live_and_export() {
    assert_same_pose(|fps, offline| rigid_jump(Case::InGraphSteps, fps, offline, 0.3137, 0, 1.5));
}

/// A placement while paused, two paused frames, then Play.
#[test]
fn a_rigid_world_after_a_paused_placement_then_play_matches_at_30_60_120_144_fps_live_and_export() {
    assert_same_pose(|fps, offline| rigid_jump(Case::InGraphSteps, fps, offline, 0.9137, 2, 2.0));
    assert_same_pose(|fps, offline| rigid_jump(Case::InGraphSteps, fps, offline, 0.3137, 2, 1.5));
}

/// Speed on a rising lane: every tick after a forward seek, a seek back, or
/// a paused placement runs at the lane's Speed at its own start, so the body
/// lands on the same bits at every rate.
#[test]
fn a_rigid_world_with_automated_speed_after_a_jump_matches_at_30_60_120_144_fps_live_and_export() {
    for (target, end) in [(0.9137, 2.0), (0.3137, 1.5)] {
        for paused in [0, 2] {
            assert_same_pose(|fps, offline| {
                rigid_jump(Case::Lane, fps, offline, target, paused, end)
            });
        }
    }
}
