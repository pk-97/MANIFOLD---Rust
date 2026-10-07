mod fused_tests {
use manifold_node_engine::exec::effect_node::{EffectNodeContext,ParamValues};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::water::primitives::whitewater_step::WhitewaterStep;
#[test]
fn whitewater_unpack_extent_matches_adapter_storage() {
    use manifold_node_engine::water::primitives::gpu_flip_preset::{render_def, with_whitewater_axes, WaterScene};
    use manifold_node_engine::water::liquid::extent::check_preset_extents;
    let packed = render_def(WaterScene::dam_break(64));
    let axes = with_whitewater_axes(packed.clone());
    assert_eq!(check_preset_extents(&packed, 64).unwrap().scene_bytes,
        check_preset_extents(&axes, 64).unwrap().scene_bytes,
        "the stage holds exactly the three adapter arrays it replaces");
}

fn face_refusal(packed: bool, axes: [bool; 3], tick: bool, phrase: &str) {
    use manifold_node_engine::{exec::effect_node::FrameTime, exec::backend::MockBackend, bindings::NodeInputs, bindings::NodeOutputs, bindings::Slot};
    use manifold_node_engine::water::primitives::gpu_flip_preset::{render_def, WaterScene};
    use manifold_node_engine::water::primitives::gpu_flip_preset::with_whitewater_axes;
    use manifold_node_engine::water::liquid::extent::{check_preset_extents, ExtentError};
    let backend = MockBackend::new();
    let mut inputs = Vec::new();
    if tick { inputs.push(("distance", Slot(0))); }
    if packed { inputs.push(("faces", Slot(1))); }
    for (axis, port) in ["face_u", "face_v", "face_w"].into_iter().enumerate() {
        if axes[axis] { inputs.push((port, Slot(axis as u32 + 2))); }
    }
    let (mut scalars, mut cameras, mut lights, mut materials, mut transforms, mut atmospheres, mut modes, mut objects) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let outputs = NodeOutputs::new(&[], &backend, &mut scalars, &mut cameras, &mut lights,
        &mut materials, &mut transforms, &mut atmospheres, &mut modes, &mut objects);
    let params = ParamValues::default();
    let mut errors = Vec::new();
    let time = FrameTime { beats: manifold_core::Beats(0.0), seconds: manifold_core::Seconds(0.0), delta: manifold_core::Seconds(0.0), frame_count: 0 };
    let mut ctx = EffectNodeContext::new(time, &params, NodeInputs::new(&inputs, &backend, &[]), outputs, None)
        .with_errors(&mut errors);
    WhitewaterStep::new().run(&mut ctx);
    assert!(!ctx.gpu_accessed, "refusal must precede any GPU access");
    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains(phrase), "{:?}", errors);

    let mut def = with_whitewater_axes(render_def(WaterScene::dam_break(16)));
    let whitewater = def.nodes.iter().find(|n| n.node_id.as_str() == "whitewater").unwrap().id;
    let step = def.nodes.iter().find(|n| n.node_id.as_str() == "step").unwrap().id;
    def.wires.retain(|w| {
        if w.to_node != whitewater { return true; }
        if !tick && w.to_port == "distance" { return false; }
        ["face_u", "face_v", "face_w"].iter().position(|p| *p == w.to_port).is_none_or(|axis| axes[axis])
    });
    // An unused adapter would itself be an illegal reader outside the
    // liquid region, masking the whitewater refusal under test.
    let unused: Vec<_> = def.nodes.iter().filter(|n| ["whitewater_face_u", "whitewater_face_v", "whitewater_face_w"].iter()
        .position(|name| *name == n.node_id.as_str()).is_some_and(|axis| !axes[axis])).map(|n| n.id).collect();
    def.nodes.retain(|n| !unused.contains(&n.id));
    def.wires.retain(|w| !unused.contains(&w.from_node) && !unused.contains(&w.to_node));
    if packed {
        def.wires.push(serde_json::from_value(serde_json::json!({"fromNode": step, "fromPort": "faces", "toNode": whitewater, "toPort": "faces"})).unwrap());
    }
    match check_preset_extents(&def, 16) {
        Err(ExtentError::Refused { node, reason }) => {
            assert!(node.contains("whitewater_step"), "{node}");
            assert_eq!(reason, errors[0]);
        }
        other => panic!("expected {phrase} through extent rule, got {other:?}"),
    }
}

#[test]
fn whitewater_refuses_two_face_sources() {
    for mask in 1..8 { face_refusal(true, std::array::from_fn(|a| mask & (1 << a) != 0), true, "not both"); }
}

#[test]
fn whitewater_refuses_no_face_source() {
    for tick in [false, true] { face_refusal(false, [false; 3], tick, "neither"); }
}

#[test]
fn whitewater_refuses_partial_axes() {
    for mask in 1..7 {
        let axes = std::array::from_fn(|a| mask & (1 << a) != 0);
        for tick in [false, true] { face_refusal(false, axes, tick, "partial axes"); }
        face_refusal(true, axes, true, "not both");
    }
}

#[test]
fn whitewater_legacy_refuses_packed_faces() {
    face_refusal(true, [false; 3], false, "legacy level-set interface");
}
}
