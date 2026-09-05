//! BUG-1gqt — the RT trace kernels ignore the emissive TEXTURE: a mesh
//! with an emissive factor but an all-black (or tiny-glow-region) emissive
//! map emits into GI/reflections as if FULLY emissive (the trace path
//! reads only `GiMaterial.emissive`, the factor). The raster's
//! `resolve_emissive` multiplies factor × texture sample at the fragment;
//! the trace path must do the same at the hit sample, with the material's
//! emissive UV transform applied (`emissive_uv_m/t`, the raster's
//! `apply_uv_transform` convention).
//!
//! One ground+emitter scene (`rt_p3_emissive_gi.rs`'s geometry, the
//! emitter's material swapped to `node.pbr_material` for its `em_uv_*`
//! params, a `node.circle_mask` glow spot wired to `emissive_map_1`,
//! `node.bake_environment` because pbr_material hard-requires an envmap),
//! asserted over a window grid with no UV→world mapping assumptions:
//!
//! 1. [`black_emissive_texture_gives_no_gi`] — with an ALL-BLACK emissive
//!    map and a full-strength factor, NO ground window may brighten vs the
//!    emission-off control. Pre-fix every window near the emitter
//!    brightens (uniform factor emission) — red. The companion guard
//!    proves direction: with the glow-spot texture, SOME ground window
//!    must brighten >2% (the GI emissive-hit path is alive).
//! 2. [`emissive_uv_transform_applies_at_hit_sample`] — same scene with
//!    `em_uv_tx = 0.5` vs untransformed: some ground window must CHANGE.
//!    Pre-fix the trace path samples neither texture nor transform, so the
//!    two renders are identical — red.
//!
//! Circle-mask {0,1} values are colorspace-invariant, so the
//! sRGB-emissive-map question cannot confound either proof. Run via
//! `scripts/gpu_proofs_gate.py` (cargo test, never nextest).

use half::f16;
use manifold_gpu::GpuTextureFormat;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::node_graph::camera::Camera;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;

use crate::harness;

const ORBIT: f32 = 0.7;
const TILT: f32 = 0.95;
const DISTANCE: f32 = 10.0;
const FOV_Y: f32 = 0.8;
const NEAR: f32 = 0.05;
const FAR: f32 = 200.0;

/// Same settle discipline as `rt_p3_emissive_gi.rs` (RT-D4 async accel).
const RT_WARMUP_FRAMES: i64 = 32;

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
    .expect("BUG-1gqt scene graph must build");

    let target = h.make_target("rt-p3-emissive-texture");
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
            gpu_signal_committed: 0,
            gpu_signaled: 0,
        };
        let mut enc = h.device.create_encoder("rt-p3-emissive-texture-enc");
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

fn f16l(px: &[u8]) -> f32 {
    let r = f16::from_le_bytes([px[0], px[1]]).to_f32();
    let g = f16::from_le_bytes([px[2], px[3]]).to_f32();
    let b = f16::from_le_bytes([px[4], px[5]]).to_f32();
    assert!(r.is_finite() && g.is_finite() && b.is_finite(), "non-finite pixel");
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

/// Ground(8x8)+emitter(3x3 @ y=1.5)+sun+env, `rt_p3_emissive_gi.rs` proof
/// 2's geometry. `black_texture` collapses the glow spot to a zero-radius
/// mask (all-black emissive map); `emit_on` zeroes the emission FACTOR
/// (the control); `em_uv_tx` shifts the glow spot in U (proof 2).
fn scene_json(
    black_texture: bool,
    emit_on: bool,
    em_uv_tx: f32,
    emitter_y: f32,
    emitter_size: f32,
    emission_intensity: f32,
    spot_cx: f32,
    spot_radius: f32,
) -> String {
    let (er, eg, eb) = if emit_on {
        (EMIT[0], EMIT[1], EMIT[2])
    } else {
        (0.0, 0.0, 0.0)
    };
    let (rx, ry, soft) = if black_texture {
        (0.001, 0.001, 0.001)
    } else {
        (spot_radius, spot_radius, 0.05)
    };
    format!(
        r#"{{"version":2,"name":"RtBug1gqtEmissiveTexture","nodes":[
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
            "size_x":{{"type":"Float","value":{emitter_size}}},
            "size_y":{{"type":"Float","value":{emitter_size}}}}}}},
        {{"id":6,"typeId":"node.make_triangles","nodeId":"emitter_tris","params":{{
            "src_cols":{{"type":"Int","value":10}},
            "src_rows":{{"type":"Int","value":10}}}}}},
        {{"id":7,"typeId":"node.transform_3d","nodeId":"emitter_xform","params":{{
            "pos_y":{{"type":"Float","value":{emitter_y}}}}}}},
        {{"id":3,"typeId":"node.orbit_camera","nodeId":"cam","params":{{
            "orbit":{{"type":"Float","value":{ORBIT}}},
            "tilt":{{"type":"Float","value":{TILT}}},
            "distance":{{"type":"Float","value":{DISTANCE}}},
            "fov_y":{{"type":"Float","value":{FOV_Y}}}}}}},
        {{"id":4,"typeId":"node.phong_material","nodeId":"ground_mat","params":{{
            "color_r":{{"type":"Float","value":1.0}},
            "color_g":{{"type":"Float","value":1.0}},
            "color_b":{{"type":"Float","value":1.0}},
            "ambient":{{"type":"Float","value":0.05}}}}}},
        {{"id":8,"typeId":"node.pbr_material","nodeId":"emitter_mat","params":{{
            "color_r":{{"type":"Float","value":0.02}},
            "color_g":{{"type":"Float","value":0.02}},
            "color_b":{{"type":"Float","value":0.02}},
            "ambient":{{"type":"Float","value":0.0}},
            "emission_r":{{"type":"Float","value":{er}}},
            "emission_g":{{"type":"Float","value":{eg}}},
            "emission_b":{{"type":"Float","value":{eb}}},
            "emission_intensity":{{"type":"Float","value":{emission_intensity}}},
            "em_uv_tx":{{"type":"Float","value":{em_uv_tx}}}}}}},
        {{"id":9,"typeId":"node.circle_mask","nodeId":"glow_spot","params":{{
            "cx":{{"type":"Float","value":{spot_cx}}},
            "cy":{{"type":"Float","value":0.5}},
            "radius_x":{{"type":"Float","value":{rx}}},
            "radius_y":{{"type":"Float","value":{ry}}},
            "softness":{{"type":"Float","value":{soft}}}}}}},
        {{"id":10,"typeId":"node.bake_environment","nodeId":"env","params":{{
            "width":{{"type":"Int","value":512}},
            "height":{{"type":"Int","value":256}},
            "intensity":{{"type":"Float","value":0.0}}}}}},
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
            "cast_shadows":{{"type":"Float","value":1.0}}}}}},
        {{"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{{
            "objects":{{"type":"Int","value":2}},
            "lights":{{"type":"Int","value":1}},
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
        {{"fromNode":9,"fromPort":"out","toNode":20,"toPort":"emissive_map_1"}},
        {{"fromNode":10,"fromPort":"envmap","toNode":20,"toPort":"envmap"}},
        {{"fromNode":30,"fromPort":"out","toNode":20,"toPort":"light_0"}},
        {{"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}}
        ]}}"#
    )
}

/// `rt_p3_emissive_gi.rs` proof 2's verified camera-visible ground probe
/// near the emitter, plus three unverified siblings around it — each is
/// used only if it projects in front of the camera (>= 3 required).
const PROBES: [[f32; 3]; 8] = [
    [1.0, 0.0, -1.0],
    [1.0, 0.0, 1.0],
    [-1.0, 0.0, -1.0],
    [-1.0, 0.0, 1.0],
    [2.5, 0.0, 0.0],
    [-2.5, 0.0, 0.0],
    [0.0, 0.0, 2.5],
    [0.0, 0.0, -2.5],
];

/// Emission-hue mask: the emitter's magenta (0.4, 0.15, 0.6) — blue
/// dominant, green the floor — vs the sun-lit ground's near-white. A probe
/// whose window contains even one such pixel is contaminated by the
/// RASTER's self-emission (which respects texture+transform by
/// definition) and must be excluded from GI assertions.
fn magenta_pixel(px: &[u8]) -> bool {
    let r = f16::from_le_bytes([px[0], px[1]]).to_f32();
    let g = f16::from_le_bytes([px[2], px[3]]).to_f32();
    let b = f16::from_le_bytes([px[4], px[5]]).to_f32();
    (b - g) > 0.08 && (r - g) > 0.04 && b > 0.05
}

/// Per-probe `(luma, window_is_contaminated)` for the 7x7 window at each
/// projected ground point — `rt_p3_emissive_gi.rs`'s exact measurement.
/// (A coarse tiling grid dilutes the near-emitter GI brightening ~25x
/// into the noise — measured 0.08-0.17% on a scene whose precise probe
/// reads >2%.)
fn probe_windows(bytes: &[u8], w: u32, h: u32) -> Vec<(f64, bool)> {
    let cam = Camera::orbit_perspective(ORBIT, TILT, DISTANCE, FOV_Y, 0.0, 0.0, NEAR, FAR);
    PROBES
        .iter()
        .filter_map(|world| {
            let px = cam.project_to_pixel(*world, w, h)?;
            let cxi = px.px.round() as i32;
            let cyi = px.py.round() as i32;
            let mut sum = 0.0f64;
            let mut n = 0u64;
            let mut contaminated = false;
            for dy in -3..=3 {
                for dx in -3..=3 {
                    let x = cxi + dx;
                    let y = cyi + dy;
                    if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
                        continue;
                    }
                    let idx = ((y as u32 * w + x as u32) * 8) as usize;
                    let pxl = &bytes[idx..idx + 8];
                    sum += f16l(pxl) as f64;
                    if magenta_pixel(pxl) {
                        contaminated = true;
                    }
                    n += 1;
                }
            }
            (n > 0).then(|| (sum / n as f64, contaminated))
        })
        .collect()
}

#[test]
fn black_emissive_texture_gives_no_gi() {
    let (glow_bytes, w, h) = render_readback(&scene_json(false, true, 0.0, 1.5, 3.0, 1.0, 0.5, 0.45));
    let (black_bytes, _, _) = render_readback(&scene_json(true, true, 0.0, 1.5, 3.0, 1.0, 0.5, 0.45));
    let (off_bytes, _, _) = render_readback(&scene_json(false, false, 0.0, 1.5, 3.0, 1.0, 0.5, 0.45));

    let glow = probe_windows(&glow_bytes, w, h);
    let black = probe_windows(&black_bytes, w, h);
    let off = probe_windows(&off_bytes, w, h);
    // Usable = sun-lit ground AND no emitter raster pixel in the window in
    // ANY render (the glow render's magenta marks the emitter's screen
    // footprint; the same window is excluded everywhere).
    let usable: Vec<usize> = (0..glow.len())
        .filter(|&i| off[i].0 > 0.2 && !glow[i].1 && !black[i].1 && !off[i].1)
        .collect();
    assert!(!usable.is_empty(), "need at least one clean sun-lit ground probe");

    // Three metrics over the clean probes (pre-fix expectations in
    // comments — every assert fails pre-fix):
    //  - texture_response = best |glow - black| / off: the texture making
    //    ANY difference. Pre-fix EXACTLY 0 (measured: glow and black read
    //    identical luma to 4 decimals at every probe — the trace never
    //    samples the map).
    //  - black_vs_off = worst |black - off| / off: an all-black map must
    //    render IDENTICALLY to no emission at all (rasters match, so this
    //    isolates the trace term). Pre-fix 2.9% — the uniform factor
    //    emitting through the black map.
    //  - factor_response = best |glow - off| / off: sanity that the
    //    emissive factor moves SOMETHING at these probes (holds pre and
    //    post — fixture validity, not the bug).
    let mut texture_response = 0.0f64;
    let mut black_vs_off = 0.0f64;
    let mut factor_response = 0.0f64;
    for &i in &usable {
        let t = (glow[i].0 - black[i].0).abs() / off[i].0.max(1e-9);
        let b = (black[i].0 - off[i].0).abs() / off[i].0.max(1e-9);
        let f = (glow[i].0 - off[i].0).abs() / off[i].0.max(1e-9);
        eprintln!("  probe {i}: |glow-black|={:.2}% |black-off|={:.2}% |glow-off|={:.2}% (abs glow={:.4} black={:.4} off={:.4})",
            t * 100.0, b * 100.0, f * 100.0, glow[i].0, black[i].0, off[i].0);
        texture_response = texture_response.max(t);
        black_vs_off = black_vs_off.max(b);
        factor_response = factor_response.max(f);
    }
    eprintln!(
        "emissive-texture GI: texture_response={:.2}% black_vs_off={:.2}% factor_response={:.2}%",
        texture_response * 100.0,
        black_vs_off * 100.0,
        factor_response * 100.0,
    );

    assert!(
        factor_response > 0.01,
        "the emissive factor moved nothing at the clean probes ({:.2}%) — fixture is blind",
        factor_response * 100.0,
    );
    assert!(
        texture_response > 0.01,
        "glow-spot vs all-black emissive map read identically at every probe ({:.2}% best) — \
         the trace kernels ignore the emissive texture (BUG-1gqt)",
        texture_response * 100.0,
    );
    assert!(
        black_vs_off < 0.005,
        "all-black emissive map differed from no-emission by {:.2}% — the trace kernels \
         emit the flat factor through the black map (BUG-1gqt)",
        black_vs_off * 100.0,
    );
}

#[test]
fn emissive_uv_transform_applies_at_hit_sample() {
    let (base_bytes, w, h) = render_readback(&scene_json(false, true, 0.0, 1.5, 3.0, 1.0, 0.5, 0.45));
    let (shift_bytes, _, _) = render_readback(&scene_json(false, true, -0.3, 1.5, 3.0, 2.5, 0.8, 0.2));
    let (off_bytes, _, _) = render_readback(&scene_json(false, false, 0.0, 1.5, 3.0, 2.5, 0.8, 0.2));

    let base = probe_windows(&base_bytes, w, h);
    let shift = probe_windows(&shift_bytes, w, h);
    let off = probe_windows(&off_bytes, w, h);

    // The spot (off-center at cx=0.8, small) shifts +0.3 in U to the
    // texture center — a 0.9-world-unit move at this emitter size. Compare
    // each probe's glow-vs-control delta between the two transforms.
    // Pre-fix the trace path samples neither texture nor transform, so
    // every per-probe delta is identical and the max change is zero — red.
    // The glow spot MOVES with tx, so a window clean at tx=0 can catch
    // the emitter at tx=0.5 — exclude on the union of both renders'
    // contamination masks. What remains is pure ground: pre-fix the trace
    // reads neither texture nor transform, so every per-probe delta is
    // identical and the max change is zero — red.
    let usable: Vec<usize> = (0..base.len())
        .filter(|&i| off[i].0 > 0.2 && !base[i].1 && !shift[i].1 && !off[i].1)
        .collect();
    assert!(!usable.is_empty(), "need at least one clean sun-lit ground probe");
    let mut best_change = 0.0f64;
    for &i in &usable {
        let d0 = (base[i].0 - off[i].0) / off[i].0.max(1e-9);
        let d1 = (shift[i].0 - off[i].0) / off[i].0.max(1e-9);
        let change = (d0 - d1).abs();
        eprintln!("  probe {i}: delta(tx=0)={:.2}% delta(tx=0.5)={:.2}% (abs base={:.4} shift={:.4} off={:.4})", d0 * 100.0, d1 * 100.0, base[i].0, shift[i].0, off[i].0);
        best_change = best_change.max(change);
    }
    eprintln!("emissive UV transform: best per-probe delta change={:.2}%", best_change * 100.0);
    assert!(
        best_change > 0.005,
        "shifting the emissive map by em_uv_tx=0.5 changed no probe's GI ({:.2}% best) — \
         the trace hit sample ignores the emissive UV transform (BUG-1gqt)",
        best_change * 100.0,
    );
}
