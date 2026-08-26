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
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;

use crate::harness;

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
/// → motion_blur.velocity; motion_blur.out → final.
fn scene_json(shutter_angle: f32) -> String {
    let size = quad_size();
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
            "max_blur_px":{{"type":"Float","value":32.0}}}}}},
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
        {{"fromNode":30,"fromPort":"out","toNode":99,"toPort":"in"}}
        ]}}"#
    )
}

/// Render the measured frame: warm up at beat 0 (pos_y = 0, no motion),
/// then render at `beat` and read back the final output as f32 RGBA.
/// When `beat` is 0.0 the measured frame has no motion either (the
/// static control); at `BEAT_MOVED` the quad jumps and the frame carries
/// real velocity.
fn render_frame(json: &str, beat: f64, label: &str) -> Vec<f32> {
    let h = harness::shared();
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
    let mut pixels = vec![0.0f32; (h.width * h.height * 4) as usize];
    for (frame_count, b) in [(0i64, 0.0f64), (1, 0.0), (2, beat)] {
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
        harness::retry_on_gpu_commit_error(|| {
            let mut enc = h.device.create_encoder("motion-blur-visibility-enc");
            {
                let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
                runtime.render(
                    &mut gpu,
                    &target.texture,
                    &ctx,
                    &manifold_core::params::ParamManifest::default(),
                );
            }
            enc.commit_and_wait_completed();
        });
    }
    let bytes = harness::retry_on_gpu_commit_error(|| h.readback(&target.texture));
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

/// The latent fused-route suspect: a fused region containing motion_blur
/// resolves its shutter via the `camera_ext_N` external; an unresolved one
/// zero-fills the derived block — the exact-no-op failure. If the freeze
/// compiler refuses this chain, say so and skip (the import tail's
/// assembled-graph tests in CINEMATIC_SCENE_TAIL P1 carry the fused
/// coverage for the shipped topology).
#[test]
fn motion_blur_output_differs_under_motion_fused_route() {
    let def_on: manifold_core::effect_graph_def::EffectGraphDef =
        serde_json::from_str(&scene_json(SHUTTER)).expect("shutter=180 def parses");
    let def_off: manifold_core::effect_graph_def::EffectGraphDef =
        serde_json::from_str(&scene_json(0.0)).expect("shutter=0 def parses");
    let fused_on = manifold_renderer::node_graph::freeze::install::fused_generator_def_for(&def_on);
    let fused_off = manifold_renderer::node_graph::freeze::install::fused_generator_def_for(&def_off);
    if fused_on.is_none() || fused_off.is_none() {
        eprintln!(
            "motion_blur fused-route: freeze compiler refused the chain \
             (fused_on={}, fused_off={}) — fused assertion skipped",
            fused_on.is_some(),
            fused_off.is_some()
        );
        return;
    }
    let (fused_on, fused_off) = (fused_on.unwrap(), fused_off.unwrap());
    let json_on = serde_json::to_string(&*fused_on).expect("fused def serializes");
    let json_off = serde_json::to_string(&*fused_off).expect("fused def serializes");
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
