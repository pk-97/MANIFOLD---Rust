//! RAYTRACING_DESIGN.md §8.2 D22 (T2-B) gate — the live reduced-res render
//! path + MetalFX Temporal wiring inside `node.render_scene::evaluate()`
//! itself (as opposed to `rt_p4_metalfx_temporal.rs`, which exercises the
//! standalone `MetalFxTemporalUpscaler` unit in isolation with jitter held
//! at (0,0)). This file proves the WIRING: `temporal_upscale=true` on a
//! real `node.render_scene` node actually renders reduced-res and upscales,
//! `color` lands back at native res, `depth`/`velocity` stay at render res
//! (D22 point 2), and the shared `TemporalResetDetector` still drives a cut
//! correctly end-to-end.
//!
//! `RESET_EPSILON` here is deliberately looser than `rt_p4_metalfx_
//! temporal.rs`'s 0.02: this node's `jitter_frame_index` free-runs every
//! `evaluate()` call (unlike that file's tests, which hold jitter fixed at
//! (0,0) for exactly this reason), so the warmed-then-cut frame and the
//! cold-start frame land at different jitter phases — a small, expected,
//! reset-unrelated subpixel difference. The TIGHT reset proof already lives
//! in `rt_p4_metalfx_temporal.rs`; this file's job is the end-to-end wiring,
//! not re-proving the reset mechanism at unit-test tightness.

use manifold_gpu::GpuTextureFormat;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;

use crate::harness;

/// D22 point 1: `RT_TEMPORAL_RENDER_SCALE_NUM`/`_DEN` mirrored here as a
/// plain oracle (not `use`d from `render_scene.rs` — that fn is private to
/// the primitive module) so this test can independently compute the
/// expected render-res dims with the SAME truncating formula
/// `execution.rs::resolve_dims` and `render_scene.rs::scale_dim` share.
fn expected_render_dim(native: u32) -> u32 {
    (u64::from(native) * 2 / 3).max(1) as u32
}

/// Coarse epsilon for "upscales the real scene, not garbage" — same order
/// of magnitude as `rt_p4_metalfx_temporal.rs`'s `UPSCALE_COARSE_EPSILON`
/// (0.15), loosened slightly for the extra raster/shading variance a real
/// `node.render_scene` pass adds on top of the isolated-upscaler unit test.
const UPSCALE_COARSE_EPSILON: f32 = 0.2;

/// Looser than `rt_p4_metalfx_temporal.rs`'s `RESET_EPSILON` (0.02) for the
/// jitter-phase-drift reason in this file's module doc.
const CUT_RESET_EPSILON: f32 = 0.12;

// Deliberately the SAME fixed dims `harness::ParityHarness` uses
// (`PARITY_WIDTH`/`PARITY_HEIGHT`) — every render target in this file comes
// from `h.make_target()`, which is hardcoded to those dims; building the
// `PresetRuntime` at a different canvas size than the target it renders into
// is a graph/target dims mismatch, not a `render_scene` question (caught via
// a real GPU page fault during authoring — matching sizes here isn't
// optional).
const NATIVE_W: u32 = harness::PARITY_WIDTH;
const NATIVE_H: u32 = harness::PARITY_HEIGHT;

/// A single flat-lit quad, one light, camera head-on and static — simple
/// enough that both native and upscaled renders should closely agree, and
/// static enough that MetalFX's temporal history has nothing to reproject
/// (no camera/object motion), isolating the resolution/reconstruction
/// question this file's gates ask about from the motion-reprojection
/// question `rt_w0_gbuffer.rs`/`rt_t1b_vertex_normals.rs` already cover.
fn scene_json(temporal_upscale: bool) -> String {
    format!(
        r#"{{"version":2,"name":"RtT2bTemporalWiring","nodes":[
        {{"id":0,"typeId":"system.generator_input","nodeId":"input"}},
        {{"id":1,"typeId":"node.grid_mesh","nodeId":"grid","params":{{
            "max_capacity":{{"type":"Int","value":16}},
            "resolution_x":{{"type":"Int","value":2}},
            "resolution_y":{{"type":"Int","value":2}},
            "size_x":{{"type":"Float","value":1.6}},
            "size_y":{{"type":"Float","value":1.6}}}}}},
        {{"id":2,"typeId":"node.make_triangles","nodeId":"tris","params":{{
            "src_cols":{{"type":"Int","value":2}},
            "src_rows":{{"type":"Int","value":2}}}}}},
        {{"id":3,"typeId":"node.orbit_camera","nodeId":"cam","params":{{
            "orbit":{{"type":"Float","value":0.0}},
            "tilt":{{"type":"Float","value":0.0}},
            "distance":{{"type":"Float","value":4.0}},
            "fov_y":{{"type":"Float","value":0.9}}}}}},
        {{"id":4,"typeId":"node.unlit_material","nodeId":"mat","params":{{
            "color_r":{{"type":"Float","value":0.8}},
            "color_g":{{"type":"Float","value":0.4}},
            "color_b":{{"type":"Float","value":0.2}},
            "color_a":{{"type":"Float","value":1.0}}}}}},
        {{"id":5,"typeId":"node.transform_3d","nodeId":"xf0","params":{{
            "rot_z":{{"type":"Float","value":1.5707963267948966}}}}}},
        {{"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{{
            "objects":{{"type":"Int","value":1}},
            "lights":{{"type":"Int","value":0}},
            "temporal_upscale":{{"type":"Bool","value":{temporal_upscale}}}}}}},
        {{"id":99,"typeId":"system.final_output","nodeId":"out"}}
        ],"wires":[
        {{"fromNode":1,"fromPort":"vertices","toNode":2,"toPort":"in"}},
        {{"fromNode":2,"fromPort":"out","toNode":20,"toPort":"mesh_0"}},
        {{"fromNode":3,"fromPort":"out","toNode":20,"toPort":"camera"}},
        {{"fromNode":4,"fromPort":"out","toNode":20,"toPort":"material_0"}},
        {{"fromNode":5,"fromPort":"transform","toNode":20,"toPort":"transform_0"}},
        {{"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}}
        ]}}"#
    )
}

fn build_runtime(h: &harness::ParityHarness, temporal_upscale: bool) -> PresetRuntime {
    let registry = PrimitiveRegistry::with_builtin();
    let json = scene_json(temporal_upscale);
    let mut runtime = PresetRuntime::from_json_str_with_device(
        &json,
        &registry,
        std::sync::Arc::clone(&h.device),
        NATIVE_W,
        NATIVE_H,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .unwrap_or_else(|e| panic!("rt_t2b graph must build (temporal_upscale={temporal_upscale}): {e}\n{json}"));
    runtime.set_dump_all(true);
    runtime
}

fn ctx(owner_key: i64, frame_count: i64) -> PresetContext {
    PresetContext {
        time: 0.0,
        beat: 0.0,
        dt: 1.0 / 60.0,
        width: NATIVE_W,
        height: NATIVE_H,
        output_width: NATIVE_W,
        output_height: NATIVE_H,
        aspect: NATIVE_W as f32 / NATIVE_H as f32,
        owner_key,
        is_clip_level: false,
        frame_count,
        anim_progress: 0.0,
        trigger_count: 0,
    }
}

fn render_frame(runtime: &mut PresetRuntime, h: &harness::ParityHarness, target: &manifold_gpu::GpuTexture, owner_key: i64, frame_count: i64) {
    let c = ctx(owner_key, frame_count);
    let mut enc = h.device.create_encoder("rt-t2b-frame");
    {
        let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
        runtime.render(&mut gpu, target, &c, &manifold_core::params::ParamManifest::default());
    }
    enc.commit_and_wait_completed();
}

pub(crate) fn readback_rgba_f32(device: &manifold_gpu::GpuDevice, texture: &manifold_gpu::GpuTexture) -> Vec<f32> {
    use half::f16;
    let bytes_per_row = texture.width * 8; // Rgba16Float = 8 bytes/px
    let total_bytes = u64::from(texture.height * bytes_per_row);
    let buf = device.create_buffer_shared(total_bytes);
    let mut enc = device.create_encoder("rt-t2b-readback");
    enc.copy_texture_to_buffer(texture, &buf, texture.width, texture.height, bytes_per_row);
    enc.commit_and_wait_completed();
    let ptr = buf.mapped_ptr().expect("shared readback buffer must expose mapped pointer");
    let f16s: &[f16] =
        unsafe { std::slice::from_raw_parts(ptr.cast::<f16>(), (texture.width * texture.height * 4) as usize) };
    f16s.iter().map(|v| v.to_f32()).collect()
}

fn mean_abs_diff_rgb(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len());
    let mut sum = 0.0f32;
    let mut n = 0u32;
    for (i, (&av, &bv)) in a.iter().zip(b.iter()).enumerate() {
        if i % 4 == 3 {
            continue;
        }
        sum += (av - bv).abs();
        n += 1;
    }
    sum / n as f32
}

/// D22: `temporal_upscale=true`'s `color` graph output is EXACTLY native
/// (canvas) resolution, not "close to" — MetalFX Temporal's whole job is
/// landing back at native res, and `output_canvas_scale`'s D22 branch only
/// touches `depth`/`velocity`.
#[test]
fn temporal_upscale_color_output_is_exact_native_res() {
    let h = harness::shared();
    let mut runtime = build_runtime(h, true);
    let target = h.make_target("rt-t2b-native-target");
    render_frame(&mut runtime, h, &target.texture, 1, 0);

    let dumped = runtime.dump_textures_all();
    let color = dumped
        .iter()
        .find(|(node_id, port, _, _)| node_id == "scene" && port == "color")
        .unwrap_or_else(|| panic!("scene.color must be dumped"));
    assert_eq!(color.3.width, NATIVE_W, "temporal_upscale color output width must be exact native res");
    assert_eq!(color.3.height, NATIVE_H, "temporal_upscale color output height must be exact native res");
}

/// D22 point 2: `depth`/`velocity` stay at RENDER res when
/// `temporal_upscale` is on (MetalFX upscales color only; a bespoke
/// depth/velocity upscaler is FORBIDDEN) — checked against the SAME
/// truncating `native * 2 / 3` formula `render_scene.rs::scale_dim` and
/// `execution.rs::resolve_dims` both use.
#[test]
fn temporal_upscale_depth_velocity_outputs_stay_render_res() {
    let h = harness::shared();
    // rt_enabled off, temporal_upscale on: force_consumed_outputs still
    // forces depth/velocity (P4's `|| temporal_upscale` branch), proving
    // D22's render-res sizing fires independent of RT.
    let registry = PrimitiveRegistry::with_builtin();
    let json = scene_json(true);
    let mut runtime = PresetRuntime::from_json_str_with_device(
        &json,
        &registry,
        std::sync::Arc::clone(&h.device),
        NATIVE_W,
        NATIVE_H,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .unwrap_or_else(|e| panic!("rt_t2b graph must build: {e}\n{json}"));
    runtime.set_dump_all(true);

    let target = h.make_target("rt-t2b-depth-vel-render-res");
    render_frame(&mut runtime, h, &target.texture, 1, 0);

    let dumped = runtime.dump_textures_all();
    let expected_w = expected_render_dim(NATIVE_W);
    let expected_h = expected_render_dim(NATIVE_H);
    for port in ["depth", "velocity"] {
        let (_, _, _, tex) = dumped
            .iter()
            .find(|(node_id, p, _, _)| node_id == "scene" && p == port)
            .unwrap_or_else(|| panic!("temporal_upscale=true must force `{port}` into consumed_outputs"));
        assert_eq!(tex.width, expected_w, "`{port}` width must be render res ({expected_w}), not native");
        assert_eq!(tex.height, expected_h, "`{port}` height must be render res ({expected_h}), not native");
    }
}

/// D22: a temporal-upscaled still-frame render approximates the SAME scene
/// rendered natively (`temporal_upscale=false`) within a coarse epsilon —
/// proves the wiring upscales the actual scene content, not garbage. Not a
/// quality judgment (Peter's morning call, same disclaimer as
/// `rt_p4_metalfx_temporal.rs`).
#[test]
fn temporal_upscale_vs_native_still_frame_mean_abs_diff_below_coarse_epsilon() {
    let h = harness::shared();

    let mut native_runtime = build_runtime(h, false);
    let native_target = h.make_target("rt-t2b-native-still");
    // Two frames: first primes `prev_view_proj`/pool state, second is the
    // measured still frame (same warm-up convention `rt_w0_gbuffer.rs` uses).
    render_frame(&mut native_runtime, h, &native_target.texture, 1, 0);
    render_frame(&mut native_runtime, h, &native_target.texture, 1, 1);
    let native_pixels = readback_rgba_f32(&h.device, &native_target.texture);

    let mut upscaled_runtime = build_runtime(h, true);
    let upscaled_target = h.make_target("rt-t2b-upscaled-still");
    render_frame(&mut upscaled_runtime, h, &upscaled_target.texture, 1, 0);
    render_frame(&mut upscaled_runtime, h, &upscaled_target.texture, 1, 1);
    let upscaled_pixels = readback_rgba_f32(&h.device, &upscaled_target.texture);

    let diff = mean_abs_diff_rgb(&native_pixels, &upscaled_pixels);
    eprintln!("[T2-B] native-vs-upscaled still-frame mean abs diff = {diff}");
    assert!(
        diff < UPSCALE_COARSE_EPSILON,
        "temporal-upscaled render diverges too far from native (mean abs diff {diff} >= {UPSCALE_COARSE_EPSILON})"
    );
}

/// D22/RT-D2: a `temporal_upscale=true` scene warmed up under one
/// `owner_key` (simulating a clip/layer holding history), then "cut" to a
/// different `owner_key` — the shared `TemporalResetDetector` (RT-D2, the
/// SAME node-local instance `rt_enabled`'s irradiance accumulator would
/// use) must flag the owner_key change as a reset, discarding MetalFX's
/// history so the cut frame looks like a cold start, not scene A's ghost.
#[test]
fn temporal_upscale_cut_reset_matches_cold_start_within_epsilon() {
    let h = harness::shared();

    // Warmed: 8 frames under owner_key 1, then a 9th frame under owner_key
    // 2 (the cut).
    let mut warmed_runtime = build_runtime(h, true);
    let warmed_target = h.make_target("rt-t2b-warmed-then-cut");
    for i in 0..8i64 {
        render_frame(&mut warmed_runtime, h, &warmed_target.texture, 1, i);
    }
    render_frame(&mut warmed_runtime, h, &warmed_target.texture, 2, 8);
    let cut_plus_one = readback_rgba_f32(&h.device, &warmed_target.texture);

    // Cold: a FRESH runtime's very first frame, owner_key 2 (matches the
    // cut's owner_key — a brand-new node instance has never seen it
    // either way, but keeping it identical removes one more variable).
    let mut cold_runtime = build_runtime(h, true);
    let cold_target = h.make_target("rt-t2b-cold-start");
    render_frame(&mut cold_runtime, h, &cold_target.texture, 2, 0);
    let cold_start = readback_rgba_f32(&h.device, &cold_target.texture);

    let diff = mean_abs_diff_rgb(&cut_plus_one, &cold_start);
    eprintln!("[T2-B] cut+1-vs-cold-start mean abs diff = {diff}");
    assert!(
        diff < CUT_RESET_EPSILON,
        "cut+1 frame still shows scene A's ghost through the render_scene node's wiring \
         (mean abs diff vs cold-start {diff} >= {CUT_RESET_EPSILON})"
    );
}

/// D22: `temporal_upscale=false` (the default/untouched path) is
/// deterministic across two independent runtimes rendering the identical
/// scene — the closest same-session proxy for "native mode is
/// byte-identical to before this change" available to a scripted test
/// (a literal pre-change-commit byte diff needs a separate checked-out
/// build; see this lane's report to the dispatcher for that gate's
/// disposition). Any non-determinism here would itself be a native-path
/// regression this change could plausibly have introduced (e.g. the
/// `width`/`height` shadow leaking into the non-upscale branch).
#[test]
fn native_mode_render_is_deterministic_across_independent_runtimes() {
    let h = harness::shared();

    let mut runtime_a = build_runtime(h, false);
    let target_a = h.make_target("rt-t2b-native-det-a");
    render_frame(&mut runtime_a, h, &target_a.texture, 1, 0);
    render_frame(&mut runtime_a, h, &target_a.texture, 1, 1);
    let pixels_a = readback_rgba_f32(&h.device, &target_a.texture);

    let mut runtime_b = build_runtime(h, false);
    let target_b = h.make_target("rt-t2b-native-det-b");
    render_frame(&mut runtime_b, h, &target_b.texture, 1, 0);
    render_frame(&mut runtime_b, h, &target_b.texture, 1, 1);
    let pixels_b = readback_rgba_f32(&h.device, &target_b.texture);

    assert_eq!(pixels_a.len(), pixels_b.len());
    for (i, (&a, &b)) in pixels_a.iter().zip(pixels_b.iter()).enumerate() {
        assert_eq!(a, b, "native-mode render is not deterministic at component {i}: {a} != {b}");
    }
}

/// Frame-time proxy for the mode-toggle gate (a real `MANIFOLD_RENDER_
/// TRACE=1` in-app run needs a live display/app process, out of reach for a
/// scripted `gpu-proofs` test — see this lane's report to the dispatcher).
/// Same shape as `rt_p1_region_probe.rs`'s `rt_enable_first_frame_never_
/// stalls_past_20ms`: a performer flipping `temporal_upscale` mid-set must
/// never hit a synchronous stall — the first frame after EITHER direction
/// (native -> upscaled allocates the scratch/upscaler; upscaled -> native
/// just stops using them) is exempted as the one-time allocation/JIT
/// window, every frame after must stay under budget.
#[test]
fn temporal_upscale_toggle_never_stalls_past_20ms() {
    let h = harness::shared();
    let mut native_runtime = build_runtime(h, false);
    let mut upscale_runtime = build_runtime(h, true);
    let native_target = h.make_target("rt-t2b-toggle-native");
    let upscale_target = h.make_target("rt-t2b-toggle-upscale");

    const BUDGET_MS: f64 = 20.0;
    const WARMUP_FRAMES_EXEMPT: i64 = 2;
    let mut worst: (&str, i64, f64) = ("", -1, 0.0);

    // native -> upscaled -> native, a few frames each side, simulating a
    // performer toggling the mode mid-set.
    let sequence: [(&str, i64); 12] = [
        ("native", 0), ("native", 1), ("native", 2), ("native", 3),
        ("upscale", 0), ("upscale", 1), ("upscale", 2), ("upscale", 3),
        ("native", 4), ("native", 5), ("native", 6), ("native", 7),
    ];
    for (label, frame) in sequence {
        let (runtime, target) = if label == "native" {
            (&mut native_runtime, &native_target.texture)
        } else {
            (&mut upscale_runtime, &upscale_target.texture)
        };
        let start = std::time::Instant::now();
        render_frame(runtime, h, target, 1, frame);
        let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
        eprintln!("[T2-B] {label} frame {frame}: {elapsed_ms:.2}ms");
        if frame >= WARMUP_FRAMES_EXEMPT && elapsed_ms > worst.2 {
            worst = (label, frame, elapsed_ms);
        }
        assert!(
            frame < WARMUP_FRAMES_EXEMPT || elapsed_ms <= BUDGET_MS,
            "{label} frame {frame} took {elapsed_ms:.2}ms (>{BUDGET_MS}ms budget) — toggling \
             temporal_upscale must never stall a frame past its one-time allocation window"
        );
    }
    eprintln!("[T2-B] worst post-warmup frame: {} #{} at {:.2}ms", worst.0, worst.1, worst.2);
}

/// Peter-only visual artifact — NOT a gate (CLAUDE.md: agent gates are
/// numbers, image judgment is Peter's call). Ignored by default so it never
/// runs in the automated `gpu-proofs` sweep; run explicitly with
/// `cargo test -p manifold-renderer --features gpu-proofs --test gpu_proofs \
/// rt_t2b_temporal_wiring::dump_side_by_side_pngs_for_peter -- --ignored`.
/// Writes native-vs-upscaled PNGs for a still frame and a post-orbit frame.
#[test]
#[ignore]
fn dump_side_by_side_pngs_for_peter() {
    let h = harness::shared();
    let out_dir = std::path::Path::new(
        "/private/tmp/claude-501/-Users-peterkiemann-MANIFOLD---Rust/7cea0ff3-26f1-41a6-8472-26b7d4411e1a/scratchpad",
    );

    for (label, temporal_upscale) in [("native", false), ("upscale", true)] {
        let mut runtime = build_runtime(h, temporal_upscale);
        let target = h.make_target(&format!("rt-t2b-png-{label}"));
        // Still frame: two frames at the same orbit (warm-up + measured).
        render_frame(&mut runtime, h, &target.texture, 1, 0);
        render_frame(&mut runtime, h, &target.texture, 1, 1);
        let png = manifold_renderer::headless_readback::readback_to_srgb_png(
            &h.device,
            &target.texture,
            NATIVE_W,
            NATIVE_H,
        );
        let path = out_dir.join(format!("rt_t2b_{label}_still.png"));
        std::fs::write(&path, &png).unwrap_or_else(|e| panic!("write {path:?}: {e}"));
        eprintln!("[T2-B] wrote {path:?}");

        // One more frame after a small orbit step (motion frame).
        let c = PresetContext {
            time: 0.0,
            beat: 0.0,
            dt: 1.0 / 60.0,
            width: NATIVE_W,
            height: NATIVE_H,
            output_width: NATIVE_W,
            output_height: NATIVE_H,
            aspect: NATIVE_W as f32 / NATIVE_H as f32,
            owner_key: 1,
            is_clip_level: false,
            frame_count: 2,
            anim_progress: 0.0,
            trigger_count: 0,
        };
        let mut enc = h.device.create_encoder("rt-t2b-png-orbit");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
            runtime.render(&mut gpu, &target.texture, &c, &manifold_core::params::ParamManifest::default());
        }
        enc.commit_and_wait_completed();
        let png = manifold_renderer::headless_readback::readback_to_srgb_png(
            &h.device,
            &target.texture,
            NATIVE_W,
            NATIVE_H,
        );
        let path = out_dir.join(format!("rt_t2b_{label}_orbit.png"));
        std::fs::write(&path, &png).unwrap_or_else(|e| panic!("write {path:?}: {e}"));
        eprintln!("[T2-B] wrote {path:?}");
    }
}

/// BUG-317 regression pin — Peter's live crash, reproduced through the real
/// runtime path: build with `temporal_upscale=false` (so the compiled plan's
/// `consumed_outputs` fold has no forced `depth`/`velocity`), then flip the
/// param LIVE via `Graph::set_param` (the same funnel every host write uses)
/// and render again. Pre-fix: the stale plan had no velocity target and the
/// first upscaled frame panicked ("force_consumed_outputs forces `velocity`
/// into consumed_outputs whenever temporal_upscale is on"). Post-fix:
/// `Graph::forced_outputs_epoch` moves on the write and `PresetRuntime`
/// recompiles the plan before the frame executes — no panic, and the
/// upscaled output still matches a native render within the coarse epsilon
/// (proves the recompiled path actually upscales the scene, not garbage).
#[test]
fn live_temporal_upscale_toggle_recompiles_plan_and_does_not_panic() {
    let h = harness::shared();

    let mut runtime = build_runtime(h, false);
    let target = h.make_target("rt-t2b-live-toggle");
    render_frame(&mut runtime, h, &target.texture, 1, 0);

    let scene_node = runtime
        .graph
        .nodes()
        .find(|n| n.params.get("temporal_upscale").is_some())
        .map(|n| n.id)
        .expect("scene graph contains the node.render_scene instance");
    runtime
        .graph
        .set_param(
            scene_node,
            "temporal_upscale",
            manifold_renderer::node_graph::ParamValue::Bool(true),
        )
        .expect("temporal_upscale param exists");

    // Pre-fix this frame aborted the process. Two frames: the toggle frame
    // (plan recompile + first upscale) and one steady frame after it.
    render_frame(&mut runtime, h, &target.texture, 1, 1);
    render_frame(&mut runtime, h, &target.texture, 1, 2);
    let toggled = readback_rgba_f32(&h.device, &target.texture);

    let mut native_runtime = build_runtime(h, false);
    let native_target = h.make_target("rt-t2b-live-toggle-native");
    render_frame(&mut native_runtime, h, &native_target.texture, 1, 0);
    render_frame(&mut native_runtime, h, &native_target.texture, 1, 1);
    let native = readback_rgba_f32(&h.device, &native_target.texture);

    let diff = mean_abs_diff_rgb(&toggled, &native);
    assert!(
        diff < UPSCALE_COARSE_EPSILON,
        "live-toggled upscale output diverges from native render: mean abs diff {diff} >= {UPSCALE_COARSE_EPSILON} — the recompiled plan is not rendering the scene"
    );
}

/// BUG-318 smoke test — NOT the regression pin: this synthetic hand-wired
/// scene does NOT reproduce the break even pre-fix (recorded honestly; the
/// discriminating repro is `rt_bug318_import_toggle`, which goes through the
/// real `assemble_import_graph` path in the shrink direction). Kept as cheap
/// coverage of the same toggle flow on a
/// `node.scene_object`-fed scene (the path his gltf scenes use — the
/// SceneObject VALUE carries its mesh as a backend `Slot` handle) with a
/// STATIC mesh subgraph, `dump_all` OFF (dumping changes the executor's
/// output-holding behavior, which is exactly why the BUG-317 pin missed
/// this). Toggle `rt_enabled` live: BUG-317's plan recompile swaps the
/// plan under the executor, and pre-fix the memo-CLEAN static mesh steps
/// kept serving held slots recorded under the OLD plan — the SceneObject's
/// `vertices` slot dangled and every object magenta-cleared. Post-fix the
/// recompile invalidates the memoized dataflow, everything re-executes
/// once, and the frame still renders the scene (asserted: output matches a
/// fresh rt-off render of the same scene within the coarse epsilon, and is
/// not the magenta fallback).
#[test]
fn live_rt_toggle_with_scene_object_does_not_dangle_mesh_slots() {
    let h = harness::shared();
    let registry = PrimitiveRegistry::with_builtin();
    let json = r#"{"version":2,"name":"RtBug318","nodes":[
        {"id":0,"typeId":"system.generator_input","nodeId":"input"},
        {"id":1,"typeId":"node.grid_mesh","nodeId":"grid","params":{
            "max_capacity":{"type":"Int","value":16},
            "resolution_x":{"type":"Int","value":2},
            "resolution_y":{"type":"Int","value":2},
            "size_x":{"type":"Float","value":1.6},
            "size_y":{"type":"Float","value":1.6}}},
        {"id":2,"typeId":"node.make_triangles","nodeId":"tris","params":{
            "src_cols":{"type":"Int","value":2},
            "src_rows":{"type":"Int","value":2}}},
        {"id":3,"typeId":"node.orbit_camera","nodeId":"cam","params":{
            "orbit":{"type":"Float","value":0.0},
            "tilt":{"type":"Float","value":0.0},
            "distance":{"type":"Float","value":4.0},
            "fov_y":{"type":"Float","value":0.9}}},
        {"id":4,"typeId":"node.unlit_material","nodeId":"mat","params":{
            "color_r":{"type":"Float","value":0.8},
            "color_g":{"type":"Float","value":0.4},
            "color_b":{"type":"Float","value":0.2},
            "color_a":{"type":"Float","value":1.0}}},
        {"id":5,"typeId":"node.transform_3d","nodeId":"xf0","params":{
            "rot_z":{"type":"Float","value":1.5707963267948966}}},
        {"id":6,"typeId":"node.scene_object","nodeId":"obj0"},
        {"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{
            "objects":{"type":"Int","value":1},
            "lights":{"type":"Int","value":0}}},
        {"id":99,"typeId":"system.final_output","nodeId":"out"}
        ],"wires":[
        {"fromNode":1,"fromPort":"vertices","toNode":2,"toPort":"in"},
        {"fromNode":2,"fromPort":"out","toNode":6,"toPort":"vertices"},
        {"fromNode":4,"fromPort":"out","toNode":6,"toPort":"material"},
        {"fromNode":5,"fromPort":"transform","toNode":6,"toPort":"transform"},
        {"fromNode":3,"fromPort":"out","toNode":20,"toPort":"camera"},
        {"fromNode":6,"fromPort":"object","toNode":20,"toPort":"object_0"},
        {"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}
        ]}"#;
    let build = |label: &str| {
        PresetRuntime::from_json_str_with_device(
            json,
            &registry,
            std::sync::Arc::clone(&h.device),
            NATIVE_W,
            NATIVE_H,
            GpuTextureFormat::Rgba16Float,
            None,
        )
        .unwrap_or_else(|e| panic!("bug318 graph must build ({label}): {e}"))
        // deliberately NO set_dump_all — see the doc comment
    };

    let mut runtime = build("toggled");
    let target = h.make_target("rt-bug318-toggle");
    // Several steady frames so the static mesh subgraph goes memo-CLEAN.
    for f in 0..4 {
        render_frame(&mut runtime, h, &target.texture, 1, f);
    }
    let scene_node = runtime
        .graph
        .nodes()
        .find(|n| n.params.get("rt_enabled").is_some())
        .map(|n| n.id)
        .expect("scene graph contains the node.render_scene instance");
    runtime
        .graph
        .set_param(
            scene_node,
            "rt_enabled",
            manifold_renderer::node_graph::ParamValue::Bool(true),
        )
        .expect("rt_enabled param exists");
    for f in 4..7 {
        render_frame(&mut runtime, h, &target.texture, 1, f);
    }
    let toggled = readback_rgba_f32(&h.device, &target.texture);

    let mut reference = build("reference");
    let ref_target = h.make_target("rt-bug318-ref");
    for f in 0..4 {
        render_frame(&mut reference, h, &ref_target.texture, 1, f);
    }
    let reference_px = readback_rgba_f32(&h.device, &ref_target.texture);

    // Magenta fallback detector: the error path clears to (1,0,1). A
    // healthy toggled frame must stay close to the rt-off reference (RT
    // adds lighting terms; with zero lights the delta stays small) and
    // must NOT be the fallback clear.
    let diff = mean_abs_diff_rgb(&toggled, &reference_px);
    assert!(
        diff < UPSCALE_COARSE_EPSILON,
        "live rt_enabled toggle broke the scene: mean abs diff vs rt-off reference {diff} >= {UPSCALE_COARSE_EPSILON} (magenta fallback / dangling mesh slots — BUG-318)"
    );
}
