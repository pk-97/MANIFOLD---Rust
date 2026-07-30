//! `docs/RAYTRACING_DESIGN.md` section 12 — Screen-space AO handoff
//! (decisions AM1/AM2/AM6): `node.render_scene`'s lazy `ao_mask` output.
//! Value-level proof of the three documented behaviours, per pixel:
//!
//! - lit raster pixel (an ordinary `node.pbr_material` object) -> 1.0
//! - a `baked_look = true` `node.pbr_material` object (`MaterialKind::Unlit`)
//!   -> 0.0
//! - `rt_enabled && rt_ready` -> 0.0 everywhere a fragment is actually
//!   drawn, regardless of material kind
//! - background (no geometry, the aux attachment's own clear value) -> 1.0
//!
//! Camera/quad-facing setup mirrors `gbuffer_velocity.rs`: `node.grid_mesh`
//! generates in the XZ plane (normal `+Y`); `node.transform_3d`'s
//! `rot_z = PI/2` rotates that normal onto `+X`, directly facing the
//! `orbit=0, tilt=0` camera sitting on `+X`. Post-rotation the quad's
//! rectangle spans world Y (was X) and Z (unrotated), so `pos_y` shifts an
//! object along the image's vertical axis and a `pos_z`-free point at
//! `[0,0,BACKGROUND_Z]` lands well clear of both objects (which sit at
//! `z=0`) along the image's horizontal axis — same "translate along an
//! axis this camera maps to one screen dimension" trick, just borrowing the
//! OTHER axis for horizontal separation instead of vertical.
//!
//! Both objects need `envmap` wired: `node.pbr_material` always reports
//! `requires_envmap() == true` for its default (non-baked) `Pbr` kind —
//! `render_scene.rs`'s per-object loop (~line 3398) clears the WHOLE canvas
//! magenta and returns early if a `Pbr`-kind object's envmap is unwired.
//! `baked_look = true` flips that object's kind to `Unlit` before this
//! check runs (`pbr_material.rs`'s `run()`), so its `requires_envmap()` is
//! false — but the envmap is wired regardless since object_0 is a plain
//! `Pbr` material and needs it. `lights: 0` is fine: `render_scene.rs` never
//! runtime-gates on `requires_light()`, only `requires_envmap()` (grepped —
//! no `requires_light` call in that file), and the IBL envmap alone is
//! enough to light object_0 (`render_scene_ibl.rs`'s own zero-light scenes
//! do the same).

use half::f16;
use manifold_gpu::GpuTextureFormat;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::camera::Camera;
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;

use crate::harness;

const ORBIT: f32 = 0.0;
const TILT: f32 = 0.0;
const DISTANCE: f32 = 5.0;
const FOV_Y: f32 = 0.9;
const NEAR: f32 = 0.05;
const FAR: f32 = 200.0;
/// Matches `gbuffer_velocity.rs`'s `ROT_Z` — rotates the quad's normal from
/// `grid_mesh`'s native `+Y` to `+X`, directly facing the `orbit=0, tilt=0`
/// camera (which sits on `+X` looking toward `-X`).
const ROT_Z: f32 = std::f32::consts::FRAC_PI_2;
/// `0.1 * DISTANCE`, same sizing rule `gbuffer_velocity.rs` uses — small
/// enough that two objects at `+-OBJECT_OFFSET` don't overlap, large enough
/// to give each a multi-pixel region to probe.
fn quad_size() -> f32 {
    0.1 * DISTANCE
}
/// World-Y offset for each object (post-`ROT_Z` rotation, Y is the quad's
/// screen-vertical extent axis) — big enough to clear the other object's
/// `quad_size()/2` half-extent with headroom.
const OBJECT_OFFSET: f32 = 1.4;
/// World-Z point used for the background probe — lands on the image's
/// horizontal axis, well clear of both objects (which sit at `z=0`,
/// `+-quad_size()/2` wide).
const BACKGROUND_Z: f32 = 2.0;
/// Region-probe half-width in pixels. `quad_size()/2 == 0.25` world units
/// at this camera/distance projects to several pixels of half-extent
/// (empirically comfortable at this FOV/distance/canvas-size combination,
/// same order of magnitude as `gbuffer_velocity.rs`'s own probes) — `RADIUS`
/// stays well inside that, clear of the resolved-MSAA edge.
const RADIUS: i32 = 3;
/// RT-D4's async accel build needs a few frames to settle — same warm-up
/// discipline as `rt_p3_emissive_gi.rs`'s `RT_WARMUP_FRAMES`.
const RT_WARMUP_FRAMES: i64 = 16;

fn cam() -> Camera {
    Camera::orbit_perspective(ORBIT, TILT, DISTANCE, FOV_Y, 0.0, 0.0, NEAR, FAR)
}

/// `node.grid_mesh(2x2) -> node.make_triangles -> render_scene(2 objects, 0
/// lights, envmap-lit)`. Object 0: plain `node.pbr_material` (lit, requires
/// the wired envmap). Object 1: `node.pbr_material` with `baked_look =
/// true` (`MaterialKind::Unlit`). `ao_mask` is wired to `node.invert`'s dead
/// end when `wire_ao_mask` is true (the `gbuffer_depth.rs`/
/// `gbuffer_velocity.rs` trick for giving a lazy output a genuine binding
/// without a second `system.final_output`); `rt_enabled` is wired into
/// `render_scene`'s own param when `rt_enabled` is true.
fn scene_json(wire_ao_mask: bool, rt_enabled: bool) -> String {
    let size = quad_size();
    let rt_param = if rt_enabled {
        r#","rt_enabled":{"type":"Bool","value":true}"#
    } else {
        ""
    };
    let ao_mask_node = if wire_ao_mask {
        r#",{"id":21,"typeId":"node.invert","nodeId":"ao_sink","params":{}}"#
    } else {
        ""
    };
    let ao_mask_wire = if wire_ao_mask {
        r#",{"fromNode":20,"fromPort":"ao_mask","toNode":21,"toPort":"in"}"#
    } else {
        ""
    };
    format!(
        r#"{{"version":2,"name":"RenderSceneAoMaskProof","nodes":[
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
            "orbit":{{"type":"Float","value":{ORBIT}}},
            "tilt":{{"type":"Float","value":{TILT}}},
            "distance":{{"type":"Float","value":{DISTANCE}}},
            "fov_y":{{"type":"Float","value":{FOV_Y}}},
            "look_y":{{"type":"Float","value":0.0}},
            "roll":{{"type":"Float","value":0.0}},
            "near":{{"type":"Float","value":{NEAR}}},
            "far":{{"type":"Float","value":{FAR}}}}}}},
        {{"id":8,"typeId":"node.bake_environment","nodeId":"env","params":{{
            "width":{{"type":"Int","value":512}},
            "height":{{"type":"Int","value":256}},
            "intensity":{{"type":"Float","value":1.0}}}}}},
        {{"id":4,"typeId":"node.pbr_material","nodeId":"mat_lit","params":{{
            "color_r":{{"type":"Float","value":0.8}},
            "color_g":{{"type":"Float","value":0.8}},
            "color_b":{{"type":"Float","value":0.8}}}}}},
        {{"id":5,"typeId":"node.pbr_material","nodeId":"mat_baked","params":{{
            "color_r":{{"type":"Float","value":0.8}},
            "color_g":{{"type":"Float","value":0.8}},
            "color_b":{{"type":"Float","value":0.8}},
            "baked_look":{{"type":"Bool","value":true}}}}}},
        {{"id":6,"typeId":"node.transform_3d","nodeId":"xf0","params":{{
            "rot_z":{{"type":"Float","value":{ROT_Z}}},
            "pos_y":{{"type":"Float","value":{neg_offset}}}}}}},
        {{"id":7,"typeId":"node.transform_3d","nodeId":"xf1","params":{{
            "rot_z":{{"type":"Float","value":{ROT_Z}}},
            "pos_y":{{"type":"Float","value":{OBJECT_OFFSET}}}}}}},
        {{"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{{
            "objects":{{"type":"Int","value":2}},
            "lights":{{"type":"Int","value":0}}{rt_param}}}}}{ao_mask_node},
        {{"id":99,"typeId":"system.final_output","nodeId":"color_out"}}
        ],"wires":[
        {{"fromNode":1,"fromPort":"vertices","toNode":2,"toPort":"in"}},
        {{"fromNode":2,"fromPort":"out","toNode":20,"toPort":"mesh_0"}},
        {{"fromNode":2,"fromPort":"out","toNode":20,"toPort":"mesh_1"}},
        {{"fromNode":3,"fromPort":"out","toNode":20,"toPort":"camera"}},
        {{"fromNode":8,"fromPort":"envmap","toNode":20,"toPort":"envmap"}},
        {{"fromNode":4,"fromPort":"out","toNode":20,"toPort":"material_0"}},
        {{"fromNode":5,"fromPort":"out","toNode":20,"toPort":"material_1"}},
        {{"fromNode":6,"fromPort":"transform","toNode":20,"toPort":"transform_0"}},
        {{"fromNode":7,"fromPort":"transform","toNode":20,"toPort":"transform_1"}},
        {{"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}}{ao_mask_wire}
        ]}}"#,
        neg_offset = -OBJECT_OFFSET,
    )
}

/// Read an `R8Unorm` texture back to host memory as raw bytes (1 byte/px).
fn readback_r8unorm(device: &manifold_gpu::GpuDevice, texture: &manifold_gpu::GpuTexture) -> Vec<u8> {
    const BYTES_PER_PIXEL: u32 = 1;
    let bytes_per_row = texture.width * BYTES_PER_PIXEL;
    let total_bytes = u64::from(texture.height * bytes_per_row);
    let buf = device.create_buffer_shared(total_bytes);

    let mut enc = device.create_encoder("ao-mask-readback");
    enc.copy_texture_to_buffer(texture, &buf, texture.width, texture.height, bytes_per_row);
    enc.commit_and_wait_completed();

    let ptr = buf
        .mapped_ptr()
        .expect("shared readback buffer must expose mapped pointer");
    let bytes: &[u8] = unsafe {
        std::slice::from_raw_parts(ptr.cast::<std::ffi::c_void>().cast::<u8>(), total_bytes as usize)
    };
    bytes.to_vec()
}

fn region_minmax(bytes: &[u8], w: u32, h: u32, cx: f32, cy: f32, radius: i32) -> (f32, f32) {
    let cxi = cx.round() as i32;
    let cyi = cy.round() as i32;
    let mut lo = f32::MAX;
    let mut hi = f32::MIN;
    let mut n = 0u32;
    for dy in -radius..=radius {
        for dx in -radius..=radius {
            let x = cxi + dx;
            let y = cyi + dy;
            if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
                continue;
            }
            let idx = (y as u32 * w + x as u32) as usize;
            let v = bytes[idx] as f32 / 255.0;
            lo = lo.min(v);
            hi = hi.max(v);
            n += 1;
        }
    }
    assert!(n > 0, "region window is entirely off-screen");
    (lo, hi)
}

/// Build + render `json`, returning the `ao_mask` readback (bytes, w, h) and
/// the `color` readback bytes, after `frames` committed calls (>=1; the
/// last one is what gets read back).
fn render_and_read(json: &str, frames: i64) -> (Vec<u8>, u32, u32, Vec<u8>) {
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
    .unwrap_or_else(|e| panic!("render_scene_ao_mask graph must build: {e}\n{json}"));
    runtime.set_dump_all(true);

    let target = h.make_target("render-scene-ao-mask");
    for frame in 0..frames {
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
        let mut enc = h.device.create_encoder("render-scene-ao-mask-enc");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
            runtime.render(&mut gpu, &target.texture, &ctx, &manifold_core::params::ParamManifest::default());
        }
        enc.commit_and_wait_completed();
    }

    let color_bytes = h.readback(&target.texture);
    let dumped = runtime.dump_textures_all();
    let ao_entry = dumped
        .iter()
        .find(|(node_id, port, _, _)| node_id == "scene" && port == "ao_mask");
    let (ao_bytes, aw, ah) = match ao_entry {
        Some((_, _, _, tex)) => {
            assert_eq!(
                tex.format,
                GpuTextureFormat::R8Unorm,
                "ao_mask's allocated texture must be R8Unorm (output_format override)"
            );
            (readback_r8unorm(&h.device, tex), tex.width, tex.height)
        }
        None => (Vec::new(), 0, 0),
    };
    (ao_bytes, aw, ah, color_bytes)
}

#[test]
fn lit_vs_baked_look_rt_off() {
    let json = scene_json(true, false);
    let (ao_bytes, w, h, _color) = render_and_read(&json, 2);
    assert_eq!((w, h), (harness::PARITY_WIDTH, harness::PARITY_HEIGHT), "ao_mask dims must match canvas");

    let camera = cam();
    let lit_px = camera
        .project_to_pixel([0.0, -OBJECT_OFFSET, 0.0], w, h)
        .expect("lit object center must project in front of the camera");
    let baked_px = camera
        .project_to_pixel([0.0, OBJECT_OFFSET, 0.0], w, h)
        .expect("baked-look object center must project in front of the camera");
    let bg_px = camera
        .project_to_pixel([0.0, 0.0, BACKGROUND_Z], w, h)
        .expect("background probe point must project in front of the camera");

    let (lit_lo, _lit_hi) = region_minmax(&ao_bytes, w, h, lit_px.px, lit_px.py, RADIUS);
    let (_baked_lo, baked_hi) = region_minmax(&ao_bytes, w, h, baked_px.px, baked_px.py, RADIUS);
    let (bg_lo, _bg_hi) = region_minmax(&ao_bytes, w, h, bg_px.px, bg_px.py, RADIUS);

    eprintln!(
        "lit region ({:.0},{:.0}) min={lit_lo:.4} | baked region ({:.0},{:.0}) max={baked_hi:.4} | \
         background ({:.0},{:.0}) min={bg_lo:.4}",
        lit_px.px, lit_px.py, baked_px.px, baked_px.py, bg_px.px, bg_px.py
    );

    assert!(
        lit_lo >= 0.99,
        "lit object region must read >= 0.99 (every raster pixel of a lit-kind material is \
         ao_mask_owed = 1): got min {lit_lo:.4} at ({:.0},{:.0})",
        lit_px.px,
        lit_px.py
    );
    assert!(
        baked_hi <= 0.01,
        "baked-look object region must read <= 0.01 (MaterialKind::Unlit writes ao_mask_owed = 0): \
         got max {baked_hi:.4} at ({:.0},{:.0})",
        baked_px.px,
        baked_px.py
    );
    assert!(
        bg_lo >= 0.99,
        "background region must read >= 0.99 (the aux attachment clears to 1, and no fragment \
         ever writes there): got min {bg_lo:.4} at ({:.0},{:.0})",
        bg_px.px,
        bg_px.py
    );
}

/// RT-on gate: with `rt_enabled` true and `rt_ready` latched, EVERY raster
/// fragment (lit or baked-look) writes `ao_mask_owed = 0`, per
/// `render_scene.rs`'s `fog_params[2]` write (`material.kind ==
/// MaterialKind::Unlit || (rt_enabled && rt_ready)`). This does NOT extend
/// to the background: `render_scene.rs`'s own comment on the aux
/// attachment's clear value ("ao_mask 1 (background keeps full screen-space
/// AO)") documents that background pixels are never touched by any
/// fragment invocation, RT or not — so the background probe stays >= 0.99
/// here too. That's why this test asserts "every drawn pixel" rather than
/// literally every pixel in the canvas.
#[test]
fn rt_enabled_and_ready_forces_zero_everywhere_drawn() {
    let json = scene_json(true, true);
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
    .unwrap_or_else(|e| panic!("RT-on ao_mask graph must build: {e}\n{json}"));
    runtime.set_dump_all(true);

    let target = h.make_target("render-scene-ao-mask-rt");
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
        let mut enc = h.device.create_encoder("render-scene-ao-mask-rt-enc");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
            runtime.render(&mut gpu, &target.texture, &ctx, &manifold_core::params::ParamManifest::default());
        }
        enc.commit_and_wait_completed();
    }

    let dumped = runtime.dump_textures_all();
    let (_, _, _, ao_tex) = dumped
        .iter()
        .find(|(node_id, port, _, _)| node_id == "scene" && port == "ao_mask")
        .expect("ao_mask must be dumped once wired");
    assert_eq!(ao_tex.format, GpuTextureFormat::R8Unorm);
    let (w, h_) = (ao_tex.width, ao_tex.height);
    let ao_bytes = readback_r8unorm(&harness::shared().device, ao_tex);

    let camera = cam();
    let lit_px = camera
        .project_to_pixel([0.0, -OBJECT_OFFSET, 0.0], w, h_)
        .expect("lit object center must project in front of the camera");
    let baked_px = camera
        .project_to_pixel([0.0, OBJECT_OFFSET, 0.0], w, h_)
        .expect("baked-look object center must project in front of the camera");
    let bg_px = camera
        .project_to_pixel([0.0, 0.0, BACKGROUND_Z], w, h_)
        .expect("background probe point must project in front of the camera");

    let (_lit_lo, lit_hi) = region_minmax(&ao_bytes, w, h_, lit_px.px, lit_px.py, RADIUS);
    let (_baked_lo, baked_hi) = region_minmax(&ao_bytes, w, h_, baked_px.px, baked_px.py, RADIUS);
    let (bg_lo, _bg_hi) = region_minmax(&ao_bytes, w, h_, bg_px.px, bg_px.py, RADIUS);

    eprintln!(
        "RT-on: previously-lit region max={lit_hi:.4} | previously-baked region max={baked_hi:.4} | \
         background min={bg_lo:.4}"
    );

    assert!(
        lit_hi <= 0.01,
        "with rt_enabled && rt_ready, the FORMERLY-lit object's region (1.0 with RT off) must now \
         read <= 0.01: got max {lit_hi:.4}"
    );
    assert!(
        baked_hi <= 0.01,
        "with rt_enabled && rt_ready, the baked-look object's region must still read <= 0.01: got \
         max {baked_hi:.4}"
    );
    assert!(
        bg_lo >= 0.99,
        "background must stay >= 0.99 even with RT on — no fragment ever writes there, RT or not: \
         got min {bg_lo:.4}"
    );
}

/// Lazy inertness: the same scene rendered with `ao_mask` NEVER wired must
/// not crash, and its `color` output must match case A's `color` output
/// within `COLOR_EPS` at each probed region — NOT byte-equality, since
/// wiring `ao_mask` selects a different compiled pipeline variant (an extra
/// MRT attachment), and that's legitimately allowed to perturb float
/// results at the ULP level without being a bug.
#[test]
fn unwired_ao_mask_stays_inert_color_output_matches_within_epsilon() {
    const COLOR_EPS: f32 = 5e-3;

    let wired_json = scene_json(true, false);
    let (_, _, _, wired_color) = render_and_read(&wired_json, 2);

    let unwired_json = scene_json(false, false);
    let (ao_bytes, aw, ah, unwired_color) = render_and_read(&unwired_json, 2);
    assert!(ao_bytes.is_empty() && aw == 0 && ah == 0, "ao_mask must not be dumped when unwired");

    let camera = cam();
    let probes = [
        ("lit", [0.0f32, -OBJECT_OFFSET, 0.0]),
        ("baked", [0.0, OBJECT_OFFSET, 0.0]),
        ("background", [0.0, 0.0, BACKGROUND_Z]),
    ];

    for (name, world) in probes {
        let px = camera
            .project_to_pixel(world, harness::PARITY_WIDTH, harness::PARITY_HEIGHT)
            .unwrap_or_else(|| panic!("{name} probe point must project in front of the camera"));
        let cx = px.px.round() as i32;
        let cy = px.py.round() as i32;
        let mut max_delta = 0.0f32;
        for dy in -RADIUS..=RADIUS {
            for dx in -RADIUS..=RADIUS {
                let x = cx + dx;
                let y = cy + dy;
                if x < 0 || y < 0 || x >= harness::PARITY_WIDTH as i32 || y >= harness::PARITY_HEIGHT as i32 {
                    continue;
                }
                let idx = ((y as u32 * harness::PARITY_WIDTH + x as u32) * 8) as usize;
                for c in 0..4 {
                    let a = f16::from_le_bytes([wired_color[idx + c * 2], wired_color[idx + c * 2 + 1]]).to_f32();
                    let b =
                        f16::from_le_bytes([unwired_color[idx + c * 2], unwired_color[idx + c * 2 + 1]]).to_f32();
                    assert!(a.is_finite() && b.is_finite(), "{name} region: non-finite color channel");
                    max_delta = max_delta.max((a - b).abs());
                }
            }
        }
        eprintln!("{name} region ({:.0},{:.0}): max color delta = {max_delta:.6}", px.px, px.py);
        assert!(
            max_delta < COLOR_EPS,
            "{name} region ({:.0},{:.0}): color delta {max_delta:.6} exceeds epsilon {COLOR_EPS} — \
             wiring ao_mask must not perturb color beyond pipeline-variant float noise",
            px.px,
            px.py
        );
    }
}

// ─── I-AM2 / I-AM3: group-level proofs (RAYTRACING_DESIGN.md section 12.3) ─
//
// These prove the CONSUMER side of the handoff: the importer's "Ambient
// Occlusion" node group (`gltf_import/scene.rs:628-694`) actually reads
// `ao_mask` correctly, not just that `render_scene` emits the right values
// (the earlier tests in this file already cover that half).
//
// Built as the FLATTENED atom equivalent of the group rather than an
// authored `node.group` — same atoms, same wires, same params, just without
// the group wrapper (the brief's own suggested simplification; groups
// flatten to this exact shape at load per `docs/NODE_GROUPS_DESIGN.md`, so
// this is not a shortcut on the proof, only on how the JSON is authored):
// `ssao_gtao -> bilateral_blur(H) -> bilateral_blur(V) -> node.mix(Multiply)
// -> node.masked_mix`. Wires mirror `gltf_import/scene.rs` exactly: `camera`
// feeds `ssao`/`bilat_h`/`bilat_v` DIRECTLY from the camera node (not through
// `render_scene`), `depth` and `color` come from `render_scene`, `ao_mask`
// feeds `masked_mix.mask`, and `masked_mix.a` (untouched passthrough) is
// `render_scene.color` again — `masked_mix(a=color, b=ssao_mix_out, mask)`
// selects `a` at mask=0, `b` at mask=1 (confirmed against `masked_mix.rs`'s
// own `mask=0 -> a` / `mask=1 -> b` gpu_tests).
//
// Fixture: a ground `node.grid_mesh` (XZ plane, native `+Y` normal, matches
// `rt_p3_emissive_gi.rs`'s ground) plus two `node.cube_mesh` objects sitting
// ON the ground (`pos_y = size/2`, cube spans `+-size/2` per
// `generate_cube_mesh_body.wgsl`) at `x = -CUBE_OFFSET` (lit) and
// `x = +CUBE_OFFSET` (baked-look) — real geometric contact corners where
// `ssao_gtao` computes non-trivial occlusion, so the baked-look leg proves
// masking actually suppressed a REAL AO term, not a zero-magnitude one.
// `ssao_gtao`/`bilateral_blur` read screen-space depth and are unaware of
// per-object materials, so the ground's own material doesn't matter to
// which pixels show geometric occlusion — only `masked_mix`'s per-pixel
// `ao_mask` decides whether that occlusion reaches the final color.
//
// "Group input"/"group output" are read directly via `dump_textures_all`
// (no `node.invert` dead-end sink needed here, unlike the lazy `ao_mask`
// proofs above — `render_scene.color` is never lazy, and `masked_mix.out` is
// wired to `system.final_output`, so both are live plan outputs already).

const CUBE_SIZE: f32 = 1.4;
const CUBE_OFFSET: f32 = 2.0;
const AO_ORBIT: f32 = 0.7;
const AO_TILT: f32 = 0.95;
const AO_DISTANCE: f32 = 10.0;
const AO_FOV_Y: f32 = 0.8;
const AO_NEAR: f32 = 0.05;
const AO_FAR: f32 = 200.0;
const AO_RADIUS: f32 = 1.0;

fn ao_group_scene_json(rt_enabled: bool) -> String {
    let rt_param = if rt_enabled {
        r#","rt_enabled":{"type":"Bool","value":true}"#
    } else {
        ""
    };
    format!(
        r#"{{"version":2,"name":"RenderSceneAoGroupProof","nodes":[
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
        {{"id":3,"typeId":"node.cube_mesh","nodeId":"cube_lit","params":{{
            "size":{{"type":"Float","value":{CUBE_SIZE}}}}}}},
        {{"id":4,"typeId":"node.cube_mesh","nodeId":"cube_baked","params":{{
            "size":{{"type":"Float","value":{CUBE_SIZE}}}}}}},
        {{"id":5,"typeId":"node.transform_3d","nodeId":"xf_lit","params":{{
            "pos_x":{{"type":"Float","value":{neg_offset}}},
            "pos_y":{{"type":"Float","value":{half_size}}}}}}},
        {{"id":6,"typeId":"node.transform_3d","nodeId":"xf_baked","params":{{
            "pos_x":{{"type":"Float","value":{CUBE_OFFSET}}},
            "pos_y":{{"type":"Float","value":{half_size}}}}}}},
        {{"id":7,"typeId":"node.orbit_camera","nodeId":"cam","params":{{
            "orbit":{{"type":"Float","value":{AO_ORBIT}}},
            "tilt":{{"type":"Float","value":{AO_TILT}}},
            "distance":{{"type":"Float","value":{AO_DISTANCE}}},
            "fov_y":{{"type":"Float","value":{AO_FOV_Y}}},
            "near":{{"type":"Float","value":{AO_NEAR}}},
            "far":{{"type":"Float","value":{AO_FAR}}}}}}},
        {{"id":8,"typeId":"node.bake_environment","nodeId":"env","params":{{
            "width":{{"type":"Int","value":512}},
            "height":{{"type":"Int","value":256}},
            "intensity":{{"type":"Float","value":1.0}}}}}},
        {{"id":9,"typeId":"node.pbr_material","nodeId":"mat_ground","params":{{
            "color_r":{{"type":"Float","value":0.7}},
            "color_g":{{"type":"Float","value":0.7}},
            "color_b":{{"type":"Float","value":0.7}}}}}},
        {{"id":10,"typeId":"node.pbr_material","nodeId":"mat_lit","params":{{
            "color_r":{{"type":"Float","value":0.8}},
            "color_g":{{"type":"Float","value":0.8}},
            "color_b":{{"type":"Float","value":0.8}}}}}},
        {{"id":11,"typeId":"node.pbr_material","nodeId":"mat_baked","params":{{
            "color_r":{{"type":"Float","value":0.8}},
            "color_g":{{"type":"Float","value":0.8}},
            "color_b":{{"type":"Float","value":0.8}},
            "baked_look":{{"type":"Bool","value":true}}}}}},
        {{"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{{
            "objects":{{"type":"Int","value":3}},
            "lights":{{"type":"Int","value":0}}{rt_param}}}}},
        {{"id":30,"typeId":"node.ssao_gtao","nodeId":"ssao","params":{{
            "radius":{{"type":"Float","value":{AO_RADIUS}}},
            "intensity":{{"type":"Float","value":1.0}}}}}},
        {{"id":31,"typeId":"node.bilateral_blur","nodeId":"bilat_h","params":{{
            "axis":{{"type":"Enum","value":0}},
            "depth_sigma":{{"type":"Float","value":0.1}}}}}},
        {{"id":32,"typeId":"node.bilateral_blur","nodeId":"bilat_v","params":{{
            "axis":{{"type":"Enum","value":1}},
            "depth_sigma":{{"type":"Float","value":0.1}}}}}},
        {{"id":33,"typeId":"node.mix","nodeId":"ssao_mix","params":{{
            "amount":{{"type":"Float","value":1.0}},
            "mode":{{"type":"Enum","value":4}}}}}},
        {{"id":34,"typeId":"node.masked_mix","nodeId":"mask_mix","params":{{}}}},
        {{"id":99,"typeId":"system.final_output","nodeId":"final"}}
        ],"wires":[
        {{"fromNode":1,"fromPort":"vertices","toNode":2,"toPort":"in"}},
        {{"fromNode":2,"fromPort":"out","toNode":20,"toPort":"mesh_0"}},
        {{"fromNode":3,"fromPort":"vertices","toNode":20,"toPort":"mesh_1"}},
        {{"fromNode":4,"fromPort":"vertices","toNode":20,"toPort":"mesh_2"}},
        {{"fromNode":5,"fromPort":"transform","toNode":20,"toPort":"transform_1"}},
        {{"fromNode":6,"fromPort":"transform","toNode":20,"toPort":"transform_2"}},
        {{"fromNode":7,"fromPort":"out","toNode":20,"toPort":"camera"}},
        {{"fromNode":8,"fromPort":"envmap","toNode":20,"toPort":"envmap"}},
        {{"fromNode":9,"fromPort":"out","toNode":20,"toPort":"material_0"}},
        {{"fromNode":10,"fromPort":"out","toNode":20,"toPort":"material_1"}},
        {{"fromNode":11,"fromPort":"out","toNode":20,"toPort":"material_2"}},
        {{"fromNode":20,"fromPort":"depth","toNode":30,"toPort":"depth"}},
        {{"fromNode":7,"fromPort":"out","toNode":30,"toPort":"camera"}},
        {{"fromNode":30,"fromPort":"out","toNode":31,"toPort":"in"}},
        {{"fromNode":20,"fromPort":"depth","toNode":31,"toPort":"depth"}},
        {{"fromNode":7,"fromPort":"out","toNode":31,"toPort":"camera"}},
        {{"fromNode":31,"fromPort":"out","toNode":32,"toPort":"in"}},
        {{"fromNode":20,"fromPort":"depth","toNode":32,"toPort":"depth"}},
        {{"fromNode":7,"fromPort":"out","toNode":32,"toPort":"camera"}},
        {{"fromNode":20,"fromPort":"color","toNode":33,"toPort":"a"}},
        {{"fromNode":32,"fromPort":"out","toNode":33,"toPort":"b"}},
        {{"fromNode":20,"fromPort":"color","toNode":34,"toPort":"a"}},
        {{"fromNode":33,"fromPort":"out","toNode":34,"toPort":"b"}},
        {{"fromNode":20,"fromPort":"ao_mask","toNode":34,"toPort":"mask"}},
        {{"fromNode":34,"fromPort":"out","toNode":99,"toPort":"in"}}
        ]}}"#,
        neg_offset = -CUBE_OFFSET,
        half_size = CUBE_SIZE / 2.0,
    )
}

fn ao_group_cam() -> Camera {
    Camera::orbit_perspective(AO_ORBIT, AO_TILT, AO_DISTANCE, AO_FOV_Y, 0.0, 0.0, AO_NEAR, AO_FAR)
}

/// Render `json` for `frames` committed calls, returning `(group_input,
/// group_output, w, h)` — `render_scene.color` (the AO group's `color`
/// input) and `mask_mix.out` (the group's `out`), both read via
/// `dump_textures_all` (neither is lazy: `color` always computes,
/// `mask_mix.out` is wired to `system.final_output`).
fn render_ao_group(json: &str, frames: i64) -> (Vec<u8>, Vec<u8>, u32, u32) {
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
    .unwrap_or_else(|e| panic!("ao_group graph must build: {e}\n{json}"));
    runtime.set_dump_all(true);

    let target = h.make_target("render-scene-ao-group");
    for frame in 0..frames {
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
        let mut enc = h.device.create_encoder("render-scene-ao-group-enc");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
            runtime.render(&mut gpu, &target.texture, &ctx, &manifold_core::params::ParamManifest::default());
        }
        enc.commit_and_wait_completed();
    }

    let dumped = runtime.dump_textures_all();
    let find = |node: &str, port: &str| -> (Vec<u8>, u32, u32) {
        let (_, _, _, tex) = dumped
            .iter()
            .find(|(n, p, _, _)| n == node && p == port)
            .unwrap_or_else(|| {
                panic!(
                    "no dumped `{port}` output on node `{node}` — dumped ports: {:?}",
                    dumped.iter().map(|(n, p, _, _)| format!("{n}.{p}")).collect::<Vec<_>>()
                )
            });
        (h.readback(tex), tex.width, tex.height)
    };
    let (group_in, iw, ih) = find("scene", "color");
    let (group_out, ow, oh) = find("mask_mix", "out");
    assert_eq!((iw, ih), (ow, oh), "group input/output dims must match");
    (group_in, group_out, iw, ih)
}

fn luma_rgba16f(bytes: &[u8], idx: usize) -> f32 {
    let r = f16::from_le_bytes([bytes[idx], bytes[idx + 1]]).to_f32();
    let g = f16::from_le_bytes([bytes[idx + 2], bytes[idx + 3]]).to_f32();
    let b = f16::from_le_bytes([bytes[idx + 4], bytes[idx + 5]]).to_f32();
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

/// Region probe: returns `(max_abs_channel_delta, mean_luma_in, mean_luma_out)`
/// over a `radius`-pixel window around `(cx, cy)`.
fn ao_region_probe(
    group_in: &[u8],
    group_out: &[u8],
    w: u32,
    h: u32,
    cx: f32,
    cy: f32,
    radius: i32,
) -> (f32, f64, f64) {
    let cxi = cx.round() as i32;
    let cyi = cy.round() as i32;
    let mut max_delta = 0.0f32;
    let mut luma_in_sum = 0.0f64;
    let mut luma_out_sum = 0.0f64;
    let mut n = 0u64;
    for dy in -radius..=radius {
        for dx in -radius..=radius {
            let x = cxi + dx;
            let y = cyi + dy;
            if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
                continue;
            }
            let idx = ((y as u32 * w + x as u32) * 8) as usize;
            for c in 0..4 {
                let a = f16::from_le_bytes([group_in[idx + c * 2], group_in[idx + c * 2 + 1]]).to_f32();
                let b = f16::from_le_bytes([group_out[idx + c * 2], group_out[idx + c * 2 + 1]]).to_f32();
                assert!(a.is_finite() && b.is_finite(), "non-finite AO-group channel at ({x},{y})");
                max_delta = max_delta.max((a - b).abs());
            }
            luma_in_sum += luma_rgba16f(group_in, idx) as f64;
            luma_out_sum += luma_rgba16f(group_out, idx) as f64;
            n += 1;
        }
    }
    assert!(n > 0, "AO region window is entirely off-screen");
    (max_delta, luma_in_sum / n as f64, luma_out_sum / n as f64)
}

/// I-AM2: with `rt_enabled && rt_ready`, `render_scene` writes `ao_mask = 0`
/// everywhere a fragment draws (proven in `rt_enabled_and_ready_forces_zero_
/// everywhere_drawn` above) — `masked_mix`'s `mix(a, b, weight=0) == a`
/// exactly (its own `wgsl_body`: `clamp(mask.r * amount, 0, 1)` then
/// `mix(a,b,weight)`), so the WHOLE group must be an identity on colour: BUG-tgbd.
#[test]
fn rt_ready_makes_ao_group_identity_on_color() {
    let json = ao_group_scene_json(true);
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
    .unwrap_or_else(|e| panic!("I-AM2 graph must build: {e}\n{json}"));
    runtime.set_dump_all(true);

    let target = h.make_target("ao-group-rt-identity");
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
        let mut enc = h.device.create_encoder("ao-group-rt-identity-enc");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
            runtime.render(&mut gpu, &target.texture, &ctx, &manifold_core::params::ParamManifest::default());
        }
        enc.commit_and_wait_completed();
    }

    let dumped = runtime.dump_textures_all();
    let get = |node: &str, port: &str| -> (Vec<u8>, u32, u32) {
        let (_, _, _, tex) = dumped
            .iter()
            .find(|(n, p, _, _)| n == node && p == port)
            .unwrap_or_else(|| panic!("no dumped `{port}` on `{node}`"));
        (h.readback(tex), tex.width, tex.height)
    };
    let (group_in, w, ht) = get("scene", "color");
    let (group_out, _, _) = get("mask_mix", "out");

    let camera = ao_group_cam();
    let lit_px = camera
        .project_to_pixel([-CUBE_OFFSET, 0.1, 0.0], w, ht)
        .expect("lit cube base must project in front of the camera");

    const AO_RADIUS_PX: i32 = 6;
    let (max_delta, luma_in, luma_out) =
        ao_region_probe(&group_in, &group_out, w, ht, lit_px.px, lit_px.py, AO_RADIUS_PX);

    eprintln!(
        "[I-AM2] RT-on AO-group identity check at lit-cube-base region ({:.0},{:.0}): \
         max channel delta = {max_delta:.6}, luma_in = {luma_in:.4}, luma_out = {luma_out:.4}",
        lit_px.px, lit_px.py
    );

    const IDENTITY_EPS: f32 = 2e-3;
    assert!(
        max_delta < IDENTITY_EPS,
        "I-AM2 (BUG-tgbd): with rt_enabled && rt_ready the AO group must be an identity on \
         colour (masked_mix mask=0 -> a everywhere), but the lit-cube-base region differs by up \
         to {max_delta:.6} (epsilon {IDENTITY_EPS}) — RT scene is still getting double-darkened"
    );
}

/// I-AM3: RT off. Two legs, both required:
///   (a) baked-look object region: group output == group input within
///       epsilon (masked_mix mask=0 there -> untouched), even though
///       `ssao_gtao` computes REAL occlusion at that contact corner (same
///       geometry as the lit leg) — proves masking suppressed a real term,
///       not a zero one.
///   (b) lit object region: group output is measurably DARKER than group
///       input, by a stated relative-luma threshold — proves the AO group
///       still darkens ordinary lit geometry. BUG-ay0e.
#[test]
fn baked_look_passes_through_lit_neighbour_darkens() {
    let json = ao_group_scene_json(false);
    let (group_in, group_out, w, h) = render_ao_group(&json, 3);

    let camera = ao_group_cam();
    let lit_px = camera
        .project_to_pixel([-CUBE_OFFSET, 0.1, 0.0], w, h)
        .expect("lit cube base must project in front of the camera");
    let baked_px = camera
        .project_to_pixel([CUBE_OFFSET, 0.1, 0.0], w, h)
        .expect("baked-look cube base must project in front of the camera");

    const AO_RADIUS_PX: i32 = 6;
    let (lit_delta, lit_luma_in, lit_luma_out) =
        ao_region_probe(&group_in, &group_out, w, h, lit_px.px, lit_px.py, AO_RADIUS_PX);
    let (baked_delta, baked_luma_in, baked_luma_out) =
        ao_region_probe(&group_in, &group_out, w, h, baked_px.px, baked_px.py, AO_RADIUS_PX);

    let lit_darkening = (lit_luma_in - lit_luma_out) / lit_luma_in.max(1e-9);
    eprintln!(
        "[I-AM3] lit-cube-base region ({:.0},{:.0}): max channel delta = {lit_delta:.6}, \
         luma_in = {lit_luma_in:.4}, luma_out = {lit_luma_out:.4}, relative darkening = {:.2}%",
        lit_px.px,
        lit_px.py,
        lit_darkening * 100.0
    );
    eprintln!(
        "[I-AM3] baked-look-cube-base region ({:.0},{:.0}): max channel delta = {baked_delta:.6}, \
         luma_in = {baked_luma_in:.4}, luma_out = {baked_luma_out:.4}",
        baked_px.px, baked_px.py
    );

    const IDENTITY_EPS: f32 = 2e-3;
    const DARKEN_THRESHOLD: f64 = 0.02;
    assert!(
        baked_delta < IDENTITY_EPS,
        "I-AM3 leg (a) (BUG-ay0e): baked-look region must pass the AO group unchanged (max \
         channel delta {baked_delta:.6} exceeds epsilon {IDENTITY_EPS}) — masked_mix is not \
         gating this object's AO"
    );
    assert!(
        lit_darkening > DARKEN_THRESHOLD,
        "I-AM3 leg (b): lit-neighbour region must darken measurably vs its own group input \
         (relative darkening {:.2}% does not exceed {:.0}%) — the AO group stopped darkening \
         ordinary lit geometry",
        lit_darkening * 100.0,
        DARKEN_THRESHOLD * 100.0
    );
}
