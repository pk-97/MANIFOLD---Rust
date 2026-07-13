//! `node.render_scene` PCSS contact-hardening proof (REALTIME_3D_DESIGN
//! §11 P9 gate).
//!
//! Numeric, no image judgment (per the phase brief — no PNGs here, unlike
//! the sibling `render_scene_shadows` suite). Decisive scene: a small plate
//! occluder above a large ground plane, lit by one angled Sun with
//! `shadow_softness = Contact`. Rendered at two occluder heights — 0.3
//! ("contact": the shadow's leading/trailing edges are dominated by the
//! blocker-search's own near-zero radius, i.e. as hard as this scene gets)
//! and 4.0 ("far") — with the SAME near-orthographic camera (narrow FOV,
//! long distance — see the camera params below) so a fixed COLUMN's
//! world-units-per-pixel scale stays close to constant as the shadow's
//! screen position shifts with occluder height, isolating the true PCSS
//! penumbra-width effect from ordinary perspective foreshortening (an
//! oblique low-FOV camera was tried first and produced a confound: the
//! umbra's screen row moves toward the horizon as height grows, which
//! compresses its apparent pixel width and fights the widening PCSS is
//! supposed to produce).
//!
//! `SCANLINE_COL` is a fixed column scanned top-to-bottom (not
//! left-to-right): this occluder's shadow silhouette is wide in X and
//! narrow in the "into the distance" direction, so its front/back
//! penumbra — the transition PCSS actually widens — shows up as ROW
//! variation along one column, not column variation along one row.
//!
//! Shade is read per-pixel as `luma / lit_reference_luma`, where
//! `lit_reference_luma` is sampled from a ground point far from any shadow
//! — this turns absolute brightness into a stable 0 (fully shadowed) ..
//! 1 (fully lit) fraction without depending on scene exposure. Gradient
//! width = count of scanline pixels with `0.05 < shade < 0.95` (the PCSS
//! penumbra band).

use half::f16;
use manifold_gpu::GpuTextureFormat;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;

use crate::harness;

/// Ground plane (10×10 at y=0) + a small plate occluder (3×3) at height
/// `occluder_y`, lit by one Sun at `(3, 20, 3)` aimed at the origin — the
/// same light angle `render_scene_shadows`'s proven `shadow_scene_json`
/// uses. The camera is near-orthographic (narrow `fov_y`, long
/// `distance`) rather than that suite's wide-FOV framing — see the module
/// doc for why. `softness` picks the enum index (0=Hard, 1=Soft,
/// 2=VerySoft, 3=Contact); `light_size` only matters when `softness == 3`.
fn pcss_scene_json(occluder_y: f32, softness: u32, light_size: f32) -> String {
    format!(
        r#"{{"version":2,"name":"RenderScenePcssProof","nodes":[
        {{"id":0,"typeId":"system.generator_input","nodeId":"input"}},
        {{"id":1,"typeId":"node.grid_mesh","nodeId":"ground_grid","params":{{
            "max_capacity":{{"type":"Int","value":8192}},
            "resolution_x":{{"type":"Int","value":20}},
            "resolution_y":{{"type":"Int","value":20}},
            "size_x":{{"type":"Float","value":10.0}},
            "size_y":{{"type":"Float","value":10.0}}}}}},
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
            "pos_y":{{"type":"Float","value":{occluder_y}}}}}}},
        {{"id":3,"typeId":"node.orbit_camera","nodeId":"cam","params":{{
            "orbit":{{"type":"Float","value":0.7}},
            "tilt":{{"type":"Float","value":0.95}},
            "distance":{{"type":"Float","value":60.0}},
            "fov_y":{{"type":"Float","value":0.14}}}}}},
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
            "cast_shadows":{{"type":"Float","value":1.0}},
            "shadow_softness":{{"type":"Enum","value":{softness}}},
            "light_size":{{"type":"Float","value":{light_size}}}}}}},
        {{"id":99,"typeId":"system.final_output","nodeId":"out"}}
        ],"wires":[
        {{"fromNode":1,"fromPort":"vertices","toNode":2,"toPort":"in"}},
        {{"fromNode":2,"fromPort":"out","toNode":20,"toPort":"mesh_0"}},
        {{"fromNode":5,"fromPort":"vertices","toNode":6,"toPort":"in"}},
        {{"fromNode":6,"fromPort":"out","toNode":20,"toPort":"mesh_1"}},
        {{"fromNode":7,"fromPort":"transform","toNode":20,"toPort":"transform_1"}},
        {{"fromNode":3,"fromPort":"out","toNode":20,"toPort":"camera"}},
        {{"fromNode":4,"fromPort":"out","toNode":20,"toPort":"material_0"}},
        {{"fromNode":4,"fromPort":"out","toNode":20,"toPort":"material_1"}},
        {{"fromNode":30,"fromPort":"out","toNode":20,"toPort":"light_0"}},
        {{"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}}
        ]}}"#
    )
}

/// Render a scene-graph JSON to `Rgba16Float`, returning readback bytes.
/// Two committed frames so pipeline warm-up is past; `commit_and_wait_completed`
/// hard-checks for Metal GPU errors, so a bad shader compile surfaces as a
/// panic here, not a silently wrong frame.
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
    .expect("pcss scene graph must build");

    let target = h.make_target("render-scene-pcss");
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
        let mut enc = h.device.create_encoder("render-scene-pcss-enc");
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

/// Per-pixel luma (Rec.709) for one `Rgba16Float` readback.
fn luma_image(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(8)
        .map(|px| {
            let r = f16::from_le_bytes([px[0], px[1]]).to_f32();
            let g = f16::from_le_bytes([px[2], px[3]]).to_f32();
            let b = f16::from_le_bytes([px[4], px[5]]).to_f32();
            assert!(r.is_finite() && g.is_finite() && b.is_finite(), "non-finite pixel");
            0.2126 * r + 0.7152 * g + 0.0722 * b
        })
        .collect()
}

/// Shade profile down one fixed column, normalised by a known-lit
/// reference pixel `(REF_COL, REF_ROW)` — `0.0` = fully shadowed, `1.0` =
/// fully lit, independent of overall scene exposure.
fn shade_column(luma: &[f32], w: u32, h: u32, col: u32) -> Vec<f32> {
    let reference = luma[(REF_ROW * w + REF_COL) as usize].max(1e-6);
    (0..h).map(|row| luma[(row * w + col) as usize] / reference).collect()
}

/// Count of samples with shade strictly between `lo` and `hi` — the PCSS
/// penumbra band width, in pixels, along one scanline.
fn gradient_width(shade: &[f32], lo: f32, hi: f32) -> usize {
    shade.iter().filter(|&&s| s > lo && s < hi).count()
}

/// Scanline/reference constants, committed once from the scene geometry
/// above (128×128 canonical harness dims) — not re-derived per render.
/// `SCANLINE_COL` sits inside the occluder's shadow footprint for every
/// occluder height this suite tests (found empirically by scanning the
/// rendered frame — see the landing report). `REF_COL`/`REF_ROW` sample a
/// ground point near the top of frame, always outside the shadow.
const SCANLINE_COL: u32 = 64;
const REF_COL: u32 = 64;
const REF_ROW: u32 = 5;

/// Occluder heights bracketing the contact-hardening property: `NEAR` is
/// close enough to the ground that the blocker search's own near-zero
/// radius dominates (as hard an edge as this scene produces); `FAR` is
/// well clear of it. Chosen from an empirical sweep (0.15..6.0) that
/// showed `SCANLINE_COL`'s gradient width growing monotonically with
/// height at every column sampled — this pair gives a >4x margin over the
/// gate's 3x requirement, well clear of measurement noise.
const NEAR_HEIGHT: f32 = 0.3;
const FAR_HEIGHT: f32 = 4.0;

#[test]
fn contact_hardening_gradient_narrows_at_contact() {
    let (near_bytes, w, h) = render_readback(&pcss_scene_json(NEAR_HEIGHT, 3, 1.5));
    let (far_bytes, _, _) = render_readback(&pcss_scene_json(FAR_HEIGHT, 3, 1.5));

    let near_shade = shade_column(&luma_image(&near_bytes), w, h, SCANLINE_COL);
    let far_shade = shade_column(&luma_image(&far_bytes), w, h, SCANLINE_COL);

    let near_width = gradient_width(&near_shade, 0.05, 0.95);
    let far_width = gradient_width(&far_shade, 0.05, 0.95);

    eprintln!("PCSS gradient width: near={near_width}px far={far_width}px");
    assert!(
        far_width >= 3,
        "far-height render shows no measurable penumbra (width {far_width}px) — \
         scene geometry didn't produce a visible shadow edge on the committed scanline"
    );
    assert!(
        near_width * 3 <= far_width,
        "near-height gradient ({near_width}px) should be at least 3x narrower \
         than far-height ({far_width}px) — contact-hardening property failed"
    );
}

#[test]
fn contact_tier_with_zero_light_size_matches_hard_tier() {
    // Both at the near (hardest-edge) height — light_size=0 collapses the
    // PCSS branch to the exact same pcf_average(khw=1) call Hard uses.
    let (contact_bytes, w, h) = render_readback(&pcss_scene_json(NEAR_HEIGHT, 3, 0.0));
    let (hard_bytes, _, _) = render_readback(&pcss_scene_json(NEAR_HEIGHT, 0, 0.0));

    let contact_shade = shade_column(&luma_image(&contact_bytes), w, h, SCANLINE_COL);
    let hard_shade = shade_column(&luma_image(&hard_bytes), w, h, SCANLINE_COL);

    let contact_width = gradient_width(&contact_shade, 0.05, 0.95);
    let hard_width = gradient_width(&hard_shade, 0.05, 0.95);

    eprintln!("light_size=0 gradient width: contact={contact_width}px hard={hard_width}px");
    let diff = contact_width.abs_diff(hard_width);
    assert!(
        diff <= 1,
        "Contact{{light_size: 0}} ({contact_width}px) should match Hard-tier's hard edge \
         ({hard_width}px) within 1px, got a {diff}px difference"
    );
}

#[test]
fn existing_softness_tiers_are_unaffected_by_contact() {
    // D12 negative gate: adding the Contact tier must not perturb the
    // fixed-kernel tiers. Soft (index 1) at FAR_HEIGHT should still show a
    // measurable penumbra — the same property `render_scene_shadows`
    // checks via total luma, exercised here as an independent witness
    // through this file's own scene builder and column-based measure.
    let (soft_bytes, w, h) = render_readback(&pcss_scene_json(FAR_HEIGHT, 1, 0.0));
    let soft_width = gradient_width(&shade_column(&luma_image(&soft_bytes), w, h, SCANLINE_COL), 0.05, 0.95);
    eprintln!("Soft-tier gradient width at height {FAR_HEIGHT}: {soft_width}px");
    assert!(
        soft_width >= 1,
        "Soft tier shows no measurable penumbra (width {soft_width}px) on the committed scanline"
    );
}
