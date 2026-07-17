//! `node.scene_object`'s `visible` port-shadow proof
//! (SCENE_OBJECT_AND_PANEL_V2_DESIGN.md D4/P2 gate: "an invisible object
//! leaves no shadow").
//!
//! Ground plane (object 0, always visible) + an occluder plane (object 1,
//! `visible` toggled) hung above it, lit by one shadow-casting sun. Wired
//! through explicit `node.scene_object` nodes (the P2 surface), not the
//! legacy `mesh_k`/`transform_k` ports `render_scene_shadows.rs` still uses
//! (proving those migrate transparently is that file's job, unchanged
//! here — this file proves the NEW `visible` port-shadow behavior
//! directly). `visible = 0` must remove BOTH the occluder's own draw AND
//! its shadow on the ground — skip = no draw AND no shadow (D4).

use half::f16;
use manifold_gpu::GpuTextureFormat;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;

use crate::harness;

/// `occluder_visible`: the occluder scene_object's `visible` param (0.0 or
/// 1.0). Ground is object_0 (grid_mesh), occluder is object_1 (grid_mesh,
/// raised on Y, RED so its own presence/absence in-frame is unmistakable).
/// One shadow-casting sun overhead-and-to-one-side (matches
/// `render_scene_shadows.rs`'s decisive-scene shape).
fn scene_json(occluder_visible: f32) -> String {
    format!(
        r#"{{"version":2,"name":"RenderSceneObjectVisibilityProof","nodes":[
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
            "orbit":{{"type":"Float","value":0.7}},
            "tilt":{{"type":"Float","value":0.95}},
            "distance":{{"type":"Float","value":10.0}},
            "fov_y":{{"type":"Float","value":0.8}}}}}},
        {{"id":4,"typeId":"node.phong_material","nodeId":"ground_mat","params":{{
            "color_r":{{"type":"Float","value":1.0}},
            "color_g":{{"type":"Float","value":1.0}},
            "color_b":{{"type":"Float","value":1.0}},
            "ambient":{{"type":"Float","value":0.05}}}}}},
        {{"id":8,"typeId":"node.phong_material","nodeId":"occ_mat","params":{{
            "color_r":{{"type":"Float","value":1.0}},
            "color_g":{{"type":"Float","value":0.0}},
            "color_b":{{"type":"Float","value":0.0}},
            "ambient":{{"type":"Float","value":0.05}}}}}},
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
        {{"id":40,"typeId":"node.scene_object","nodeId":"obj0","params":{{
            "visible":{{"type":"Float","value":1.0}}}}}},
        {{"id":41,"typeId":"node.scene_object","nodeId":"obj1","params":{{
            "visible":{{"type":"Float","value":{occluder_visible}}}}}}},
        {{"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{{
            "objects":{{"type":"Int","value":2}},
            "lights":{{"type":"Int","value":1}}}}}},
        {{"id":99,"typeId":"system.final_output","nodeId":"out"}}
        ],"wires":[
        {{"fromNode":1,"fromPort":"vertices","toNode":2,"toPort":"in"}},
        {{"fromNode":2,"fromPort":"out","toNode":40,"toPort":"vertices"}},
        {{"fromNode":4,"fromPort":"out","toNode":40,"toPort":"material"}},
        {{"fromNode":5,"fromPort":"vertices","toNode":6,"toPort":"in"}},
        {{"fromNode":6,"fromPort":"out","toNode":41,"toPort":"vertices"}},
        {{"fromNode":8,"fromPort":"out","toNode":41,"toPort":"material"}},
        {{"fromNode":7,"fromPort":"transform","toNode":41,"toPort":"transform"}},
        {{"fromNode":40,"fromPort":"object","toNode":20,"toPort":"object_0"}},
        {{"fromNode":41,"fromPort":"object","toNode":20,"toPort":"object_1"}},
        {{"fromNode":3,"fromPort":"out","toNode":20,"toPort":"camera"}},
        {{"fromNode":30,"fromPort":"out","toNode":20,"toPort":"light_0"}},
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
    .expect("object-visibility scene graph must build");

    let target = h.make_target("render-scene-object-visibility");
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
        let mut enc = h.device.create_encoder("render-scene-object-visibility-enc");
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

/// Sum of `R - G` over the frame. The white/grey ground (and its shadow)
/// has R≈G≈B everywhere, contributing ~0 here regardless of how bright it
/// is; the occluder is the only red-TINTED (R > G) surface in the scene,
/// so this isolates "did the occluder's own draw run" from the ground's
/// much larger (and visibility-toggle-sensitive, via the shadow) neutral
/// luma — a plain red-channel sum conflates the two (verified: it moved
/// the WRONG direction on the first cut of this test, since the brighter
/// no-shadow ground outweighs the small occluder patch in total red).
fn red_excess_sum(bytes: &[u8]) -> f64 {
    let mut sum = 0.0f64;
    for px in bytes.chunks_exact(8) {
        let r = f16::from_le_bytes([px[0], px[1]]).to_f32();
        let g = f16::from_le_bytes([px[2], px[3]]).to_f32();
        sum += (r - g).max(0.0) as f64;
    }
    sum
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
fn invisible_object_casts_no_shadow_and_does_not_draw() {
    let (visible_bytes, w, h) = render_readback(&scene_json(1.0));
    let (invisible_bytes, _, _) = render_readback(&scene_json(0.0));

    write_png(&visible_bytes, w, h, "/tmp/render_scene_object_visible.png");
    write_png(&invisible_bytes, w, h, "/tmp/render_scene_object_invisible.png");

    let (sum_visible, peak_visible) = luma(&visible_bytes);
    let (sum_invisible, peak_invisible) = luma(&invisible_bytes);
    assert!(peak_visible > 0.2, "visible-occluder frame is unlit (peak {peak_visible})");
    assert!(peak_invisible > 0.2, "invisible-occluder frame is unlit (peak {peak_invisible})");

    // No shadow: total luma rises when the occluder (and its shadow) is
    // gone — same direction/magnitude test as render_scene_shadows.rs's
    // occluder_casts_shadow_that_darkens_the_ground.
    let rise = (sum_invisible - sum_visible) / sum_visible;
    eprintln!(
        "object-visibility luma: visible={sum_visible:.1} invisible={sum_invisible:.1} rise={:.2}%",
        rise * 100.0
    );
    assert!(
        sum_invisible > sum_visible && rise > 0.01,
        "invisible occluder must remove its shadow (frame gets brighter): \
         visible={sum_visible:.1} invisible={sum_invisible:.1} rise={:.4}%",
        rise * 100.0
    );

    // No draw: the red-TINTED occluder itself must vanish from the frame,
    // not just its shadow (isolated from the ground's own, much larger,
    // colour-neutral luma via R-G — see `red_excess_sum`'s doc comment).
    let red_visible = red_excess_sum(&visible_bytes);
    let red_invisible = red_excess_sum(&invisible_bytes);
    eprintln!(
        "object-visibility red-excess (R-G) sum: visible={red_visible:.1} invisible={red_invisible:.1}"
    );
    assert!(
        red_invisible < red_visible * 0.05,
        "invisible occluder's own red draw must not appear: \
         visible={red_visible:.1} invisible={red_invisible:.1}"
    );
}
