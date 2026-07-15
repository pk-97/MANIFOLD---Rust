//! `node.render_scene` sorted transparent-pass proof (IMPORT_FIDELITY_DESIGN.md
//! D8, F-P5). Numeric, no image judgment — same discipline as
//! `render_scene_ibl.rs` / `render_scene_map_set.rs`.
//!
//! Scene shape shared by every test: a top-down `node.orbit_camera` over
//! stacked `node.grid_mesh` planes in the XZ plane (N = +Y — same convention
//! `render_scene_map_set.rs` documents), each object's height controlled by
//! its own `node.transform_3d` `pos_y`. Larger `pos_y` = physically closer
//! to the camera (which sits above, looking down), so "nearer" and "farther"
//! below always mean pos_y order, not draw order — draw order (back-to-front)
//! is exactly what D8's sort is being proven to get right.
//!
//! All materials here are Unlit (no lighting math), so measured pixel values
//! are pure compositing arithmetic — the reference formula
//! (`over(fg, fg_a, bg) = fg*fg_a + bg*(1-fg_a)`) is exact, not a ratio.

use half::f16;
use manifold_gpu::GpuTextureFormat;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;

use crate::harness;

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
    .unwrap_or_else(|e| panic!("glass scene graph must build: {e}\n{json}"));

    let target = h.make_target("render-scene-glass");
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
        let mut enc = h.device.create_encoder("render-scene-glass-enc");
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

fn center_rgb(bytes: &[u8], w: u32, h: u32) -> [f32; 3] {
    let idx = ((h / 2) * w + w / 2) as usize;
    let px = &bytes[idx * 8..idx * 8 + 8];
    [
        f16::from_le_bytes([px[0], px[1]]).to_f32(),
        f16::from_le_bytes([px[2], px[3]]).to_f32(),
        f16::from_le_bytes([px[4], px[5]]).to_f32(),
    ]
}

/// Straight-alpha "over" — the exact formula D8 commits the blend pipeline
/// to (`src_alpha / one_minus_src_alpha`).
fn over(fg: [f32; 3], fg_a: f32, bg: [f32; 3]) -> [f32; 3] {
    [
        fg[0] * fg_a + bg[0] * (1.0 - fg_a),
        fg[1] * fg_a + bg[1] * (1.0 - fg_a),
        fg[2] * fg_a + bg[2] * (1.0 - fg_a),
    ]
}

fn assert_rgb_close(got: [f32; 3], expected: [f32; 3], tol: f32, label: &str) {
    eprintln!("{label}: got={got:?} expected={expected:?}");
    for c in 0..3 {
        assert!(
            (got[c] - expected[c]).abs() < tol,
            "{label} channel {c}: got {} expected {} (tol {tol})",
            got[c],
            expected[c]
        );
    }
}

fn camera_node() -> &'static str {
    r#"{"id":3,"typeId":"node.orbit_camera","nodeId":"cam","params":{
        "orbit":{"type":"Float","value":0.0},
        "tilt":{"type":"Float","value":1.5},
        "distance":{"type":"Float","value":8.0},
        "fov_y":{"type":"Float","value":0.6}}},"#
}

/// One unlit plane object: `node.grid_mesh` + `node.make_triangles` +
/// `node.transform_3d` (pos_y only) + `node.unlit_material`, all id-offset
/// by `base` so multiple planes in one graph never collide.
fn plane_object(
    base: u32,
    pos_y: f32,
    color: [f32; 3],
    alpha: f32,
    alpha_mode: u32,
) -> (String, String) {
    let grid = base;
    let tris = base + 1;
    let xform = base + 2;
    let mat = base + 3;
    let nodes = format!(
        "{{\"id\":{grid},\"typeId\":\"node.grid_mesh\",\"nodeId\":\"grid_{base}\",\"params\":{{\
            \"max_capacity\":{{\"type\":\"Int\",\"value\":256}},\
            \"resolution_x\":{{\"type\":\"Int\",\"value\":4}},\
            \"resolution_y\":{{\"type\":\"Int\",\"value\":4}},\
            \"size_x\":{{\"type\":\"Float\",\"value\":6.0}},\
            \"size_y\":{{\"type\":\"Float\",\"value\":6.0}}}}}},\
        {{\"id\":{tris},\"typeId\":\"node.make_triangles\",\"nodeId\":\"tris_{base}\",\"params\":{{\
            \"src_cols\":{{\"type\":\"Int\",\"value\":4}},\
            \"src_rows\":{{\"type\":\"Int\",\"value\":4}}}}}},\
        {{\"id\":{xform},\"typeId\":\"node.transform_3d\",\"nodeId\":\"xform_{base}\",\"params\":{{\
            \"pos_y\":{{\"type\":\"Float\",\"value\":{pos_y}}}}}}},\
        {{\"id\":{mat},\"typeId\":\"node.unlit_material\",\"nodeId\":\"mat_{base}\",\"params\":{{\
            \"color_r\":{{\"type\":\"Float\",\"value\":{cr}}},\
            \"color_g\":{{\"type\":\"Float\",\"value\":{cg}}},\
            \"color_b\":{{\"type\":\"Float\",\"value\":{cb}}},\
            \"color_a\":{{\"type\":\"Float\",\"value\":{alpha}}},\
            \"alpha_mode\":{{\"type\":\"Enum\",\"value\":{alpha_mode}}}}}}},",
        cr = color[0],
        cg = color[1],
        cb = color[2],
    );
    (nodes, format!("{grid}|{tris}|{xform}|{mat}"))
}

/// Assemble object `slot` (`mesh_slot`/`transform_slot`/`material_slot`) from
/// a `plane_object` id quadruple string ("grid|tris|xform|mat").
fn wire_object(ids: &str, slot: u32) -> String {
    let parts: Vec<&str> = ids.split('|').collect();
    let (grid, tris, xform, mat) = (parts[0], parts[1], parts[2], parts[3]);
    format!(
        "{{\"fromNode\":{grid},\"fromPort\":\"vertices\",\"toNode\":{tris},\"toPort\":\"in\"}},\
        {{\"fromNode\":{tris},\"fromPort\":\"out\",\"toNode\":20,\"toPort\":\"mesh_{slot}\"}},\
        {{\"fromNode\":{xform},\"fromPort\":\"transform\",\"toNode\":20,\"toPort\":\"transform_{slot}\"}},\
        {{\"fromNode\":{mat},\"fromPort\":\"out\",\"toNode\":20,\"toPort\":\"material_{slot}\"}},"
    )
}

fn assemble(name: &str, extra_nodes: &str, extra_wires: &str, objects: u32) -> String {
    assemble_with_lights(name, extra_nodes, extra_wires, objects, 0)
}

fn assemble_with_lights(
    name: &str,
    extra_nodes: &str,
    extra_wires: &str,
    objects: u32,
    lights: u32,
) -> String {
    format!(
        "{{\"version\":2,\"name\":\"{name}\",\"nodes\":[\
        {{\"id\":0,\"typeId\":\"system.generator_input\",\"nodeId\":\"input\"}},\
        {camera}\
        {extra_nodes}\
        {{\"id\":20,\"typeId\":\"node.render_scene\",\"nodeId\":\"scene\",\"params\":{{\
            \"objects\":{{\"type\":\"Int\",\"value\":{objects}}},\
            \"lights\":{{\"type\":\"Int\",\"value\":{lights}}}}}}},\
        {{\"id\":99,\"typeId\":\"system.final_output\",\"nodeId\":\"out\"}}\
        ],\"wires\":[\
        {{\"fromNode\":3,\"fromPort\":\"out\",\"toNode\":20,\"toPort\":\"camera\"}},\
        {extra_wires}\
        {{\"fromNode\":20,\"fromPort\":\"color\",\"toNode\":99,\"toPort\":\"in\"}}\
        ]}}",
        camera = camera_node(),
    )
}

/// D8: a glass plane over an opaque background must blend to the exact
/// straight-alpha "over" value — proves the blend pipeline's fixed-function
/// state (`src_alpha`/`one_minus_src_alpha`), not just "something changed".
#[test]
fn see_through_blend_matches_straight_alpha_over_formula() {
    let bg_color = [1.0f32, 1.0, 1.0]; // white opaque background
    let fg_color = [1.0f32, 0.2, 0.2]; // red-tinted glass
    let fg_alpha = 0.4f32;

    let (bg_nodes, bg_ids) = plane_object(100, 0.0, bg_color, 1.0, 0); // Opaque
    let (fg_nodes, fg_ids) = plane_object(200, 1.0, fg_color, fg_alpha, 2); // Blend, nearer

    let nodes = format!("{bg_nodes}{fg_nodes}");
    let wires = format!("{}{}", wire_object(&bg_ids, 0), wire_object(&fg_ids, 1));
    let json = assemble("GlassOverWhite", &nodes, &wires, 2);

    let (bytes, w, h) = render_readback(&json);
    let got = center_rgb(&bytes, w, h);
    let expected = over(fg_color, fg_alpha, bg_color);
    assert_rgb_close(got, expected, 0.02, "see-through over-blend");
}

/// D8: two stacked glass panes must sort back-to-front — swapping their
/// world positions (which one is nearer the camera) must swap which colour
/// dominates the compositing order, matching the "over(over())" formula
/// EACH way, not just "the two renders differ".
#[test]
fn swapping_pane_positions_swaps_the_blend_order() {
    let bg = [1.0f32, 1.0, 1.0];
    let red = [1.0f32, 0.0, 0.0];
    let blue = [0.0f32, 0.0, 1.0];
    let a_red = 0.5f32;
    let a_blue = 0.5f32;

    // Config 1: red pane nearer (pos_y 2.0) than blue (pos_y 1.0).
    let (bg_nodes, bg_ids) = plane_object(100, 0.0, bg, 1.0, 0);
    let (red_nodes_1, red_ids_1) = plane_object(200, 2.0, red, a_red, 2);
    let (blue_nodes_1, blue_ids_1) = plane_object(300, 1.0, blue, a_blue, 2);
    let nodes_1 = format!("{bg_nodes}{red_nodes_1}{blue_nodes_1}");
    let wires_1 = format!(
        "{}{}{}",
        wire_object(&bg_ids, 0),
        wire_object(&red_ids_1, 1),
        wire_object(&blue_ids_1, 2)
    );
    let json_1 = assemble("GlassSortConfig1", &nodes_1, &wires_1, 3);

    // Config 2: swapped — blue pane nearer (2.0), red farther (1.0).
    let (bg_nodes_2, bg_ids_2) = plane_object(100, 0.0, bg, 1.0, 0);
    let (red_nodes_2, red_ids_2) = plane_object(200, 1.0, red, a_red, 2);
    let (blue_nodes_2, blue_ids_2) = plane_object(300, 2.0, blue, a_blue, 2);
    let nodes_2 = format!("{bg_nodes_2}{red_nodes_2}{blue_nodes_2}");
    let wires_2 = format!(
        "{}{}{}",
        wire_object(&bg_ids_2, 0),
        wire_object(&red_ids_2, 1),
        wire_object(&blue_ids_2, 2)
    );
    let json_2 = assemble("GlassSortConfig2", &nodes_2, &wires_2, 3);

    let (bytes_1, w, h) = render_readback(&json_1);
    let (bytes_2, _, _) = render_readback(&json_2);
    let got_1 = center_rgb(&bytes_1, w, h);
    let got_2 = center_rgb(&bytes_2, w, h);

    // Config 1: red nearer -> over(red, over(blue, bg)).
    let mid_1 = over(blue, a_blue, bg);
    let expected_1 = over(red, a_red, mid_1);
    // Config 2: blue nearer -> over(blue, over(red, bg)).
    let mid_2 = over(red, a_red, bg);
    let expected_2 = over(blue, a_blue, mid_2);

    assert_rgb_close(got_1, expected_1, 0.02, "sort config 1 (red nearer)");
    assert_rgb_close(got_2, expected_2, 0.02, "sort config 2 (blue nearer, swapped)");
    let delta: f32 = (0..3).map(|c| (got_1[c] - got_2[c]).abs()).sum();
    assert!(
        delta > 0.05,
        "swapping pane positions must measurably change the composited colour, got delta={delta:.4} \
         ({got_1:?} vs {got_2:?})"
    );
}

/// D8 (Invariants table): a Blend object fully behind an opaque object
/// contributes nothing — the depth test (still ON in the blend pass, only
/// WRITE is off) must discard it against the opaque pass's already-resolved
/// depth. Compare against the identical scene with the glass object simply
/// absent: the two must render byte-nearly-identical.
#[test]
fn blend_object_fully_behind_opaque_contributes_nothing() {
    let opaque_color = [0.3f32, 0.6, 0.9];
    let glass_color = [1.0f32, 1.0, 0.0]; // would be strongly visible if it leaked through

    // Opaque nearer the camera (pos_y 2.0), glass farther behind it (pos_y 0.0).
    let (opaque_nodes, opaque_ids) = plane_object(100, 2.0, opaque_color, 1.0, 0);
    let (glass_nodes, glass_ids) = plane_object(200, 0.0, glass_color, 0.8, 2);

    let with_glass_nodes = format!("{opaque_nodes}{glass_nodes}");
    let with_glass_wires =
        format!("{}{}", wire_object(&opaque_ids, 0), wire_object(&glass_ids, 1));
    let with_glass_json = assemble("OccludedGlassPresent", &with_glass_nodes, &with_glass_wires, 2);

    let without_glass_json = assemble("OccludedGlassAbsent", &opaque_nodes, &wire_object(&opaque_ids, 0), 1);

    let (with_bytes, w, h) = render_readback(&with_glass_json);
    let (without_bytes, _, _) = render_readback(&without_glass_json);
    let got_with = center_rgb(&with_bytes, w, h);
    let got_without = center_rgb(&without_bytes, w, h);

    assert_rgb_close(
        got_with,
        got_without,
        0.01,
        "fully-occluded glass must contribute nothing (matches opaque-only render)",
    );
}

/// Ground fixture shared by `blend_material_casts_no_shadow`: a big Phong
/// plane (so `shadow_factor` actually runs — Unlit skips lighting math
/// entirely, which would make the test vacuous) + one sun whose
/// `cast_shadows` is the caller's choice. Fully self-closed fragment,
/// mirroring `render_scene_shadows.rs`'s `shadow_scene_json` node shapes
/// exactly.
fn ground_and_sun_nodes(cast_shadows: bool) -> String {
    let cast_v = if cast_shadows { 1.0 } else { 0.0 };
    format!(
        concat!(
            "{{\"id\":1,\"typeId\":\"node.grid_mesh\",\"nodeId\":\"ground_grid\",\"params\":{{",
            "\"max_capacity\":{{\"type\":\"Int\",\"value\":8192}},",
            "\"resolution_x\":{{\"type\":\"Int\",\"value\":20}},",
            "\"resolution_y\":{{\"type\":\"Int\",\"value\":20}},",
            "\"size_x\":{{\"type\":\"Float\",\"value\":8.0}},",
            "\"size_y\":{{\"type\":\"Float\",\"value\":8.0}}}}}},",
            "{{\"id\":2,\"typeId\":\"node.make_triangles\",\"nodeId\":\"ground_tris\",\"params\":{{",
            "\"src_cols\":{{\"type\":\"Int\",\"value\":20}},",
            "\"src_rows\":{{\"type\":\"Int\",\"value\":20}}}}}},",
            "{{\"id\":5,\"typeId\":\"node.light\",\"nodeId\":\"sun\",\"params\":{{",
            "\"mode\":{{\"type\":\"Enum\",\"value\":0}},",
            "\"pos_x\":{{\"type\":\"Float\",\"value\":3.0}},",
            "\"pos_y\":{{\"type\":\"Float\",\"value\":20.0}},",
            "\"pos_z\":{{\"type\":\"Float\",\"value\":3.0}},",
            "\"aim_x\":{{\"type\":\"Float\",\"value\":0.0}},",
            "\"aim_y\":{{\"type\":\"Float\",\"value\":0.0}},",
            "\"aim_z\":{{\"type\":\"Float\",\"value\":0.0}},",
            "\"cast_shadows\":{{\"type\":\"Float\",\"value\":{cast_v}}}}}}},",
            "{{\"id\":6,\"typeId\":\"node.phong_material\",\"nodeId\":\"ground_mat\",\"params\":{{",
            "\"color_r\":{{\"type\":\"Float\",\"value\":1.0}},",
            "\"color_g\":{{\"type\":\"Float\",\"value\":1.0}},",
            "\"color_b\":{{\"type\":\"Float\",\"value\":1.0}},",
            "\"ambient\":{{\"type\":\"Float\",\"value\":0.05}}}}}},",
        ),
        cast_v = cast_v,
    )
}

const GROUND_AND_SUN_WIRES: &str = concat!(
    "{\"fromNode\":1,\"fromPort\":\"vertices\",\"toNode\":2,\"toPort\":\"in\"},",
    "{\"fromNode\":2,\"fromPort\":\"out\",\"toNode\":20,\"toPort\":\"mesh_0\"},",
    "{\"fromNode\":6,\"fromPort\":\"out\",\"toNode\":20,\"toPort\":\"material_0\"},",
    "{\"fromNode\":5,\"fromPort\":\"out\",\"toNode\":20,\"toPort\":\"light_0\"},",
);

fn total_luma(bytes: &[u8]) -> f64 {
    let mut sum = 0.0f64;
    for px in bytes.chunks_exact(8) {
        let r = f16::from_le_bytes([px[0], px[1]]).to_f32();
        let g = f16::from_le_bytes([px[2], px[3]]).to_f32();
        let b = f16::from_le_bytes([px[4], px[5]]).to_f32();
        assert!(r.is_finite() && g.is_finite() && b.is_finite(), "non-finite pixel");
        sum += (0.2126 * r + 0.7152 * g + 0.0722 * b) as f64;
    }
    sum
}

/// D8/§4 Invariants: a Blend object between the sun and the ground casts NO
/// shadow — the object is excluded from every shadow-caster depth pass at
/// the CODE-PATH level, regardless of the light's own `cast_shadows` flag.
/// Same Blend occluder present in BOTH renders (so its own visible footprint
/// contributes identically to each — no confound from comparing frames with
/// a different object count); the ONLY variable is the sun's `cast_shadows`
/// (1 vs 0). If Blend truly never casts, toggling it must be a no-op.
#[test]
fn blend_material_casts_no_shadow() {
    let (occ_nodes, occ_ids) = plane_object(200, 1.5, [1.0, 1.0, 0.0], 0.7, 2); // Blend occluder
    let occ_wires = wire_object(&occ_ids, 1);

    let shadows_on_nodes = format!("{}{occ_nodes}", ground_and_sun_nodes(true));
    let shadows_on_wires = format!("{GROUND_AND_SUN_WIRES}{occ_wires}");
    let shadows_on_json =
        assemble_with_lights("BlendCastsNoShadowOn", &shadows_on_nodes, &shadows_on_wires, 2, 1);

    let shadows_off_nodes = format!("{}{occ_nodes}", ground_and_sun_nodes(false));
    let shadows_off_json =
        assemble_with_lights("BlendCastsNoShadowOff", &shadows_off_nodes, &shadows_on_wires, 2, 1);

    let (on_bytes, _w, _h) = render_readback(&shadows_on_json);
    let (off_bytes, _, _) = render_readback(&shadows_off_json);

    let sum_on = total_luma(&on_bytes);
    let sum_off = total_luma(&off_bytes);

    eprintln!("blend-casts-no-shadow luma: cast_shadows=1 -> {sum_on:.1}, cast_shadows=0 -> {sum_off:.1}");
    let drop = (sum_off - sum_on).abs() / sum_off;
    assert!(
        drop < 0.01,
        "a Blend occluder must cast NO shadow regardless of the light's cast_shadows flag: \
         cast_shadows=1 luma={sum_on:.1} cast_shadows=0 luma={sum_off:.1} differ by {:.2}% (expected < 1%)",
        drop * 100.0
    );
}
