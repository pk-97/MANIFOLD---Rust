//! `node.render_scene` per-object instancing proof (REALTIME_3D_DESIGN.md
//! §10 D11+P8 gate).
//!
//! Four things the unit tests can't reach: that a wired `instances_n`
//! storage buffer actually binds and drives `@builtin(instance_index)`
//! through SPIRV-Cross → MSL in both the main pass AND the shadow depth
//! pass; that an instance participates in the scene's ONE shared depth
//! buffer (occluded by, and occluding, ordinary objects); that a
//! shadow-casting object drawn through the instance path still darkens
//! the ground; and that fog still reads the correct `world_pos` for an
//! instanced vertex. A fifth proves the D11 invariant that makes an
//! unwired object free: a wired 1-entry identity instance buffer must
//! render byte-identical to the same scene with `instances_n` left
//! unwired (the cached Rust-side stub).
//!
//! All four scenes reuse one procedural 1-entry `node.arrange_copies`
//! (Ring layout, `active_count=1`, all extents 0, `base_scale=1`,
//! rotation 0) as the "identity instance" building block — verified
//! against `generate_instance_transforms_body.wgsl`'s Ring formula to
//! collapse to `pos_scale=[0,0,0,1]`, `rot_pad=[0,0,0,0]` bit-for-bit,
//! the same values as `render_scene.rs`'s Rust-side stub.

use half::f16;
use manifold_gpu::GpuTextureFormat;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;

use crate::harness;

/// A 1-entry `Array<InstanceTransform>` producer. `rot_x` lets callers
/// additionally orient the instance (e.g. to face a camera) while keeping
/// the position/scale identity — Ring's position formula ignores rotation
/// entirely, so this stays a pure per-instance rotate.
fn identity_instance_node(id: u32, node_id: &str, rot_x: f32) -> String {
    format!(
        r#"{{"id":{id},"typeId":"node.arrange_copies","nodeId":"{node_id}","params":{{
            "max_capacity":{{"type":"Int","value":1}},
            "active_count":{{"type":"Int","value":1}},
            "layout":{{"type":"Enum","value":1}},
            "seed":{{"type":"Int","value":0}},
            "extent_x":{{"type":"Float","value":0.0}},
            "extent_y":{{"type":"Float","value":0.0}},
            "extent_z":{{"type":"Float","value":0.0}},
            "base_scale":{{"type":"Float","value":1.0}},
            "rot_x":{{"type":"Float","value":{rot_x}}},
            "rot_y":{{"type":"Float","value":0.0}},
            "rot_z":{{"type":"Float","value":0.0}}}}}}"#
    )
}

/// Render a scene-graph JSON to `Rgba16Float`, returning readback bytes.
/// Two committed frames so pipeline warm-up is past; `commit_and_wait_completed`
/// hard-checks for Metal GPU errors, so a bad instance-buffer bind surfaces
/// as a panic here, not a silently wrong frame.
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
    .expect("instancing scene graph must build");

    let target = h.make_target("render-scene-instances");
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
        let mut enc = h.device.create_encoder("render-scene-instances-enc");
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

/// Mean (r, g, b) over a square window centred on the frame, `half_extent`
/// pixels each side of centre.
fn mean_rgb_center(bytes: &[u8], w: u32, h: u32, half_extent: u32) -> (f64, f64, f64) {
    let (cx, cy) = (w / 2, h / 2);
    let (mut sr, mut sg, mut sb, mut n) = (0.0f64, 0.0f64, 0.0f64, 0u64);
    for y in cy.saturating_sub(half_extent)..(cy + half_extent).min(h) {
        for x in cx.saturating_sub(half_extent)..(cx + half_extent).min(w) {
            let idx = ((y * w + x) * 8) as usize;
            let px = &bytes[idx..idx + 8];
            let r = f16::from_le_bytes([px[0], px[1]]).to_f32();
            let g = f16::from_le_bytes([px[2], px[3]]).to_f32();
            let b = f16::from_le_bytes([px[4], px[5]]).to_f32();
            assert!(r.is_finite() && g.is_finite() && b.is_finite(), "non-finite pixel");
            sr += r as f64;
            sg += g as f64;
            sb += b as f64;
            n += 1;
        }
    }
    let n = n.max(1) as f64;
    (sr / n, sg / n, sb / n)
}

// ===================== identity parity =====================

/// One grid-mesh object, `objects=1, lights=0`, unlit material (no light
/// node needed). `wired` selects whether `instances_0` carries the real
/// 1-entry identity buffer or is left unwired (Rust-side stub).
fn identity_parity_scene_json(wired: bool) -> String {
    let mut nodes = String::from(
        r#"{"id":0,"typeId":"system.generator_input","nodeId":"input"},
        {"id":1,"typeId":"node.orbit_camera","nodeId":"cam","params":{
            "orbit":{"type":"Float","value":0.6},
            "tilt":{"type":"Float","value":0.5},
            "distance":{"type":"Float","value":8.0},
            "fov_y":{"type":"Float","value":0.8}}},
        {"id":2,"typeId":"node.grid_mesh","nodeId":"grid","params":{
            "max_capacity":{"type":"Int","value":4096},
            "resolution_x":{"type":"Int","value":12},
            "resolution_y":{"type":"Int","value":12},
            "size_x":{"type":"Float","value":4.0},
            "size_y":{"type":"Float","value":4.0}}},
        {"id":3,"typeId":"node.make_triangles","nodeId":"tris","params":{
            "src_cols":{"type":"Int","value":12},
            "src_rows":{"type":"Int","value":12}}},
        {"id":4,"typeId":"node.unlit_material","nodeId":"mat","params":{
            "color_r":{"type":"Float","value":0.8},
            "color_g":{"type":"Float","value":0.5},
            "color_b":{"type":"Float","value":0.2}}},
        {"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{
            "objects":{"type":"Int","value":1},
            "lights":{"type":"Int","value":0}}},
        {"id":99,"typeId":"system.final_output","nodeId":"out"}"#,
    );

    let mut wires = String::from(
        r#"{"fromNode":2,"fromPort":"vertices","toNode":3,"toPort":"in"},
        {"fromNode":3,"fromPort":"out","toNode":20,"toPort":"mesh_0"},
        {"fromNode":4,"fromPort":"out","toNode":20,"toPort":"material_0"},
        {"fromNode":1,"fromPort":"out","toNode":20,"toPort":"camera"},
        {"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}"#,
    );

    if wired {
        nodes.push(',');
        nodes.push_str(&identity_instance_node(30, "ident", 0.0));
        wires.push_str(r#",{"fromNode":30,"fromPort":"instances","toNode":20,"toPort":"instances_0"}"#);
    }

    format!(r#"{{"version":2,"name":"RenderSceneInstanceIdentityParity","nodes":[{nodes}],"wires":[{wires}]}}"#)
}

#[test]
fn wired_identity_instance_buffer_renders_byte_identical_to_unwired() {
    let (wired_bytes, w, h) = render_readback(&identity_parity_scene_json(true));
    let (unwired_bytes, _, _) = render_readback(&identity_parity_scene_json(false));

    write_png(&wired_bytes, w, h, "/tmp/render_scene_instances_identity_wired.png");
    write_png(&unwired_bytes, w, h, "/tmp/render_scene_instances_identity_unwired.png");

    let (_, peak) = luma(&wired_bytes);
    assert!(peak > 0.05, "identity-instance scene should not be blank (peak {peak})");
    assert_eq!(
        wired_bytes, unwired_bytes,
        "a wired 1-entry identity instances_n buffer must render byte-identical \
         to the same object with instances_n left unwired"
    );
}

// ===================== occlusion =====================

/// A grey occluder wall (rotated to face the camera, at world z=0) plus a
/// small red marker drawn via a wired `instances_1` on object 1, placed at
/// `marker_z` along the SAME axis the camera looks down — `pos_z > 0` is
/// between the camera (z=10) and the occluder (closer, visible over it);
/// `pos_z < 0` is farther than the occluder (hidden behind it). `look_at_camera`
/// at (0,0,10) -> (0,0,0) keeps this exactly axis-aligned, no oblique-view
/// trigonometry to get wrong. Unlit materials — no light node needed.
fn occlusion_scene_json(marker_z: f32) -> String {
    const HALF_PI: f32 = std::f32::consts::FRAC_PI_2;
    let nodes = format!(
        r#"{{"id":0,"typeId":"system.generator_input","nodeId":"input"}},
        {{"id":1,"typeId":"node.look_at_camera","nodeId":"cam","params":{{
            "pos_x":{{"type":"Float","value":0.0}},
            "pos_y":{{"type":"Float","value":0.0}},
            "pos_z":{{"type":"Float","value":10.0}},
            "target_x":{{"type":"Float","value":0.0}},
            "target_y":{{"type":"Float","value":0.0}},
            "target_z":{{"type":"Float","value":0.0}},
            "fov_y":{{"type":"Float","value":0.8}}}}}},
        {{"id":2,"typeId":"node.grid_mesh","nodeId":"occ_grid","params":{{
            "max_capacity":{{"type":"Int","value":4096}},
            "resolution_x":{{"type":"Int","value":10}},
            "resolution_y":{{"type":"Int","value":10}},
            "size_x":{{"type":"Float","value":3.0}},
            "size_y":{{"type":"Float","value":3.0}}}}}},
        {{"id":3,"typeId":"node.make_triangles","nodeId":"occ_tris","params":{{
            "src_cols":{{"type":"Int","value":10}},
            "src_rows":{{"type":"Int","value":10}}}}}},
        {{"id":4,"typeId":"node.transform_3d","nodeId":"occ_xform","params":{{
            "rot_x":{{"type":"Float","value":{HALF_PI}}}}}}},
        {{"id":5,"typeId":"node.unlit_material","nodeId":"occ_mat","params":{{
            "color_r":{{"type":"Float","value":0.5}},
            "color_g":{{"type":"Float","value":0.5}},
            "color_b":{{"type":"Float","value":0.5}}}}}},
        {{"id":6,"typeId":"node.grid_mesh","nodeId":"marker_grid","params":{{
            "max_capacity":{{"type":"Int","value":256}},
            "resolution_x":{{"type":"Int","value":4}},
            "resolution_y":{{"type":"Int","value":4}},
            "size_x":{{"type":"Float","value":2.2}},
            "size_y":{{"type":"Float","value":2.2}}}}}},
        {{"id":7,"typeId":"node.make_triangles","nodeId":"marker_tris","params":{{
            "src_cols":{{"type":"Int","value":4}},
            "src_rows":{{"type":"Int","value":4}}}}}},
        {{"id":8,"typeId":"node.unlit_material","nodeId":"marker_mat","params":{{
            "color_r":{{"type":"Float","value":1.0}},
            "color_g":{{"type":"Float","value":0.0}},
            "color_b":{{"type":"Float","value":0.0}}}}}},
        {marker_inst},
        {{"id":10,"typeId":"node.transform_3d","nodeId":"marker_xform","params":{{
            "pos_z":{{"type":"Float","value":{marker_z}}}}}}},
        {{"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{{
            "objects":{{"type":"Int","value":2}},
            "lights":{{"type":"Int","value":0}}}}}},
        {{"id":99,"typeId":"system.final_output","nodeId":"out"}}"#,
        marker_inst = identity_instance_node(9, "marker_inst", HALF_PI),
    );

    let wires = r#"{"fromNode":2,"fromPort":"vertices","toNode":3,"toPort":"in"},
        {"fromNode":3,"fromPort":"out","toNode":20,"toPort":"mesh_0"},
        {"fromNode":5,"fromPort":"out","toNode":20,"toPort":"material_0"},
        {"fromNode":4,"fromPort":"transform","toNode":20,"toPort":"transform_0"},
        {"fromNode":1,"fromPort":"out","toNode":20,"toPort":"camera"},
        {"fromNode":6,"fromPort":"vertices","toNode":7,"toPort":"in"},
        {"fromNode":7,"fromPort":"out","toNode":20,"toPort":"mesh_1"},
        {"fromNode":8,"fromPort":"out","toNode":20,"toPort":"material_1"},
        {"fromNode":9,"fromPort":"instances","toNode":20,"toPort":"instances_1"},
        {"fromNode":10,"fromPort":"transform","toNode":20,"toPort":"transform_1"},
        {"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}"#;

    format!(r#"{{"version":2,"name":"RenderSceneInstanceOcclusion","nodes":[{nodes}],"wires":[{wires}]}}"#)
}

#[test]
fn instance_fully_behind_an_occluder_contributes_no_pixels() {
    let (near_bytes, w, h) = render_readback(&occlusion_scene_json(1.5));
    let (far_bytes, _, _) = render_readback(&occlusion_scene_json(-1.5));

    write_png(&near_bytes, w, h, "/tmp/render_scene_instances_occlusion_near.png");
    write_png(&far_bytes, w, h, "/tmp/render_scene_instances_occlusion_far.png");

    let (nr, ng, nb) = mean_rgb_center(&near_bytes, w, h, 15);
    let (fr, fg, fb) = mean_rgb_center(&far_bytes, w, h, 15);
    eprintln!("occlusion near-center rgb=({nr:.3},{ng:.3},{nb:.3}) far-center rgb=({fr:.3},{fg:.3},{fb:.3})");

    // In front of the occluder: the red marker instance wins the depth
    // test and dominates the frame centre — red clearly above green/blue.
    assert!(
        nr > ng + 0.15 && nr > nb + 0.15,
        "instance in front of the occluder must be visible (red-dominant): rgb=({nr:.3},{ng:.3},{nb:.3})"
    );
    // Behind the occluder: the instance contributes NO pixels — the centre
    // reads as the grey occluder (r≈g≈b), not red.
    assert!(
        (fr - fg).abs() < 0.05 && (fr - fb).abs() < 0.05,
        "instance fully behind the occluder must contribute no pixels (grey occluder only): rgb=({fr:.3},{fg:.3},{fb:.3})"
    );
    assert!(
        nr > fr + 0.15,
        "front-of-occluder red must clearly exceed behind-occluder red: near_r={nr:.3} far_r={fr:.3}"
    );
}

// ===================== shadow =====================

/// Ground receiver (object 0, direct mesh) + occluder caster (object 1,
/// drawn via a wired `instances_1` identity buffer so the shadow pre-pass
/// exercises the instance path) lit by one overhead sun. Mirrors
/// `render_scene_shadows.rs`'s decisive on/off diff, on the instanced caster.
fn shadow_instanced_scene_json(cast: bool) -> String {
    let cast_v = if cast { 1.0 } else { 0.0 };
    format!(
        r#"{{"version":2,"name":"RenderSceneInstanceShadow","nodes":[
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
        {marker_inst},
        {{"id":3,"typeId":"node.orbit_camera","nodeId":"cam","params":{{
            "orbit":{{"type":"Float","value":0.7}},
            "tilt":{{"type":"Float","value":0.95}},
            "distance":{{"type":"Float","value":10.0}},
            "fov_y":{{"type":"Float","value":0.8}}}}}},
        {{"id":4,"typeId":"node.phong_material","nodeId":"mat","params":{{
            "color_r":{{"type":"Float","value":1.0}},
            "color_g":{{"type":"Float","value":1.0}},
            "color_b":{{"type":"Float","value":1.0}},
            "ambient":{{"type":"Float","value":0.05}}}}}},
        {{"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{{
            "objects":{{"type":"Int","value":2}},
            "lights":{{"type":"Int","value":1}}}}}},
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
            "cast_shadows":{{"type":"Float","value":{cast_v}}}}}}},
        {{"id":99,"typeId":"system.final_output","nodeId":"out"}}],
        "wires":[
        {{"fromNode":1,"fromPort":"vertices","toNode":2,"toPort":"in"}},
        {{"fromNode":2,"fromPort":"out","toNode":20,"toPort":"mesh_0"}},
        {{"fromNode":5,"fromPort":"vertices","toNode":6,"toPort":"in"}},
        {{"fromNode":6,"fromPort":"out","toNode":20,"toPort":"mesh_1"}},
        {{"fromNode":7,"fromPort":"transform","toNode":20,"toPort":"transform_1"}},
        {{"fromNode":9,"fromPort":"instances","toNode":20,"toPort":"instances_1"}},
        {{"fromNode":3,"fromPort":"out","toNode":20,"toPort":"camera"}},
        {{"fromNode":4,"fromPort":"out","toNode":20,"toPort":"material_0"}},
        {{"fromNode":4,"fromPort":"out","toNode":20,"toPort":"material_1"}},
        {{"fromNode":30,"fromPort":"out","toNode":20,"toPort":"light_0"}},
        {{"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}}]}}"#,
        marker_inst = identity_instance_node(9, "occ_inst", 0.0),
    )
}

#[test]
fn instanced_occluder_still_casts_a_shadow_that_darkens_the_ground() {
    let (on_bytes, w, h) = render_readback(&shadow_instanced_scene_json(true));
    let (off_bytes, _, _) = render_readback(&shadow_instanced_scene_json(false));

    write_png(&on_bytes, w, h, "/tmp/render_scene_instances_shadow_on.png");
    write_png(&off_bytes, w, h, "/tmp/render_scene_instances_shadow_off.png");

    let (sum_on, peak_on) = luma(&on_bytes);
    let (sum_off, peak_off) = luma(&off_bytes);

    assert!(peak_on > 0.2, "shadowed frame is unlit (peak {peak_on})");
    assert!(peak_off > 0.2, "unshadowed frame is unlit (peak {peak_off})");

    let drop = (sum_off - sum_on) / sum_off;
    eprintln!(
        "instanced-caster shadow luma: off={sum_off:.1} on={sum_on:.1} drop={:.2}%",
        drop * 100.0
    );
    assert!(
        sum_on < sum_off && drop > 0.01,
        "an instance between a sun caster and the ground must darken the ground: \
         off={sum_off:.1} on={sum_on:.1} drop={:.2}%",
        drop * 100.0
    );
}

// ===================== fog =====================

/// Large grazing-angle ground plane drawn via a wired `instances_0`
/// identity buffer (mirrors `render_scene_fog.rs`'s scene, instanced),
/// proving `apply_fog`'s `world_pos` is correct for instanced vertices.
fn fog_instanced_scene_json(fog: Option<(f32, f32, f32, f32)>) -> String {
    let mut nodes = String::from(
        r#"{"id":0,"typeId":"system.generator_input","nodeId":"input"},
        {"id":1,"typeId":"node.grid_mesh","nodeId":"ground_grid","params":{
            "max_capacity":{"type":"Int","value":16384},
            "resolution_x":{"type":"Int","value":32},
            "resolution_y":{"type":"Int","value":32},
            "size_x":{"type":"Float","value":40.0},
            "size_y":{"type":"Float","value":40.0}}},
        {"id":2,"typeId":"node.make_triangles","nodeId":"ground_tris","params":{
            "src_cols":{"type":"Int","value":32},
            "src_rows":{"type":"Int","value":32}}},
        {"id":3,"typeId":"node.orbit_camera","nodeId":"cam","params":{
            "orbit":{"type":"Float","value":0.0},
            "tilt":{"type":"Float","value":0.12},
            "distance":{"type":"Float","value":15.0},
            "fov_y":{"type":"Float","value":1.0}}},
        {"id":4,"typeId":"node.phong_material","nodeId":"mat","params":{
            "color_r":{"type":"Float","value":1.0},
            "color_g":{"type":"Float","value":1.0},
            "color_b":{"type":"Float","value":1.0},
            "ambient":{"type":"Float","value":0.1}}},
        {"id":30,"typeId":"node.light","nodeId":"sun","params":{
            "mode":{"type":"Enum","value":0},
            "pos_x":{"type":"Float","value":0.0},
            "pos_y":{"type":"Float","value":30.0},
            "pos_z":{"type":"Float","value":0.0},
            "aim_x":{"type":"Float","value":0.0},
            "aim_y":{"type":"Float","value":0.0},
            "aim_z":{"type":"Float","value":0.0},
            "color_r":{"type":"Float","value":1.0},
            "color_g":{"type":"Float","value":1.0},
            "color_b":{"type":"Float","value":1.0},
            "intensity":{"type":"Float","value":1.0},
            "cast_shadows":{"type":"Float","value":0.0}}}"#,
    );
    nodes.push(',');
    nodes.push_str(&identity_instance_node(40, "ground_inst", 0.0));
    nodes.push_str(
        r#",{"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{
            "objects":{"type":"Int","value":1},
            "lights":{"type":"Int","value":1}}},
        {"id":99,"typeId":"system.final_output","nodeId":"out"}"#,
    );

    let mut wires = String::from(
        r#"{"fromNode":1,"fromPort":"vertices","toNode":2,"toPort":"in"},
        {"fromNode":2,"fromPort":"out","toNode":20,"toPort":"mesh_0"},
        {"fromNode":40,"fromPort":"instances","toNode":20,"toPort":"instances_0"},
        {"fromNode":3,"fromPort":"out","toNode":20,"toPort":"camera"},
        {"fromNode":4,"fromPort":"out","toNode":20,"toPort":"material_0"},
        {"fromNode":30,"fromPort":"out","toNode":20,"toPort":"light_0"},
        {"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}"#,
    );

    if let Some((density, r, g, b)) = fog {
        nodes.push_str(&format!(
            r#",{{"id":41,"typeId":"node.atmosphere","nodeId":"atmo","params":{{
                "fog_color_r":{{"type":"Float","value":{r}}},
                "fog_color_g":{{"type":"Float","value":{g}}},
                "fog_color_b":{{"type":"Float","value":{b}}},
                "fog_density":{{"type":"Float","value":{density}}},
                "height_falloff":{{"type":"Float","value":0.0}}}}}}"#,
        ));
        wires.push_str(r#",{"fromNode":41,"fromPort":"atmosphere","toNode":20,"toPort":"atmosphere"}"#);
    }

    format!(r#"{{"version":2,"name":"RenderSceneInstanceFog","nodes":[{nodes}],"wires":[{wires}]}}"#)
}

/// Mean (r, g, b) over lit (non-black) pixels — same shape as
/// `render_scene_fog.rs`'s helper.
fn mean_lit_rgb(bytes: &[u8]) -> (f64, f64, f64) {
    let (mut sr, mut sg, mut sb, mut n) = (0.0f64, 0.0f64, 0.0f64, 0u64);
    for px in bytes.chunks_exact(8) {
        let r = f16::from_le_bytes([px[0], px[1]]).to_f32();
        let g = f16::from_le_bytes([px[2], px[3]]).to_f32();
        let b = f16::from_le_bytes([px[4], px[5]]).to_f32();
        assert!(r.is_finite() && g.is_finite() && b.is_finite(), "non-finite pixel");
        if r + g + b > 0.02 {
            sr += r as f64;
            sg += g as f64;
            sb += b as f64;
            n += 1;
        }
    }
    let n = n.max(1) as f64;
    (sr / n, sg / n, sb / n)
}

#[test]
fn far_instance_shifts_toward_the_fog_color() {
    let (fog_bytes, w, h) = render_readback(&fog_instanced_scene_json(Some((0.06, 0.1, 0.3, 0.9))));
    let (clear_bytes, _, _) = render_readback(&fog_instanced_scene_json(None));

    write_png(&fog_bytes, w, h, "/tmp/render_scene_instances_fog_on.png");
    write_png(&clear_bytes, w, h, "/tmp/render_scene_instances_fog_off.png");

    let (fr, fg, fb) = mean_lit_rgb(&fog_bytes);
    let (cr, cg, cb) = mean_lit_rgb(&clear_bytes);
    eprintln!("instanced fog mean rgb = ({fr:.3},{fg:.3},{fb:.3}), clear = ({cr:.3},{cg:.3},{cb:.3})");

    assert!(cr > 0.2 && (cr - cb).abs() < 0.05, "clear instanced scene should be ~neutral white");
    assert!(
        fb > fr + 0.05 && fb > fg + 0.02,
        "blue fog on an instanced object must make blue the dominant channel: fog rgb=({fr:.3},{fg:.3},{fb:.3})"
    );
}
