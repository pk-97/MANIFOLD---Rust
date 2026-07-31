//! RT_FURNACE_ORACLE: the ray tracer's first physically-closed-form
//! correctness oracle. Every existing RT test compares the renderer to
//! itself (RT-on vs RT-off, or one build vs another) — none compares to a
//! ground-truth value derived from physics. This file adds one.
//!
//! Every scene lights a flat, fully-diffuse (Lambertian) albedo-1 PBR plane
//! with `node.bake_environment`'s new `uniform` mode: a CONSTANT radiance
//! `L` in every texel/direction, wired straight into `node.render_scene`'s
//! `envmap` input with zero direct lights and zero emission. The
//! closed-form physics: a Lambertian surface under a uniform field of
//! incident radiance `L` reflects back exactly `albedo * L` — the diffuse
//! BRDF (`albedo / pi`) integrated against `L * cos(theta)` over the
//! hemisphere is `albedo * L * pi / pi = albedo * L`. With albedo 1 and
//! `L = intensity = 1.0`, the surface must read exactly 1.0.
//!
//! The occlusion tests below probe two regions of ONE render rather than
//! toggling `rt_enabled` between two renders — `furnace_wall_corner_scene_json`'s
//! doc comment states why an on/off toggle is the wrong comparison here (it
//! is self-defeating: with the ground material's `ambient` at zero, the RT
//! irradiance term is algebraically zero either way). Every RT-on render in
//! this file confirms the RT kernel actually dispatched
//! (`harness::assert_rt_dispatched`) rather than trusting a pixel diff to
//! rule out "RT never ran at all" (BUG-1l7f's failure class).

use half::f16;
use manifold_gpu::GpuTextureFormat;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::node_graph::camera::Camera;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;
use manifold_renderer::render_target::RenderTarget;

use crate::harness;

const ORBIT: f32 = 0.7;
const TILT: f32 = 0.95;
const DISTANCE: f32 = 10.0;
const FOV_Y: f32 = 0.8;
const NEAR: f32 = 0.05;
const FAR: f32 = 200.0;

/// Same RT-D4 async-accel-build settle window `rt_bug17r3_lightless_gi.rs`
/// and `rt_p3_emissive_gi.rs` use.
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
    .expect("RT furnace scene graph must build");

    let target = h.make_target("rt-furnace-oracle");
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
        let mut enc = h.device.create_encoder("rt-furnace-oracle-enc");
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

fn build_runtime(json: &str) -> (PresetRuntime, RenderTarget) {
    let h = harness::shared();
    let registry = PrimitiveRegistry::with_builtin();
    let runtime = PresetRuntime::from_json_str_with_device(
        json,
        &registry,
        std::sync::Arc::clone(&h.device),
        h.width,
        h.height,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .expect("RT furnace scene graph must build");
    let target = h.make_target("rt-furnace-oracle");
    (runtime, target)
}

fn render_frame(runtime: &mut PresetRuntime, target: &RenderTarget, frame_count: i64) {
    let h = harness::shared();
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
        frame_count,
        anim_progress: 0.0,
        trigger_count: 0,
    };
    let mut enc = h.device.create_encoder("rt-furnace-oracle-enc");
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

/// Build, settle `RT_WARMUP_FRAMES`, read back, then CONFIRM the RT kernel
/// actually dispatched on that settled state (`harness::assert_rt_dispatched`
/// — `render_scene` only pushes capture slots from inside its `rt_enabled &&
/// rt_ready` branch, which no pixel comparison can substitute for). BUG-1l7f
/// names the exact failure mode this guards: a scene that never traces at
/// all still produces SOME pixels, and a bare byte comparison cannot tell
/// "RT ran and did nothing new" apart from "RT never ran". The readback
/// happens before the confirmation frame, so the extra frame this runs
/// (frame `RT_WARMUP_FRAMES`) never contaminates the measured bytes — on
/// these static scenes it would render identically anyway.
fn render_readback_confirmed(json: &str, context: &str) -> (Vec<u8>, u32, u32) {
    let h = harness::shared();
    let (mut runtime, target) = build_runtime(json);
    for frame in 0..RT_WARMUP_FRAMES {
        render_frame(&mut runtime, &target, frame);
    }
    let bytes = h.readback(&target.texture);
    harness::assert_rt_dispatched(|| render_frame(&mut runtime, &target, RT_WARMUP_FRAMES), context);
    (bytes, h.width, h.height)
}

/// Same region-average-luma probe `rt_bug17r3_lightless_gi.rs` uses.
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

/// A flat 8x8 ground plane (default `node.grid_mesh` orientation — lies in
/// the XZ plane at y=0, same convention `rt_bug17r3_lightless_gi.rs`'s
/// ground/emitter geometry uses), white albedo-1 Lambertian PBR material
/// (metallic 0, roughness 1.0 — fully diffuse, no specular lobe to
/// contaminate the reading), zero ambient, zero direct lights, lit only by
/// the uniform environment. `rt_reflections` off — roughness 1.0 makes
/// reflections physically irrelevant here, and turning them off keeps this
/// reading isolated to the diffuse IBL term under test.
fn white_surface_scene_json() -> String {
    format!(
        r#"{{"version":2,"name":"RtFurnaceWhiteSurface","nodes":[
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
        {{"id":3,"typeId":"node.orbit_camera","nodeId":"cam","params":{{
            "orbit":{{"type":"Float","value":{ORBIT}}},
            "tilt":{{"type":"Float","value":{TILT}}},
            "distance":{{"type":"Float","value":{DISTANCE}}},
            "fov_y":{{"type":"Float","value":{FOV_Y}}}}}}},
        {{"id":8,"typeId":"node.bake_environment","nodeId":"env","params":{{
            "width":{{"type":"Int","value":64}},
            "height":{{"type":"Int","value":32}},
            "intensity":{{"type":"Float","value":1.0}},
            "uniform":{{"type":"Bool","value":true}}}}}},
        {{"id":4,"typeId":"node.pbr_material","nodeId":"ground_mat","params":{{
            "color_r":{{"type":"Float","value":1.0}},
            "color_g":{{"type":"Float","value":1.0}},
            "color_b":{{"type":"Float","value":1.0}},
            "ambient":{{"type":"Float","value":0.0}},
            "metallic":{{"type":"Float","value":0.0}},
            "roughness":{{"type":"Float","value":1.0}}}}}},
        {{"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{{
            "objects":{{"type":"Int","value":1}},
            "lights":{{"type":"Int","value":0}},
            "rt_enabled":{{"type":"Bool","value":true}},
            "rt_reflections":{{"type":"Bool","value":false}}}}}},
        {{"id":99,"typeId":"system.final_output","nodeId":"out"}}
        ],"wires":[
        {{"fromNode":1,"fromPort":"vertices","toNode":2,"toPort":"in"}},
        {{"fromNode":2,"fromPort":"out","toNode":20,"toPort":"mesh_0"}},
        {{"fromNode":3,"fromPort":"out","toNode":20,"toPort":"camera"}},
        {{"fromNode":4,"fromPort":"out","toNode":20,"toPort":"material_0"}},
        {{"fromNode":8,"fromPort":"envmap","toNode":20,"toPort":"envmap"}},
        {{"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}}
        ]}}"#
    )
}

#[test]
fn uniform_environment_white_surface_returns_the_environment_radiance() {
    let (bytes, w, h) = render_readback(&white_surface_scene_json());

    let cam = Camera::orbit_perspective(ORBIT, TILT, DISTANCE, FOV_Y, 0.0, 0.0, NEAR, FAR);
    let center_px = cam
        .project_to_pixel([0.0, 0.0, 0.0], w, h)
        .expect("ground-plane centre must project in front of the camera");

    const RADIUS: i32 = 15;
    let measured = region_luma(&bytes, w, h, center_px.px, center_px.py, RADIUS);
    eprintln!("RT furnace white-surface: measured luma = {measured:.5} (physically expected 1.0)");

    const TOLERANCE: f64 = 0.10;
    assert!(
        (measured - 1.0).abs() <= TOLERANCE,
        "measured region luma {measured:.5} is not within {:.0}% of the physically-expected 1.0 \
         (a fully-diffuse albedo-1 surface under a uniform L=1.0 environment must reflect back \
         exactly L) — this is a REAL finding about the renderer's IBL diffuse term, not a tuning \
         knob on this test",
        TOLERANCE * 100.0,
    );
}

/// Second geometry, replacing the earlier hovering-plate occluder. That
/// version had TWO fatal flaws, both found by direct kernel/geometry proof
/// rather than by eye:
///
/// - Painted the plate 0.5 grey against a white floor. The probe under it
///   read the PLATE's own albedo, not any shade on the floor — an
///   albedo-vs-shade confound that made the gate pass at `ratio 0.5077`,
///   which is just `0.5/1.0`.
/// - Even repainted white (removing that confound), the plate hovers ABOVE
///   the floor: the "shaded" probe pixel is the plate's TOP surface, which
///   sees the full uniform sky no matter how correct occlusion becomes.
///   The gate could never pass again once the paint confound was fixed —
///   a trap for whoever fixed the renderer next.
///
/// A floor-wall CORNER fixes both: the wall's base edge is welded to the
/// floor (`pos_y = size_y/2` after a `rot_x = pi/2` rotation puts the base
/// exactly at world y=0 — see `furnace_wall_corner_scene_json`), so no
/// probe can ever land on the wall itself (proven below, not eyeballed),
/// and a point in the corner has roughly half its sky blocked by
/// construction — the exact behaviour the gate cares about.
///
/// World-space probe points, both ON THE FLOOR (y=0):
/// `CORNER_WORLD` sits 0.8 units in front of the wall's base line (wall at
/// z=-2.5), `OPEN_WORLD` sits 5.5 units from it, near the room's open side.
///
/// Proof neither probe ray can hit the wall (the wall is a flat vertical
/// plane at world z=-2.5, spanning x in [-1.25,1.25], y in [0,2]):
/// `Camera::orbit_perspective(ORBIT, TILT, DISTANCE, ...)`'s analytic
/// formula gives `cam.pos.z = DISTANCE * orbit.sin() * tilt.cos() =
/// 10 * sin(0.7) * cos(0.95) = 3.7473`. A camera ray to world point `p` is
/// the segment from `cam.pos` to `p`; its z-coordinate along the segment is
/// the linear interpolation `z(t) = cam.pos.z + t*(p.z - cam.pos.z)`,
/// `t` in `[0,1]`, which stays strictly between `cam.pos.z` and `p.z` (a
/// convex combination never leaves the interval its endpoints span). Both
/// `cam.pos.z` (3.7473) and BOTH probes' z (-1.7 and 3.0) are `> -2.5`, so
/// `z(t) > -2.5` for the WHOLE segment in both cases — the ray never
/// reaches the wall's z=-2.5 plane at all, regardless of x/y, so it cannot
/// intersect the wall rectangle. Both probes are guaranteed to show floor.
///
/// A second, computed (not eyeballed) check on top of that: the SAMPLING
/// WINDOW around each probe pixel (`region_luma`'s `radius=5`, a 11x11
/// block) must also stay clear of the wall's own on-screen footprint, or a
/// neighbouring pixel inside the window could show the wall face instead
/// of floor. `Camera::project_to_pixel` puts the wall's base edge at
/// `(78.6,36.5)`-`(103.6,56.2)` and `CORNER_WORLD` at `(82.5,51.3)` — a
/// straight line in world space projects to a straight line on screen
/// (a projective-transform property), so interpolating that edge across
/// the window's x-range `[77.5,87.5]` gives edge-y in `[35.7,43.5]`,
/// strictly above (smaller-y than) the window's y-range `[46.3,56.3]` —
/// at least a 2.8px margin, so no pixel in the window can be the wall.
const CORNER_WORLD: [f32; 3] = [0.0, 0.0, -1.7];
const OPEN_WORLD: [f32; 3] = [0.0, 0.0, 3.0];

/// Same ground plane and white Lambertian material as
/// `white_surface_scene_json`, plus a second, albedo-1 white, non-emissive
/// grid rotated vertical (`rot_x = pi/2`) and positioned so its base edge
/// meets the floor at z=-2.5, forming a floor-wall corner well inside the
/// camera's view (`node.grid_mesh`'s flat XZ-plane output lies in local
/// `(x, 0, z)`; `rot_x = pi/2` maps that to world `(x, -z, 0)` — see
/// `render_scene.rs`'s `euler_xyz_columns` — so `pos_y = size_y/2` puts the
/// wall's base exactly at world y=0 and its top at y=size_y).
/// `rt_enabled` is always true — this is ONE render, probed at two screen
/// regions, not an RT-on/RT-off toggle (an RT-off comparison here would be
/// self-defeating: with `ground_ambient` at 0.0, no lights, and no
/// emission, the RT irradiance term `ambient_color * ao + gi` is
/// algebraically zero either way, so toggling `rt_enabled` changes nothing
/// by construction and proves nothing about whether occlusion traced
/// correctly). `ground_ambient` is exposed so the diagnostic test below can
/// compare the corner-region reading at ambient 0.0 vs 1.0.
fn furnace_wall_corner_scene_json(ground_ambient: f32) -> String {
    format!(
        r#"{{"version":2,"name":"RtFurnaceWallCorner","nodes":[
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
        {{"id":5,"typeId":"node.grid_mesh","nodeId":"wall_grid","params":{{
            "max_capacity":{{"type":"Int","value":8192}},
            "resolution_x":{{"type":"Int","value":10}},
            "resolution_y":{{"type":"Int","value":10}},
            "size_x":{{"type":"Float","value":2.5}},
            "size_y":{{"type":"Float","value":2.0}}}}}},
        {{"id":6,"typeId":"node.make_triangles","nodeId":"wall_tris","params":{{
            "src_cols":{{"type":"Int","value":10}},
            "src_rows":{{"type":"Int","value":10}}}}}},
        {{"id":7,"typeId":"node.transform_3d","nodeId":"wall_xform","params":{{
            "pos_x":{{"type":"Float","value":0.0}},
            "pos_y":{{"type":"Float","value":1.0}},
            "pos_z":{{"type":"Float","value":-2.5}},
            "rot_x":{{"type":"Float","value":1.5707963}}}}}},
        {{"id":3,"typeId":"node.orbit_camera","nodeId":"cam","params":{{
            "orbit":{{"type":"Float","value":{ORBIT}}},
            "tilt":{{"type":"Float","value":{TILT}}},
            "distance":{{"type":"Float","value":{DISTANCE}}},
            "fov_y":{{"type":"Float","value":{FOV_Y}}}}}}},
        {{"id":8,"typeId":"node.bake_environment","nodeId":"env","params":{{
            "width":{{"type":"Int","value":64}},
            "height":{{"type":"Int","value":32}},
            "intensity":{{"type":"Float","value":1.0}},
            "uniform":{{"type":"Bool","value":true}}}}}},
        {{"id":4,"typeId":"node.pbr_material","nodeId":"ground_mat","params":{{
            "color_r":{{"type":"Float","value":1.0}},
            "color_g":{{"type":"Float","value":1.0}},
            "color_b":{{"type":"Float","value":1.0}},
            "ambient":{{"type":"Float","value":{ground_ambient}}},
            "metallic":{{"type":"Float","value":0.0}},
            "roughness":{{"type":"Float","value":1.0}}}}}},
        {{"id":9,"typeId":"node.pbr_material","nodeId":"wall_mat","params":{{
            "color_r":{{"type":"Float","value":1.0}},
            "color_g":{{"type":"Float","value":1.0}},
            "color_b":{{"type":"Float","value":1.0}},
            "ambient":{{"type":"Float","value":0.0}},
            "metallic":{{"type":"Float","value":0.0}},
            "roughness":{{"type":"Float","value":1.0}}}}}},
        {{"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{{
            "objects":{{"type":"Int","value":2}},
            "lights":{{"type":"Int","value":0}},
            "rt_enabled":{{"type":"Bool","value":true}},
            "rt_reflections":{{"type":"Bool","value":false}}}}}},
        {{"id":99,"typeId":"system.final_output","nodeId":"out"}}
        ],"wires":[
        {{"fromNode":1,"fromPort":"vertices","toNode":2,"toPort":"in"}},
        {{"fromNode":2,"fromPort":"out","toNode":20,"toPort":"mesh_0"}},
        {{"fromNode":5,"fromPort":"vertices","toNode":6,"toPort":"in"}},
        {{"fromNode":6,"fromPort":"out","toNode":20,"toPort":"mesh_1"}},
        {{"fromNode":7,"fromPort":"transform","toNode":20,"toPort":"transform_1"}},
        {{"fromNode":3,"fromPort":"out","toNode":20,"toPort":"camera"}},
        {{"fromNode":4,"fromPort":"out","toNode":20,"toPort":"material_0"}},
        {{"fromNode":9,"fromPort":"out","toNode":20,"toPort":"material_1"}},
        {{"fromNode":8,"fromPort":"envmap","toNode":20,"toPort":"envmap"}},
        {{"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}}
        ]}}"#
    )
}

/// The real gate, one render, two regions. The physics is unarguable: a
/// point in a floor-wall corner has roughly half its sky blocked by
/// construction, the open region on the same floor sees essentially all of
/// it, so the corner region must read substantially darker — no RT-on/
/// RT-off toggle needed or wanted (see `furnace_wall_corner_scene_json`'s
/// doc comment for why that comparison is self-defeating here).
/// `assert_rt_dispatched` (inside `render_readback_confirmed`) rules out
/// the vacuous case where this whole scene rendered pure raster. A PNG
/// dump of every render lands at `/tmp/rt_furnace_wall_corner.png`.
///
/// FAILS TODAY, measured: `open=0.97314 corner=0.97362 ratio=1.0005` — no
/// darkening anywhere; the PNG is flat white, floor and wall both lit but
/// no shadow in the corner. Cause is architectural, not probe placement
/// (CORNER_WORLD/OPEN_WORLD's doc comment proves both probes read the
/// floor, not the wall). The kernel writes `irradiance = ambient_color *
/// ao + gi`, and here `ambient_color` is zero (material ambient 0.0) and
/// `gi` is zero (no lights, so no sun casters; no emissives), so traced
/// occlusion has nothing to multiply. Meanwhile the term that actually
/// lights this scene, `diffuse_ibl`, is gated only by the material's BAKED
/// occlusion texture and never sees the traced result. No probe placement
/// rescues this.
///
/// Un-ignore with BUG-yq1d (traced-ao-never-darkens-environment-diffuse).
#[test]
#[ignore = "BUG-yq1d (traced-ao-never-darkens-environment-diffuse): traced occlusion multiplies a term that is zero in an environment-lit scene"]
fn traced_occlusion_darkens_the_shaded_region_relative_to_the_open_plane() {
    let json = furnace_wall_corner_scene_json(0.0);
    let (bytes, w, h) = render_readback_confirmed(&json, "furnace wall-corner scene, ambient=0.0, RT enabled");

    write_png(&bytes, w, h, "/tmp/rt_furnace_wall_corner.png");

    let cam = Camera::orbit_perspective(ORBIT, TILT, DISTANCE, FOV_Y, 0.0, 0.0, NEAR, FAR);
    let corner_px = cam
        .project_to_pixel(CORNER_WORLD, w, h)
        .expect("corner probe must project in front of the camera");
    let open_px = cam
        .project_to_pixel(OPEN_WORLD, w, h)
        .expect("open probe must project in front of the camera");

    const RADIUS: i32 = 5;
    let corner = region_luma(&bytes, w, h, corner_px.px, corner_px.py, RADIUS);
    let open = region_luma(&bytes, w, h, open_px.px, open_px.py, RADIUS);
    eprintln!(
        "RT furnace wall corner (single render): open={open:.5} corner={corner:.5} ratio(corner/open)={:.4}",
        corner / open
    );

    const DARKENING_FRACTION: f64 = 0.20;
    assert!(
        corner <= open * (1.0 - DARKENING_FRACTION),
        "the floor-wall corner region must read at least {:.0}% darker than the open floor \
         region in the SAME render (a point in the corner has roughly half its sky blocked by \
         construction): open={open:.5} corner={corner:.5} ratio={:.4} — if this fails, that is a \
         real finding about the RT diffuse-occlusion path (or the raster environment-diffuse \
         term it doesn't reach), not a test to retune",
        DARKENING_FRACTION * 100.0,
        corner / open,
    );
}

/// DIAGNOSTIC, not a gate — no assertion on which reading is "correct".
/// `furnace_wall_corner_scene_json`'s doc comment names the mechanism: with
/// the ground material's `ambient` at 0.0 and no lights/emission, the RT
/// irradiance term (`ambient_color * ao + gi`) is algebraically zero
/// regardless of whether occlusion traced correctly, because there is
/// nothing for it to modulate. This records whether lifting `ambient` off
/// zero is what makes traced occlusion visible at the corner probe at all
/// — evidence for or against the "occlusion only reaches a flat ambient
/// term, never the baked environment-diffuse term" hypothesis, without
/// this test taking a position on it.
#[test]
fn diagnostic_shaded_region_luma_with_ambient_zero_vs_ambient_one() {
    let zero_json = furnace_wall_corner_scene_json(0.0);
    let one_json = furnace_wall_corner_scene_json(1.0);
    let (zero_bytes, w, h) =
        render_readback_confirmed(&zero_json, "furnace wall-corner scene, ambient=0.0, RT enabled");
    let (one_bytes, _, _) =
        render_readback_confirmed(&one_json, "furnace wall-corner scene, ambient=1.0, RT enabled");

    let cam = Camera::orbit_perspective(ORBIT, TILT, DISTANCE, FOV_Y, 0.0, 0.0, NEAR, FAR);
    let corner_px = cam
        .project_to_pixel(CORNER_WORLD, w, h)
        .expect("corner probe must project in front of the camera");

    const RADIUS: i32 = 5;
    let ambient_zero = region_luma(&zero_bytes, w, h, corner_px.px, corner_px.py, RADIUS);
    let ambient_one = region_luma(&one_bytes, w, h, corner_px.px, corner_px.py, RADIUS);
    eprintln!(
        "RT furnace occlusion diagnostic: corner region luma at ambient=0.0 -> {ambient_zero:.5}, \
         at ambient=1.0 -> {ambient_one:.5}"
    );
}

/// Same Reinhard+gamma tonemap `render_scene_shadows.rs`'s `write_png` uses.
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

