//! SCENE_LOOP_DESIGN.md INV-3: wrap purity — frame at phase 0 == frame at phase 1.
//!
//! Renders a minimal scene loop graph at beat=0 (phase 0) and beat=8 (phase
//! wraps to 0 via `.fract()`) via `render_viewport_frame`, then asserts
//! pixel-identical output (max abs diff == 0). A red result means a
//! non-loop-phased driver snuck in.
//!
//! **Gate protocol (P1 brief):** this test MUST be shown red first against
//! a deliberately non-phase-locked camera (orbit_camera vs loop_camera at
//! the same beat), then green against the real loop_camera.

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

/// Build a minimal loop scene graph with a visible cube:
///   system.generator_input (boundary)
///   beat_ramp (rate=0.125, attack=1) → loop_camera.phase
///   loop_camera → render_scene.camera
///   cube_mesh → scene_object.vertices
///   unlit_material → scene_object.material
///   scene_array → scene_object.instances
///   scene_object → render_scene.object_0
///   render_scene.color → system.final_output.in
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

    let mut params_scene = BTreeMap::new();
    params_scene.insert("objects".to_string(), SerializedParamValue::Float { value: 1.0 });
    params_scene.insert("lights".to_string(), SerializedParamValue::Float { value: 0.0 });

    let mut params_mat = BTreeMap::new();
    params_mat.insert("color_r".to_string(), SerializedParamValue::Float { value: 0.8 });
    params_mat.insert("color_g".to_string(), SerializedParamValue::Float { value: 0.3 });
    params_mat.insert("color_b".to_string(), SerializedParamValue::Float { value: 0.3 });

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
            node(0, "input", "system.generator_input", BTreeMap::new()),
            node(1, "loop_phase", "node.beat_ramp", params_phase),
            node(2, "scene_array", "node.scene_array", params_array),
            node(3, "loop_camera", "node.loop_camera", params_camera),
            node(4, "cube_mesh", "node.cube_mesh", BTreeMap::new()),
            node(5, "mat", "node.unlit_material", params_mat),
            node(6, "scene_object", "node.scene_object", BTreeMap::new()),
            node(7, "scene", "node.render_scene", params_scene),
            node(8, "out", "system.final_output", BTreeMap::new()),
        ],
        wires: vec![
            wire(1, "out", 3, "phase"),
            wire(3, "out", 7, "camera"),
            wire(4, "vertices", 6, "vertices"),
            wire(5, "out", 6, "material"),
            wire(2, "out", 6, "instances"),
            wire(6, "object", 7, "object_0"),
            wire(7, "color", 8, "in"),
        ],
    }
}

/// P3 fog-driver graph: same as build_loop_graph but adds atmosphere +
/// scale_offset_value driver so fog_density oscillates ±20% over the loop.
/// cell_size = 10.0 → base_fog_density = 1/(1.5*10) ≈ 0.0667.
/// scale = 0.4 * base ≈ 0.0267, offset = 0.8 * base ≈ 0.0533.
fn build_loop_graph_with_fog() -> EffectGraphDef {
    let mut def = build_loop_graph();

    let cell_size = 10.0_f32;
    let base_fog_density = 1.0 / (1.5 * cell_size);
    let driver_scale = 0.4 * base_fog_density;
    let driver_offset = 0.8 * base_fog_density;

    let mut params_atmo = BTreeMap::new();
    params_atmo.insert(
        "fog_density".to_string(),
        SerializedParamValue::Float {
            value: base_fog_density,
        },
    );

    let mut params_driver = BTreeMap::new();
    params_driver.insert(
        "scale".to_string(),
        SerializedParamValue::Float {
            value: driver_scale,
        },
    );
    params_driver.insert(
        "offset".to_string(),
        SerializedParamValue::Float {
            value: driver_offset,
        },
    );

    // Node 9 = fog_driver (scale_offset_value), node 10 = loop_fog (atmosphere).
    def.nodes.push(node(
        9,
        "fog_driver",
        "node.scale_offset_value",
        params_driver,
    ));
    def.nodes.push(node(10, "loop_fog", "node.atmosphere", params_atmo));

    // beat_ramp.out → fog_driver.a (phase input)
    def.wires.push(wire(1, "out", 9, "a"));
    // fog_driver.out → loop_fog.fog_density (P3 wrap-pure driver)
    def.wires.push(wire(9, "out", 10, "fog_density"));
    // loop_fog.atmosphere → render_scene.atmosphere
    def.wires.push(wire(10, "atmosphere", 7, "atmosphere"));

    def
}

/// RED graph: same cube + scene_object, but `node.orbit_camera` (static,
/// non-looping) instead of loop_camera. Used to prove the scene renders
/// visible geometry and is camera-dependent — orbit_camera and loop_camera
/// at the same beat MUST produce different pixel output.
fn build_red_graph() -> EffectGraphDef {
    let mut params_orbit_cam = BTreeMap::new();
    params_orbit_cam.insert("orbit".to_string(), SerializedParamValue::Float { value: 0.7 });
    params_orbit_cam.insert("tilt".to_string(), SerializedParamValue::Float { value: 0.3 });
    params_orbit_cam.insert("distance".to_string(), SerializedParamValue::Float { value: 5.0 });
    params_orbit_cam.insert("fov_y".to_string(), SerializedParamValue::Float { value: 0.9 });

    let mut params_scene = BTreeMap::new();
    params_scene.insert("objects".to_string(), SerializedParamValue::Float { value: 1.0 });
    params_scene.insert("lights".to_string(), SerializedParamValue::Float { value: 0.0 });

    let mut params_mat = BTreeMap::new();
    params_mat.insert("color_r".to_string(), SerializedParamValue::Float { value: 0.8 });
    params_mat.insert("color_g".to_string(), SerializedParamValue::Float { value: 0.3 });
    params_mat.insert("color_b".to_string(), SerializedParamValue::Float { value: 0.3 });

    EffectGraphDef {
        version: 1,
        name: None,
        description: None,
        preset_metadata: Some(PresetMetadata {
            id: PresetTypeId::from_string("WrapParityRedTest".to_string()),
            display_name: "Wrap Parity Red Test".to_string(),
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
            node(0, "input", "system.generator_input", BTreeMap::new()),
            node(3, "cam", "node.orbit_camera", params_orbit_cam),
            node(4, "cube_mesh", "node.cube_mesh", BTreeMap::new()),
            node(5, "mat", "node.unlit_material", params_mat),
            node(6, "scene_object", "node.scene_object", BTreeMap::new()),
            node(7, "scene", "node.render_scene", params_scene),
            node(8, "out", "system.final_output", BTreeMap::new()),
        ],
        wires: vec![
            wire(3, "out", 7, "camera"),
            wire(4, "vertices", 6, "vertices"),
            wire(5, "out", 6, "material"),
            wire(6, "object", 7, "object_0"),
            wire(7, "color", 8, "in"),
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

/// RED-FIRST: orbit_camera (non-looping) must produce a DIFFERENT frame
/// than loop_camera at the same beat — proves the scene renders visible
/// geometry and is camera-dependent.
#[test]
fn red_phase_must_differ_with_non_locked_camera() {
    let red_def = build_red_graph();
    let green_def = build_loop_graph();

    let red_frame = render_frame(&red_def, 0.0);
    let green_frame = render_frame(&green_def, 0.0);

    let red_nonblack = red_frame.iter().filter(|&&v| v > 0).count();
    assert!(red_nonblack > 0, "orbit_camera scene should render visible pixels");

    let diff = max_pixel_diff(&red_frame, &green_frame);
    assert!(
        diff > 0,
        "RED gate failed: orbit_camera and loop_camera produced identical frames \
         (max diff = 0) — scene is not camera-dependent"
    );
}

/// GREEN: loop_camera at phase 0 vs phase 1 must be pixel-identical.
///
/// beat_ramp rate=0.125 means one cycle per 8 beats. beat=0 -> phase=0.
/// beat=8 -> phase = fract(8 * 0.125) = fract(1.0) = 0.0, so the loop
/// camera wraps to the same position. The `.fract()` in loop_camera's
/// phase reader handles the phase=1 -> 0 wrap (INV-3).
#[test]
fn wrap_parity_phase_0_vs_phase_1() {
    let def = build_loop_graph();

    let frame_a = render_frame(&def, 0.0);
    let frame_b = render_frame(&def, 8.0);

    let diff = max_pixel_diff(&frame_a, &frame_b);
    assert_eq!(
        diff, 0,
        "INV-3: wrap purity violated — phase 0 vs phase 1 (beat=8) max pixel diff = {diff}"
    );
}

/// P3: wrap parity MUST hold with the fog driver wired. The driver is
/// loop-phased (phase rides the same beat_ramp as the camera), so phase 0
/// and phase 8 (fract wraps back to 0) produce identical fog density and
/// identical pixels.
#[test]
fn wrap_parity_with_fog_driver() {
    let def = build_loop_graph_with_fog();

    let frame_a = render_frame(&def, 0.0);
    let frame_b = render_frame(&def, 8.0);

    let diff = max_pixel_diff(&frame_a, &frame_b);
    assert_eq!(
        diff, 0,
        "P3 INV-3: fog driver broke wrap purity — phase 0 vs phase 1 (beat=8) max pixel diff = {diff}"
    );
}

/// P3: numeric fog-swing assertion. The fog density driver maps loop phase
/// through scale_offset_value(out = phase * scale + offset) where
/// scale = 0.4 * base, offset = 0.8 * base. At phase 0.25 the driver
/// output is 0.9 * base; at phase 0.75 it is 1.1 * base. The difference
/// is 0.2 * base ≈ 0.0133 for cell_size=10. Different fog densities
/// produce different pixel output, so max_pixel_diff > 0 proves the swing
/// is live. beat=2 → phase=0.25, beat=6 → phase=0.75 (rate=0.125).
#[test]
fn fog_density_swings_over_loop() {
    let def = build_loop_graph_with_fog();

    // phase 0.25: beat=2, rate=0.125 → fract(2*0.125) = 0.25
    let frame_lo = render_frame(&def, 2.0);
    // phase 0.75: beat=6, rate=0.125 → fract(6*0.125) = 0.75
    let frame_hi = render_frame(&def, 6.0);

    let diff = max_pixel_diff(&frame_lo, &frame_hi);
    assert!(
        diff > 0,
        "P3 fog-swing assertion failed: phase 0.25 and 0.75 produced identical pixels \
         (max diff = 0) — fog density driver is not affecting the render"
    );
}
