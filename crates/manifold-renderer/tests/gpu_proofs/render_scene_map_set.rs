//! `node.render_scene` per-object map-set proof (IMPORT_FIDELITY_DESIGN.md
//! D3/D4, F-P2). Numeric, no image judgment — same discipline as
//! `render_scene_ibl.rs`.
//!
//! Test texture recipe: `node.linear_gradient` -> `node.gradient_map` with
//! `color_a == color_b` collapses to an exact SOLID colour texture
//! regardless of the gradient's own luma (the ramp lerp is a no-op when
//! both stops match), premultiplied by `color_a.a` -- setting alpha to 1.0
//! makes that premultiply a no-op, so the sampled texel equals `color_a.rgb`
//! exactly. This is how every fixed-texel test below builds its normal/mr/
//! occlusion/emissive map without any file I/O.
//!
//! JSON is built with plain string concatenation (`push_str`/`format!` on
//! individual scalar placeholders only) rather than brace-heavy nested
//! `format!` templates -- deeply escaped `{{`/`}}` JSON literals are easy
//! to miscount; string pieces avoid that failure mode entirely.

use half::f16;
use manifold_gpu::GpuTextureFormat;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;

use crate::harness;

/// `node.linear_gradient` (id `id_base`) -> `node.gradient_map`
/// (id `id_base+1`) with `color_a == color_b == rgba` -- a solid-colour
/// texture fixture, no file I/O. Returns `(node_json, wire_json, output_node_id)`.
fn solid_texture_nodes(id_base: u32, rgba: [f32; 4]) -> (String, String, u32) {
    let grad_id = id_base;
    let ramp_id = id_base + 1;
    let color = format!("[{},{},{},{}]", rgba[0], rgba[1], rgba[2], rgba[3]);
    let nodes = format!(
        "{{\"id\":{grad_id},\"typeId\":\"node.linear_gradient\",\"nodeId\":\"solid_src_{grad_id}\",\"params\":{{\
            \"cx\":{{\"type\":\"Float\",\"value\":-5.0}},\
            \"softness\":{{\"type\":\"Float\",\"value\":0.0}}}}}},\
        {{\"id\":{ramp_id},\"typeId\":\"node.gradient_map\",\"nodeId\":\"solid_{grad_id}\",\"params\":{{\
            \"color_a\":{{\"type\":\"Color\",\"value\":{color}}},\
            \"color_b\":{{\"type\":\"Color\",\"value\":{color}}}}}}},"
    );
    let wires = format!(
        "{{\"fromNode\":{grad_id},\"fromPort\":\"out\",\"toNode\":{ramp_id},\"toPort\":\"source\"}},"
    );
    (nodes, wires, ramp_id)
}

/// Render a scene-graph JSON to `Rgba16Float`, returning readback bytes.
/// Two committed frames (pipeline warm-up + F-P1's per-frame IBL
/// convolution past); `commit_and_wait_completed` hard-checks Metal errors.
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
    .unwrap_or_else(|e| panic!("map-set scene graph must build: {e}\n{json}"));

    let target = h.make_target("render-scene-map-set");
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
        let mut enc = h.device.create_encoder("render-scene-map-set-enc");
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

fn luma(rgb: [f32; 3]) -> f32 {
    0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2]
}

/// Common scene skeleton: a flat `grid_mesh` in the XZ plane (N = +Y, UV
/// axis-aligned to world X/Z -- `generate_grid_mesh_body.wgsl`), viewed
/// nearly top-down by `node.orbit_camera` (tilt close to vertical so
/// `dot(world_normal, V) > 0` for every plausible tilted normal below --
/// no `fs_phong` N-flip). `node.light` defaults to a Sun at
/// `pos=(0,30,0) aim=(0,0,0)` -- rays travel straight down, so
/// `l_dir = -dir` (the "toward light" direction `fs_phong` reads) is
/// exactly `(0,1,0)`, world-space UP. `cast_shadows` is forced off so
/// `shadow_factor` never runs (this scene has no caster fixture).
struct GridScene {
    nodes: String,
    wires: String,
}

fn grid_camera_light_scene() -> GridScene {
    let nodes = concat!(
        "{\"id\":1,\"typeId\":\"node.grid_mesh\",\"nodeId\":\"grid\",\"params\":{",
        "\"max_capacity\":{\"type\":\"Int\",\"value\":256},",
        "\"resolution_x\":{\"type\":\"Int\",\"value\":8},",
        "\"resolution_y\":{\"type\":\"Int\",\"value\":8},",
        "\"size_x\":{\"type\":\"Float\",\"value\":6.0},",
        "\"size_y\":{\"type\":\"Float\",\"value\":6.0}}},",
        "{\"id\":2,\"typeId\":\"node.make_triangles\",\"nodeId\":\"tris\",\"params\":{",
        "\"src_cols\":{\"type\":\"Int\",\"value\":8},",
        "\"src_rows\":{\"type\":\"Int\",\"value\":8}}},",
        "{\"id\":3,\"typeId\":\"node.orbit_camera\",\"nodeId\":\"cam\",\"params\":{",
        "\"orbit\":{\"type\":\"Float\",\"value\":0.0},",
        "\"tilt\":{\"type\":\"Float\",\"value\":1.4},",
        "\"distance\":{\"type\":\"Float\",\"value\":6.0},",
        "\"fov_y\":{\"type\":\"Float\",\"value\":0.6}}},",
        "{\"id\":5,\"typeId\":\"node.light\",\"nodeId\":\"sun\",\"params\":{",
        "\"cast_shadows\":{\"type\":\"Float\",\"value\":0.0}}},",
    )
    .to_string();
    let wires = concat!(
        "{\"fromNode\":1,\"fromPort\":\"vertices\",\"toNode\":2,\"toPort\":\"in\"},",
        "{\"fromNode\":2,\"fromPort\":\"out\",\"toNode\":20,\"toPort\":\"mesh_0\"},",
        "{\"fromNode\":3,\"fromPort\":\"out\",\"toNode\":20,\"toPort\":\"camera\"},",
        "{\"fromNode\":5,\"fromPort\":\"out\",\"toNode\":20,\"toPort\":\"light_0\"},",
    )
    .to_string();
    GridScene { nodes, wires }
}

fn phong_material_node(color: [f32; 3], ambient: f32, specular: f32) -> String {
    format!(
        "{{\"id\":4,\"typeId\":\"node.phong_material\",\"nodeId\":\"mat\",\"params\":{{\
            \"color_r\":{{\"type\":\"Float\",\"value\":{}}},\
            \"color_g\":{{\"type\":\"Float\",\"value\":{}}},\
            \"color_b\":{{\"type\":\"Float\",\"value\":{}}},\
            \"ambient\":{{\"type\":\"Float\",\"value\":{ambient}}},\
            \"specular_color_r\":{{\"type\":\"Float\",\"value\":{specular}}},\
            \"specular_color_g\":{{\"type\":\"Float\",\"value\":{specular}}},\
            \"specular_color_b\":{{\"type\":\"Float\",\"value\":{specular}}}}}}},",
        color[0], color[1], color[2]
    )
}

/// Assemble a full graph JSON: preamble + node fragments +
/// `node.render_scene` + `system.final_output`, then the wires section
/// (material_0 wire is always object 4 -> scene, given first).
fn assemble(name: &str, extra_nodes: &str, extra_wires: &str, objects: u32, lights: u32) -> String {
    format!(
        "{{\"version\":2,\"name\":\"{name}\",\"nodes\":[\
        {{\"id\":0,\"typeId\":\"system.generator_input\",\"nodeId\":\"input\"}},\
        {extra_nodes}\
        {{\"id\":20,\"typeId\":\"node.render_scene\",\"nodeId\":\"scene\",\"params\":{{\
            \"objects\":{{\"type\":\"Int\",\"value\":{objects}}},\
            \"lights\":{{\"type\":\"Int\",\"value\":{lights}}}}}}},\
        {{\"id\":99,\"typeId\":\"system.final_output\",\"nodeId\":\"out\"}}\
        ],\"wires\":[\
        {{\"fromNode\":4,\"fromPort\":\"out\",\"toNode\":20,\"toPort\":\"material_0\"}},\
        {extra_wires}\
        {{\"fromNode\":20,\"fromPort\":\"color\",\"toNode\":99,\"toPort\":\"in\"}}\
        ]}}"
    )
}

/// D4: a tangent-space normal map tilts the surface normal by a PREDICTED
/// amount. `resolve_normal`'s cotangent frame reconstructs T/B purely from
/// the surface's own UV parameterization (`generate_grid_mesh_body.wgsl`:
/// `p = (x,0,z)`, `x = (nx-0.5)*size_x`, `z = (nz-0.5)*size_y`, both LINEAR
/// in `uv` with size_x,size_y > 0) -- so T and B are each confined to a
/// single world axis (X and Z respectively) REGARDLESS of the cotangent
/// formula's handedness convention; both have world-Y component exactly 0.
/// The vertex normal `N_vertex = (0,1,0)` is the frame's third column, so
/// `world_N.y = tangent_normal.z` EXACTLY, independent of any T/B sign
/// ambiguity. With a Sun light straight overhead (`L = (0,1,0)`) and
/// Phong's specular/ambient zeroed out, `lit = albedo * n_dot_l =
/// albedo * world_N.y = albedo * tangent_normal.z` -- a ratio-based,
/// camera/light-magnitude-independent, exactly computed prediction.
#[test]
fn normal_map_tilts_the_lit_value_by_the_cotangent_frames_predicted_amount() {
    // Packed tangent-space normal (tx=0, ty=0.6, tz=0.8 -- already unit
    // length) -> rgb = tangent*0.5+0.5.
    let tz: f32 = 0.8;
    let normal_rgb = [0.5, 0.8, 0.9, 1.0];

    let grid = grid_camera_light_scene();
    let material = phong_material_node([0.7, 0.7, 0.7], 0.0, 0.0);

    let unwired_extra_nodes = format!("{}{}", grid.nodes, material);
    let unwired_json = assemble("NormalMapUnwired", &unwired_extra_nodes, &grid.wires, 1, 1);

    let (tex_nodes, tex_wires, tex_out) = solid_texture_nodes(900, normal_rgb);
    let wired_extra_nodes = format!("{}{}{}", grid.nodes, material, tex_nodes);
    let wire_to_port = format!(
        "{{\"fromNode\":{tex_out},\"fromPort\":\"out\",\"toNode\":20,\"toPort\":\"normal_map_0\"}},"
    );
    let wired_extra_wires = format!("{}{}{}", grid.wires, tex_wires, wire_to_port);
    let wired_json = assemble("NormalMapWired", &wired_extra_nodes, &wired_extra_wires, 1, 1);

    let (unwired_bytes, w, h) = render_readback(&unwired_json);
    let (wired_bytes, _, _) = render_readback(&wired_json);

    let unwired_rgb = center_rgb(&unwired_bytes, w, h);
    let wired_rgb = center_rgb(&wired_bytes, w, h);
    let unwired_luma = luma(unwired_rgb);
    let wired_luma = luma(wired_rgb);

    eprintln!(
        "normal_map tilt: unwired_luma={unwired_luma:.5} wired_luma={wired_luma:.5} \
         ratio={:.4} predicted={tz:.4}",
        wired_luma / unwired_luma
    );

    assert!(unwired_luma.is_finite() && unwired_luma > 0.01, "unwired baseline must be lit");
    let ratio = wired_luma / unwired_luma;
    assert!(
        (ratio - tz).abs() < 0.03,
        "normal-map-tilted lit value ratio ({ratio:.4}) must match the cotangent frame's exact \
         prediction (tangent_normal.z = {tz:.4}) within tolerance"
    );
}

/// D3: the unwired case above (no `normal_map_0` wired) must reproduce the
/// classic Lambert formula EXACTLY -- `lit = albedo * n_dot_l` with
/// `N = N_vertex = (0,1,0)`, `L = (0,1,0)` (n_dot_l = 1). This is the
/// "unwired new ports render byte-identical to before this phase" negative
/// gate: `resolve_normal`'s unwired branch (`texture_flags.x == 0`) is
/// textually UNCHANGED by F-P2 (`return normalize(vertex_normal);`), so an
/// exact match here proves the new cotangent-frame code path (and the new
/// `texture_flags2` field it shares the uniform block with) introduces no
/// regression on the untouched path.
#[test]
fn unwired_normal_map_reproduces_the_pre_fp2_lambert_formula_exactly() {
    let grid = grid_camera_light_scene();
    let material = phong_material_node([0.7, 0.5, 0.3], 0.0, 0.0);
    let extra_nodes = format!("{}{}", grid.nodes, material);
    let json = assemble("UnwiredLambert", &extra_nodes, &grid.wires, 1, 1);

    let (bytes, w, h) = render_readback(&json);
    let rgb = center_rgb(&bytes, w, h);
    // node.light defaults: color (1,1,1), intensity 1.0 -> premultiplied
    // light.color = (1,1,1). n_dot_l = 1 (L = N = (0,1,0)).
    // lit = albedo * n_dot_l * light.color * intensity = albedo exactly.
    let expected = [0.7f32, 0.5, 0.3];
    eprintln!("unwired Lambert: got={rgb:?} expected={expected:?}");
    for c in 0..3 {
        assert!(
            (rgb[c] - expected[c]).abs() < 0.01,
            "channel {c}: got {} expected {} (unwired path must be byte-identical Lambert)",
            rgb[c],
            expected[c]
        );
    }
}

fn pbr_scene_nodes(albedo: [f32; 3], metallic: f32, roughness: f32, ambient: f32) -> String {
    format!(
        "{{\"id\":1,\"typeId\":\"node.grid_mesh\",\"nodeId\":\"grid\",\"params\":{{\
            \"max_capacity\":{{\"type\":\"Int\",\"value\":8192}},\
            \"resolution_x\":{{\"type\":\"Int\",\"value\":16}},\
            \"resolution_y\":{{\"type\":\"Int\",\"value\":16}},\
            \"size_x\":{{\"type\":\"Float\",\"value\":6.0}},\
            \"size_y\":{{\"type\":\"Float\",\"value\":6.0}}}}}},\
        {{\"id\":2,\"typeId\":\"node.make_triangles\",\"nodeId\":\"tris\",\"params\":{{\
            \"src_cols\":{{\"type\":\"Int\",\"value\":16}},\
            \"src_rows\":{{\"type\":\"Int\",\"value\":16}}}}}},\
        {{\"id\":7,\"typeId\":\"node.transform_3d\",\"nodeId\":\"xform\",\"params\":{{\
            \"rot_x\":{{\"type\":\"Float\",\"value\":1.2}}}}}},\
        {{\"id\":3,\"typeId\":\"node.orbit_camera\",\"nodeId\":\"cam\",\"params\":{{\
            \"orbit\":{{\"type\":\"Float\",\"value\":0.0}},\
            \"tilt\":{{\"type\":\"Float\",\"value\":0.5}},\
            \"distance\":{{\"type\":\"Float\",\"value\":2.5}},\
            \"fov_y\":{{\"type\":\"Float\",\"value\":1.4}}}}}},\
        {{\"id\":8,\"typeId\":\"node.bake_environment\",\"nodeId\":\"env\",\"params\":{{\
            \"width\":{{\"type\":\"Int\",\"value\":512}},\
            \"height\":{{\"type\":\"Int\",\"value\":256}},\
            \"intensity\":{{\"type\":\"Float\",\"value\":1.0}}}}}},\
        {{\"id\":4,\"typeId\":\"node.pbr_material\",\"nodeId\":\"mat\",\"params\":{{\
            \"color_r\":{{\"type\":\"Float\",\"value\":{ar}}},\
            \"color_g\":{{\"type\":\"Float\",\"value\":{ag}}},\
            \"color_b\":{{\"type\":\"Float\",\"value\":{ab}}},\
            \"ambient\":{{\"type\":\"Float\",\"value\":{ambient}}},\
            \"metallic\":{{\"type\":\"Float\",\"value\":{metallic}}},\
            \"roughness\":{{\"type\":\"Float\",\"value\":{roughness}}}}}}},",
        ar = albedo[0],
        ag = albedo[1],
        ab = albedo[2],
    )
}

const PBR_WIRES: &str = concat!(
    "{\"fromNode\":1,\"fromPort\":\"vertices\",\"toNode\":2,\"toPort\":\"in\"},",
    "{\"fromNode\":2,\"fromPort\":\"out\",\"toNode\":20,\"toPort\":\"mesh_0\"},",
    "{\"fromNode\":7,\"fromPort\":\"transform\",\"toNode\":20,\"toPort\":\"transform_0\"},",
    "{\"fromNode\":3,\"fromPort\":\"out\",\"toNode\":20,\"toPort\":\"camera\"},",
    "{\"fromNode\":8,\"fromPort\":\"envmap\",\"toNode\":20,\"toPort\":\"envmap\"},",
);

/// D3: MR map's B channel (metallic) drives F0 = mix(0.04, albedo, metallic)
/// in `fs_pbr`. Zero direct lights (IBL only) isolates the split-sum path.
/// At metallic=1, `kd_ibl = (1-f_view)*(1-metallic)` is EXACTLY zero, so the
/// ENTIRE output is `specular_ibl = prefiltered*(F0*lut.x+lut.y)` with
/// `F0 = albedo` -- a saturated (colourful) albedo must therefore tint the
/// metal render, proving metallic reached F0 (a broken wire would leave
/// F0 at the neutral dielectric 0.04, producing a near-neutral render).
#[test]
fn mr_map_blue_channel_drives_f0_via_metallic() {
    let albedo = [0.9f32, 0.5, 0.1]; // saturated, r > g > b

    let scene = |metallic: f32| {
        let (tex_nodes, tex_wires, tex_out) = solid_texture_nodes(900, [0.0, 0.5, metallic, 1.0]);
        let extra_nodes = format!("{}{}", pbr_scene_nodes(albedo, 0.0, 0.4, 0.0), tex_nodes);
        let wire_to_port = format!(
            "{{\"fromNode\":{tex_out},\"fromPort\":\"out\",\"toNode\":20,\"toPort\":\"mr_map_0\"}},"
        );
        let extra_wires = format!("{PBR_WIRES}{tex_wires}{wire_to_port}");
        assemble("MrMapF0", &extra_nodes, &extra_wires, 1, 0)
    };

    let (dielectric_bytes, w, h) = render_readback(&scene(0.0));
    let (metal_bytes, _, _) = render_readback(&scene(1.0));

    let d_rgb = center_rgb(&dielectric_bytes, w, h);
    let m_rgb = center_rgb(&metal_bytes, w, h);
    let d_ratio = d_rgb[0] / d_rgb[2].max(1e-4);
    let m_ratio = m_rgb[0] / m_rgb[2].max(1e-4);
    eprintln!(
        "mr_map F0: dielectric rgb={d_rgb:?} r/b={d_ratio:.3}; metal rgb={m_rgb:?} r/b={m_ratio:.3}"
    );

    assert!(d_rgb.iter().all(|c| c.is_finite()) && m_rgb.iter().all(|c| c.is_finite()));
    // Diffuse-dominated dielectric case: strongly tinted toward albedo's
    // r/b ratio (9.0).
    assert!(d_ratio > 3.0, "dielectric (metallic=0) render should be strongly albedo-tinted, got r/b={d_ratio:.3}");
    // Metal case (diffuse EXACTLY zero, kd_ibl=(1-f)*(1-metallic)=0): F0 =
    // albedo must still tint the specular-only result measurably above
    // neutral (1.0) -- proves mr_map's B channel reached F0, not the
    // untouched dielectric default (which would read near-neutral here).
    assert!(m_ratio > 1.05, "metallic=1 (F0=albedo) render should be measurably tinted above neutral, got r/b={m_ratio:.3}");
}

/// D3: emissive adds AFTER lighting -- present in the output even with zero
/// incident light. Unlit material (no lighting math at all) with BLACK
/// base_color isolates the emissive term completely:
/// `rgb = albedo(=0) + resolve_emissive(uv) = material.emission * texel`.
/// Exact value-level match, not a ratio.
#[test]
fn emissive_map_adds_after_lighting_with_zero_incident_light() {
    let emission_factor = [0.6f32, 0.4, 0.2]; // material's emission_rgb * emission_intensity
    let texel = [0.5f32, 0.5, 0.5, 1.0];
    let expected = [
        emission_factor[0] * texel[0],
        emission_factor[1] * texel[1],
        emission_factor[2] * texel[2],
    ];

    let unlit_material = format!(
        "{{\"id\":4,\"typeId\":\"node.unlit_material\",\"nodeId\":\"mat\",\"params\":{{\
            \"color_r\":{{\"type\":\"Float\",\"value\":0.0}},\
            \"color_g\":{{\"type\":\"Float\",\"value\":0.0}},\
            \"color_b\":{{\"type\":\"Float\",\"value\":0.0}},\
            \"emission_r\":{{\"type\":\"Float\",\"value\":{}}},\
            \"emission_g\":{{\"type\":\"Float\",\"value\":{}}},\
            \"emission_b\":{{\"type\":\"Float\",\"value\":{}}},\
            \"emission_intensity\":{{\"type\":\"Float\",\"value\":1.0}}}}}},",
        emission_factor[0], emission_factor[1], emission_factor[2]
    );

    let grid_nodes = concat!(
        "{\"id\":1,\"typeId\":\"node.grid_mesh\",\"nodeId\":\"grid\",\"params\":{",
        "\"max_capacity\":{\"type\":\"Int\",\"value\":256},",
        "\"resolution_x\":{\"type\":\"Int\",\"value\":4},",
        "\"resolution_y\":{\"type\":\"Int\",\"value\":4},",
        "\"size_x\":{\"type\":\"Float\",\"value\":6.0},",
        "\"size_y\":{\"type\":\"Float\",\"value\":6.0}}},",
        "{\"id\":2,\"typeId\":\"node.make_triangles\",\"nodeId\":\"tris\",\"params\":{",
        "\"src_cols\":{\"type\":\"Int\",\"value\":4},",
        "\"src_rows\":{\"type\":\"Int\",\"value\":4}}},",
        "{\"id\":3,\"typeId\":\"node.orbit_camera\",\"nodeId\":\"cam\",\"params\":{",
        "\"orbit\":{\"type\":\"Float\",\"value\":0.0},",
        "\"tilt\":{\"type\":\"Float\",\"value\":1.4},",
        "\"distance\":{\"type\":\"Float\",\"value\":6.0},",
        "\"fov_y\":{\"type\":\"Float\",\"value\":0.6}}},",
    );
    let grid_wires = concat!(
        "{\"fromNode\":1,\"fromPort\":\"vertices\",\"toNode\":2,\"toPort\":\"in\"},",
        "{\"fromNode\":2,\"fromPort\":\"out\",\"toNode\":20,\"toPort\":\"mesh_0\"},",
        "{\"fromNode\":3,\"fromPort\":\"out\",\"toNode\":20,\"toPort\":\"camera\"},",
    );

    let (tex_nodes, tex_wires, tex_out) = solid_texture_nodes(900, texel);
    let extra_nodes = format!("{grid_nodes}{unlit_material}{tex_nodes}");
    let wire_to_port = format!(
        "{{\"fromNode\":{tex_out},\"fromPort\":\"out\",\"toNode\":20,\"toPort\":\"emissive_map_0\"}},"
    );
    let extra_wires = format!("{grid_wires}{tex_wires}{wire_to_port}");
    let json = assemble("EmissiveNoLight", &extra_nodes, &extra_wires, 1, 0);

    let (bytes, w, h) = render_readback(&json);
    let rgb = center_rgb(&bytes, w, h);
    eprintln!("emissive (zero light): got={rgb:?} expected={expected:?}");
    for c in 0..3 {
        assert!(
            (rgb[c] - expected[c]).abs() < 0.01,
            "channel {c}: got {} expected {} -- emissive must equal material.emission * texel \
             exactly, with zero contribution from lighting",
            rgb[c],
            expected[c]
        );
    }
}

/// D3 (Invariants table): occlusion darkens ONLY the PBR diffuse IBL term --
/// never direct lighting, never specular IBL. Zero direct lights isolates
/// the split-sum path; `V(occ) = specular_ibl + occ * diffuse_ibl` is
/// LINEAR in `occ` if (and only if) occlusion multiplies one additive term
/// cleanly. Three renders (`occ` = 0.0, 0.5, 1.0) prove: (a) `V(0)` is
/// non-trivial -- specular_ibl survives full occlusion (a wire that occluded
/// the WHOLE lit result would zero this out); (b) `V(1) > V(0)` -- occlusion
/// measurably darkens something; (c) `V(0.5)` sits at the exact midpoint of
/// `V(0)`/`V(1)` -- the one-additive-term-only linear relationship.
#[test]
fn occlusion_map_darkens_diffuse_ibl_term_only() {
    let scene = |occlusion: f32, tex_id: u32| {
        let (tex_nodes, tex_wires, tex_out) = solid_texture_nodes(tex_id, [occlusion, 0.0, 0.0, 1.0]);
        let extra_nodes = format!("{}{}", pbr_scene_nodes([0.6, 0.6, 0.6], 0.4, 0.5, 0.0), tex_nodes);
        let wire_to_port = format!(
            "{{\"fromNode\":{tex_out},\"fromPort\":\"out\",\"toNode\":20,\"toPort\":\"occlusion_map_0\"}},"
        );
        let extra_wires = format!("{PBR_WIRES}{tex_wires}{wire_to_port}");
        assemble("OcclusionDiffuseOnly", &extra_nodes, &extra_wires, 1, 0)
    };

    let (b0, w, h) = render_readback(&scene(0.0, 900));
    let (b_half, _, _) = render_readback(&scene(0.5, 910));
    let (b1, _, _) = render_readback(&scene(1.0, 920));

    let v0 = center_rgb(&b0, w, h);
    let vh = center_rgb(&b_half, w, h);
    let v1 = center_rgb(&b1, w, h);
    eprintln!("occlusion linearity: V(0)={v0:?} V(0.5)={vh:?} V(1)={v1:?}");

    let l0 = luma(v0);
    let lh = luma(vh);
    let l1 = luma(v1);
    assert!(l0.is_finite() && l0 > 0.005, "specular_ibl must survive full occlusion (V(0)={l0:.5})");
    assert!(l1 > l0 * 1.05, "occlusion must measurably darken the result: V(1)={l1:.5} V(0)={l0:.5}");
    let predicted_mid = (l0 + l1) * 0.5;
    assert!(
        (lh - predicted_mid).abs() < (l1 - l0).max(1e-4) * 0.1,
        "V(0.5)={lh:.5} must sit at the linear midpoint of V(0)={l0:.5}/V(1)={l1:.5} \
         (predicted {predicted_mid:.5}) -- occlusion must multiply exactly ONE additive term"
    );
}
