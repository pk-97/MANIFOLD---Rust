//! `node.scene_object`'s per-object `cast_shadows` toggle — proof it
//! removes an object from direct-light shadowing ONLY, on both the RT and
//! raster paths, while the object stays drawn (primary visibility unaffected)
//! and stays in the RT accel structure/AO/GI/reflections (RT-D3 mask split:
//! `RT_MASK_VISIBLE` always set, `RT_MASK_SHADOW_CASTER` gated on the param).
//!
//! Scene: `rt_multi_caster_shadow.rs`'s exact ground(8x8, y=0) + occluder
//! (3x3, y=1.5) fixture, one sun caster at `(3,20,3)` aimed at the origin —
//! reused unmodified so this proof inherits its already-verified
//! occluded/lit probe points (`OCCLUDED_WORLD`/`LIT_WORLD`) instead of
//! re-deriving shadow-footprint geometry. Wired through explicit
//! `node.scene_object` nodes (the occluder's `cast_shadows` param is the
//! thing under test), matching `render_scene_object_visibility.rs`'s wiring
//! style for its `visible` port-shadow proof.
//!
//! (a) RT path: occluder `cast_shadows` 1→0 brightens the previously-
//!     shadowed ground probe to the unoccluded probe's level, while the
//!     occluder's own red-tinted pixels stay in the frame (primary
//!     visibility untouched — `RT_MASK_VISIBLE` is unconditional).
//! (b) RT path, reflections on: `cast_shadows=0` still draws the occluder —
//!     proven by comparing against a render with the occluder REMOVED from
//!     the graph entirely (`objects=1`, ground only). If turning
//!     `cast_shadows` off silently dropped the object from the accel/primary
//!     pass too, this render would be indistinguishable from the
//!     occluder-removed one; it isn't.
//! (c) Raster path (`rt_enabled` unset/false): the same 1→0 flip removes the
//!     ground shadow, `render_scene_object_visibility.rs`'s exact toggle
//!     style with `cast_shadows` in place of `visible`.

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

/// `rt_multi_caster_shadow.rs`'s exact verified probe points for a sun at
/// `(3,20,3)` aimed at the origin, over this same ground+occluder fixture.
const OCCLUDED_WORLD: [f32; 3] = [1.0, 0.0, -1.0];
const LIT_WORLD: [f32; 3] = [2.5, 0.0, -2.5];
const SUN_POS: [f32; 3] = [3.0, 20.0, 3.0];

const GROUND_AND_CAMERA_NODES: &str = r#"
        {"id":0,"typeId":"system.generator_input","nodeId":"input"},
        {"id":1,"typeId":"node.grid_mesh","nodeId":"ground_grid","params":{
            "max_capacity":{"type":"Int","value":8192},
            "resolution_x":{"type":"Int","value":20},
            "resolution_y":{"type":"Int","value":20},
            "size_x":{"type":"Float","value":8.0},
            "size_y":{"type":"Float","value":8.0}}},
        {"id":2,"typeId":"node.make_triangles","nodeId":"ground_tris","params":{
            "src_cols":{"type":"Int","value":20},
            "src_rows":{"type":"Int","value":20}}},
        {"id":3,"typeId":"node.orbit_camera","nodeId":"cam","params":{
            "orbit":{"type":"Float","value":0.7},
            "tilt":{"type":"Float","value":0.95},
            "distance":{"type":"Float","value":10.0},
            "fov_y":{"type":"Float","value":0.8}}},
        {"id":4,"typeId":"node.phong_material","nodeId":"ground_mat","params":{
            "color_r":{"type":"Float","value":1.0},
            "color_g":{"type":"Float","value":1.0},
            "color_b":{"type":"Float","value":1.0},
            "ambient":{"type":"Float","value":0.05}}},
        {"id":40,"typeId":"node.scene_object","nodeId":"obj0","params":{
            "visible":{"type":"Float","value":1.0}}}"#;

/// Ground-only scene (occluder entirely absent — `objects: 1`), used as the
/// "occluder removed" baseline for proof (b).
fn scene_json_no_occluder() -> String {
    format!(
        r#"{{"version":2,"name":"RtObjectCastShadowsNoOccluder","nodes":[{GROUND_AND_CAMERA_NODES},
        {{"id":30,"typeId":"node.light","nodeId":"sun","params":{{
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
            "intensity":{{"type":"Float","value":1.0}},
            "cast_shadows":{{"type":"Float","value":1.0}}}}}},
        {{"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{{
            "objects":{{"type":"Int","value":1}},
            "lights":{{"type":"Int","value":1}},
            "rt_enabled":{{"type":"Bool","value":true}},
            "rt_reflections":{{"type":"Bool","value":true}}}}}},
        {{"id":99,"typeId":"system.final_output","nodeId":"out"}}
        ],"wires":[
        {{"fromNode":1,"fromPort":"vertices","toNode":2,"toPort":"in"}},
        {{"fromNode":2,"fromPort":"out","toNode":40,"toPort":"vertices"}},
        {{"fromNode":4,"fromPort":"out","toNode":40,"toPort":"material"}},
        {{"fromNode":40,"fromPort":"object","toNode":20,"toPort":"object_0"}},
        {{"fromNode":3,"fromPort":"out","toNode":20,"toPort":"camera"}},
        {{"fromNode":30,"fromPort":"out","toNode":20,"toPort":"light_0"}},
        {{"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}}
        ]}}"#,
        sun_x = SUN_POS[0],
        sun_y = SUN_POS[1],
        sun_z = SUN_POS[2],
    )
}

/// Ground + occluder, `rt_enabled`/`occluder_cast_shadows`/`rt_reflections`
/// all test parameters. Occluder is `node.scene_object` `obj1` — its
/// `cast_shadows` param is the thing under test; the light's OWN
/// `cast_shadows` stays permanently on (it must, for there to be any shadow
/// at all to toggle).
fn scene_json(rt_enabled: bool, occluder_cast_shadows: f32, rt_reflections: bool) -> String {
    format!(
        r#"{{"version":2,"name":"RtObjectCastShadows","nodes":[{GROUND_AND_CAMERA_NODES},
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
        {{"id":8,"typeId":"node.phong_material","nodeId":"occ_mat","params":{{
            "color_r":{{"type":"Float","value":1.0}},
            "color_g":{{"type":"Float","value":0.0}},
            "color_b":{{"type":"Float","value":0.0}},
            "ambient":{{"type":"Float","value":0.05}}}}}},
        {{"id":41,"typeId":"node.scene_object","nodeId":"obj1","params":{{
            "visible":{{"type":"Float","value":1.0}},
            "cast_shadows":{{"type":"Float","value":{occluder_cast_shadows}}}}}}},
        {{"id":30,"typeId":"node.light","nodeId":"sun","params":{{
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
            "intensity":{{"type":"Float","value":1.0}},
            "cast_shadows":{{"type":"Float","value":1.0}}}}}},
        {{"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{{
            "objects":{{"type":"Int","value":2}},
            "lights":{{"type":"Int","value":1}},
            "rt_enabled":{{"type":"Bool","value":{rt_enabled}}},
            "rt_reflections":{{"type":"Bool","value":{rt_reflections}}}}}}},
        {{"id":99,"typeId":"system.final_output","nodeId":"out"}}
        ],"wires":[
        {{"fromNode":1,"fromPort":"vertices","toNode":2,"toPort":"in"}},
        {{"fromNode":2,"fromPort":"out","toNode":40,"toPort":"vertices"}},
        {{"fromNode":4,"fromPort":"out","toNode":40,"toPort":"material"}},
        {{"fromNode":40,"fromPort":"object","toNode":20,"toPort":"object_0"}},
        {{"fromNode":5,"fromPort":"vertices","toNode":6,"toPort":"in"}},
        {{"fromNode":6,"fromPort":"out","toNode":41,"toPort":"vertices"}},
        {{"fromNode":8,"fromPort":"out","toNode":41,"toPort":"material"}},
        {{"fromNode":7,"fromPort":"transform","toNode":41,"toPort":"transform"}},
        {{"fromNode":41,"fromPort":"object","toNode":20,"toPort":"object_1"}},
        {{"fromNode":3,"fromPort":"out","toNode":20,"toPort":"camera"}},
        {{"fromNode":30,"fromPort":"out","toNode":20,"toPort":"light_0"}},
        {{"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}}
        ]}}"#,
        sun_x = SUN_POS[0],
        sun_y = SUN_POS[1],
        sun_z = SUN_POS[2],
        occluder_cast_shadows = occluder_cast_shadows,
        rt_enabled = rt_enabled,
        rt_reflections = rt_reflections,
    )
}

/// Same async-accel warm-up discipline as `rt_multi_caster_shadow.rs` — long
/// enough for the RT path's accel build to land; harmless extra frames on
/// the raster path.
const WARMUP_FRAMES: i64 = 16;

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
    .expect("rt_object_cast_shadows scene graph must build");

    let target = h.make_target("rt-object-cast-shadows");
    for frame in 0..WARMUP_FRAMES {
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
            gpu_signal_committed: 0,
            gpu_signaled: 0,
        };
        let mut enc = h.device.create_encoder("rt-object-cast-shadows-enc");
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

/// Sum of `R - G` over the whole frame — isolates the red-tinted occluder's
/// own draw from the grey ground (same technique/rationale as
/// `render_scene_object_visibility.rs`'s `red_excess_sum`).
fn red_excess_sum(bytes: &[u8]) -> f64 {
    let mut sum = 0.0f64;
    for px in bytes.chunks_exact(8) {
        let r = f16::from_le_bytes([px[0], px[1]]).to_f32();
        let g = f16::from_le_bytes([px[2], px[3]]).to_f32();
        sum += (r - g).max(0.0) as f64;
    }
    sum
}

// ─── (a) RT path: cast_shadows off brightens the shadowed probe, occluder stays drawn ──

#[test]
fn rt_cast_shadows_off_brightens_shadow_but_keeps_occluder_drawn() {
    let on_json = scene_json(true, 1.0, true);
    let off_json = scene_json(true, 0.0, true);

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

    let brighten = (occ_off - occ_on) / occ_on.max(1e-9);
    eprintln!(
        "rt cast_shadows off: occluded on={occ_on:.4} off={occ_off:.4} brighten={:.1}% | lit on={lit_on:.4} off={lit_off:.4}",
        brighten * 100.0
    );

    assert!(
        occ_off > occ_on && brighten > 0.20,
        "cast_shadows=0 must brighten the previously-shadowed ground probe by >20%: \
         on={occ_on:.4} off={occ_off:.4} brighten={:.1}%",
        brighten * 100.0
    );
    // Brightened into the SAME ballpark as the unoccluded probe (both
    // renders read the SAME lit probe, which cast_shadows never touches) —
    // not exact parity: OCCLUDED_WORLD sits closer to the (still-present)
    // occluder than LIT_WORLD, so it keeps a little extra AO/ambient falloff
    // even with its direct-light shadow gone.
    let occ_off_vs_lit = (occ_off - lit_off).abs() / lit_off.max(1e-9);
    assert!(
        occ_off_vs_lit < 0.30,
        "cast_shadows=0's formerly-shadowed probe must land in the unoccluded probe's ballpark \
         (<30% apart): occ_off={occ_off:.4} lit_off={lit_off:.4} delta={:.1}%",
        occ_off_vs_lit * 100.0
    );

    // Occluder's own red-tinted draw must stay in frame either way —
    // RT_MASK_VISIBLE is unconditional, cast_shadows only clears
    // RT_MASK_SHADOW_CASTER.
    let red_on = red_excess_sum(&on_bytes);
    let red_off = red_excess_sum(&off_bytes);
    eprintln!("rt cast_shadows off: red-excess on={red_on:.1} off={red_off:.1}");
    assert!(red_on > 0.0, "occluder must be visible with cast_shadows on (red-excess {red_on:.1})");
    let red_delta = (red_off - red_on).abs() / red_on.max(1e-9);
    assert!(
        red_delta < 0.05,
        "occluder's own primary-visible pixels must not change with cast_shadows toggled \
         (<5%): red_on={red_on:.1} red_off={red_off:.1} delta={:.1}%",
        red_delta * 100.0
    );
}

// ─── (b) RT path, reflections on: cast_shadows=0 still draws the occluder ──

#[test]
fn rt_cast_shadows_off_still_differs_from_occluder_removed() {
    let cast_shadows_off_json = scene_json(true, 0.0, true);
    let occluder_removed_json = scene_json_no_occluder();

    let (off_bytes, _, _) = render_readback(&cast_shadows_off_json);
    let (removed_bytes, _, _) = render_readback(&occluder_removed_json);

    let red_off = red_excess_sum(&off_bytes);
    let red_removed = red_excess_sum(&removed_bytes);
    eprintln!(
        "rt cast_shadows=0 vs occluder-removed: red-excess cast_shadows_off={red_off:.1} removed={red_removed:.1}"
    );

    assert!(
        red_removed < red_off * 0.05,
        "an ACTUALLY-removed occluder must show near-zero red-excess (the occluder is the only \
         red-tinted surface): removed={red_removed:.1} cast_shadows_off={red_off:.1}"
    );
    assert!(
        red_off > red_removed + 1.0,
        "cast_shadows=0 must still draw the occluder (primary hit + reflections/GI unaffected \
         by the flag) — it must NOT read the same as an occluder-removed render: \
         cast_shadows_off={red_off:.1} removed={red_removed:.1}"
    );
}

// ─── (c) Raster path: same toggle removes the ground shadow ───────────────

#[test]
fn raster_cast_shadows_off_removes_ground_shadow() {
    let on_json = scene_json(false, 1.0, true);
    let off_json = scene_json(false, 0.0, true);

    let (on_bytes, w, h) = render_readback(&on_json);
    let (off_bytes, _, _) = render_readback(&off_json);

    let cam = Camera::orbit_perspective(ORBIT, TILT, DISTANCE, FOV_Y, 0.0, 0.0, NEAR, FAR);
    let occ_px = cam
        .project_to_pixel(OCCLUDED_WORLD, w, h)
        .expect("occluded probe point must project in front of the camera");

    const RADIUS: i32 = 7;
    let occ_on = region_luma(&on_bytes, w, h, occ_px.px, occ_px.py, RADIUS);
    let occ_off = region_luma(&off_bytes, w, h, occ_px.px, occ_px.py, RADIUS);
    let brighten = (occ_off - occ_on) / occ_on.max(1e-9);
    eprintln!("raster cast_shadows off: on={occ_on:.4} off={occ_off:.4} brighten={:.1}%", brighten * 100.0);

    assert!(
        occ_off > occ_on && brighten > 0.20,
        "raster path: cast_shadows=0 must remove the ground shadow (>20% brighten): \
         on={occ_on:.4} off={occ_off:.4} brighten={:.1}%",
        brighten * 100.0
    );

    // Occluder itself must still be drawn on the raster path too.
    let red_on = red_excess_sum(&on_bytes);
    let red_off = red_excess_sum(&off_bytes);
    let red_delta = (red_off - red_on).abs() / red_on.max(1e-9);
    assert!(
        red_delta < 0.05,
        "raster path: occluder's own draw must not change with cast_shadows toggled (<5%): \
         red_on={red_on:.1} red_off={red_off:.1} delta={:.1}%",
        red_delta * 100.0
    );
}
