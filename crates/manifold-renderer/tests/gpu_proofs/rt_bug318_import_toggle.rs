//! BUG-318 repro through the REAL import path — the app-faithful version of
//! `rt_t2b_temporal_wiring::live_rt_toggle_with_scene_object_does_not_dangle_mesh_slots`
//! (which did NOT reproduce Peter's break): `assemble_import_graph` on a real
//! GLB (owned variadic port names, per-object scene_object chains, installed
//! mesh buffers), rendered through `PresetRuntime`, then `rt_enabled` toggled
//! LIVE. Peter's symptom: every post-toggle frame magenta-clears with
//! "object_0: missing required `vertices` input".

use manifold_gpu::GpuTextureFormat;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::gltf_import::assemble_import_graph;
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;

use crate::harness;

const W: u32 = harness::PARITY_WIDTH;
const H: u32 = harness::PARITY_HEIGHT;

fn ctx(frame_count: i64) -> PresetContext {
    PresetContext {
        time: frame_count as f64 / 60.0,
        beat: 0.0,
        dt: 1.0 / 60.0,
        width: W,
        height: H,
        output_width: W,
        output_height: H,
        aspect: W as f32 / H as f32,
        owner_key: 7,
        is_clip_level: false,
        frame_count,
        anim_progress: 0.0,
        trigger_count: 0,
    }
}

fn frame(runtime: &mut PresetRuntime, h: &harness::ParityHarness, target: &manifold_gpu::GpuTexture, f: i64) {
    let c = ctx(f);
    let mut enc = h.device.create_encoder("bug318-import-frame");
    {
        let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
        runtime.render(&mut gpu, target, &c, &manifold_core::params::ParamManifest::default());
    }
    enc.commit_and_wait_completed();
}

fn magenta_fraction(px: &[f32]) -> f32 {
    let mut magenta = 0usize;
    let n = px.len() / 4;
    for i in 0..n {
        let (r, g, b) = (px[i * 4], px[i * 4 + 1], px[i * 4 + 2]);
        if r > 0.9 && g < 0.1 && b > 0.9 {
            magenta += 1;
        }
    }
    magenta as f32 / n as f32
}

#[test]
fn live_rt_toggle_on_imported_glb_scene_never_magenta_clears() {
    let h = harness::shared();
    let glb = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/gltf/hostile/mixamo_like.glb");
    assert!(glb.exists(), "fixture missing: {glb:?}");
    let (mut def, report) = assemble_import_graph(&glb).expect("import must succeed");
    eprintln!("[bug318] import report: {report:?}");
    // Peter's project plausibly SAVED rt_enabled=true — make the build-time
    // state RT-on so the live toggle exercises the consumed-set SHRINK
    // direction too (forced depth/velocity dropped, then re-added).
    {
        use manifold_core::effect_graph_def::SerializedParamValue;
        let n = def
            .nodes
            .iter_mut()
            .find(|n| n.type_id == "node.render_scene")
            .expect("imported def has render_scene");
        n.params.insert("rt_enabled".into(), SerializedParamValue::Bool { value: true });
    }

    let registry = PrimitiveRegistry::with_builtin();
    let mut runtime = PresetRuntime::from_def_with_device(
        def,
        &registry,
        std::sync::Arc::clone(&h.device),
        W,
        H,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .expect("imported def must build a runtime");

    let target = h.make_target("bug318-import");
    for f in 0..4 {
        frame(&mut runtime, h, &target.texture, f);
    }
    let before = crate::rt_t2b_temporal_wiring::readback_rgba_f32(&h.device, &target.texture);
    assert!(
        magenta_fraction(&before) < 0.5,
        "scene must render sanely BEFORE the toggle (magenta fraction {})",
        magenta_fraction(&before)
    );

    let scene_node = runtime
        .graph
        .nodes()
        .find(|n| n.params.get("rt_enabled").is_some())
        .map(|n| n.id)
        .expect("imported scene contains node.render_scene");
    // Peter's actual app state: temporal_upscale was toggled on FIRST (the
    // MetalFX scaler-creation log precedes the break), THEN rt_enabled.
    // Two live recompiles, the second on a plan whose depth/velocity dims
    // are already canvas-scaled to 2/3.
    runtime
        .graph
        .set_param(
            scene_node,
            "temporal_upscale",
            manifold_renderer::node_graph::ParamValue::Bool(true),
        )
        .expect("temporal_upscale exists");
    for f in 4..8 {
        frame(&mut runtime, h, &target.texture, f);
    }
    runtime
        .graph
        .set_param(
            scene_node,
            "rt_enabled",
            manifold_renderer::node_graph::ParamValue::Bool(false),
        )
        .expect("rt_enabled exists");
    for f in 8..12 {
        frame(&mut runtime, h, &target.texture, f);
    }
    runtime
        .graph
        .set_param(
            scene_node,
            "rt_enabled",
            manifold_renderer::node_graph::ParamValue::Bool(true),
        )
        .expect("rt_enabled exists");
    for f in 12..18 {
        frame(&mut runtime, h, &target.texture, f);
    }
    let after = crate::rt_t2b_temporal_wiring::readback_rgba_f32(&h.device, &target.texture);
    let frac = magenta_fraction(&after);
    assert!(
        frac < 0.5,
        "BUG-318: post-toggle frames are the magenta error fallback (fraction {frac}) — dangling mesh/vertices bindings after plan recompile"
    );
}
