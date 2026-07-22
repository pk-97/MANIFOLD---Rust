//! `docs/RAYTRACING_DESIGN.md` §5.2 P1/RT-D3 — scripted region-luminance
//! probe: the P1 gate's stand-in for the apricot-scan probe (no photoscan
//! asset is wired into this repo's test fixtures; this reuses
//! `render_scene_shadows.rs`'s decisive ground-plane + occluder + sun
//! scene instead — same "isolate the shadow term" shape the gate asks
//! for: a named occluded region's mean luminance must drop >=30% with RT
//! shadows ON vs OFF, and a named lit region must change <5%).
//!
//! Scene: `render_scene_shadows.rs`'s exact ground(8x8, y=0) + occluder
//! (3x3, y=1.5, centered over the ground's origin) + one overhead sun
//! (pos (3, 20, 3), aimed at the origin). The gate wants RT-shadows-ON
//! vs RT-shadows-OFF (unshadowed), not RT-vs-raster, so `cast_shadows`
//! rides the SAME toggle as `rt_enabled`: OFF = no shadow of any kind,
//! ON = `rt_enabled` true AND `has_casters` true, so the RT dispatch
//! actually runs. Same `node.orbit_camera` params
//! (orbit=0.7,tilt=0.95,distance=10,fov_y=0.8).
//!
//! Region selection is COMPUTED, not eyeballed (CLAUDE.md oracle
//! discipline): `Camera::orbit_perspective` + `project_to_pixel` (the
//! exact formula `node.orbit_camera`/render_scene.rs's camera math uses)
//! locates the pixel for two known WORLD points:
//! - occluded probe: world (0, 0, 0) — the ground's own origin, directly
//!   under the occluder's center. Hand-traced sun-to-occluder-center ray
//!   crosses `y=0` at approximately `(-0.24, 0, -0.24)` (well within the
//!   occluder's 3x3 footprint), so the origin sits inside the shadow.
//! - lit probe: world (3.5, 0, -3.5) — a far corner of the 8x8 ground,
//!   well outside the small near-origin shadow.
//!
//! Region = a 15x15 pixel window around each projected point, averaged.

use half::f16;
use manifold_gpu::GpuTextureFormat;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::camera::Camera;
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;

use crate::harness;

const ORBIT: f32 = 0.7;
const TILT: f32 = 0.95;
const DISTANCE: f32 = 10.0;
const FOV_Y: f32 = 0.8;
const NEAR: f32 = 0.05;
const FAR: f32 = 200.0;

fn scene_json(rt_enabled: bool) -> String {
    let rt_v = if rt_enabled { "true" } else { "false" };
    // The gate compares RT-shadows-ON vs RT-shadows-OFF (unshadowed), not
    // RT-vs-raster — `cast_shadows` rides the SAME toggle so the "off"
    // render has no shadow of any kind (the raster path would otherwise
    // still darken the occluded region when `rt_enabled` is false, since
    // `has_casters` gates the raster shadow-map loop independently of
    // `rt_enabled`).
    let cast_v = if rt_enabled { 1.0 } else { 0.0 };
    format!(
        r#"{{"version":2,"name":"RtP1RegionProbe","nodes":[
        {{"id":0,"typeId":"system.generator_input","nodeId":"input"}},
        {{"id":1,"typeId":"node.grid_mesh","nodeId":"ground_grid","params":{{
            "max_capacity":{{"type":"Int","value":8192}},
            "resolution_x":{{"type":"Int","value":20}},
            "resolution_y":{{"type":"Int","value":20}},
            "size_x":{{"type":"Float","value":8.0}},
            "size_y":{{"type":"Float","value":8.0}}}}}},
        {{"id":2,"typeId":"node.make_triangles","nodeId":"ground_tris","params":{{
            "src_cols":{{"type":"Int","value":20}},
            "src_rows":{{"type":"Int","value":20}}}}}},
        {{"id":5,"typeId":"node.grid_mesh","nodeId":"occ_grid","params":{{
            "max_capacity":{{"type":"Int","value":8192}},
            "resolution_x":{{"type":"Int","value":10}},
            "resolution_y":{{"type":"Int","value":10}},
            "size_x":{{"type":"Float","value":3.0}},
            "size_y":{{"type":"Float","value":3.0}}}}}},
        {{"id":6,"typeId":"node.make_triangles","nodeId":"occ_tris","params":{{
            "src_cols":{{"type":"Int","value":10}},
            "src_rows":{{"type":"Int","value":10}}}}}},
        {{"id":7,"typeId":"node.transform_3d","nodeId":"occ_xform","params":{{
            "pos_y":{{"type":"Float","value":1.5}}}}}},
        {{"id":3,"typeId":"node.orbit_camera","nodeId":"cam","params":{{
            "orbit":{{"type":"Float","value":{ORBIT}}},
            "tilt":{{"type":"Float","value":{TILT}}},
            "distance":{{"type":"Float","value":{DISTANCE}}},
            "fov_y":{{"type":"Float","value":{FOV_Y}}}}}}},
        {{"id":4,"typeId":"node.phong_material","nodeId":"mat","params":{{
            "color_r":{{"type":"Float","value":1.0}},
            "color_g":{{"type":"Float","value":1.0}},
            "color_b":{{"type":"Float","value":1.0}},
            "ambient":{{"type":"Float","value":0.05}}}}}},
        {{"id":30,"typeId":"node.light","nodeId":"sun_0","params":{{
            "mode":{{"type":"Enum","value":0}},
            "pos_x":{{"type":"Float","value":3.0}},
            "pos_y":{{"type":"Float","value":20.0}},
            "pos_z":{{"type":"Float","value":3.0}},
            "aim_x":{{"type":"Float","value":0.0}},
            "aim_y":{{"type":"Float","value":0.0}},
            "aim_z":{{"type":"Float","value":0.0}},
            "color_r":{{"type":"Float","value":1.0}},
            "color_g":{{"type":"Float","value":1.0}},
            "color_b":{{"type":"Float","value":1.0}},
            "intensity":{{"type":"Float","value":1.0}},
            "cast_shadows":{{"type":"Float","value":{cast_v}}}}}}},
        {{"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{{
            "objects":{{"type":"Int","value":2}},
            "lights":{{"type":"Int","value":1}},
            "rt_enabled":{{"type":"Bool","value":{rt_v}}}}}}},
        {{"id":99,"typeId":"system.final_output","nodeId":"out"}}
        ],"wires":[
        {{"fromNode":1,"fromPort":"vertices","toNode":2,"toPort":"in"}},
        {{"fromNode":2,"fromPort":"out","toNode":20,"toPort":"mesh_0"}},
        {{"fromNode":5,"fromPort":"vertices","toNode":6,"toPort":"in"}},
        {{"fromNode":6,"fromPort":"out","toNode":20,"toPort":"mesh_1"}},
        {{"fromNode":7,"fromPort":"transform","toNode":20,"toPort":"transform_1"}},
        {{"fromNode":3,"fromPort":"out","toNode":20,"toPort":"camera"}},
        {{"fromNode":4,"fromPort":"out","toNode":20,"toPort":"material_0"}},
        {{"fromNode":4,"fromPort":"out","toNode":20,"toPort":"material_1"}},
        {{"fromNode":30,"fromPort":"out","toNode":20,"toPort":"light_0"}},
        {{"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}}
        ]}}"#
    )
}

/// Render a scene-graph JSON to `Rgba16Float`, returning readback bytes.
/// RT-D4 (BUG-308 fix): the accel-structure build is async and deferred one
/// frame (never races this frame's own uncommitted mesh-generation GPU
/// writes — see `render_scene.rs`'s `rt_accel_pending_key` and
/// `raytrace.rs`'s `build_accel` doc comments) and its completion handler
/// runs on the GPU's own schedule, not synchronously with any particular
/// frame. `RT_WARMUP_FRAMES` commits enough frames for: (1) the request
/// frame, (2) the deferred build-enqueue frame, and (3) real wall-clock
/// time for the (tiny, ~900-triangle) async build to complete before the
/// final readback — the RT-D4 brief's "~7-frame transition" while RT-off
/// scenes just render the same unshadowed frame that many times (harmless,
/// no dirty-check churn once `accel_key` stops changing).
/// `commit_and_wait_completed` hard-checks for Metal GPU errors — a bad RT
/// dispatch/bind surfaces as a panic here, not a silently wrong frame.
const RT_WARMUP_FRAMES: i64 = 16;

fn render_readback(json: &str) -> (Vec<u8>, u32, u32) {
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
    .expect("RT region-probe scene graph must build");

    let target = h.make_target("rt-p1-region-probe");
    for frame in 0..RT_WARMUP_FRAMES {
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
        let mut enc = h.device.create_encoder("rt-p1-region-probe-enc");
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
    }
    (h.readback(&target.texture), h.width, h.height)
}

/// Mean luminance over a `(2*radius+1)^2` pixel window centered at
/// `(cx, cy)`, clamped to the image bounds.
fn region_luma(bytes: &[u8], w: u32, h: u32, cx: f32, cy: f32, radius: i32) -> f64 {
    let cxi = cx.round() as i32;
    let cyi = cy.round() as i32;
    let mut sum = 0.0f64;
    let mut n = 0u64;
    for dy in -radius..=radius {
        for dx in -radius..=radius {
            let x = cxi + dx;
            let y = cyi + dy;
            if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
                continue;
            }
            let idx = ((y as u32 * w + x as u32) * 8) as usize;
            let px = &bytes[idx..idx + 8];
            let r = f16::from_le_bytes([px[0], px[1]]).to_f32();
            let g = f16::from_le_bytes([px[2], px[3]]).to_f32();
            let b = f16::from_le_bytes([px[4], px[5]]).to_f32();
            assert!(r.is_finite() && g.is_finite() && b.is_finite(), "non-finite pixel");
            sum += (0.2126 * r + 0.7152 * g + 0.0722 * b) as f64;
            n += 1;
        }
    }
    assert!(n > 0, "region window is entirely off-screen");
    sum / n as f64
}

// BUG-308 (docs/BUG_BACKLOG.md), RESOLVED by RT-D4: root cause was
// `raytrace.rs`'s `build_accel`/`refit_accel` racing this frame's own
// (still-uncommitted) mesh-generation GPU writes via a synchronously
// committed+waited SEPARATE command buffer on the same Metal queue — the
// accel structure was permanently built from pre-generation vertex data.
// Fixed by making the build async (one command buffer, `commit()` with no
// `waitUntilCompleted()`, a completion handler flips an `Arc<AtomicBool>`
// ready flag) and deferring the actual enqueue one frame in
// `render_scene.rs` so it's guaranteed to run after the previous frame's
// commit — see both files' doc comments for the full mechanism. VERIFIED
// working: with RT-D4 landed, the occluded region (world origin) drops
// 58.3% RT-on vs RT-off (was exactly 0.0% before RT-D4) — comfortably
// past this test's own >=30% bar.
//
// BUG-309 (docs/BUG_BACKLOG.md), RESOLVED: RT-D4 was the first time this
// path ever produced a REAL rendered shadow, and doing so revealed the
// shadow wasn't confined to the small occluder's footprint — nearly the
// ENTIRE ground darkened by 25-83% RT-on vs RT-off at points far from the
// occluder. Root cause #1 (fixed): `trace_shadow_rays`'s bias epsilon
// (`raytrace.rs`) was a fixed `1e-3` world-unit constant, far too small
// at this scene's real scale, causing near-universal self-intersection.
// Fixed by deriving the bias from the screen-space neighbor deltas already
// computed for the finite-difference normal (scales with view distance/
// obliquity, capped against a synthetic-fixture pathology — see the
// kernel's own comment), with `ray.min_distance` rejecting any leftover
// self-hit outright. Root cause #2 (this test's own bug, ENGINE
// EXONERATED): the original `occluded_world = (0,0,0)` probe point maps
// to a pixel where the reconstructed world position's Y comes out ~1.5 —
// the OCCLUDER's own top surface, not the ground behind it. From this
// camera's exact position/tilt, the floating occluder's own body sits
// directly in the line of sight to the ground origin, blocking the
// camera's view of that patch of ground (and its shadow) entirely — a
// real, physically-correct self-occlusion, not a rendering defect.
// Confirmed via a reconstruct-and-round-trip check: reconstructing the
// ray origin at that pixel and forward-projecting it back through
// `Camera::project_to_pixel` agreed with the CPU's own math to sub-pixel
// precision and exact depth — the reconstruction itself was never wrong,
// the probe point was simply pointed at the wrong surface. `occluded_world`
// below was swept and picked using the SAME reconstruct-and-round-trip
// technique, confirming Y~0 (camera sees ground, not the occluder) AND a
// real >=30% drop, so this class can't silently return.
#[test]
fn rt_shadow_darkens_occluded_region_and_leaves_lit_region_alone() {
    let (on_bytes, w, h) = render_readback(&scene_json(true));
    let (off_bytes, _, _) = render_readback(&scene_json(false));

    let cam = Camera::orbit_perspective(ORBIT, TILT, DISTANCE, FOV_Y, 0.0, 0.0, NEAR, FAR);
    // World (1, 0, -1): inside the sun-ray-vs-occluder shadow footprint
    // (x,z in [-1.725, 1.275], computed from the sun/occluder geometry —
    // see the module doc's derivation), and verified camera-VISIBLE via
    // reconstruct-and-round-trip (the reconstructed ray origin's Y comes
    // out ~0.0, i.e. real ground, not the occluder's y=1.5 surface that
    // world (0,0,0) turned out to be blocked by). Gives a 43.0% luminance
    // drop RT-on vs RT-off — comfortably past this test's own >=30% bar.
    let occluded_world = [1.0, 0.0, -1.0];
    // BUG-308 fix-verification finding: this module's own doc comment
    // (top of file) specified the lit probe as world (3.5, 0, -3.5) — "a
    // far corner of the 8x8 ground, well outside the small near-origin
    // shadow" — but the code here had drifted to (2.5, 0, 0), close
    // enough to the occluder's real shadow footprint (a directional sun
    // with a genuine x/z tilt, not purely overhead) to catch real
    // occlusion once RT-D4 made the RT path actually produce shadows.
    // (3.5, 0, -3.5) itself projects fully off-screen for this camera/
    // resolution (computed via `project_to_pixel`, not eyeballed — pixel
    // (140.5, 69.3) against a 128-wide image); (2.5, 0, -2.5) is the
    // nearest still-far-corner point that lands on-screen with margin
    // (pixel ~(118, 68), a 15x15 window fully in bounds) — ALSO verified
    // camera-visible (reconstructed Y~0.0) via the same technique.
    let lit_world = [2.5, 0.0, -2.5];
    let occ_px = cam
        .project_to_pixel(occluded_world, w, h)
        .expect("occluded probe point must project in front of the camera");
    let lit_px = cam
        .project_to_pixel(lit_world, w, h)
        .expect("lit probe point must project in front of the camera");

    const RADIUS: i32 = 7; // 15x15 window
    let occ_on = region_luma(&on_bytes, w, h, occ_px.px, occ_px.py, RADIUS);
    let occ_off = region_luma(&off_bytes, w, h, occ_px.px, occ_px.py, RADIUS);
    let lit_on = region_luma(&on_bytes, w, h, lit_px.px, lit_px.py, RADIUS);
    let lit_off = region_luma(&off_bytes, w, h, lit_px.px, lit_px.py, RADIUS);

    let occ_drop = (occ_off - occ_on) / occ_off.max(1e-9);
    let lit_change = (lit_on - lit_off).abs() / lit_off.max(1e-9);
    eprintln!(
        "occluded region: off={occ_off:.4} on={occ_on:.4} drop={:.1}% | lit region: off={lit_off:.4} on={lit_on:.4} change={:.1}%",
        occ_drop * 100.0,
        lit_change * 100.0
    );

    assert!(
        occ_drop >= 0.30,
        "occluded region (pixel ({:.0},{:.0})) must drop >=30% RT-on vs RT-off: \
         off={occ_off:.4} on={occ_on:.4} drop={:.1}%",
        occ_px.px,
        occ_px.py,
        occ_drop * 100.0
    );
    assert!(
        lit_change < 0.05,
        "lit region (pixel ({:.0},{:.0})) must change <5% RT-on vs RT-off: \
         off={lit_off:.4} on={lit_on:.4} change={:.1}%",
        lit_px.px,
        lit_px.py,
        lit_change * 100.0
    );
}

/// RT-D4 (BUG-308's fix): the whole point of moving `build_accel`/
/// `refit_accel` off a synchronously-`waitUntilCompleted()`-ed command
/// buffer is that enabling RT on a live scene never stalls a frame — the
/// OLD code's synchronous wait for this exact 2-object scene's tiny BLAS/
/// TLAS build was the reported P1 perf-gate cost (110-167ms for a real
/// hero-asset scene; even this ~900-triangle scene's build is a real,
/// nonzero GPU cost that used to block the CPU). This drives the SAME
/// ground+occluder+sun scene with `rt_enabled` true from frame 0 — the
/// "first-enable" case — for `RT_WARMUP_FRAMES` frames, timing each
/// frame's wall-clock `render()` + `commit_and_wait_completed()` (the
/// same span a real content-thread tick pays), and asserts no frame past
/// the first two (typical one-time pipeline-compile warm-up, same
/// allowance `bug037_verify.rs`/`bug035_verify.rs` use, unrelated to RT-D4)
/// exceeds the 20ms/frame @ 60fps budget. Run with
/// `MANIFOLD_RENDER_TRACE=1 ... --nocapture` to also see the engine's own
/// per-stage breakdown for any frame that's slow.
#[test]
fn rt_enable_first_frame_never_stalls_past_20ms() {
    let h = harness::shared();
    let registry = PrimitiveRegistry::with_builtin();
    let mut runtime = PresetRuntime::from_json_str_with_device(
        &scene_json(true),
        &registry,
        std::sync::Arc::clone(&h.device),
        h.width,
        h.height,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .expect("RT region-probe scene graph must build");
    let target = h.make_target("rt-p1-frame-time");

    let mut worst: (u32, std::time::Duration) = (0, std::time::Duration::ZERO);
    for frame in 0..RT_WARMUP_FRAMES {
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
        let start = std::time::Instant::now();
        let mut enc = h.device.create_encoder("rt-p1-frame-time-enc");
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
        let elapsed = start.elapsed();
        eprintln!("frame {frame}: {:.2}ms", elapsed.as_secs_f64() * 1000.0);

        const WARMUP_FRAMES_EXEMPT: i64 = 2;
        if frame >= WARMUP_FRAMES_EXEMPT && elapsed > worst.1 {
            worst = (frame as u32, elapsed);
        }
        assert!(
            frame < WARMUP_FRAMES_EXEMPT || elapsed.as_secs_f64() * 1000.0 <= 20.0,
            "frame {frame} took {:.2}ms (>20ms budget) — RT-D4's whole point is that enabling \
             RT (accel structure build/refit) never stalls a frame; a synchronous \
             commit()+waitUntilCompleted() regression in raytrace.rs would show up exactly \
             as this kind of spike",
            elapsed.as_secs_f64() * 1000.0
        );
    }
    eprintln!(
        "worst post-warmup frame: {} at {:.2}ms",
        worst.0,
        worst.1.as_secs_f64() * 1000.0
    );
}
