//! BUG-136 (motion blur no visible effect) / CINEMATIC_SCENE_TAIL P0 —
//! the output-diff the 2026-07-13 probe session never ran: it verified
//! `node.motion_blur`'s INPUTS (velocity nonzero, shutter at the atom) and
//! stopped. This test convicts or clears the atom end-to-end by rendering
//! the shipping kernel through a real graph and diffing its OUTPUT.
//!
//! Fixture: the `gbuffer_velocity.rs` shape (grid quad, static camera,
//! `beat_ramp` driving `transform_3d.pos_y` — a rigid-object velocity
//! source with an exactly-known moving frame), plus `node.camera_lens`
//! (shutter_angle = 180) feeding BOTH `render_scene.camera` and
//! `motion_blur.camera`, and `render_scene.color/velocity` feeding
//! `motion_blur.in/.velocity`. `motion_blur.out` is the sole final output.
//!
//! Assertions, per route (raw def, and the fused view when the freeze
//! compiler accepts the chain):
//! - moving frame, shutter=180 vs shutter=0: outputs must differ
//!   materially (the smear actually happens — a silent zero anywhere in
//!   the shutter chain fails this, which is exactly BUG-136's shape).
//! - static frame, shutter=180 vs shutter=0: outputs must agree
//!   (no motion, no blur — the difference above can't be chalked up to
//!   the shutter term perturbing anything else).
//!
//! Continuity: `render_scene`'s `prev_model`/`prev_view_proj` lives on the
//! node instance, so each (shutter, route) pair renders warm-up frames at
//! beat 0 before the measured beat — same `PresetRuntime` throughout.

use half::f16;
use manifold_gpu::GpuTextureFormat;
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_node_engine::persistence::PrimitiveRegistry;
use manifold_node_engine::runtime::preset_context::PresetContext;
use manifold_node_engine::runtime::PresetRuntime;


const DISTANCE: f32 = 5.0;
const FOV_Y: f32 = 0.9;
const NEAR: f32 = 0.05;
const FAR: f32 = 200.0;
const ROT_Z: f32 = std::f32::consts::FRAC_PI_2;
/// Big on purpose (unlike `gbuffer_velocity`'s 0.02): the smear must span
/// multiple pixels at the harness's 128px canvas. `pos_y` 0 → 0.5 gives an
/// NDC delta ~0.2 → smear ≈ 0.2 * 0.5 * 128 * (180/360) ≈ 6 px.
const POS_Y_MOVED: f32 = 0.5;
const SHUTTER: f32 = 180.0;

/// `pos_y` jumps from 0 to `POS_Y_MOVED` at this beat (`beat_ramp`
/// rate=1 attack=1 emits `fract(beat)`, so beat 0.5 → 0.5).
const BEAT_MOVED: f64 = POS_Y_MOVED as f64;

fn quad_size() -> f32 {
    0.1 * DISTANCE
}

/// grid → tris → render_scene; orbit_camera → camera_lens → scene.camera
/// and → motion_blur.camera; scene.color → motion_blur.in; scene.velocity
/// → motion_blur.velocity; motion_blur.out → invert → final.
fn scene_json(shutter_angle: f32) -> String {
    scene_json_with_enabled(shutter_angle, true)
}

fn scene_json_with_enabled(shutter_angle: f32, enabled: bool) -> String {
    let size = quad_size();
    let enabled_value = if enabled { 1.0 } else { 0.0 };
    format!(
        r#"{{"version":2,"name":"MotionBlurVisibility","nodes":[
        {{"id":0,"typeId":"system.generator_input","nodeId":"input"}},
        {{"id":1,"typeId":"node.grid_mesh","nodeId":"grid","params":{{
            "max_capacity":{{"type":"Int","value":16}},
            "resolution_x":{{"type":"Int","value":2}},
            "resolution_y":{{"type":"Int","value":2}},
            "size_x":{{"type":"Float","value":{size}}},
            "size_y":{{"type":"Float","value":{size}}}}}}},
        {{"id":2,"typeId":"node.make_triangles","nodeId":"tris","params":{{
            "src_cols":{{"type":"Int","value":2}},
            "src_rows":{{"type":"Int","value":2}}}}}},
        {{"id":3,"typeId":"node.orbit_camera","nodeId":"cam","params":{{
            "orbit":{{"type":"Float","value":0.0}},
            "tilt":{{"type":"Float","value":0.0}},
            "distance":{{"type":"Float","value":{DISTANCE}}},
            "fov_y":{{"type":"Float","value":{FOV_Y}}},
            "look_y":{{"type":"Float","value":0.0}},
            "roll":{{"type":"Float","value":0.0}},
            "near":{{"type":"Float","value":{NEAR}}},
            "far":{{"type":"Float","value":{FAR}}}}}}},
        {{"id":7,"typeId":"node.camera_lens","nodeId":"lens","params":{{
            "focus_distance":{{"type":"Float","value":5.0}},
            "f_stop":{{"type":"Float","value":1000.0}},
            "shutter_angle":{{"type":"Float","value":{shutter_angle}}},
            "exposure_ev":{{"type":"Float","value":0.0}}}}}},
        {{"id":4,"typeId":"node.unlit_material","nodeId":"mat","params":{{
            "color_r":{{"type":"Float","value":1.0}},
            "color_g":{{"type":"Float","value":1.0}},
            "color_b":{{"type":"Float","value":1.0}},
            "color_a":{{"type":"Float","value":1.0}}}}}},
        {{"id":5,"typeId":"node.transform_3d","nodeId":"xf","params":{{
            "rot_z":{{"type":"Float","value":{ROT_Z}}}}}}},
        {{"id":6,"typeId":"node.beat_ramp","nodeId":"ramp","params":{{
            "rate":{{"type":"Float","value":1.0}},
            "attack":{{"type":"Float","value":1.0}}}}}},
        {{"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{{
            "objects":{{"type":"Int","value":1}},
            "lights":{{"type":"Int","value":0}}}}}},
        {{"id":30,"typeId":"node.motion_blur","nodeId":"mb","params":{{
            "max_blur_px":{{"type":"Float","value":32.0}},
            "enabled":{{"type":"Bool","value":{enabled}}}}}}},
        {{"id":31,"typeId":"node.invert","nodeId":"mb_sink","params":{{}}}},
        {{"id":99,"typeId":"system.final_output","nodeId":"color_out"}}
        ],"wires":[
        {{"fromNode":1,"fromPort":"vertices","toNode":2,"toPort":"in"}},
        {{"fromNode":2,"fromPort":"out","toNode":20,"toPort":"mesh_0"}},
        {{"fromNode":3,"fromPort":"out","toNode":7,"toPort":"camera"}},
        {{"fromNode":7,"fromPort":"out","toNode":20,"toPort":"camera"}},
        {{"fromNode":7,"fromPort":"out","toNode":30,"toPort":"camera"}},
        {{"fromNode":4,"fromPort":"out","toNode":20,"toPort":"material_0"}},
        {{"fromNode":6,"fromPort":"out","toNode":5,"toPort":"pos_y"}},
        {{"fromNode":5,"fromPort":"transform","toNode":20,"toPort":"transform_0"}},
        {{"fromNode":20,"fromPort":"color","toNode":30,"toPort":"in"}},
        {{"fromNode":20,"fromPort":"velocity","toNode":30,"toPort":"velocity"}},
        {{"fromNode":30,"fromPort":"out","toNode":31,"toPort":"in"}},
        {{"fromNode":31,"fromPort":"out","toNode":99,"toPort":"in"}}
        ],"presetMetadata":{{
            "params":[{{
                "id":"mb_enabled", "name":"Enabled", "min":0.0,
                "max":1.0, "defaultValue":{enabled_value}, "isToggle":true
            }}],
            "bindings":[{{
                "id":"mb_enabled", "label":"Enabled",
                "defaultValue":{enabled_value}, "userAdded":true,
                "target":{{"kind":"node","nodeId":"mb","param":"enabled"}},
                "convert":{{"type":"BoolThreshold"}}
            }}]
        }}}}}}"#
    )
}

/// Render the measured frame: warm up at beat 0 (pos_y = 0, no motion),
/// then render at `beat` and read back the final output as f32 RGBA.
/// When `beat` is 0.0 the measured frame has no motion either (the
/// static control); at `BEAT_MOVED` the quad jumps and the frame carries
/// real velocity.
fn render_frame(json: &str, beat: f64, label: &str) -> Vec<f32> {
    render_frame_with_enabled_values(json, beat, label, None)
}

fn render_frame_with_enabled_values(
    json: &str,
    beat: f64,
    label: &str,
    enabled_values: Option<[f32; 3]>,
) -> Vec<f32> {
    let h = manifold_node_engine::testkit::gpu_harness::shared();
    let registry = PrimitiveRegistry::with_builtin();
    let mut runtime = PresetRuntime::from_json_str_with_device(
        json,
        &registry,
        std::sync::Arc::clone(&h.device),
        h.width,
        h.height,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .unwrap_or_else(|e| panic!("{label}: graph must build: {e}\n{json}"));

    let target = h.make_target(label);
    let empty_manifest = manifold_core::params::ParamManifest::default();
    let enabled_manifests = enabled_values.map(|values| {
        values
            .into_iter()
            .map(|value| {
                let spec = manifold_core::effect_graph_def::ParamSpecDef {
                    id: "mb_enabled".into(),
                    name: "Enabled".into(),
                    min: 0.0,
                    max: 1.0,
                    default_value: value,
                    is_toggle: true,
                    ..Default::default()
                };
                let mut param = manifold_core::params::Param::bundled(spec);
                param.value = value;
                manifold_core::params::ParamManifest::from_params(vec![param])
            })
            .collect::<Vec<_>>()
    });
    let mut pixels = vec![0.0f32; (h.width * h.height * 4) as usize];
    for (frame_index, (frame_count, b)) in [(0i64, 0.0f64), (1, 0.0), (2, beat)]
        .into_iter()
        .enumerate()
    {
        let ctx = PresetContext {
            time: 0.0,
            beat: b,
            dt: 1.0 / 60.0,
            width: h.width,
            height: h.height,
            output_width: h.width,
            output_height: h.height,
            aspect: h.width as f32 / h.height as f32,
            owner_key: 0,
            is_clip_level: false,
            frame_count,
            anim_progress: 0.0,
            trigger_count: 0,
        };
        manifold_node_engine::testkit::gpu_harness::retry_on_gpu_commit_error(|| {
            let mut enc = h.device.create_encoder("motion-blur-visibility-enc");
            {
                let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
                runtime.render(
                    &mut gpu,
                    &target.texture,
                    &ctx,
                    enabled_manifests
                        .as_ref()
                        .map_or(&empty_manifest, |manifests| &manifests[frame_index]),
                );
            }
            enc.commit_and_wait_completed();
        });
    }
    let bytes = manifold_node_engine::testkit::gpu_harness::retry_on_gpu_commit_error(|| h.readback(&target.texture));
    for (i, px) in bytes.chunks_exact(8).enumerate() {
        for c in 0..4 {
            pixels[i * 4 + c] = f16::from_le_bytes([px[c * 2], px[c * 2 + 1]]).to_f32();
        }
    }
    pixels
}

struct Diff {
    max: f32,
    count_above: usize,
}

fn diff(a: &[f32], b: &[f32]) -> Diff {
    assert_eq!(a.len(), b.len());
    let mut max = 0.0f32;
    let mut count_above = 0usize;
    for (x, y) in a.iter().zip(b.iter()) {
        let d = (x - y).abs();
        if d > max {
            max = d;
        }
        if d > 0.01 {
            count_above += 1;
        }
    }
    Diff { max, count_above }
}

fn fused_scene_json(shutter_angle: f32, enabled: bool) -> String {
    let def: manifold_core::effect_graph_def::EffectGraphDef =
        serde_json::from_str(&scene_json_with_enabled(shutter_angle, enabled))
            .expect("motion blur def parses");
    let fused = manifold_node_engine::freeze::install::fused_generator_view_for(&def)
        .expect("motion blur graph must fuse through its downstream pointwise node");
    assert!(
        fused
            .retarget
            .contains_key(&("mb".to_owned(), "enabled".to_owned())),
        "fused motion blur must retarget its enabled binding"
    );
    serde_json::to_string(&*fused.def).expect("fused motion blur def serializes")
}

/// The measured assertions for one route (raw or fused): blur must be
/// visible under motion with a 180° shutter, and absent with no motion.
fn assert_blur_visible_on_route(json_for: &dyn Fn(f32) -> String, route: &str) {
    let moved_on = render_frame(&json_for(SHUTTER), BEAT_MOVED, &format!("mb-{route}-moved-180"));
    let moved_off = render_frame(&json_for(0.0), BEAT_MOVED, &format!("mb-{route}-moved-0"));
    let d_moved = diff(&moved_on, &moved_off);
    assert!(
        d_moved.max > 0.05 && d_moved.count_above >= 32,
        "{route}: shutter=180 under motion must visibly smear vs shutter=0 — \
         max diff {:.5} over {} channel values above 0.01 (BUG-136's exact-no-op \
         shape is a zero here)",
        d_moved.max,
        d_moved.count_above
    );

    let static_on = render_frame(&json_for(SHUTTER), 0.0, &format!("mb-{route}-static-180"));
    let static_off = render_frame(&json_for(0.0), 0.0, &format!("mb-{route}-static-0"));
    let d_static = diff(&static_on, &static_off);
    assert!(
        d_static.max < 1e-3,
        "{route}: with no motion, shutter=180 must equal shutter=0 (taps collapse) — \
         max diff {:.5}; the moved-frame difference above must come from velocity, \
         not the shutter term alone",
        d_static.max
    );
}

#[test]
fn motion_blur_output_differs_under_motion_raw_route() {
    assert_blur_visible_on_route(&scene_json, "raw");
}

#[test]
fn motion_blur_disabled_bool_is_passthrough_raw_and_fused() {
    // Start enabled for the warm-up frames, then toggle the BoolThreshold
    // binding off for the measured moving frame.
    let toggle_off = [1.0, 1.0, 0.0];
    let raw_on = render_frame_with_enabled_values(
        &scene_json_with_enabled(SHUTTER, false),
        BEAT_MOVED,
        "mb-raw-disabled-180",
        Some(toggle_off),
    );
    let raw_off = render_frame_with_enabled_values(
        &scene_json_with_enabled(0.0, false),
        BEAT_MOVED,
        "mb-raw-disabled-0",
        Some(toggle_off),
    );
    let raw_diff = diff(&raw_on, &raw_off);
    assert!(
        raw_diff.max < 1e-3,
        "raw disabled motion blur must ignore shutter: max diff {:.5}",
        raw_diff.max
    );

    let fused_on = render_frame_with_enabled_values(
        &fused_scene_json(SHUTTER, false),
        BEAT_MOVED,
        "mb-fused-disabled-180",
        Some(toggle_off),
    );
    let fused_off = render_frame_with_enabled_values(
        &fused_scene_json(0.0, false),
        BEAT_MOVED,
        "mb-fused-disabled-0",
        Some(toggle_off),
    );
    let fused_diff = diff(&fused_on, &fused_off);
    assert!(
        fused_diff.max < 1e-3,
        "fused disabled motion blur must ignore shutter: max diff {:.5}",
        fused_diff.max
    );
    let parity = diff(&fused_on, &raw_on);
    assert!(
        parity.max < 1e-3,
        "fused disabled motion blur must match the raw passthrough: max diff {:.5}",
        parity.max
    );
}

/// The fused region must retain motion_blur's camera external and retarget
/// its live enabled binding through the downstream pointwise node.
#[test]
fn motion_blur_output_differs_under_motion_fused_route() {
    let json_on = fused_scene_json(SHUTTER, true);
    let json_off = fused_scene_json(0.0, true);
    assert_blur_visible_on_route(
        &move |shutter: f32| {
            if shutter > 0.0 {
                json_on.clone()
            } else {
                json_off.clone()
            }
        },
        "fused",
    );
}
