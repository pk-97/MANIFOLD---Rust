//! SCENE_LOOP_DESIGN.md INV-3: wrap purity — frame at phase 0 == frame at phase 1.
//!
//! Renders a minimal scene loop graph at beat=0 (phase 0) and beat=7.99999
//! (phase ~1) via `render_viewport_frame`, then asserts pixel-identical
//! output (max abs diff == 0). A red result means a non-loop-phased driver
//! snuck in.
//!
//! **Gate protocol (P1 brief):** this test MUST be shown red first against
//! a deliberately non-phase-locked driver (e.g. swap loop_camera for a
//! plain orbit_camera), then green against the real loop_camera.

use std::collections::BTreeMap;

use manifold_core::effect_graph_def::{
    EffectGraphDef, EffectGraphNode, EffectGraphWire, PresetMetadata, SerializedParamValue,
};
use manifold_core::preset_type_id::PresetTypeId;
use manifold_renderer::node_graph::{PrimitiveRegistry, render_viewport_frame};
use manifold_renderer::preset_context::PresetContext;

fn node(
    id: u32,
    node_id: &str,
    type_id: &str,
    params: BTreeMap<String, SerializedParamValue>,
) -> EffectGraphNode {
    EffectGraphNode {
        id,
        node_id: manifold_core::NodeId::new(node_id),
        type_id: type_id.to_string(),
        handle: Some(node_id.to_string()),
        params,
        exposed_params: Default::default(),
        editor_pos: None,
        wgsl_source: None,
        title: None,
        output_formats: BTreeMap::new(),
        output_canvas_scales: BTreeMap::new(),
        group: None,
    }
}

fn wire(from_node: u32, from_port: &str, to_node: u32, to_port: &str) -> EffectGraphWire {
    EffectGraphWire {
        from_node,
        from_port: from_port.to_string(),
        to_node,
        to_port: to_port.to_string(),
    }
}

/// Build a minimal loop scene graph:
///   loop_phase (beat_ramp rate=0.125, attack=1) → loop_camera.phase
///   loop_camera → render_scene.camera
///   scene_array → render_scene.instances_0
fn build_loop_graph() -> EffectGraphDef {
    let mut params_phase = BTreeMap::new();
    params_phase.insert("rate".to_string(), SerializedParamValue::Float { value: 0.125 });
    params_phase.insert("attack".to_string(), SerializedParamValue::Float { value: 1.0 });

    let mut params_array = BTreeMap::new();
    params_array.insert("count".to_string(), SerializedParamValue::Float { value: 3.0 });
    params_array.insert("axis".to_string(), SerializedParamValue::Enum { value: 4 });
    params_array.insert("cell_size".to_string(), SerializedParamValue::Float { value: 10.0 });

    let mut params_camera = BTreeMap::new();
    params_camera.insert("cell_size".to_string(), SerializedParamValue::Float { value: 10.0 });
    params_camera.insert("axis".to_string(), SerializedParamValue::Enum { value: 4 });
    params_camera.insert("fov_y".to_string(), SerializedParamValue::Float { value: 0.9 });

    EffectGraphDef {
        version: 1,
        name: None,
        description: None,
        preset_metadata: Some(PresetMetadata {
            id: PresetTypeId::from_string("WrapParityTest".to_string()),
            display_name: "Wrap Parity Test".to_string(),
            category: "Test".to_string(),
            osc_prefix: "test".to_string(),
            legacy_discriminant: None,
            available: true,
            is_line_based: false,
            params: Vec::new(),
            bindings: Vec::new(),
            param_aliases: Vec::new(),
            value_aliases: Vec::new(),
            string_params: Vec::new(),
            string_bindings: Vec::new(),
            scene_bounds: None,
        }),
        nodes: vec![
            node(0, "render_scene", "node.render_scene", BTreeMap::new()),
            node(1, "loop_phase", "node.beat_ramp", params_phase),
            node(2, "scene_array", "node.scene_array", params_array),
            node(3, "loop_camera", "node.loop_camera", params_camera),
        ],
        wires: vec![
            wire(1, "out", 3, "phase"),
            wire(3, "out", 0, "camera"),
            wire(2, "out", 0, "instances_0"),
        ],
    }
}

/// Render one frame at the given beat value.
fn render_frame(def: &EffectGraphDef, beat: f64) -> Vec<u8> {
    let device = manifold_gpu::GpuDevice::new();
    let registry = PrimitiveRegistry::with_builtin();
    let width = 64u32;
    let height = 64u32;
    let ctx = PresetContext {
        time: beat * 0.5, // seconds = beat * 0.5 (120 BPM)
        beat,
        dt: 0.016,
        width,
        height,
        output_width: width,
        output_height: height,
        aspect: width as f32 / height as f32,
        owner_key: 0,
        is_clip_level: false,
        frame_count: 0,
        anim_progress: 0.0,
        trigger_count: 0,
    };
    let (rgba, _, _) = render_viewport_frame(
        def.clone(),
        &registry,
        std::sync::Arc::new(device),
        width,
        height,
        &ctx,
    )
    .expect("render_viewport_frame");
    rgba
}

/// Compute max absolute pixel difference between two RGBA8 buffers.
fn max_pixel_diff(a: &[u8], b: &[u8]) -> u8 {
    a.iter()
        .zip(b.iter())
        .map(|(x, y)| x.abs_diff(*y))
        .max()
        .unwrap_or(0)
}

#[test]
fn wrap_parity_phase_0_vs_phase_1() {
    let def = build_loop_graph();

    // beat_ramp rate=0.125 means one cycle per 8 beats.
    // beat=0 → phase=0.
    let frame_a = render_frame(&def, 0.0);
    // beat=7.99999 → (7.99999 * 0.125).fract() = 0.99999875 → near phase 1.
    let frame_b = render_frame(&def, 7.99999);

    let diff = max_pixel_diff(&frame_a, &frame_b);
    assert_eq!(
        diff, 0,
        "INV-3: wrap purity violated — phase 0 vs phase 0.99999 max pixel diff = {diff}"
    );
}
