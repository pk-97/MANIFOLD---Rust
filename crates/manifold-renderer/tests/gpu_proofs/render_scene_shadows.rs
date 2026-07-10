//! `node.render_scene` shadow-map proof (REALTIME_3D_DESIGN §5 P2 gate).
//!
//! What the isolated Rust unit tests CAN'T reach: that the depth-only
//! shadow pipeline (void `@fragment`, no colour attachment) actually
//! compiles through SPIRV-Cross → MSL and runs, that the per-caster depth
//! map created `RENDER_TARGET | SHADER_READ` binds as a `texture_depth_2d`
//! in the SAME frame without tripping the AGX render-target-only crash
//! (0x78), and that the PCF `textureSampleCompareLevel` path measurably
//! darkens occluded geometry.
//!
//! Decisive scene: a big ground plane with a smaller plane floating above
//! it, lit by one angled sun. Rendered TWICE — `cast_shadows` on and off.
//! The occluder is present in both, so it contributes equal light in both;
//! the ONLY difference is the shadow it drops on the ground. Total scene
//! luma with shadows on MUST be lower than with shadows off — a direct
//! readout of "the occluder removed light from the ground". A second test
//! wires MORE than `MAX_SHADOW_CASTING_LIGHTS` casters and asserts the frame
//! still renders finite and lit (the caster beyond the cap illuminates even
//! though it casts no shadow — D4/F2).

use half::f16;
use manifold_gpu::GpuTextureFormat;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;

use crate::harness;

/// Ground plane (8×8 at y=0) + an occluder plane (3×3 at y=1.5) lit by
/// `num_lights` suns positioned overhead-and-to-one-side so the occluder's
/// shadow lands beside it on the ground (visible to the tilted camera).
/// Every sun's `cast_shadows` is `cast`.
fn shadow_scene_json(cast: bool, num_lights: usize) -> String {
    let cast_v = if cast { 1.0 } else { 0.0 };
    let mut nodes = String::from(
        r#"{"id":0,"typeId":"system.generator_input","nodeId":"input"},
        {"id":1,"typeId":"node.grid_mesh","nodeId":"ground_grid","params":{
            "max_capacity":{"type":"Int","value":8192},
            "resolution_x":{"type":"Int","value":20},
            "resolution_y":{"type":"Int","value":20},
            "size_x":{"type":"Float","value":8.0},
            "size_y":{"type":"Float","value":8.0}}},
        {"id":2,"typeId":"node.make_triangles","nodeId":"ground_tris","params":{
            "src_cols":{"type":"Int","value":20},
            "src_rows":{"type":"Int","value":20}}},
        {"id":5,"typeId":"node.grid_mesh","nodeId":"occ_grid","params":{
            "max_capacity":{"type":"Int","value":8192},
            "resolution_x":{"type":"Int","value":10},
            "resolution_y":{"type":"Int","value":10},
            "size_x":{"type":"Float","value":3.0},
            "size_y":{"type":"Float","value":3.0}}},
        {"id":6,"typeId":"node.make_triangles","nodeId":"occ_tris","params":{
            "src_cols":{"type":"Int","value":10},
            "src_rows":{"type":"Int","value":10}}},
        {"id":7,"typeId":"node.transform_3d","nodeId":"occ_xform","params":{
            "pos_y":{"type":"Float","value":1.5}}},
        {"id":3,"typeId":"node.orbit_camera","nodeId":"cam","params":{
            "orbit":{"type":"Float","value":0.7},
            "tilt":{"type":"Float","value":0.95},
            "distance":{"type":"Float","value":10.0},
            "fov_y":{"type":"Float","value":0.8}}},
        {"id":4,"typeId":"node.phong_material","nodeId":"mat","params":{
            "color_r":{"type":"Float","value":1.0},
            "color_g":{"type":"Float","value":1.0},
            "color_b":{"type":"Float","value":1.0},
            "ambient":{"type":"Float","value":0.05}}},"#,
    );

    nodes.push_str(&format!(
        r#"{{"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{{
            "objects":{{"type":"Int","value":2}},
            "lights":{{"type":"Int","value":{num_lights}}}}}}},
        {{"id":99,"typeId":"system.final_output","nodeId":"out"}}"#,
    ));

    // Suns spread around +Y, each aimed at the origin, each `intensity` set
    // so the summed light doesn't blow out with several of them.
    let intensity = (1.0 / num_lights as f32).max(0.3);
    for i in 0..num_lights {
        let id = 30 + i;
        let px = 3.0 + i as f32; // slightly different positions so multiple casters differ
        nodes.push_str(&format!(
            r#",{{"id":{id},"typeId":"node.light","nodeId":"sun_{i}","params":{{
                "mode":{{"type":"Enum","value":0}},
                "pos_x":{{"type":"Float","value":{px}}},
                "pos_y":{{"type":"Float","value":20.0}},
                "pos_z":{{"type":"Float","value":3.0}},
                "aim_x":{{"type":"Float","value":0.0}},
                "aim_y":{{"type":"Float","value":0.0}},
                "aim_z":{{"type":"Float","value":0.0}},
                "color_r":{{"type":"Float","value":1.0}},
                "color_g":{{"type":"Float","value":1.0}},
                "color_b":{{"type":"Float","value":1.0}},
                "intensity":{{"type":"Float","value":{intensity}}},
                "cast_shadows":{{"type":"Float","value":{cast_v}}}}}}}"#,
        ));
    }

    let mut wires = String::from(
        r#"{"fromNode":1,"fromPort":"vertices","toNode":2,"toPort":"in"},
        {"fromNode":2,"fromPort":"out","toNode":20,"toPort":"mesh_0"},
        {"fromNode":5,"fromPort":"vertices","toNode":6,"toPort":"in"},
        {"fromNode":6,"fromPort":"out","toNode":20,"toPort":"mesh_1"},
        {"fromNode":7,"fromPort":"transform","toNode":20,"toPort":"transform_1"},
        {"fromNode":3,"fromPort":"out","toNode":20,"toPort":"camera"},
        {"fromNode":4,"fromPort":"out","toNode":20,"toPort":"material_0"},
        {"fromNode":4,"fromPort":"out","toNode":20,"toPort":"material_1"},
        {"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}"#,
    );
    for i in 0..num_lights {
        let id = 30 + i;
        wires.push_str(&format!(
            r#",{{"fromNode":{id},"fromPort":"out","toNode":20,"toPort":"light_{i}"}}"#,
        ));
    }

    format!(r#"{{"version":2,"name":"RenderSceneShadowProof","nodes":[{nodes}],"wires":[{wires}]}}"#)
}

/// Render a scene-graph JSON to `Rgba16Float`, returning readback bytes.
/// Two committed frames so pipeline warm-up is past; `commit_and_wait_completed`
/// hard-checks for Metal GPU errors, so a bad shadow-map bind or a failed
/// depth-only PSO surfaces as a panic here, not a silently wrong frame.
fn render_readback(json: &str) -> (Vec<u8>, u32, u32) {
    let h = harness::shared();
    let registry = PrimitiveRegistry::with_builtin();
    let mut runtime = PresetRuntime::from_json_str_with_device(
        json,
        &registry,
        &h.device,
        h.width,
        h.height,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .expect("shadow scene graph must build");

    let target = h.make_target("render-scene-shadows");
    for frame in 0..2 {
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
        let mut enc = h.device.create_encoder("render-scene-shadows-enc");
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

/// Total luma + peak over an `Rgba16Float` readback (assert-finite as it goes).
fn luma(bytes: &[u8]) -> (f64, f32) {
    let mut sum = 0.0f64;
    let mut peak = 0.0f32;
    for px in bytes.chunks_exact(8) {
        let r = f16::from_le_bytes([px[0], px[1]]).to_f32();
        let g = f16::from_le_bytes([px[2], px[3]]).to_f32();
        let b = f16::from_le_bytes([px[4], px[5]]).to_f32();
        assert!(r.is_finite() && g.is_finite() && b.is_finite(), "non-finite pixel");
        sum += (0.2126 * r + 0.7152 * g + 0.0722 * b) as f64;
        peak = peak.max(r.max(g).max(b));
    }
    (sum, peak)
}

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

#[test]
fn occluder_casts_shadow_that_darkens_the_ground() {
    let (on_bytes, w, h) = render_readback(&shadow_scene_json(true, 1));
    let (off_bytes, _, _) = render_readback(&shadow_scene_json(false, 1));

    write_png(&on_bytes, w, h, "/tmp/render_scene_shadow_on.png");
    write_png(&off_bytes, w, h, "/tmp/render_scene_shadow_off.png");

    let (sum_on, peak_on) = luma(&on_bytes);
    let (sum_off, peak_off) = luma(&off_bytes);

    // Both frames are lit (the depth-only shadow PSO didn't crash the run,
    // and the scene renders).
    assert!(peak_on > 0.2, "shadowed frame is unlit (peak {peak_on})");
    assert!(peak_off > 0.2, "unshadowed frame is unlit (peak {peak_off})");

    // The shadow removes light: same geometry, same lights, only difference
    // is the cast shadow → total luma must drop measurably.
    let drop = (sum_off - sum_on) / sum_off;
    eprintln!(
        "shadow luma: off={sum_off:.1} on={sum_on:.1} drop={:.1}%",
        drop * 100.0
    );
    let drop_pct = drop * 100.0;
    assert!(
        sum_on < sum_off && drop > 0.01,
        "shadows-on should be measurably darker than shadows-off: \
         off={sum_off:.1} on={sum_on:.1} drop={drop_pct:.2}% — occluder cast no visible shadow"
    );
}

#[test]
fn more_than_k_casters_still_render_finite_and_lit() {
    // 5 casters — one past MAX_SHADOW_CASTING_LIGHTS (=4). The 5th light
    // gets no shadow map but must still illuminate; the frame must render
    // finite (no validation error, no NaN) and non-black.
    let (bytes, w, h) = render_readback(&shadow_scene_json(true, 5));
    write_png(&bytes, w, h, "/tmp/render_scene_shadow_5casters.png");
    let (sum, peak) = luma(&bytes);
    assert!(peak > 0.2, "5-caster scene is unlit (peak {peak})");
    assert!(sum > 0.0, "5-caster scene rendered black");
}
