//! BUG-88m regression gate: with rt_reflections on, a Blend object must
//! keep its prefiltered-env specular IBL — the kernel writes `.a = -1`
//! ("no traced value") at texels the trace domain excludes (Blend is not
//! in the depth prepass or the accel), and fs_pbr substitutes
//! rt_reflection only where `.a >= 0`. Pre-fix the substitution was
//! unconditional, so Blend pixels substituted rgb=0 and their env
//! streaks went black (AMG GT3 glass/lenses, Peter's helmet visor).
//!
//! Hand-authored fixture (the imported-AMG harness path proved
//! unusable: the import stamps scene exposures and `render`'s empty
//! manifest re-asserts them at defaults every frame, and even with
//! that stripped the Opaque body is byte-identical across the refl
//! toggle — Raster-parity — so only glass could discriminate and the
//! glass produced zero measurable effect there). Scene: ONE chrome
//! Blend quad (alpha_mode=2, metallic=1.0, roughness 0.02 — dielectric
//! F0 ≈ 0.04 makes the specular loss too small to discriminate from
//! trace noise; chrome F0 = albedo makes it ~1 HDR unit), bright env,
//! nothing else. refl=1 vs refl=0: pre-fix the quad's specular dies
//! (substituted 0), post-fix it falls back to the same env fetch the
//! OFF leg uses — the diff collapses to zero.

use manifold_gpu::GpuTextureFormat;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;

use crate::harness;

const ORBIT: f32 = 0.7;
const TILT: f32 = 0.95;
const DISTANCE: f32 = 10.0;
const FOV_Y: f32 = 0.8;
const RT_WARMUP_FRAMES: i64 = 16;

fn scene_json(rt_reflections: bool) -> String {
    let rt_v = if rt_reflections { "true" } else { "false" };
    format!(
        r#"{{"version":2,"name":"RtBug88mBlendGate","nodes":[
        {{"id":0,"typeId":"system.generator_input","nodeId":"input"}},
        {{"id":3,"typeId":"node.orbit_camera","nodeId":"cam","params":{{
            "orbit":{{"type":"Float","value":{ORBIT}}},
            "tilt":{{"type":"Float","value":{TILT}}},
            "distance":{{"type":"Float","value":{DISTANCE}}},
            "fov_y":{{"type":"Float","value":{FOV_Y}}}}}}},
        {{"id":30,"typeId":"node.light","nodeId":"sun","params":{{
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
        {{"id":5,"typeId":"node.grid_mesh","nodeId":"quad_grid","params":{{
            "max_capacity":{{"type":"Int","value":8192}},
            "resolution_x":{{"type":"Int","value":4}},
            "resolution_y":{{"type":"Int","value":4}},
            "size_x":{{"type":"Float","value":3.0}},
            "size_y":{{"type":"Float","value":3.0}}}}}},
        {{"id":6,"typeId":"node.make_triangles","nodeId":"quad_tris","params":{{
            "src_cols":{{"type":"Int","value":4}},
            "src_rows":{{"type":"Int","value":4}}}}}},
        {{"id":7,"typeId":"node.transform_3d","nodeId":"quad_xform","params":{{
            "pos_x":{{"type":"Float","value":0.0}},
            "pos_y":{{"type":"Float","value":0.0}},
            "pos_z":{{"type":"Float","value":0.0}}}}}},
        {{"id":8,"typeId":"node.pbr_material","nodeId":"quad_mat","params":{{
            "color_r":{{"type":"Float","value":0.8}},
            "color_g":{{"type":"Float","value":0.8}},
            "color_b":{{"type":"Float","value":0.8}},
            "color_a":{{"type":"Float","value":1.0}},
            "alpha_mode":{{"type":"Enum","value":2}},
            "ambient":{{"type":"Float","value":0.0}},
            "metallic":{{"type":"Float","value":1.0}},
            "roughness":{{"type":"Float","value":0.02}},
            "emission_intensity":{{"type":"Float","value":0.0}}}}}},
        {{"id":10,"typeId":"node.bake_environment","nodeId":"env","params":{{
            "width":{{"type":"Int","value":64}},
            "height":{{"type":"Int","value":32}},
            "intensity":{{"type":"Float","value":1.5}}}}}},
        {{"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{{
            "objects":{{"type":"Int","value":1}},
            "lights":{{"type":"Int","value":1}},
            "rt_enabled":{{"type":"Bool","value":true}},
            "rt_reflections":{{"type":"Bool","value":{rt_v}}}}}}},
        {{"id":99,"typeId":"system.final_output","nodeId":"out"}}
        ],"wires":[
        {{"fromNode":3,"fromPort":"out","toNode":20,"toPort":"camera"}},
        {{"fromNode":5,"fromPort":"vertices","toNode":6,"toPort":"in"}},
        {{"fromNode":6,"fromPort":"out","toNode":20,"toPort":"mesh_0"}},
        {{"fromNode":7,"fromPort":"transform","toNode":20,"toPort":"transform_0"}},
        {{"fromNode":8,"fromPort":"out","toNode":20,"toPort":"material_0"}},
        {{"fromNode":30,"fromPort":"out","toNode":20,"toPort":"light_0"}},
        {{"fromNode":10,"fromPort":"envmap","toNode":20,"toPort":"envmap"}},
        {{"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}}
        ]}}"#
    )
}

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
    .expect("BUG-88m scene graph must build");

    let target = h.make_target("rt-bug88m-blend-gate");
    // Poll until a non-black frame: the BUG-326 rerun suppression window
    // (composite skipped while the rerun accel build is in flight) reads
    // back black on this harness's fresh target, and its length is
    // load-dependent — a fixed warmup can land inside it.
    let mut last = Vec::new();
    for frame in 0..600 {
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
        let mut enc = h.device.create_encoder("rt-bug88m-enc");
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
        if frame >= RT_WARMUP_FRAMES {
            last = h.readback(&target.texture);
            let lit = last.chunks(8).filter(|px| {
                (0..3).any(|c| {
                    half::f16::from_bits(u16::from_le_bytes([px[c * 2], px[c * 2 + 1]])).to_f32() > 0.02
                })
            });
            if lit.count() * 50 > (h.width * h.height) as usize {
                break; // >2% lit — outside the suppression window
            }
        }
    }
    (last, h.width, h.height)
}

/// Fraction of pixels whose f16 rgb differs by more than `thresh`
/// between the two readbacks.
fn diff_frac(a: &[u8], b: &[u8], thresh: f32) -> f64 {
    let n = a.len() / 8;
    let mut diff = 0usize;
    for i in 0..n {
        let mut dmax = 0.0f32;
        for c in 0..3 {
            let av = half::f16::from_bits(u16::from_le_bytes([a[i * 8 + c * 2], a[i * 8 + c * 2 + 1]])).to_f32();
            let bv = half::f16::from_bits(u16::from_le_bytes([b[i * 8 + c * 2], b[i * 8 + c * 2 + 1]])).to_f32();
            dmax = dmax.max((av - bv).abs());
        }
        if dmax > thresh {
            diff += 1;
        }
    }
    diff as f64 / n as f64
}

#[test]
fn blend_quad_keeps_env_specular_with_rt_reflections() {
    let (off, _w, _h) = render_readback(&scene_json(false));
    let (on, _, _) = render_readback(&scene_json(true));

    let frac = diff_frac(&off, &on, 0.3);
    eprintln!("[bug88m-gate] diff_frac(0.3)={frac:.5}");

    // Calibrated 2026-07-25 on this exact fixture: pre-fix 0.10712
    // (the quad's env spec black-holes over its whole area), post-fix
    // 0.00000. 0.3 HDR stays above the trace-noise band (all noise sits
    // under 0.1) and two orders below the pre-fix signal.
    assert!(
        frac <= 0.005,
        "BUG-88m: {frac:.4} of pixels change when rt_reflections flips on \
         — the Blend quad is black-holing its env IBL",
    );
}
