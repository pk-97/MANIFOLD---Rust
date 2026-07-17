//! SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md C-P1c: render-level proof for
//! BUG-237 ("Camera/World/Light params do nothing" — Peter's live report).
//! The dispatch-level half is proven by
//! `inspector.rs::scene_card_convergence_tests::light_intensity_commit_writes_the_layer_instance_def_at_root_scope`
//! / `camera_orbit_commit_writes_the_layer_instance_def_at_root_scope` (a
//! real card-row `ParamSnapshot`/`ParamChanged`/`ParamCommit` sequence
//! writes the committed value into `layer.generator_graph()`'s
//! `EffectGraphDef`, at the light/camera node's OWN `node.params` entry,
//! root-scoped). What THIS proves is the other half of BUG-237's own
//! hypothesis space: that a def mutated EXACTLY that way — a plain
//! `node.params` float overwrite on the SceneStarter preset, the same
//! shape `SetGraphNodeParamCommand::execute` produces — actually renders
//! DIFFERENT pixels through the real `PresetRuntime`. Together the two
//! halves close BUG-237: card row → command → def (proven in
//! `manifold-app`) → def → pixels (proven here). If this test had failed,
//! the content-thread re-render path (BUG-237's hypothesis (c)) would be
//! the confirmed root cause instead.

use half::f16;
use manifold_core::effect_graph_def::{EffectGraphDef, SerializedParamValue};
use manifold_gpu::GpuTextureFormat;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;

use crate::harness;

fn render_def(def: &EffectGraphDef) -> (Vec<u8>, u32, u32) {
    let json = serde_json::to_string(def).expect("def must serialize");
    let h = harness::shared();
    let registry = PrimitiveRegistry::with_builtin();
    let mut runtime = PresetRuntime::from_json_str_with_device(
        &json,
        &registry,
        std::sync::Arc::clone(&h.device),
        h.width,
        h.height,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .expect("mutated SceneStarter def must build");

    let target = h.make_target("bug237-light-camera-commit-render-proof");
    for frame in 0..2 {
        let ctx = PresetContext {
            time: 0.1,
            beat: 0.2,
            dt: 1.0 / 60.0,
            width: h.width,
            height: h.height,
            output_width: h.width,
            output_height: h.height,
            aspect: h.width as f32 / h.height as f32,
            owner_key: 0,
            is_clip_level: false,
            frame_count: frame,
            anim_progress: 0.0,
            trigger_count: 0,
        };
        let mut enc = h.device.create_encoder("bug237-light-camera-commit-render-proof-enc");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
            runtime.render(&mut gpu, &target.texture, &ctx, &manifold_core::params::ParamManifest::default());
        }
        enc.commit_and_wait_completed();
    }
    (h.readback(&target.texture), h.width, h.height)
}

fn write_png(bytes: &[u8], w: u32, h: u32, path: &str) {
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for px in bytes.chunks_exact(8) {
        for c in 0..4 {
            let v = f16::from_le_bytes([px[c * 2], px[c * 2 + 1]]).to_f32();
            let mapped = (v / (1.0 + v)).clamp(0.0, 1.0);
            out.push((mapped.powf(1.0 / 2.2) * 255.0).round() as u8);
        }
    }
    image::save_buffer(path, &out, w, h, image::ExtendedColorType::Rgba8)
        .unwrap_or_else(|e| panic!("write {path}: {e}"));
}

fn mean_abs_diff(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len());
    let tonemap = |bytes: &[u8]| -> Vec<f32> {
        bytes
            .chunks_exact(8)
            .flat_map(|px| {
                (0..4).map(move |c| {
                    let v = f16::from_le_bytes([px[c * 2], px[c * 2 + 1]]).to_f32();
                    (v / (1.0 + v)).clamp(0.0, 1.0)
                })
            })
            .collect()
    };
    let ta = tonemap(a);
    let tb = tonemap(b);
    let sum: f64 = ta.iter().zip(&tb).map(|(x, y)| (x - y).abs() as f64).sum();
    sum / ta.len() as f64
}

fn scene_starter_def() -> EffectGraphDef {
    let json = include_str!("../../assets/generator-presets/SceneStarter.json");
    serde_json::from_str(json).expect("SceneStarter.json must parse")
}

/// BUG-237, Light half: node 4 (the "Sun" light, `mode: 0`) at its default
/// intensity 1.0 vs. a `ParamCommit`-shaped mutation to 8.0 — the same
/// `node.params.insert("intensity", Float(..))` write
/// `SetGraphNodeParamCommand::execute` performs on the identical
/// `EffectGraphDef` structure.
#[test]
fn sun_intensity_commit_visibly_changes_the_render() {
    let baseline = scene_starter_def();
    let mut bright = baseline.clone();
    let light = bright.nodes.iter_mut().find(|n| n.id == 4).expect("SceneStarter node 4 must be the Sun light");
    assert_eq!(light.type_id, "node.light", "node 4 must be a light");
    light.params.insert("intensity".to_string(), SerializedParamValue::Float { value: 8.0 });

    let (before, w, h) = render_def(&baseline);
    let (after, w2, h2) = render_def(&bright);
    assert_eq!((w, h), (w2, h2));

    write_png(&before, w, h, "/tmp/bug237_sun_intensity_before.png");
    write_png(&after, w, h, "/tmp/bug237_sun_intensity_after.png");

    let diff = mean_abs_diff(&before, &after);
    eprintln!("BUG-237 sun-intensity commit mean_abs_diff = {diff:.6}");
    assert!(
        diff > 0.01,
        "a def mutated exactly as SetGraphNodeParamCommand would (light intensity 1.0 -> 8.0) \
         must render visibly different pixels through the real PresetRuntime — got mean_abs_diff={diff:.6}. \
         If this fails, BUG-237's content-thread re-render hypothesis is the confirmed root cause, \
         not the card-row dispatch path (which the inspector.rs dispatch-level tests already prove correct)."
    );
}

/// BUG-237, Camera half: node 1 (`node.orbit_camera`) at its default
/// `orbit: 0.7` vs. a `ParamCommit`-shaped mutation to `0.7 + PI/2` — a
/// quarter-turn around the scene, framing a visibly different view.
#[test]
fn camera_orbit_commit_visibly_changes_the_framing() {
    let baseline = scene_starter_def();
    let mut orbited = baseline.clone();
    let cam = orbited.nodes.iter_mut().find(|n| n.id == 1).expect("SceneStarter node 1 must be the orbit camera");
    assert_eq!(cam.type_id, "node.orbit_camera", "node 1 must be the orbit camera");
    let base_orbit = match cam.params.get("orbit") {
        Some(SerializedParamValue::Float { value }) => *value,
        other => panic!("expected a Float orbit param, got {other:?}"),
    };
    cam.params.insert(
        "orbit".to_string(),
        SerializedParamValue::Float { value: base_orbit + std::f32::consts::FRAC_PI_2 },
    );

    let (before, w, h) = render_def(&baseline);
    let (after, w2, h2) = render_def(&orbited);
    assert_eq!((w, h), (w2, h2));

    write_png(&before, w, h, "/tmp/bug237_camera_orbit_before.png");
    write_png(&after, w, h, "/tmp/bug237_camera_orbit_after.png");

    let diff = mean_abs_diff(&before, &after);
    eprintln!("BUG-237 camera-orbit commit mean_abs_diff = {diff:.6}");
    assert!(
        diff > 0.01,
        "a def mutated exactly as SetGraphNodeParamCommand would (camera orbit + pi/2, a quarter-turn) \
         must render a visibly different framing through the real PresetRuntime — got mean_abs_diff={diff:.6}. \
         If this fails, BUG-237's content-thread re-render hypothesis is the confirmed root cause."
    );
}
