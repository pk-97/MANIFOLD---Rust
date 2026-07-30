//! BUG-17r3 (lift RT zero-light gate): value-level proof that RT GI/AO/
//! reflections run on a scene with ZERO lights — the pass-level gate
//! (`render_scene.rs`'s `will_rt_accumulate_this_frame` /
//! `uniforms.scene_params[3]` / the `rt_enabled && has_casters` accel-and-
//! dispatch block) predates GI: raytracing began as sun shadows (which
//! need a caster), and the GI/emissive gather inherited that switch. An
//! emissive-only zero-light scene got NO raytraced GI at all — this is
//! the test that would have caught it (`tools/rt_prototype/compare/
//! RtBleed.json`/`RtAmbientOnly.json`, both `lights: 0`, rendered pure
//! raster and byte-identical at any bounce depth before the fix).
//!
//! One ground plane (receiving geometry, white albedo, zero ambient) +
//! one emissive quad above it, `rt_enabled: true`, `lights: 0` — no
//! `node.light` in the graph at all. With zero lights and zero ambient,
//! the raster combine's `direct`/`ambient` terms are both exactly zero
//! (same algebra `rt_p3_emissive_gi.rs`'s proof 1 states), so ANY
//! brightening of the ground near the emitter must come from the RT GI
//! gather's emissive-hit term — the CPU-stated floor below is not a
//! rounding margin, it is the bar "GI clearly ran, this isn't noise".

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

/// Same RT-D4 async-accel-build settle window `rt_p3_emissive_gi.rs` uses.
const RT_WARMUP_FRAMES: i64 = 16;

const EMIT: [f32; 3] = [0.4, 0.15, 0.6];

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
    .expect("BUG-17r3 zero-light scene graph must build");

    let target = h.make_target("rt-bug17r3-lightless-gi");
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
        let mut enc = h.device.create_encoder("rt-bug17r3-lightless-gi-enc");
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

/// Ground(8x8)+emitter(3x3 @ y=1.5) — `rt_p3_emissive_gi.rs`'s
/// `ground_emitter_scene_json` geometry/camera, minus the sun entirely
/// (no `node.light` node, `lights: 0`) — this is the exact case the
/// caster gate broke.
fn lightless_ground_emitter_scene_json(emit_on: bool) -> String {
    let (er, eg, eb) = if emit_on { (EMIT[0], EMIT[1], EMIT[2]) } else { (0.0, 0.0, 0.0) };
    format!(
        r#"{{"version":2,"name":"RtBug17r3LightlessGi","nodes":[
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
        {{"id":5,"typeId":"node.grid_mesh","nodeId":"emitter_grid","params":{{
            "max_capacity":{{"type":"Int","value":8192}},
            "resolution_x":{{"type":"Int","value":10}},
            "resolution_y":{{"type":"Int","value":10}},
            "size_x":{{"type":"Float","value":3.0}},
            "size_y":{{"type":"Float","value":3.0}}}}}},
        {{"id":6,"typeId":"node.make_triangles","nodeId":"emitter_tris","params":{{
            "src_cols":{{"type":"Int","value":10}},
            "src_rows":{{"type":"Int","value":10}}}}}},
        {{"id":7,"typeId":"node.transform_3d","nodeId":"emitter_xform","params":{{
            "pos_y":{{"type":"Float","value":1.5}}}}}},
        {{"id":3,"typeId":"node.orbit_camera","nodeId":"cam","params":{{
            "orbit":{{"type":"Float","value":{ORBIT}}},
            "tilt":{{"type":"Float","value":{TILT}}},
            "distance":{{"type":"Float","value":{DISTANCE}}},
            "fov_y":{{"type":"Float","value":{FOV_Y}}}}}}},
        {{"id":4,"typeId":"node.phong_material","nodeId":"ground_mat","params":{{
            "color_r":{{"type":"Float","value":1.0}},
            "color_g":{{"type":"Float","value":1.0}},
            "color_b":{{"type":"Float","value":1.0}},
            "ambient":{{"type":"Float","value":0.0}}}}}},
        {{"id":8,"typeId":"node.phong_material","nodeId":"emitter_mat","params":{{
            "color_r":{{"type":"Float","value":0.02}},
            "color_g":{{"type":"Float","value":0.02}},
            "color_b":{{"type":"Float","value":0.02}},
            "ambient":{{"type":"Float","value":0.0}},
            "emission_r":{{"type":"Float","value":{er}}},
            "emission_g":{{"type":"Float","value":{eg}}},
            "emission_b":{{"type":"Float","value":{eb}}},
            "emission_intensity":{{"type":"Float","value":1.0}}}}}},
        {{"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{{
            "objects":{{"type":"Int","value":2}},
            "lights":{{"type":"Int","value":0}},
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
        {{"fromNode":8,"fromPort":"out","toNode":20,"toPort":"material_1"}},
        {{"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}}
        ]}}"#
    )
}

/// Same neighbor probe `rt_p3_emissive_gi.rs` uses.
const NEIGHBOR_WORLD: [f32; 3] = [1.0, 0.0, -1.0];

#[test]
fn zero_light_scene_still_gathers_emissive_gi_on_receiving_geometry() {
    let (on_bytes, w, h) = render_readback(&lightless_ground_emitter_scene_json(true));
    let (off_bytes, _, _) = render_readback(&lightless_ground_emitter_scene_json(false));

    let cam = Camera::orbit_perspective(ORBIT, TILT, DISTANCE, FOV_Y, 0.0, 0.0, NEAR, FAR);
    let neighbor_px = cam
        .project_to_pixel(NEIGHBOR_WORLD, w, h)
        .expect("neighbor probe point must project in front of the camera");

    const RADIUS: i32 = 7; // 15x15 window, same as rt_p3_emissive_gi.rs
    let on = region_luma(&on_bytes, w, h, neighbor_px.px, neighbor_px.py, RADIUS);
    let off = region_luma(&off_bytes, w, h, neighbor_px.px, neighbor_px.py, RADIUS);
    eprintln!(
        "BUG-17r3 zero-light neighbor region (pixel ({:.0},{:.0})): off={off:.5} on={on:.5}",
        neighbor_px.px, neighbor_px.py
    );

    // CPU-stated floor #1: with zero lights and zero ambient, the raster
    // combine's direct+ambient terms are both algebraically zero (same
    // reasoning `rt_p3_emissive_gi.rs`'s proof 1 states) — the emission-OFF
    // render must read as dark as that algebra predicts, not brightened by
    // some other stray term this fixture didn't account for.
    const DARK_FLOOR: f64 = 0.01;
    assert!(
        off < DARK_FLOOR,
        "emission-off baseline must be near-black with zero lights/ambient (got {off:.5}, \
         floor {DARK_FLOOR}) — a non-trivial baseline here would invalidate the on/off \
         comparison below"
    );

    // CPU-stated floor #2 (the load-bearing assertion): the pre-fix gate
    // (`rt_enabled && has_casters`, `casters.is_empty()` with zero lights)
    // skipped the ENTIRE RT dispatch block — accel build, GI gather,
    // `rt_irradiance_mask` write — so `rt_or_flat_ambient` fell through to
    // the flat-ambient formula (`albedo * scene_params.y * ambient_tint`,
    // both zero here) and this pixel would read EXACTLY `off` regardless of
    // emission. Any real separation between `on` and `off` is direct
    // evidence the RT GI gather ran with zero casters.
    const GI_BRIGHTENING_FLOOR: f64 = 0.02;
    assert!(
        on - off > GI_BRIGHTENING_FLOOR,
        "zero-light neighbor region (pixel ({:.0},{:.0})) must brighten by more than \
         {GI_BRIGHTENING_FLOOR} with the emitter's emission on vs off — this is the RT GI \
         gather's emissive-hit term, the only possible source of brightening with zero lights \
         and zero ambient: off={off:.5} on={on:.5}",
        neighbor_px.px,
        neighbor_px.py,
    );
}
