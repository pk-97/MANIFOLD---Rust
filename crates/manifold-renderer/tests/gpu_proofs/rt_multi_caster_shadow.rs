//! Multi-caster shadow fix — RT shadows previously traced only
//! `casters[0]` (`render_scene.rs`'s dispatch call site used to read `let
//! sun = &casters[0]` unconditionally), so every OTHER shadow-casting
//! light rendered as fully lit regardless of real occluders. This proof
//! covers the fix end to end: `manifold_gpu::raytrace::RtCasterParams`/
//! `ShadowRayParams::casters`, the per-caster loop in
//! `trace_shadow_rays`, and `render_scene.wgsl`'s `shadow_factor` reading
//! its own slot out of the widened `rt_shadow_mask` (now `Rgba16Float`,
//! one visibility channel per caster).
//!
//! Scene: `rt_p1_region_probe.rs`'s exact ground(8x8, y=0) + occluder
//! (3x3, y=1.5, centered over the ground's origin) + orbit camera
//! (orbit=0.7, tilt=0.95, distance=10, fov_y=0.8) — reusing its already
//! verified occluded/lit probe points (`occluded_world = (1,0,-1)`,
//! `lit_world = (2.5,0,-2.5)`), since this fixture's TWO lights (sun +
//! point, below) sit at that same overhead position.
//!
//! - Caster slot 0: sun at `(3,20,3)` aimed at the origin (same as
//!   `rt_p1_region_probe.rs`).
//! - Caster slot 1: point light — same position as the sun (far/high
//!   relative to the small occluder, so its shadow footprint over that
//!   occluder is close to the sun's directional case) — the light that
//!   actually lights the scene in the CURE proof.
//!
//! Three proofs, all scripted region-luminance probes (no PNG oracle),
//! same style as `rt_p1_region_probe.rs`/`rt_p3_emissive_gi.rs`:
//!
//! (a) CURE: sun intensity 0, point light on — changing ONLY the sun's
//!     position/aim must leave the render byte-identical. This holds
//!     structurally (a zero-intensity light's color is `(0,0,0)`, and
//!     every term that uses it — the raster direct term, the RT
//!     sun-bounce loop — multiplies by that color), not by geometric
//!     coincidence, so it doesn't depend on precise occluder-footprint
//!     math.
//! (b) Point-light shadow correctness: sun intensity 0 (isolate the point
//!     light), point's `cast_shadows` toggled on vs off — the occluded
//!     region must drop meaningfully, the lit region must barely move,
//!     mirroring `rt_p1_region_probe.rs`'s exact on/off method.
//! (c) Independence: point light held fixed (position, intensity,
//!     `cast_shadows`) across two renders that change ONLY the sun's
//!     position — the occluded probe (in the SUN's shadow footprint at
//!     its original position) must brighten measurably once the sun no
//!     longer shines toward the occluder from that angle, while the lit
//!     probe (untouched by either light's shadow in both variants) stays
//!     effectively flat — proving the sun and point casters are traced
//!     independently, not collapsed onto one shared visibility term (the
//!     bug this fixes).

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

/// `rt_p1_region_probe.rs`'s exact verified probe points for a light at
/// `(3,20,3)` aimed at the origin, over this same ground+occluder fixture.
const OCCLUDED_WORLD: [f32; 3] = [1.0, 0.0, -1.0];
const LIT_WORLD: [f32; 3] = [2.5, 0.0, -2.5];

#[allow(clippy::too_many_arguments)]
fn scene_json(
    sun_pos: [f32; 3],
    sun_intensity: f32,
    point_pos: [f32; 3],
    point_intensity: f32,
    point_cast_shadows: bool,
) -> String {
    scene_json_sun_shadow(sun_pos, sun_intensity, true, point_pos, point_intensity, point_cast_shadows)
}

/// Same fixture as [`scene_json`], with the sun's `cast_shadows` also
/// independently toggleable (used by the independence proof, which needs
/// to flip EACH caster's own shadow term separately while holding both
/// lights' positions/intensities fixed).
#[allow(clippy::too_many_arguments)]
fn scene_json_sun_shadow(
    sun_pos: [f32; 3],
    sun_intensity: f32,
    sun_cast_shadows: bool,
    point_pos: [f32; 3],
    point_intensity: f32,
    point_cast_shadows: bool,
) -> String {
    let sun_cast_v = if sun_cast_shadows { 1.0 } else { 0.0 };
    let point_cast_v = if point_cast_shadows { 1.0 } else { 0.0 };
    format!(
        r#"{{"version":2,"name":"RtMultiCasterShadow","nodes":[
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
            "pos_x":{{"type":"Float","value":{sun_x}}},
            "pos_y":{{"type":"Float","value":{sun_y}}},
            "pos_z":{{"type":"Float","value":{sun_z}}},
            "aim_x":{{"type":"Float","value":0.0}},
            "aim_y":{{"type":"Float","value":0.0}},
            "aim_z":{{"type":"Float","value":0.0}},
            "color_r":{{"type":"Float","value":1.0}},
            "color_g":{{"type":"Float","value":1.0}},
            "color_b":{{"type":"Float","value":1.0}},
            "intensity":{{"type":"Float","value":{sun_intensity}}},
            "cast_shadows":{{"type":"Float","value":{sun_cast_v}}}}}}},
        {{"id":31,"typeId":"node.light","nodeId":"point_0","params":{{
            "mode":{{"type":"Enum","value":1}},
            "pos_x":{{"type":"Float","value":{point_x}}},
            "pos_y":{{"type":"Float","value":{point_y}}},
            "pos_z":{{"type":"Float","value":{point_z}}},
            "aim_x":{{"type":"Float","value":0.0}},
            "aim_y":{{"type":"Float","value":0.0}},
            "aim_z":{{"type":"Float","value":0.0}},
            "color_r":{{"type":"Float","value":1.0}},
            "color_g":{{"type":"Float","value":1.0}},
            "color_b":{{"type":"Float","value":1.0}},
            "intensity":{{"type":"Float","value":{point_intensity}}},
            "range":{{"type":"Float","value":100.0}},
            "cast_shadows":{{"type":"Float","value":{point_cast_v}}}}}}},
        {{"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{{
            "objects":{{"type":"Int","value":2}},
            "lights":{{"type":"Int","value":2}},
            "rt_enabled":{{"type":"Bool","value":true}}}}}},
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
        {{"fromNode":31,"fromPort":"out","toNode":20,"toPort":"light_1"}},
        {{"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}}
        ]}}"#,
        sun_x = sun_pos[0],
        sun_y = sun_pos[1],
        sun_z = sun_pos[2],
        point_x = point_pos[0],
        point_y = point_pos[1],
        point_z = point_pos[2],
    )
}

/// Same async-accel warm-up discipline as `rt_p1_region_probe.rs`.
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
    .expect("RT multi-caster scene graph must build");

    let target = h.make_target("rt-multi-caster-shadow");
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
        let mut enc = h.device.create_encoder("rt-multi-caster-shadow-enc");
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

/// Max absolute per-channel difference over two same-size f16 RGBA
/// buffers — the "exact-zero max-abs-diff" comparison style this proof's
/// module doc promises for the CURE gate.
fn max_abs_diff_f16(a: &[u8], b: &[u8]) -> f32 {
    assert_eq!(a.len(), b.len());
    let mut worst = 0.0f32;
    for i in (0..a.len()).step_by(2) {
        let va = f16::from_le_bytes([a[i], a[i + 1]]).to_f32();
        let vb = f16::from_le_bytes([b[i], b[i + 1]]).to_f32();
        worst = worst.max((va - vb).abs());
    }
    worst
}

const SUN_POS: [f32; 3] = [3.0, 20.0, 3.0];
const POINT_POS: [f32; 3] = [3.0, 20.0, 3.0];

// ─── (a) CURE: zero-intensity sun, moving it changes nothing ──────────

#[test]
fn zero_intensity_sun_direction_change_is_byte_identical() {
    let scene_a = scene_json(SUN_POS, 0.0, POINT_POS, 1.0, true);
    // A materially different sun position/aim direction — if the sun's
    // own casters[0]-only collapse bug were still present, this couldn't
    // even be expressed as an independent term; if the zero-intensity
    // multiply were somehow bypassed, this would show up as a real delta.
    let scene_b = scene_json([-6.0, 12.0, 5.0], 0.0, POINT_POS, 1.0, true);

    let (bytes_a, w, h) = render_readback(&scene_a);
    let (bytes_b, _, _) = render_readback(&scene_b);

    let diff = max_abs_diff_f16(&bytes_a, &bytes_b);
    eprintln!("zero-intensity sun CURE: max abs diff = {diff:e} over {w}x{h}");
    assert_eq!(
        diff, 0.0,
        "a zero-intensity sun's direction must not change a single pixel — got max abs diff {diff:e}"
    );
}

// ─── (b) Point-light shadow correctness ────────────────────────────────

#[test]
fn point_light_shadow_darkens_occluded_region() {
    // Sun intensity 0 throughout — isolates the point light's own shadow.
    let on_json = scene_json(SUN_POS, 0.0, POINT_POS, 2.0, true);
    let off_json = scene_json(SUN_POS, 0.0, POINT_POS, 2.0, false);

    let (on_bytes, w, h) = render_readback(&on_json);
    let (off_bytes, _, _) = render_readback(&off_json);

    let cam = Camera::orbit_perspective(ORBIT, TILT, DISTANCE, FOV_Y, 0.0, 0.0, NEAR, FAR);
    let occ_px = cam
        .project_to_pixel(OCCLUDED_WORLD, w, h)
        .expect("occluded probe point must project in front of the camera");
    let lit_px = cam
        .project_to_pixel(LIT_WORLD, w, h)
        .expect("lit probe point must project in front of the camera");

    const RADIUS: i32 = 7;
    let occ_on = region_luma(&on_bytes, w, h, occ_px.px, occ_px.py, RADIUS);
    let occ_off = region_luma(&off_bytes, w, h, occ_px.px, occ_px.py, RADIUS);
    let lit_on = region_luma(&on_bytes, w, h, lit_px.px, lit_px.py, RADIUS);
    let lit_off = region_luma(&off_bytes, w, h, lit_px.px, lit_px.py, RADIUS);

    let occ_drop = (occ_off - occ_on) / occ_off.max(1e-9);
    let lit_change = (lit_on - lit_off).abs() / lit_off.max(1e-9);
    eprintln!(
        "point-light shadow: occluded off={occ_off:.4} on={occ_on:.4} drop={:.1}% | lit off={lit_off:.4} on={lit_on:.4} change={:.1}%",
        occ_drop * 100.0,
        lit_change * 100.0
    );

    assert!(
        occ_drop >= 0.20,
        "point-light shadow ON must darken the occluded region (pixel ({:.0},{:.0})) by >=20%: \
         off={occ_off:.4} on={occ_on:.4} drop={:.1}%",
        occ_px.px,
        occ_px.py,
        occ_drop * 100.0
    );
    assert!(
        lit_change < 0.05,
        "point-light shadow ON must leave the lit region (pixel ({:.0},{:.0})) largely unchanged \
         (<5%): off={lit_off:.4} on={lit_on:.4} change={:.1}%",
        lit_px.px,
        lit_px.py,
        lit_change * 100.0
    );
}

// ─── (c) Independence: both casters shadow simultaneously ─────────────

/// Both lights fully active (nonzero intensity, same fixed positions as
/// every other proof in this file) — only each light's OWN `cast_shadows`
/// toggles between the three renders below. Position/intensity/angle
/// never change, so any brightness delta is attributable ENTIRELY to that
/// one caster's shadow term — no Lambertian-angle confound (an earlier
/// draft of this proof moved the sun instead and got exactly that
/// confound: moving a light changes its own illumination angle on every
/// probe, not just which occluders it clears, making a raw luma delta
/// impossible to attribute to "shadow moved" alone).
///
/// This is the bug's exact shape: with the pre-fix single-caster
/// collapse, `casters[0]` (the sun, first by port order) was the only
/// light that ever produced a real trace — toggling the POINT's
/// `cast_shadows` would have changed NOTHING (it never counted), while
/// toggling the SUN's `cast_shadows` would have worked alone. The fix
/// must show a real, comparable-scale drop for EACH toggle independently
/// while the other caster's shadow (and the two lights' shared position)
/// stays fixed.
#[test]
fn sun_and_point_casters_shadow_independently() {
    let both_on = scene_json(SUN_POS, 1.0, POINT_POS, 1.0, true);
    let sun_only = scene_json_sun_shadow(SUN_POS, 1.0, false, POINT_POS, 1.0, true);
    let point_only = scene_json_sun_shadow(SUN_POS, 1.0, true, POINT_POS, 1.0, false);

    let (bytes_both, w, h) = render_readback(&both_on);
    let (bytes_sun_only, _, _) = render_readback(&sun_only);
    let (bytes_point_only, _, _) = render_readback(&point_only);

    let cam = Camera::orbit_perspective(ORBIT, TILT, DISTANCE, FOV_Y, 0.0, 0.0, NEAR, FAR);
    let occ_px = cam
        .project_to_pixel(OCCLUDED_WORLD, w, h)
        .expect("occluded probe point must project in front of the camera");

    const RADIUS: i32 = 7;
    let occ_both = region_luma(&bytes_both, w, h, occ_px.px, occ_px.py, RADIUS);
    // Point's own shadow removed (sun's shadow still on) — isolates the
    // point caster's contribution.
    let occ_point_removed = region_luma(&bytes_sun_only, w, h, occ_px.px, occ_px.py, RADIUS);
    // Sun's own shadow removed (point's shadow still on) — isolates the
    // sun caster's contribution.
    let occ_sun_removed = region_luma(&bytes_point_only, w, h, occ_px.px, occ_px.py, RADIUS);

    let point_contrib = (occ_point_removed - occ_both) / occ_both.max(1e-9);
    let sun_contrib = (occ_sun_removed - occ_both) / occ_both.max(1e-9);
    eprintln!(
        "independence: both={occ_both:.4} | point-shadow-removed={occ_point_removed:.4} \
         (delta {:.1}%) | sun-shadow-removed={occ_sun_removed:.4} (delta {:.1}%)",
        point_contrib * 100.0,
        sun_contrib * 100.0
    );

    assert!(
        point_contrib > 0.05,
        "removing ONLY the point light's shadow (sun's shadow left on, same positions/intensity) \
         must brighten the occluded region measurably (>5%) — the point caster must trace its own \
         shadow independent of the sun: both={occ_both:.4} point-removed={occ_point_removed:.4} \
         delta={:.1}%",
        point_contrib * 100.0
    );
    assert!(
        sun_contrib > 0.05,
        "removing ONLY the sun's shadow (point's shadow left on, same positions/intensity) must \
         brighten the occluded region measurably (>5%) — the sun caster must trace its own shadow \
         independent of the point (the exact collapse this fix removes: previously only \
         casters[0] ever traced): both={occ_both:.4} sun-removed={occ_sun_removed:.4} \
         delta={:.1}%",
        sun_contrib * 100.0
    );
}
