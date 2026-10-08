use super::testkit::*;
use super::*;
use super::assembly::*;
use super::merge::*;
use super::scene::*;
use manifold_node_engine::persistence::PrimitiveRegistry;
use manifold_node_engine::scene::boundary_nodes::{FINAL_OUTPUT_TYPE_ID, GENERATOR_INPUT_TYPE_ID};
use crate::node_graph::gltf_load::GltfImportSummary;
use crate::node_graph::primitives::render_scene::OBJECT_SAFETY_MAX;
use manifold_core::effect_graph_def::BindingTarget;
use super::synthetic_glbs::*;
use manifold_core::effect_graph_def::{EffectGraphNode, GROUP_OUTPUT_TYPE_ID, GROUP_TYPE_ID, SerializedParamValue};



/// D4's other half: a glb whose material count exceeds `OBJECT_SAFETY_MAX`
/// (1024) must error loudly at import time — never silently truncate.
/// `object_cap_exceeded_glb_errors_loudly_never_truncates` deliberately
/// builds only ONE material past the limit (1025) rather than a much
/// larger number — the synthetic-glb builder is O(n) JSON, and this test
/// only needs to cross the boundary, not stress it.
#[test]
fn object_cap_exceeded_glb_errors_loudly_never_truncates() {
    let n = OBJECT_SAFETY_MAX as usize + 1;
    let path = write_synthetic_multimaterial_glb(n);
    let result = assemble_import_graph(&path);
    std::fs::remove_file(&path).ok();

    let err = result.expect_err("a glb past OBJECT_SAFETY_MAX must error, not truncate");
    assert!(
        err.contains(&n.to_string()) && err.contains(&OBJECT_SAFETY_MAX.to_string()),
        "error must name both the actual count and the safety bound, got: {err}"
    );
}

/// The synthesized orbit camera's `far` must scale with the framed scene
/// (kuma_heavy_robot class: posed radius ~2700 clipped against the fixed
/// 200 default into a black frame), while a compact asset keeps the
/// primitive's default EXACTLY — the same golden-stability guarantee the
/// `near` scaling (BUG-165/BUG-169) makes.
#[test]
fn import_camera_far_scales_with_scene_size() {
    let cam_far = |half: f32| {
        let path = write_synthetic_sized_glb(half);
        let (def, _report) = assemble_import_graph(&path).expect("assemble sized synthetic glb");
        std::fs::remove_file(&path).ok();
        let cam = def
            .nodes
            .iter()
            .find(|n| n.type_id == "node.orbit_camera")
            .expect("assembled graph has an orbit_camera node");
        match cam.params.get("far") {
            Some(SerializedParamValue::Float { value }) => *value,
            other => panic!("orbit_camera far param missing or wrong type: {other:?}"),
        }
    };

    let small = cam_far(0.5);
    assert_eq!(
        small,
        crate::node_graph::primitives::DEFAULT_FAR,
        "compact asset keeps the default far exactly (golden stability)"
    );

    // half=1500: bbox radius ~2121, distance ~4667 — the old fixed 200
    // sat in front of the whole scene (black frame); the stamp must clear
    // distance + radius (~6788) with slack.
    let large = cam_far(1500.0);
    assert!(
        large > 6800.0,
        "large asset's far must clear distance + radius (~6788), got {large}"
    );
    assert!(
        large <= 10_000.0,
        "far stays within node.orbit_camera's declared range max, got {large}"
    );
}

/// Imported texture-source nodes stamp the source image's REAL pixel
/// dimensions, not the hardcoded 1024² v1 default — the kuma robot's
/// 4096² JPEG atlas used to land at quarter resolution (the base-color
/// block's own TODO named the hole; every map family shared it).
#[test]
fn import_texture_sources_keep_authored_resolution() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/gltf/kuma_heavy_robot_r-9000s.glb");
    if !path.exists() {
        println!("import_texture_sources_keep_authored_resolution: fixture not found, skipping");
        return;
    }
    let (def, _report) = assemble_import_graph(&path).expect("assemble robot glb");
    let tex_nodes: Vec<_> = def
        .nodes
        .iter()
        .filter_map(|n| n.group.as_ref())
        .flat_map(|g| g.nodes.iter())
        .filter(|n| n.type_id == "node.gltf_texture_source")
        .collect();
    assert!(!tex_nodes.is_empty(), "robot wires texture sources");
    for node in tex_nodes {
        for dim in ["width", "height"] {
            match node.params.get(dim) {
                Some(SerializedParamValue::Int { value }) => {
                    assert_eq!(*value, 4096, "node {} {dim} must be the authored 4096", node.node_id)
                }
                other => panic!("texture source {} missing {dim}: {other:?}", node.node_id),
            }
        }
    }
}

/// BUG-mbol: a Draco-compressed primitive must never silently vanish —
/// `Mat1` (Draco-tagged, undecoded) is dropped from the imported objects
/// (MANIFOLD has no Draco decoder), but the drop must be named in
/// `ImportReport::report_lines`, and `Mat0` (real geometry) must still
/// import normally.
#[test]
fn draco_primitive_reports_instead_of_vanishing_silently() {
    let path = write_synthetic_draco_glb();
    let (_def, report) = assemble_import_graph(&path).expect("assemble draco synthetic glb");
    std::fs::remove_file(&path).ok();

    assert_eq!(report.material_count, 1, "only Mat0 has readable geometry — Mat1 (Draco) is dropped");
    assert!(
        report
            .report_lines
            .iter()
            .any(|l| l.contains("KHR_draco_mesh_compression") && l.contains("skipped")),
        "report must name the Draco skip, got: {:?}",
        report.report_lines
    );
    assert!(
        report.report_lines.iter().any(|l| l.contains("DracoMesh") || l.contains("DracoNode")),
        "report line must identify the mesh/node that was skipped, got: {:?}",
        report.report_lines
    );
}

/// BUG-ssgz: a KTX2/BasisU-textured material must import — mesh and
/// material survive with a dummy texture substituted — instead of the
/// undecodable image aborting the entire document.
#[test]
fn ktx2_texture_reports_instead_of_aborting_whole_import() {
    let path = write_synthetic_ktx2_glb();
    let result = assemble_import_graph(&path);
    let (_def, report) =
        result.expect("a KTX2-textured glb must still import — no BasisU transcoder is a per-texture degrade, not a whole-file failure");

    assert_eq!(report.material_count, 1, "Mat0's mesh must import despite the undecodable KTX2 texture");
    assert!(
        report
            .report_lines
            .iter()
            .any(|l| l.contains("KTX2") || l.contains("BasisU")),
        "report must name the KTX2/BasisU decode failure, got: {:?}",
        report.report_lines
    );
}

/// BUG-pm9m: TEXCOORD_1 is preserved in MeshVertex's second UV lane and
/// texture-map metadata retains its authored texCoord selection.
#[test]
fn texcoord_1_reports_instead_of_silently_sampling_uv0() {
    let path = write_synthetic_multi_uv_glb();
    let result = assemble_import_graph(&path);
    let (_def, report) =
        result.expect("a TEXCOORD_1-carrying glb must still import — this is a report-only degrade");

    assert_eq!(report.material_count, 1, "Mat0's triangle must import despite the TEXCOORD_1 references");
    assert!(!report.report_lines.iter().any(|l| l.contains("additional UV sets are ignored")));

    let verts = super::gltf_load::load_gltf_mesh(
        &path,
        super::gltf_load::GltfMeshSelector::WholeScene,
    )
    .expect("load the synthetic multi-UV mesh");
    assert!(verts.iter().any(|v| (v._pad2[1] - 0.5).abs() < 1e-6));
    assert!(verts.iter().any(|v| (v._pad2[1] - 1.5).abs() < 1e-6));
    std::fs::remove_file(&path).ok();
}

/// BUG-7w79: a meshopt-compressed primitive must never be read as raw
/// bytes (garbage geometry, no error) — `Mat1` (meshopt-tagged) must be
/// dropped from the imported objects and named in
/// `ImportReport::report_lines`, while `Mat0` (real geometry) still
/// imports normally.
#[test]
fn meshopt_primitive_reports_instead_of_reading_garbage() {
    let path = write_synthetic_meshopt_glb();
    let (_def, report) = assemble_import_graph(&path).expect("assemble meshopt synthetic glb");
    std::fs::remove_file(&path).ok();

    assert_eq!(
        report.material_count, 1,
        "only Mat0 has readable geometry — Mat1 (meshopt-compressed) is dropped"
    );
    assert!(
        report
            .report_lines
            .iter()
            .any(|l| l.contains("EXT_meshopt_compression") && l.contains("skipped")),
        "report must name the meshopt skip, got: {:?}",
        report.report_lines
    );
    assert!(
        report.report_lines.iter().any(|l| l.contains("MeshoptMesh") || l.contains("MeshoptNode")),
        "report line must identify the mesh/node that was skipped, got: {:?}",
        report.report_lines
    );
}

/// BUG-jfe2: a `KHR_mesh_quantization`-style normalized-SHORT POSITION
/// accessor must never be read as raw F32 bytes (garbage geometry, no
/// error) — `Mat1` (quantized) must be dropped from the imported objects
/// and named in `ImportReport::report_lines`, while `Mat0` (real F32
/// geometry) still imports normally.
#[test]
fn quantized_position_primitive_reports_instead_of_reading_garbage() {
    let path = write_synthetic_quantized_position_glb();
    let (_def, report) = assemble_import_graph(&path).expect("assemble quantized-position synthetic glb");
    std::fs::remove_file(&path).ok();

    assert_eq!(
        report.material_count, 1,
        "only Mat0 has readable geometry — Mat1 (quantized POSITION) is dropped"
    );
    assert!(
        report
            .report_lines
            .iter()
            .any(|l| l.contains("quantized POSITION") && l.contains("skipped")),
        "report must name the quantized POSITION skip, got: {:?}",
        report.report_lines
    );
    assert!(
        report
            .report_lines
            .iter()
            .any(|l| l.contains("QuantizedPositionMesh") || l.contains("QuantizedPositionNode")),
        "report line must identify the mesh/node that was skipped, got: {:?}",
        report.report_lines
    );
}

/// BUG-jfe2: a valid F32 POSITION accessor does not save a primitive whose
/// NORMAL is `KHR_mesh_quantization`-style normalized BYTE — partial
/// garbage attributes are not acceptable, so the whole primitive is
/// dropped and named in `ImportReport::report_lines`.
#[test]
fn quantized_normal_primitive_reports_instead_of_reading_garbage() {
    let path = write_synthetic_quantized_normal_glb();
    let (_def, report) = assemble_import_graph(&path).expect("assemble quantized-normal synthetic glb");
    std::fs::remove_file(&path).ok();

    assert_eq!(
        report.material_count, 1,
        "only Mat0 has readable geometry — Mat1 (quantized NORMAL, valid POSITION) is dropped whole"
    );
    assert!(
        report
            .report_lines
            .iter()
            .any(|l| l.contains("quantized NORMAL") && l.contains("skipped")),
        "report must name the quantized NORMAL skip, got: {:?}",
        report.report_lines
    );
    assert!(
        report
            .report_lines
            .iter()
            .any(|l| l.contains("QuantizedNormalMesh") || l.contains("QuantizedNormalNode")),
        "report line must identify the mesh/node that was skipped, got: {:?}",
        report.report_lines
    );
}



/// CPU-only, fast — not gated behind `#[ignore]`. Guards the
/// file-missing case (no fixture in a checkout without
/// `tests/fixtures/gltf/`) so CI without the large fixture still
/// passes; when the fixture IS present, asserts the assembled graph's
/// known azalea shape.
#[test]
fn assembles_azalea_into_two_object_render_scene_graph() {
    let path = azalea_fixture_path();
    if !path.exists() {
        println!(
            "assembles_azalea_into_two_object_render_scene_graph: fixture not found at {}, skipping",
            path.display()
        );
        return;
    }

    let (def, report) = assemble_import_graph(&path).expect("assemble azalea");
    println!("azalea import report: {report:?}");

    assert_eq!(report.material_count, 2, "azalea has 2 materials with geometry");
    assert_eq!(report.object_count, 2);
    // BUG-pt6g (supersedes BUG-w5wv): both azalea materials actually
    // declare `KHR_materials_unlit` with a `baseColorTexture` — real-world
    // unlit-textured assets, not a synthetic case — but the importer no
    // longer routes unlit-flagged materials to `node.unlit_material`; both
    // now build `node.pbr_material` (default `baked_look = false`, lit).
    // `textures_wired` stays 2 either way: azalea's materials only declare
    // `baseColorTexture`, no normal/mr/occlusion/emissive maps to wire.
    assert_eq!(report.textures_wired, 2, "both azalea textures still wire to base_color_map");
    assert_eq!(report.default_material_vertex_count, 0);
    assert!(report.camera_synthesized);

    assert!(
        def.nodes.iter().any(|n| n.type_id == GENERATOR_INPUT_TYPE_ID),
        "assembled graph must carry a system.generator_input node"
    );
    assert!(
        def.nodes.iter().any(|n| n.type_id == FINAL_OUTPUT_TYPE_ID),
        "assembled graph must carry a system.final_output node"
    );

    let meta = def.preset_metadata.as_ref().expect("v2 metadata");
    // GLB_CONFORMANCE_DESIGN.md D6: a second string param, `hdri_file`,
    // holds the HDRI environment's own Browse field — a separate file
    // from `model_file` (the imported .glb itself).
    assert_eq!(meta.string_params.len(), 2, "model_file + hdri_file string params");
    assert_eq!(meta.string_params[0].id, "model_file");
    assert!(meta.string_params[0].is_file_picker);
    assert_eq!(meta.string_params[1].id, "hdri_file");
    assert!(meta.string_params[1].is_file_picker);

    assert_eq!(
        meta.string_bindings.len(),
        5,
        "2 mesh + 2 texture path bindings (model_file) + 1 HDRI path binding (hdri_file)"
    );
    for b in &meta.string_bindings {
        assert!(b.id == "model_file" || b.id == "hdri_file", "unexpected string binding id {}", b.id);
        match &b.target {
            BindingTarget::Node { param, .. } => assert_eq!(param, "path"),
            other => panic!("expected a Node binding target, got {other:?}"),
        }
    }
    assert_eq!(
        meta.string_bindings.iter().filter(|b| b.id == "hdri_file").count(),
        1,
        "exactly one hdri_file binding, targeting the hdri node"
    );

    // Curated performance surface. Azalea has 2 objects → 4 camera + 5 sun
    // + 1 Environment + 1 Environment Mode (D6) + 1 Fill Light + 1 Strip
    // Lights (F-P7) + 1 Ambient = 14 framing/material sliders. No
    // Atmosphere section (fog + god rays removed with the atmosphere
    // node, Peter 2026-07-15), no Motion Blur (BUG-136), no per-object
    // Metallic/Roughness and no SSAO/DoF card sliders (Peter,
    // 2026-07-15: DoF removed for buggy visuals, AO/metallic/roughness
    // hidden — defaults still apply, just not on the card).
    // P1 scene-panel exposure convergence: every scene-vocabulary atom's
    // params are exposed from the primitive registry. Spot-check structure
    // instead of pinning brittle counts.
    let camera_id = def
        .nodes
        .iter()
        .find(|n| n.type_id == "node.orbit_camera")
        .map(|n| n.id)
        .expect("import synthesizes an orbit camera");
    let sun_id = def
        .nodes
        .iter()
        .find(|n| n.type_id == "node.light")
        .map(|n| n.id)
        .expect("import synthesizes a sun");
    let envmap_id = def
        .nodes
        .iter()
        .find(|n| n.type_id == "node.bake_environment")
        .map(|n| n.id)
        .expect("import synthesizes an envmap");

    assert!(
        meta.params.len() > 14,
        "P1 exposes many more params than the old 14 curated sliders"
    );
    // Every param routes one-to-one except: the shared Ambient, which
    // fans out to every material's ambient (2 for azalea); D7's sun
    // coherence, where each of sun_x/sun_y/sun_z fans out to TWO targets
    // (the sun light AND the envmap's disc direction) — 3 extra
    // bindings; and G-P6's Environment master, which fans out to the
    // softbox bake's intensity AND the HDRI branch's exposure gain — 1
    // extra. 14 + 1 (ambient) + 3 (sun coherence) + 1 (env fan-out) = 19.
    assert!(
        meta.bindings.len() > meta.params.len(),
        "fan-outs (ambient, sun coherence, env intensity) give more bindings than params"
    );
    // Every card param routes to at least one node param.
    for p in &meta.params {
        assert!(
            meta.bindings.iter().any(|b| b.id == p.id),
            "card param `{}` has no binding",
            p.id
        );
    }
    // Every binding must reference a param that actually exists (address
    // by id, never by position — the fan-out rule).
    for b in &meta.bindings {
        assert!(
            meta.params.iter().any(|p| p.id == b.id),
            "binding `{}` has no matching param",
            b.id
        );
        assert!(
            matches!(b.target, BindingTarget::Node { .. }),
            "import card bindings route to inner nodes"
        );
    }
    // Shared framing/light/environment atoms have exposed params in their
    // named sections.
    assert!(
        meta.params.iter().any(|p| p.section.as_deref() == Some("Camera") && p.id.starts_with(&format!("{camera_id}_"))),
        "camera params exposed from ParamDef"
    );
    assert!(
        meta.params.iter().any(|p| p.section.as_deref() == Some("Sun") && p.id.starts_with(&format!("{sun_id}_"))),
        "sun params exposed from ParamDef"
    );
    let env_intensity_id = format!("{envmap_id}_intensity");
    assert!(
        meta.params.iter().any(|p| p.id == env_intensity_id),
        "envmap intensity exposed from ParamDef"
    );
    let env_intensity = meta.params.iter().find(|p| p.id == env_intensity_id).unwrap();
    assert_eq!(env_intensity.section.as_deref(), Some("Environment"));
    // D7 (F-P4): the black-void softbox studio is now the import
    // default, so Environment starts at 1.0 (range unchanged); the
    // shared Ambient fill still starts at 0 — softbox lighting comes
    // from the envmap + sun, not a flat fill floor.
    assert_eq!(env_intensity.default_value, 1.0, "environment bakes at softbox intensity 1.0 by default (D7)");
    // BUG-pt6g (supersedes BUG-w5wv): both azalea materials declare
    // `KHR_materials_unlit`, but both now route to `node.pbr_material`
    // (default lit) — every material contributes a `scene_ambient`
    // binding, so the shared "Ambient" card IS pushed (see
    // `unlit_material_imports_lit_by_default` for the routing gate itself
    // and `scene.rs`'s conditional push of this card param, which is now
    // satisfied).
    assert!(
        meta.params.iter().any(|p| p.id == "scene_ambient"),
        "azalea's materials are lit by default now — the shared Ambient card must be pushed"
    );
    assert!(
        meta.bindings.iter().any(|b| b.id == "scene_ambient"),
        "every lit-by-default material contributes a scene_ambient binding"
    );
    // The envmap intensity slider is the Environment master; it fans out
    // to envmap.intensity AND hdri_gain.gain (G-P6).
    let env_bindings: Vec<_> = meta
        .bindings
        .iter()
        .filter(|b| b.id == env_intensity_id)
        .collect();
    assert_eq!(env_bindings.len(), 2, "env intensity fans out to envmap + hdri gain");
    assert!(env_bindings.iter().any(|b| match &b.target {
        BindingTarget::Node { node_id, param } => node_id.as_str() == "envmap" && param == "intensity",
        _ => false,
    }));

    // Camera angle params are flagged as angles and wrap 360.
    let cam_orbit_id = format!("{camera_id}_orbit");
    let cam_orbit = meta.params.iter().find(|p| p.id == cam_orbit_id).unwrap();
    assert!(cam_orbit.is_angle, "camera orbit slider is an angle param");
    assert!(
        (cam_orbit.default_value - 0.7).abs() < 1e-6,
        "camera orbit default is stored in radians"
    );
    let orbit = meta.bindings.iter().find(|b| b.id == cam_orbit_id).unwrap();
    assert!(
        (orbit.scale - 1.0).abs() < 1e-6,
        "camera angle bindings pass radians straight through"
    );
    // Orbit and tilt both wrap a full 360 instead of clamping at their
    // edges (Peter, 2026-07-15).
    assert!(cam_orbit.wraps, "camera orbit must wrap 360");
    let cam_tilt_id = format!("{camera_id}_tilt");
    let cam_tilt = meta.params.iter().find(|p| p.id == cam_tilt_id).unwrap();
    assert!(cam_tilt.wraps, "camera tilt must wrap 360");
    assert!(
        (cam_tilt.min - (-std::f32::consts::TAU)).abs() < 1e-4
            && (cam_tilt.max - std::f32::consts::TAU).abs() < 1e-4,
        "camera tilt spans the ParamDef +/-360 range"
    );

    // GTAO, the lens, the polished DoF chain (coc → bokeh_gather)
    // and the motion-blur tail are all wired into the spine
    // (CINEMATIC_SCENE_TAIL D1/section 3 — reinstated after BUG-136 was
    // root-caused as missing chains, never a kernel defect). `node.variable_blur`
    // and `node.atmosphere` stay absent (P4's superseded DoF blur stage;
    // fog + god rays removed, Peter 2026-07-15).
    for present in [
        "node.ssao_gtao",
        "node.bilateral_blur",
        "node.camera_lens",
        "node.coc_from_depth",
        "node.bokeh_gather",
        "node.motion_blur",
    ] {
        assert!(
            def.nodes.iter().any(|n| n.type_id == present)
                || def.nodes.iter().filter_map(|n| n.group.as_ref()).any(|g| {
                    g.nodes.iter().any(|inner| inner.type_id == present)
                }),
            "imported graph must carry `{present}`"
        );
    }
    for absent in ["node.variable_blur", "node.atmosphere"] {
        assert!(
            !def.nodes.iter().any(|n| n.type_id == absent)
                && !def.nodes.iter().filter_map(|n| n.group.as_ref()).any(|g| {
                    g.nodes.iter().any(|inner| inner.type_id == absent)
                }),
            "`{absent}` should not be in the imported graph"
        );
    }
    // No DoF card sliders — the underlying nodes keep their defaults,
    // they're just not auto-exposed on the card.
    assert!(
        !meta.params.iter().any(|p| p.id.starts_with("dof_")),
        "no card param should start with `dof_`"
    );
    // SSAO intensity is re-exposed as a live escape hatch for the
    // flat-plane GTAO artifact (BUG-y5w7, Peter 2026-07-28) — radius and
    // the rest of the ao-group params stay unexposed per 2026-07-15.
    assert!(
        meta.params.iter().any(|p| p.id == "ssao_intensity"),
        "ssao_intensity card param must exist (BUG-y5w7 escape hatch)"
    );
    assert!(
        !meta.params.iter().any(|p| p.id.starts_with("ssao_") && p.id != "ssao_intensity"),
        "no card param other than ssao_intensity should start with `ssao_`"
    );
    for gone in [
        "dof_radius", "motion_blur_px", "mb_shutter", "ssao_bias", "fog_density", "god_rays",
    ] {
        assert!(
            !meta.params.iter().any(|p| p.id == gone),
            "unused card param id `{gone}` should not exist"
        );
    }
    // BUG-303: the importer seeds the sun node's `shadow_softness` param to
    // Hard (0) for the crisp dramatic look — the card slider's default must
    // follow that stamped value, not the primitive ParamDef's generic Soft
    // (1) default, or the exposure would clobber the import's own look at
    // bind time (`apply_binding_defaults`).
    let sun_shadow_id = format!("{sun_id}_shadow_softness");
    let sun_shadow = meta.params.iter().find(|p| p.id == sun_shadow_id).unwrap();
    assert_eq!(sun_shadow.default_value, 0.0, "shadow type default follows the node's stamped Hard value");
}

/// The tiger lily is the smallest real multi-material scan in the fixture set.
/// Keep this CPU gate focused on the compound importer contract: one authored
/// group owns both material outputs while source vertex totals survive.
#[test]
fn tiger_lily_compound_import_preserves_material_sources_and_totals() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/gltf/cc0__tiger_lily.glb");
    assert!(path.exists(), "required tiger_lily fixture is missing: {}", path.display());
    let summary = gltf_load::gltf_import_summary(&path).expect("tiger lily summary parses");
    let (def, report) = assemble_import_graph(&path).expect("tiger lily imports");
    assert_eq!(report.material_count, summary.materials.len());
    assert_eq!(report.object_count, summary.materials.len());
    let groups: Vec<_> = def.nodes.iter().filter(|node| node.type_id == GROUP_TYPE_ID).collect();
    let object_groups: Vec<_> = groups
        .iter()
        .filter(|group| group.node_id.as_str().starts_with("object_"))
        .collect();
    assert_eq!(object_groups.len(), 1, "static scan has one authored object group");
    let group = object_groups[0].group.as_ref().expect("compound group body");
    let output_ids: std::collections::HashSet<u32> = group.nodes.iter()
        .filter(|node| node.type_id == GROUP_OUTPUT_TYPE_ID)
        .map(|node| node.id)
        .collect();
    assert_eq!(output_ids.len(), summary.materials.len());
    let output_count = group.wires.iter()
        .filter(|wire| output_ids.contains(&wire.to_node) && wire.to_port.starts_with("object"))
        .count();
    assert_eq!(output_count, summary.materials.len());
    let sources: Vec<_> = group.nodes.iter().filter(|node| node.type_id == "node.gltf_mesh_source").collect();
    let materials: Vec<_> = group.nodes.iter().filter(|node| node.type_id == "node.pbr_material").collect();
    assert_eq!(sources.len(), summary.materials.len());
    assert_eq!(materials.len(), summary.materials.len());
    let imported_total: u64 = sources.iter().map(|node| match node.params.get("source_vertex_count") {
        Some(SerializedParamValue::Int { value }) => *value as u64,
        other => panic!("mesh source {} lost source_vertex_count: {other:?}", node.node_id),
    }).sum();
    let expected_total: u64 = summary.materials.iter().map(|material| u64::from(material.vertex_count)).sum();
    assert_eq!(imported_total, expected_total, "compound scan keeps every material's vertices");
    let vm = crate::node_graph::scene_vm::SceneVm::from_def(&def).unwrap();
    assert_eq!(vm.header.object_count, 1);
    assert_eq!(vm.objects.len(), summary.materials.len() + 1);
    let crate::node_graph::scene_vm::SceneObjectVm::Known(parent) = &vm.objects[0] else { panic!("parent") };
    assert!(parent.is_group);
    assert_eq!(parent.visible_addr.param_id, "parent_visible");
    let parent_transform = parent.transform.as_ref().unwrap().node_doc_id;
    for row in &vm.objects[1..] {
        let crate::node_graph::scene_vm::SceneObjectVm::Known(child) = row else { panic!("child") };
        assert_eq!(child.parent_group_id, Some(parent.object_node_id));
        assert_ne!(child.transform.as_ref().unwrap().node_doc_id, parent_transform);
        assert_eq!(child.visible_addr.param_id, "visible");
        assert!(child.physics.is_none());
        let crate::node_graph::scene_vm::MaterialVm::Known(material) = &child.material else { panic!("child material") };
        assert_eq!(material.shared_object_count, Some(1));
    }
}





/// BUG-194/BUG-195: `build_import_graph` stamps `source_vertex_count`
/// (exactly `GltfMaterialInfo::vertex_count`) and `source_bbox_radius`
/// (the whole-import bbox radius, the same value the synthesized
/// orbit camera's `distance` is derived from) onto every mesh-source
/// node it creates — read back by `SceneVm`'s header and by a future
/// merge's scale-sanity rule.
#[test]
fn build_import_graph_seeds_source_vertex_count_and_bbox_radius() {
    let half_extent = 3.0_f32;
    let summary = GltfImportSummary {
        materials: vec![full_material(0, "Leaf", 250), full_material(1, "Bark", 900)],
        bbox_min: [-half_extent, -half_extent, -half_extent],
        bbox_max: [half_extent, half_extent, half_extent],
        camera_count: 0,
        default_material_vertex_count: 0,
        animations: Vec::new(),
        animation_report_lines: Vec::new(),
        extension_report_lines: Vec::new(),
        lights: Vec::new(),
        cameras: Vec::new(),
        camera_report_lines: Vec::new(),
        texture_dims: Vec::new(),
    };
    let path = std::path::Path::new("/tmp/synthetic_seed_test.glb");
    let (def, _report) = build_import_graph(&summary, path).expect("build import graph");

    // Same radius formula `build_import_graph` uses for the synthesized
    // orbit camera (dims are the full cube, diag/2).
    let dims = 2.0 * half_extent;
    let expected_radius = ((3.0 * dims * dims).sqrt() * 0.5).max(1e-3);

    let mesh_sources: Vec<&EffectGraphNode> = def
        .nodes
        .iter()
        .filter_map(|n| n.group.as_ref())
        .flat_map(|g| g.nodes.iter())
        .filter(|n| n.type_id == "node.gltf_mesh_source")
        .collect();
    assert_eq!(mesh_sources.len(), 2, "one node.gltf_mesh_source per material");

    // Largest-by-vertex-count-first ordering (build_import_graph sorts
    // materials that way) — Bark (900) is object 0, Leaf (250) is
    // object 1.
    let mut seen_counts: Vec<i32> = Vec::new();
    for mesh in &mesh_sources {
        assert_eq!(
            mesh.params.get("vertex_colors"),
            Some(&bool_val(true)),
            "every new static mesh source must explicitly enable authored vertex colors"
        );
        let vcount = match mesh.params.get("source_vertex_count") {
            Some(SerializedParamValue::Int { value }) => *value,
            other => panic!("expected an Int source_vertex_count, got {other:?}"),
        };
        seen_counts.push(vcount);
        let radius = match mesh.params.get("source_bbox_radius") {
            Some(SerializedParamValue::Float { value }) => *value,
            other => panic!("expected a Float source_bbox_radius, got {other:?}"),
        };
        assert!(
            (radius - expected_radius).abs() < 1e-4,
            "expected {expected_radius}, got {radius}"
        );
    }
    seen_counts.sort_unstable();
    assert_eq!(seen_counts, vec![250, 900]);
}

/// AM4 (RAYTRACING_DESIGN.md section 12, Screen-space AO handoff): a
/// fresh import's "Ambient Occlusion" group carries `node.masked_mix`
/// and the 4th `ao_mask` interface input, and the outer graph carries
/// `render_scene.ao_mask -> ao_group.ao_mask`.
#[test]
fn build_import_graph_ao_group_consumes_ao_mask() {
    use manifold_core::effect_graph_def::GROUP_TYPE_ID;

    let summary = GltfImportSummary {
        materials: vec![full_material(0, "Solid", 500)],
        bbox_min: [-1.0, -1.0, -1.0],
        bbox_max: [1.0, 1.0, 1.0],
        camera_count: 0,
        default_material_vertex_count: 0,
        animations: Vec::new(),
        animation_report_lines: Vec::new(),
        extension_report_lines: Vec::new(),
        lights: Vec::new(),
        cameras: Vec::new(),
        camera_report_lines: Vec::new(),
        texture_dims: Vec::new(),
    };
    let path = std::path::Path::new("/tmp/synthetic_ao_mask_test.glb");
    let (def, _report) = build_import_graph(&summary, path).expect("build import graph");

    let render_id = def
        .nodes
        .iter()
        .find(|n| n.type_id == "node.render_scene")
        .map(|n| n.id)
        .expect("import synthesizes a render_scene node");

    let ao_group_node = def
        .nodes
        .iter()
        .find(|n| n.type_id == GROUP_TYPE_ID && n.title.as_deref() == Some("Ambient Occlusion"))
        .expect("import synthesizes the Ambient Occlusion group");
    let ao_group = ao_group_node.group.as_ref().expect("group node carries a GroupDef");

    assert_eq!(ao_group.interface.inputs.len(), 4, "depth, camera, color, ao_mask");
    let ao_mask_input = ao_group
        .interface
        .inputs
        .iter()
        .find(|p| p.name == "ao_mask")
        .expect("group interface must declare ao_mask");
    assert_eq!(ao_mask_input.port_type, "Texture2D");

    assert!(
        ao_group.nodes.iter().any(|n| n.type_id == "node.masked_mix"),
        "AO group must contain node.masked_mix (AM4)"
    );

    assert!(
        def.wires.iter().any(|w| w.from_node == render_id
            && w.from_port == "ao_mask"
            && w.to_node == ao_group_node.id
            && w.to_port == "ao_mask"),
        "outer graph must wire render_scene.ao_mask -> ao_group.ao_mask"
    );
}

/// BUG-221 inside a static compound: the shared transform stays at the
/// origin (it pivots the whole asset about its centre), while each part keeps
/// its own pivot — `mesh_k` is shifted by `-own_center` and
/// `part_transform_k` sits at `own_center - center`, so the net placement is
/// still the whole-scene recenter.
#[test]
fn bug221_compound_transform_uses_shared_asset_center_pivot() {
    let mut big = full_material(0, "Big", 999); // k=0 after the largest-vertex-count-first sort
    big.own_center = [5.0, 1.0, -0.5];
    let mut small = full_material(1, "Small", 1); // k=1
    small.own_center = [-3.0, 0.0, 0.0];

    let summary = GltfImportSummary {
        materials: vec![big, small],
        bbox_min: [-4.0, -1.0, -2.0],
        bbox_max: [8.0, 3.0, 1.0],
        camera_count: 0,
        default_material_vertex_count: 0,
        animations: Vec::new(),
        animation_report_lines: Vec::new(),
        extension_report_lines: Vec::new(),
        lights: Vec::new(),
        cameras: Vec::new(),
        camera_report_lines: Vec::new(),
        texture_dims: Vec::new(),
    };
    let center = [
        (summary.bbox_min[0] + summary.bbox_max[0]) * 0.5,
        (summary.bbox_min[1] + summary.bbox_max[1]) * 0.5,
        (summary.bbox_min[2] + summary.bbox_max[2]) * 0.5,
    ];
    assert_eq!(center, [2.0, 1.0, -0.5], "sanity: scene-wide bbox center");

    let path = std::path::Path::new("/tmp/synthetic_bug221_test.glb");
    let (def, _report) = build_import_graph(&summary, path).expect("build import graph");

    let group = def.nodes.iter().find(|n| n.type_id == GROUP_TYPE_ID).expect("object 0's group");
    let body = group.group.as_ref().expect("group has a body");
    let mesh0 = body
        .nodes
        .iter()
        .find(|n| n.handle.as_deref() == Some("mesh_0"))
        .expect("mesh_0 node inside object 0's group");
    let transform0 = body
        .nodes
        .iter()
        .find(|n| n.handle.as_deref() == Some("transform_0"))
        .expect("transform_0 node inside object 0's group");

    fn float_param(node: &EffectGraphNode, name: &str) -> f32 {
        match node.params.get(name) {
            Some(SerializedParamValue::Float { value }) => *value,
            other => panic!("expected a Float {name} param, got {other:?}"),
        }
    }

    let translate = [
        float_param(mesh0, "translate_x"),
        float_param(mesh0, "translate_y"),
        float_param(mesh0, "translate_z"),
    ];
    let pos = [
        float_param(transform0, "pos_x"),
        float_param(transform0, "pos_y"),
        float_param(transform0, "pos_z"),
    ];
    let part0 = body
        .nodes
        .iter()
        .find(|n| n.handle.as_deref() == Some("part_transform_0"))
        .expect("part_transform_0 node inside the compound group");
    let part_pos = [
        float_param(part0, "pos_x"),
        float_param(part0, "pos_y"),
        float_param(part0, "pos_z"),
    ];
    let own_center = [5.0_f32, 1.0, -0.5];
    for i in 0..3 {
        assert!(
            (translate[i] - (-own_center[i])).abs() < 1e-5,
            "mesh_0.translate_{i} should be -own_center[{i}]: got {translate:?}"
        );
        assert!(
            pos[i].abs() < 1e-5,
            "shared transform_0.pos_{i} should remain at the origin: got {pos:?}"
        );
        assert!(
            (part_pos[i] - (own_center[i] - center[i])).abs() < 1e-5,
            "part_transform_0.pos_{i} should be own_center - center: got {part_pos:?}"
        );
        // The composed net offset remains the whole-scene recenter.
        assert!(
            (translate[i] + part_pos[i] + pos[i] - (-center[i])).abs() < 1e-5,
            "mesh_0.translate_{i} + part_transform_0.pos_{i} + transform_0.pos_{i} must equal \
             -center[{i}] (net world placement unchanged): translate={translate:?} \
             part={part_pos:?} pos={pos:?} center={center:?}"
        );
    }
}

/// BUG-303: the card slider that auto-exposes the shared `transform_0.pos_x`
/// must default to the shared transform's origin. Per-part offsets live on
/// `part_transform_k`, so the shared transform's binding defaults must not
/// pick one up.
#[test]
fn bug303_object_transform_exposure_default_matches_stamped_recenter_not_origin() {
    let mut big = full_material(0, "Big", 999); // k=0 after the largest-vertex-count-first sort
    big.own_center = [5.0, 1.0, -0.5];
    let mut small = full_material(1, "Small", 1); // k=1
    small.own_center = [-3.0, 0.0, 0.0];

    let summary = GltfImportSummary {
        materials: vec![big, small],
        bbox_min: [-4.0, -1.0, -2.0],
        bbox_max: [8.0, 3.0, 1.0],
        camera_count: 0,
        default_material_vertex_count: 0,
        animations: Vec::new(),
        animation_report_lines: Vec::new(),
        extension_report_lines: Vec::new(),
        lights: Vec::new(),
        cameras: Vec::new(),
        camera_report_lines: Vec::new(),
        texture_dims: Vec::new(),
    };
    let expected_pos_x = 0.0_f32;
    let path = std::path::Path::new("/tmp/synthetic_bug303_test.glb");
    let (def, _report) = build_import_graph(&summary, path).expect("build import graph");

    let meta = def.preset_metadata.as_ref().expect("import stamps preset_metadata");
    let binding = meta
        .bindings
        .iter()
        .find(|b| matches!(
            &b.target,
            BindingTarget::Node { node_id, param } if node_id.as_str() == "transform_0" && param == "pos_x"
        ))
        .expect("a binding targets transform_0.pos_x");
    assert!(
        (binding.default_value - expected_pos_x).abs() < 1e-5,
        "transform_0.pos_x exposure default should equal the shared origin ({expected_pos_x}), got {}",
        binding.default_value,
    );

    let spec = meta
        .params
        .iter()
        .find(|p| p.id == binding.id)
        .expect("the binding's matching ParamSpecDef");
    assert!(
        (spec.default_value - expected_pos_x).abs() < 1e-5,
        "the outer-card ParamSpecDef default must match the binding default"
    );
}


/// Card-visibility curation, importer level: an imported object's
/// `transform_0.pos_x` exposure must show on the CARD (`card_visible: true`)
/// while its `scale_x` sibling and every one of its `node.pbr_material`
/// exposures are hidden (`card_visible: false`) — the scene panel still
/// gets all of them (P1 stamps every param unconditionally), this only
/// gates the generator card's row builder. Same single-material synthetic
/// summary shape as the sibling BUG-303 test above.
#[test]
fn imported_object_card_visible_shows_pos_hides_scale_and_material() {
    let summary = GltfImportSummary {
        materials: vec![full_material(0, "Mat", 100)],
        bbox_min: [-1.0, -1.0, -1.0],
        bbox_max: [1.0, 1.0, 1.0],
        camera_count: 0,
        default_material_vertex_count: 0,
        animations: Vec::new(),
        animation_report_lines: Vec::new(),
        extension_report_lines: Vec::new(),
        lights: Vec::new(),
        cameras: Vec::new(),
        camera_report_lines: Vec::new(),
        texture_dims: Vec::new(),
    };
    let path = std::path::Path::new("/tmp/synthetic_card_visible_test.glb");
    let (def, _report) = build_import_graph(&summary, path).expect("build import graph");
    let meta = def.preset_metadata.as_ref().expect("import stamps preset_metadata");

    let pos_x_binding = meta
        .bindings
        .iter()
        .find(|b| matches!(
            &b.target,
            BindingTarget::Node { node_id, param } if node_id.as_str() == "transform_0" && param == "pos_x"
        ))
        .expect("a binding targets transform_0.pos_x");
    let pos_x_spec = meta.params.iter().find(|p| p.id == pos_x_binding.id).unwrap();
    assert!(pos_x_spec.card_visible, "transform pos_x must show on the card");

    let scale_x_binding = meta
        .bindings
        .iter()
        .find(|b| matches!(
            &b.target,
            BindingTarget::Node { node_id, param } if node_id.as_str() == "transform_0" && param == "scale_x"
        ))
        .expect("a binding targets transform_0.scale_x");
    let scale_x_spec = meta.params.iter().find(|p| p.id == scale_x_binding.id).unwrap();
    assert!(!scale_x_spec.card_visible, "transform scale_x must be hidden from the card");

    // Excludes "ambient": the importer's shared "Ambient" fill knob is a
    // hand-curated card_binding (object_group.rs) targeting mat_0.ambient
    // directly — NOT a P1 auto-stamped exposure — so it's untouched by
    // `card_visible_for` and correctly stays visible (brief's own example
    // of a hand-curated exception).
    let material_binding_ids: std::collections::HashSet<&str> = meta
        .bindings
        .iter()
        .filter(|b| matches!(
            &b.target,
            BindingTarget::Node { node_id, param } if node_id.as_str() == "mat_0" && param != "ambient"
        ))
        .map(|b| b.id.as_str())
        .collect();
    assert!(!material_binding_ids.is_empty(), "the material node exposes at least one param");
    for spec in meta.params.iter().filter(|p| material_binding_ids.contains(p.id.as_str())) {
        assert!(!spec.card_visible, "material param '{}' must be hidden from the card", spec.name);
    }
}

/// Regression for a duplicate-handle panic found via the IMPORT_ANYTHING_WAVE
/// Lane W5 conformance sweep on `MetalRoughSpheresNoTextures.glb` (98
/// materials authored `"mat_0".."mat_97"`): SCENE_OBJECT_AND_PANEL_V2's P3
/// stamps both the object's group node AND its inner `node.scene_object`
/// with `unique_group_name`'s output (D6), which previously took the
/// glTF material's name verbatim — so a material named `"mat_0"` collided
/// with that SAME object's own `node.pbr_material` handle (`format!("mat_{k}")`),
/// both flattening to `"mat_0/mat_0"` and panicking in `graph.rs`'s
/// `add_node_named`. `collides_with_object_group_inner_handle` now vetoes
/// this — build must succeed and the group must NOT be literally named
/// "mat_0".
#[test]
fn material_named_like_its_own_inner_handle_does_not_collide() {
    let summary = GltfImportSummary {
        materials: vec![full_material(0, "mat_0", 100)],
        bbox_min: [-1.0, -1.0, -1.0],
        bbox_max: [1.0, 1.0, 1.0],
        camera_count: 0,
        default_material_vertex_count: 0,
        animations: Vec::new(),
        animation_report_lines: Vec::new(),
        extension_report_lines: Vec::new(),
        lights: Vec::new(),
        cameras: Vec::new(),
        camera_report_lines: Vec::new(),
        texture_dims: Vec::new(),
    };
    let path = std::path::Path::new("/tmp/synthetic_mat_0_collision.glb");
    let (def, _report) =
        build_import_graph(&summary, path).expect("build import graph for mat_0-named material");

    let group = def
        .nodes
        .iter()
        .find(|n| n.type_id == GROUP_TYPE_ID)
        .expect("one object group");
    assert_ne!(
        group.handle.as_deref(),
        Some("mat_0"),
        "the group handle must be deduped away from this object's own mat_{{k}} inner handle"
    );

    // `flatten_groups` doesn't itself assert handle uniqueness (only
    // `Graph::add_node_named`, at load time, does — graph.rs:137) — so
    // reproduce that check directly on the flattened output, which is
    // exactly what the panic message ("duplicate handle 'mat_0/mat_0'")
    // was catching.
    let flattened = manifold_core::flatten::flatten_groups(&def).expect("flatten must succeed");
    let mut seen = std::collections::HashSet::new();
    for n in &flattened.nodes {
        if let Some(h) = &n.handle {
            assert!(seen.insert(h.clone()), "duplicate flattened handle: '{h}'");
        }
    }
}

/// BUG-55lv (compound duplicate handle): a static multi-material asset folds
/// every part into ONE group body, so each part's material-derived
/// `node.scene_object` handle shares a namespace with every sibling's
/// deterministic handles (`mat_{k}`, `output_{i}`, `part_transform_{k}`, …),
/// not just its own. Each name below hits one of those: `mat_0` against the
/// primary's own material node (the `MetalRoughSpheresNoTextures.glb` panic),
/// `mat_2` against a sibling's material, `output_1` against a sibling's group
/// output, and `part_transform_0` against the primary's part transform.
#[test]
fn compound_part_names_never_collide_with_sibling_inner_handles() {
    let summary = GltfImportSummary {
        materials: vec![
            full_material(0, "mat_0", 100),
            full_material(1, "mat_2", 100),
            full_material(2, "output_1", 100),
            full_material(3, "part_transform_0", 100),
        ],
        bbox_min: [-1.0, -1.0, -1.0],
        bbox_max: [1.0, 1.0, 1.0],
        camera_count: 0,
        default_material_vertex_count: 0,
        animations: Vec::new(),
        animation_report_lines: Vec::new(),
        extension_report_lines: Vec::new(),
        lights: Vec::new(),
        cameras: Vec::new(),
        camera_report_lines: Vec::new(),
        texture_dims: Vec::new(),
    };
    let path = std::path::Path::new("/tmp/synthetic_compound_collision.glb");
    let (def, _report) =
        build_import_graph(&summary, path).expect("build compound import graph");

    let flattened = manifold_core::flatten::flatten_groups(&def).expect("flatten must succeed");
    let mut seen = std::collections::HashSet::new();
    for n in &flattened.nodes {
        if let Some(h) = &n.handle {
            assert!(seen.insert(h.clone()), "duplicate flattened handle: '{h}'");
        }
    }
}




/// Id offsetting: new nodes must be allocated ABOVE the target def's
/// current max id (recursively, including inside existing object
/// groups) — never colliding with an existing node anywhere in the def.
#[test]
fn merge_allocates_ids_above_the_targets_current_max() {
    let def = scene_def_with_bbox_half_extent(1.0);
    let existing_max = max_node_id_recursive(&def.nodes);

    let summary = merge_summary(vec![full_material(0, "Incoming", 300)], 1.0);
    let path = std::path::Path::new("/tmp/synthetic_merge_incoming.glb");
    let plan = merge_import_into_graph(&def, &summary, path).expect("merge one object");

    assert!(!plan.new_nodes.is_empty(), "merge must produce at least the one incoming object group");
    for n in &plan.new_nodes {
        assert!(
            n.id > existing_max,
            "new top-level node id {} must be above the target's existing max id {existing_max}",
            n.id
        );
        if let Some(group) = &n.group {
            assert!(max_node_id_recursive(&group.nodes) > existing_max || group.nodes.is_empty());
        }
    }
}

/// Chrome skipped: a `MergePlan`'s `new_nodes` must NEVER contain a
/// camera / envmap / hdri / light / lens node — the target scene keeps
/// its own chrome untouched, D5's core rejection ("splice the whole
/// assembled def" duplicates chrome).
#[test]
fn merge_plan_never_contains_chrome_nodes() {
    let def = scene_def_with_bbox_half_extent(1.0);
    let summary = merge_summary(
        vec![full_material(0, "A", 100), full_material(1, "B", 200)],
        1.0,
    );
    let path = std::path::Path::new("/tmp/synthetic_merge_chrome_check.glb");
    let plan = merge_import_into_graph(&def, &summary, path).expect("merge two objects");

    const CHROME_TYPE_IDS: &[&str] = &[
        "node.orbit_camera",
        "node.free_camera",
        "node.look_at_camera",
        "node.camera_lens",
        "node.bake_environment",
        "node.hdri_source",
        "node.exposure",
        "node.switch_texture",
        "node.light",
        "node.render_scene",
        "node.ssao_gtao",
        "node.bilateral_blur",
        "node.mix",
    ];
    fn assert_no_chrome(nodes: &[EffectGraphNode]) {
        for n in nodes {
            assert!(
                !CHROME_TYPE_IDS.contains(&n.type_id.as_str()),
                "merge plan must never contain chrome node type_id `{}` — the target scene \
                 keeps its own chrome",
                n.type_id
            );
            if let Some(group) = &n.group {
                assert_no_chrome(&group.nodes);
            }
        }
    }
    assert_no_chrome(&plan.new_nodes);
    // Every new node is a top-level GROUP_TYPE_ID box (one per object) —
    // never a bare chrome-shaped node at the top level either.
    for n in &plan.new_nodes {
        assert_eq!(n.type_id, GROUP_TYPE_ID, "merge only ever adds object groups at the top level");
    }
}

/// Name-collision suffixing: an incoming material named the same as an
/// existing top-level handle gets suffixed by `unique_group_name` (the
/// importer's own dedup helper, reused verbatim — not reimplemented),
/// never a silent duplicate name. `used_group_names` is seeded with the
/// target's existing handles, so the very first colliding local object
/// (whose own local index restarts at 0 for a merge) gets "Existing 1"
/// — the same helper a single import would produce "Name 2" from ONLY
/// when "Name 1" was already taken too; the exact numeral isn't the
/// contract, uniqueness is.
#[test]
fn merge_suffixes_a_colliding_group_name() {
    let def = scene_def_with_bbox_half_extent(1.0);
    // The target scene's one object group is named "Existing" (full_material's name).
    assert!(def.nodes.iter().any(|n| n.handle.as_deref() == Some("Existing")));

    let summary = merge_summary(vec![full_material(0, "Existing", 300)], 1.0);
    let path = std::path::Path::new("/tmp/synthetic_merge_name_collision.glb");
    let plan = merge_import_into_graph(&def, &summary, path).expect("merge colliding name");

    let new_group = plan.new_nodes.first().expect("one merged object group");
    assert_ne!(
        new_group.handle.as_deref(),
        Some("Existing"),
        "a colliding incoming group name must never collide with an existing top-level handle"
    );
    assert_eq!(
        new_group.handle.as_deref(),
        Some("Existing 1"),
        "unique_group_name's own dedup convention, reused verbatim"
    );
}

/// Objects count bumps correctly: merging N materials into a scene that
/// already has M objects produces `new_objects_count == M + N`, and the
/// new wires target ports `object_M..object_{M+N-1}` (continuing, never
/// restarting at 0).
#[test]
fn merge_bumps_objects_count_and_continues_port_indices() {
    let def = scene_def_with_bbox_half_extent(1.0);
    let (render_id, existing_objects) = render_scene_objects(&def);
    assert_eq!(existing_objects, 1, "scene_def_with_bbox_half_extent seeds exactly one object");

    let summary = merge_summary(
        vec![full_material(0, "A", 100), full_material(1, "B", 200), full_material(2, "C", 50)],
        1.0,
    );
    let path = std::path::Path::new("/tmp/synthetic_merge_objects_count.glb");
    let plan = merge_import_into_graph(&def, &summary, path).expect("merge three objects");

    assert_eq!(plan.render_scene_node_id, render_id);
    assert_eq!(plan.new_objects_count, existing_objects + 3);
    assert_eq!(plan.new_nodes.len(), 1, "one compound group for the incoming asset");

    for k in existing_objects..(existing_objects + 3) {
        assert!(
            plan.new_wires.iter().any(|w| w.to_node == render_id && w.to_port == format!("object_{k}")),
            "new wires must target object_{k} (continuing from the existing {existing_objects} objects), not restart at object_0"
        );
    }
    // Never re-targets an already-occupied port.
    assert!(
        !plan.new_wires.iter().any(|w| w.to_node == render_id && w.to_port == "object_0"),
        "merge must not re-wire the scene's EXISTING object_0 port"
    );
}

/// Card-spec sections extend: a glass incoming material gets an Opacity
/// card slider sectioned under its OWN group name (same as a fresh
/// import), appended to the plan's card additions — never dropped,
/// never colliding with the target's existing card params.
#[test]
fn merge_extends_card_spec_sections_for_new_objects() {
    let def = scene_def_with_bbox_half_extent(1.0);
    let mut glass = full_material(0, "GlassPane", 400);
    glass.was_blend = true;
    glass.transmission_factor = 0.0;
    let summary = merge_summary(vec![glass], 1.0);
    let path = std::path::Path::new("/tmp/synthetic_merge_card_spec.glb");
    let plan = merge_import_into_graph(&def, &summary, path).expect("merge one glass object");

    assert!(
        plan.new_card_params.iter().any(|p| p.name == "Opacity" && p.section.as_deref() == Some("GlassPane — Material")),
        "the merged glass object's material must expose Opacity from ParamDef"
    );
    assert!(
        plan.new_card_bindings.iter().any(|b| match &b.target {
            BindingTarget::Node { node_id, param } => {
                node_id.as_str().starts_with("mat_") && param == "color_a"
            }
            _ => false,
        }),
        "the merged Opacity slider must bind the material's color_a"
    );
    // The shared Ambient binding still fans out for the new material too.
    assert!(
        plan.new_card_bindings.iter().any(|b| b.id == "scene_ambient"),
        "the merged object's material still gets the shared Ambient binding"
    );
}

/// D5 scale sanity, no-op case: an incoming asset within 10x of the
/// scene's reference radius gets NO seeded scale (native units).
#[test]
fn merge_within_10x_never_normalizes() {
    let def = scene_def_with_bbox_half_extent(1.0); // scene reference radius ~= sqrt(3)
    let summary = merge_summary(vec![full_material(0, "Same", 100)], 1.0); // identical bbox, ratio 1.0
    let path = std::path::Path::new("/tmp/synthetic_merge_no_normalize.glb");
    let plan = merge_import_into_graph(&def, &summary, path).expect("merge same-scale object");

    let group = plan.new_nodes.first().unwrap();
    let transform = group
        .group
        .as_ref()
        .unwrap()
        .nodes
        .iter()
        .find(|n| n.type_id == "node.transform_3d")
        .expect("object group has a transform_3d");
    assert!(
        !transform.params.contains_key("scale_x"),
        "within 10x, no scale should be seeded at all — native units"
    );
    assert!(
        !plan.report_lines.iter().any(|l| l.contains("scaled ×")),
        "no normalize report line when the ratio is within bounds"
    );
}

/// BUG-195 real fix: when the target's own mesh-source node carries a
/// KNOWN `source_bbox_radius` that disagrees with what the
/// orbit-camera-distance proxy would derive (e.g. the user hand-retuned
/// Camera Distance on the card after import, per the confessed BUG-195
/// blind spot), the stored radius wins — never the proxy.
#[test]
fn merge_scale_sanity_prefers_stored_radius_over_camera_proxy() {
    let mut def = scene_def_with_bbox_half_extent(1.0);
    // The proxy (unmutated orbit_camera.distance / 2.2) is ~= sqrt(3) ~=
    // 1.732 — same as the stored radius build_import_graph seeded, by
    // construction. Mutate ONLY the stored radius to something wildly
    // different, simulating a scene whose stored provenance no longer
    // agrees with the (user-editable) camera distance.
    let group = def
        .nodes
        .iter_mut()
        .find(|n| n.type_id == manifold_core::effect_graph_def::GROUP_TYPE_ID)
        .expect("one object group");
    let mesh = group
        .group
        .as_mut()
        .unwrap()
        .nodes
        .iter_mut()
        .find(|n| n.type_id == "node.gltf_mesh_source")
        .expect("object group has a mesh source");
    mesh.params
        .insert("source_bbox_radius".to_string(), SerializedParamValue::Float { value: 100.0 });

    // Incoming asset has the SAME bbox as the (unmutated) target scene —
    // against the proxy (~1.732) the ratio is 1.0 (no normalize); against
    // the mutated stored radius (100.0) the ratio is ~0.017 (normalizes).
    let summary = merge_summary(vec![full_material(0, "Same", 100)], 1.0);
    let path = std::path::Path::new("/tmp/synthetic_merge_prefers_stored_radius.glb");
    let plan = merge_import_into_graph(&def, &summary, path).expect("merge");

    let new_group = plan.new_nodes.first().unwrap();
    let transform = new_group
        .group
        .as_ref()
        .unwrap()
        .nodes
        .iter()
        .find(|n| n.type_id == "node.transform_3d")
        .expect("object group has a transform_3d");
    let scale = match transform.params.get("scale_x") {
        Some(SerializedParamValue::Float { value }) => *value,
        other => panic!(
            "expected a seeded scale_x — the stored radius (100.0), not the camera proxy \
             (~1.732), must have driven this decision, got {other:?}"
        ),
    };
    assert!(
        scale > 10.0,
        "stored radius (100.0) vs incoming (~1.732) should normalize UP by ~57x, got {scale}"
    );
}

/// D5 scale sanity, too-big boundary: an incoming asset >10x LARGER
/// than the scene's reference radius gets a seeded scale < 1.0 that
/// brings it back down to the reference size.
#[test]
fn merge_over_10x_too_big_normalizes_down() {
    let def = scene_def_with_bbox_half_extent(1.0);
    // 20x the scene's half-extent -> incoming radius ~20x the scene's.
    let summary = merge_summary(vec![full_material(0, "Giant", 100)], 20.0);
    let path = std::path::Path::new("/tmp/synthetic_merge_too_big.glb");
    let plan = merge_import_into_graph(&def, &summary, path).expect("merge oversized object");

    let group = plan.new_nodes.first().unwrap();
    let transform = group
        .group
        .as_ref()
        .unwrap()
        .nodes
        .iter()
        .find(|n| n.type_id == "node.transform_3d")
        .unwrap();
    let scale = match transform.params.get("scale_x") {
        Some(SerializedParamValue::Float { value }) => *value,
        other => panic!("expected a seeded scale_x float param, got {other:?}"),
    };
    assert!(scale < 1.0, "an oversized incoming asset must be scaled DOWN, got {scale}");
    assert!(
        plan.report_lines.iter().any(|l| l.contains("scaled ×")),
        "a normalize report line must be present"
    );
}

/// D5 scale sanity, too-small boundary: an incoming asset >10x SMALLER
/// than the scene's reference radius gets a seeded scale > 1.0 that
/// brings it back up to the reference size.
#[test]
fn merge_over_10x_too_small_normalizes_up() {
    let def = scene_def_with_bbox_half_extent(1.0);
    // 1/20th the scene's half-extent -> incoming radius ~1/20th the scene's.
    let summary = merge_summary(vec![full_material(0, "Tiny", 100)], 0.05);
    let path = std::path::Path::new("/tmp/synthetic_merge_too_small.glb");
    let plan = merge_import_into_graph(&def, &summary, path).expect("merge undersized object");

    let group = plan.new_nodes.first().unwrap();
    let transform = group
        .group
        .as_ref()
        .unwrap()
        .nodes
        .iter()
        .find(|n| n.type_id == "node.transform_3d")
        .unwrap();
    let scale = match transform.params.get("scale_x") {
        Some(SerializedParamValue::Float { value }) => *value,
        other => panic!("expected a seeded scale_x float param, got {other:?}"),
    };
    assert!(scale > 1.0, "an undersized incoming asset must be scaled UP, got {scale}");
    assert!(
        plan.report_lines.iter().any(|l| l.contains("scaled ×")),
        "a normalize report line must be present"
    );
}

/// Negative gate: OBJECT_SAFETY_MAX is enforced on the POST-MERGE total
/// (existing + incoming), never silently truncated.
#[test]
fn merge_over_object_safety_max_post_merge_errors_loudly() {
    let def = scene_def_with_bbox_half_extent(1.0); // 1 existing object
    let n = OBJECT_SAFETY_MAX as usize; // exactly at the max on its own; +1 existing pushes it over
    let materials: Vec<_> = (0..n).map(|k| full_material(k as u32, &format!("M{k}"), 10)).collect();
    let summary = merge_summary(materials, 1.0);
    let path = std::path::Path::new("/tmp/synthetic_merge_over_cap.glb");
    let err = merge_import_into_graph(&def, &summary, path)
        .expect_err("existing (1) + incoming (OBJECT_SAFETY_MAX) must exceed the bound");
    assert!(err.contains(&OBJECT_SAFETY_MAX.to_string()), "error must name the safety bound: {err}");
}



/// A target `def` with no top-level `node.render_scene` at all is a
/// named escalation, not a guess — merging into a graph the panel would
/// never show as a scene must error loudly.
#[test]
fn merge_into_a_def_without_render_scene_errors() {
    let def = EffectGraphDef {
        version: manifold_core::effect_graph_def::EFFECT_GRAPH_VERSION,
        name: None,
        description: None,
        preset_metadata: None,
        scene_modifiers: Vec::new(),
        nodes: Vec::new(),
        wires: Vec::new(),
    };
    let summary = merge_summary(vec![full_material(0, "Orphan", 100)], 1.0);
    let path = std::path::Path::new("/tmp/synthetic_merge_no_render_scene.glb");
    let err = merge_import_into_graph(&def, &summary, path)
        .expect_err("a def with no render_scene must error, never silently no-op");
    assert!(err.contains("render_scene"));
}

/// D6 colour-space pinning + D3 port-wiring: a synthetic material
/// carrying all five texture kinds (base-colour, normal, MR, occlusion,
/// emissive) must wire all four NEW ports (`normal_map`, `mr_map`,
/// `occlusion_map`, `emissive_map`) into `node.scene_object`, each
/// fed by a `node.gltf_texture_source` whose `color_space` matches D6:
/// base-colour and emissive decode sRGB (0), normal/MR/occlusion decode
/// Linear (1) — the data-map convention (raw bytes ARE the value).
#[test]
fn imports_all_map_kinds_with_correct_color_spaces() {
    let mut material = full_material(0, "Helmet", 1000);
    material.diffuse_transmission_texture = Some(5);
    material.diffuse_transmission_color_texture = Some(6);
    let summary = GltfImportSummary {
        materials: vec![material],
        bbox_min: [-1.0, -1.0, -1.0],
        bbox_max: [1.0, 1.0, 1.0],
        camera_count: 0,
        default_material_vertex_count: 0,
        animations: Vec::new(),
        animation_report_lines: Vec::new(),
        extension_report_lines: Vec::new(),
        lights: Vec::new(),
        cameras: Vec::new(),
        camera_report_lines: Vec::new(),
        texture_dims: Vec::new(),
    };
    let path = std::path::Path::new("/tmp/synthetic_all_maps.glb");
    let (def, report) = build_import_graph(&summary, path).expect("build graph");
    assert_eq!(report.textures_wired, 1, "base-colour wired");

    // Flatten so the group-internal texture-source nodes and the
    // top-level render_scene wires are both queryable in one flat
    // node/wire list (same recipe the grouping-equivalence test above
    // uses).
    let flat = manifold_core::flatten::flatten_groups(&def).expect("flatten");

    // SCENE_OBJECT_AND_PANEL_V2_DESIGN D1/D3: the maps wire into this
    // object's `node.scene_object` bind node (`object_0_bind`), not
    // directly into render_scene — render_scene only ever sees the
    // single `object_0` port.
    let scene_object = flat
        .nodes
        .iter()
        .find(|n| n.type_id == "node.scene_object")
        .expect("scene_object bind node");
    for port in [
        "normal_map",
        "mr_map",
        "occlusion_map",
        "emissive_map",
        "diffuse_transmission_map",
        "diffuse_transmission_color_map",
    ] {
        assert!(
            flat.wires.iter().any(|w| w.to_node == scene_object.id && w.to_port == port),
            "expected a wire into scene_object port `{port}`"
        );
    }

    // Each new map's own source node carries the D6-correct color_space.
    let expect_color_space = |prefix: &str, expected: u32| {
        let node = flat
            .nodes
            .iter()
            .find(|n| n.node_id.starts_with(prefix) && n.type_id == "node.gltf_texture_source")
            .unwrap_or_else(|| panic!("expected a `{prefix}*` gltf_texture_source node"));
        let cs = node.params.get("color_space").expect("color_space param set");
        assert_eq!(
            *cs,
            enum_val(expected),
            "`{prefix}*` color_space must be {expected} ({})",
            if expected == 0 { "sRGB" } else { "Linear" }
        );
    };
    expect_color_space("tex_", 0); // base-colour: sRGB
    expect_color_space("normal_tex_", 1); // normal: Linear
    expect_color_space("mr_tex_", 1); // metallic-roughness: Linear
    expect_color_space("occlusion_tex_", 1); // occlusion: Linear
    expect_color_space("emissive_tex_", 0); // emissive: sRGB
    expect_color_space("diffuse_transmission_tex_", 1); // factor: Linear (alpha)
    expect_color_space("diffuse_transmission_color_tex_", 0); // colour: sRGB (RGB)

    // KHR_materials_emissive_strength folds into the existing
    // emission_intensity param rather than growing a new one (D5).
    let mat = flat
        .nodes
        .iter()
        .find(|n| n.type_id == "node.pbr_material")
        .expect("pbr_material node");
    assert_eq!(
        mat.params.get("emission_intensity"),
        Some(&float(2.5)),
        "emissive_strength (2.5) must land on emission_intensity"
    );

    // Fully-mapped, nothing report-worthy: no clearcoat/transmission/BLEND lines.
    assert!(
        report.report_lines.is_empty(),
        "a fully-mapped material with no clearcoat/transmission/BLEND should report nothing, got {:?}",
        report.report_lines
    );
}

#[test]
fn material_scalar_extensions_are_imported_without_obsolete_reports() {
    let mut m = full_material(0, "Scalar Fidelity", 1000);
    m.normal_scale = -0.35;
    m.clearcoat_normal_scale = -0.6;
    m.occlusion_strength = 0.42;
    m.diffuse_transmission_factor = 0.7;
    m.diffuse_transmission_color = [0.2, 0.3, 0.4];
    let summary = GltfImportSummary {
        materials: vec![m],
        bbox_min: [-1.0, -1.0, -1.0],
        bbox_max: [1.0, 1.0, 1.0],
        camera_count: 0,
        default_material_vertex_count: 0,
        animations: Vec::new(),
        animation_report_lines: Vec::new(),
        extension_report_lines: Vec::new(),
        lights: Vec::new(),
        cameras: Vec::new(),
        camera_report_lines: Vec::new(),
        texture_dims: Vec::new(),
    };
    let path = std::path::Path::new("/tmp/synthetic_material_scalar_extensions.glb");
    let (def, report) = build_import_graph(&summary, path).expect("build graph");
    let flat = manifold_core::flatten::flatten_groups(&def).expect("flatten");
    let mat = flat
        .nodes
        .iter()
        .find(|n| n.type_id == "node.pbr_material")
        .expect("pbr_material node");
    assert_eq!(mat.params.get("normal_scale"), Some(&float(-0.35)));
    assert_eq!(mat.params.get("clearcoat_normal_scale"), Some(&float(-0.6)));
    assert_eq!(mat.params.get("occlusion_strength"), Some(&float(0.42)));
    assert_eq!(mat.params.get("translucency"), Some(&float(0.7)));
    assert_eq!(mat.params.get("translucency_color_r"), Some(&float(0.2)));
    assert_eq!(mat.params.get("translucency_color_g"), Some(&float(0.3)));
    assert_eq!(mat.params.get("translucency_color_b"), Some(&float(0.4)));
    assert!(report.report_lines.iter().all(|line| {
        !line.contains("normalTexture.scale")
            && !line.contains("occlusionTexture.strength")
            && !line.contains("diffuseTransmissionColorFactor")
    }));
}

/// BUG-pt6g (Peter's ruling, supersedes BUG-w5wv): a material with
/// `KHR_materials_unlit` set no longer routes to `node.unlit_material` —
/// MANIFOLD is a performance instrument, the performer lights the scene,
/// so import always builds `node.pbr_material` and leaves its `baked_look`
/// param at its default (false = lit). `full_material` sets normal/mr/
/// occlusion/emissive textures too — a photoscan's unlit-flagged material
/// now wires ALL of them, exactly like a non-unlit material, since it's a
/// genuine `node.pbr_material` now (the map-family wiring gate that used
/// to skip them for `m.unlit` is gone — BUG-w5wv's `!m.unlit` check in
/// `object_group.rs`).
#[test]
fn unlit_material_imports_lit_by_default() {
    let mut m = full_material(0, "Glow", 500);
    m.unlit = true;
    m.base_color_factor = [0.9, 0.3, 0.1, 1.0];
    let summary = GltfImportSummary {
        materials: vec![m],
        bbox_min: [-1.0, -1.0, -1.0],
        bbox_max: [1.0, 1.0, 1.0],
        camera_count: 0,
        default_material_vertex_count: 0,
        animations: Vec::new(),
        animation_report_lines: Vec::new(),
        extension_report_lines: Vec::new(),
        lights: Vec::new(),
        cameras: Vec::new(),
        camera_report_lines: Vec::new(),
        texture_dims: Vec::new(),
    };
    let path = std::path::Path::new("/tmp/synthetic_unlit.glb");
    let (def, report) = build_import_graph(&summary, path).expect("build graph");
    // `textures_wired` counts ONLY the base-colour map (the other PBR
    // families are tracked via node presence, not this counter — see its
    // own doc comment in `object_group.rs`: "the base-colour map above is
    // deliberately NOT a family... it alone increments `textures_wired`").
    assert_eq!(report.textures_wired, 1, "base_color_texture wiring is unconditional on material kind");

    let flat = manifold_core::flatten::flatten_groups(&def).expect("flatten");

    assert!(
        !flat.nodes.iter().any(|n| n.type_id == "node.unlit_material"),
        "BUG-pt6g: the importer never routes to node.unlit_material anymore"
    );
    let mat = flat
        .nodes
        .iter()
        .find(|n| n.type_id == "node.pbr_material")
        .expect("an unlit-flagged material must still construct node.pbr_material");
    assert_eq!(mat.params.get("color_r"), Some(&float(0.9)));
    assert_eq!(mat.params.get("color_g"), Some(&float(0.3)));
    assert_eq!(mat.params.get("color_b"), Some(&float(0.1)));
    assert_eq!(mat.params.get("color_a"), Some(&float(1.0)));
    assert_eq!(
        mat.params.get("baked_look"),
        Some(&bool_val(false)),
        "the importer's own unlit hint is ignored for routing — baked_look stays at its \
         default (lit) on import, the performer opts in per-material"
    );

    // BUG-pt6g: the OTHER map families (normal/mr/occlusion/emissive), not
    // just base_color, now get wired for a formerly-unlit material too —
    // this is the gate `unlit_material_routes_to_unlit_material_card`
    // (the test this one replaces) used to assert the OPPOSITE of.
    for prefix in ["normal_tex_", "mr_tex_", "occlusion_tex_", "emissive_tex_"] {
        assert!(
            flat.nodes.iter().any(|n| n.node_id.starts_with(prefix)),
            "lit-by-default material must wire a `{prefix}*` PBR-extension map source"
        );
    }

    let scene_object = flat
        .nodes
        .iter()
        .find(|n| n.type_id == "node.scene_object")
        .expect("scene_object bind node");
    assert!(
        flat.wires.iter().any(|w| w.to_node == scene_object.id && w.to_port == "base_color_map"),
        "base_color_texture must still wire scene_object's base_color_map"
    );
}

/// D5 ORM-packing: when `occlusion_texture` and `mr_texture` share the
/// same glTF texture index (the common "one packed ORM image" case),
/// the importer must wire ONE `node.gltf_texture_source` into BOTH
/// `occlusion_map_0` and `mr_map_0` — never decode the same physical
/// image twice.
#[test]
fn orm_packed_occlusion_and_mr_share_one_texture_source_node() {
    let mut m = full_material(0, "ORM", 500);
    m.occlusion_texture = Some(7);
    m.mr_texture = Some(7); // same physical image as occlusion
    let summary = GltfImportSummary {
        materials: vec![m],
        bbox_min: [-1.0, -1.0, -1.0],
        bbox_max: [1.0, 1.0, 1.0],
        camera_count: 0,
        default_material_vertex_count: 0,
        animations: Vec::new(),
        animation_report_lines: Vec::new(),
        extension_report_lines: Vec::new(),
        lights: Vec::new(),
        cameras: Vec::new(),
        camera_report_lines: Vec::new(),
        texture_dims: Vec::new(),
    };
    let path = std::path::Path::new("/tmp/synthetic_orm.glb");
    let (def, _report) = build_import_graph(&summary, path).expect("build graph");
    let flat = manifold_core::flatten::flatten_groups(&def).expect("flatten");

    let orm_sources: Vec<_> = flat
        .nodes
        .iter()
        .filter(|n| {
            n.type_id == "node.gltf_texture_source"
                && n.params.get("texture_index") == Some(&int(7))
        })
        .collect();
    assert_eq!(
        orm_sources.len(),
        1,
        "occlusion_texture == mr_texture must decode through exactly ONE source node, found {}",
        orm_sources.len()
    );
    let scene_object = flat.nodes.iter().find(|n| n.type_id == "node.scene_object").unwrap();
    let source_id = orm_sources[0].id;
    for port in ["occlusion_map", "mr_map"] {
        assert!(
            flat.wires
                .iter()
                .any(|w| w.to_node == scene_object.id && w.to_port == port && w.from_node == source_id),
            "expected `{port}` wired directly from the shared ORM source node"
        );
    }
}

/// BUG-5mma (BUG-177 (glb-vertex-colors-not-wired-color0-never-read)): the
/// vertex colour loading is proven at the parse layer — this test
/// covers the OTHER half, the D9 report-line wiring in `object_group.rs`:
/// `vertex_color_varies = true` on a `GltfMaterialInfo` must produce a
/// report line naming the material and leave `color_r`/`color_g`/`color_b`
/// on `node.pbr_material` exactly at the material's own (unfolded)
/// `base_color_factor`.
#[test]
fn vertex_color_varies_flag_produces_report_line_and_leaves_base_color_alone() {
    let mut m = full_material(0, "VaryingVC", 300);
    m.base_color_factor = [0.8, 0.6, 0.4, 1.0];
    m.vertex_color_varies = true;
    let summary = GltfImportSummary {
        materials: vec![m],
        bbox_min: [-1.0, -1.0, -1.0],
        bbox_max: [1.0, 1.0, 1.0],
        camera_count: 0,
        default_material_vertex_count: 0,
        animations: Vec::new(),
        animation_report_lines: Vec::new(),
        extension_report_lines: Vec::new(),
        lights: Vec::new(),
        cameras: Vec::new(),
        camera_report_lines: Vec::new(),
        texture_dims: Vec::new(),
    };
    let path = std::path::Path::new("/tmp/synthetic_vertex_color_varies.glb");
    let (def, report) = build_import_graph(&summary, path).expect("build graph");

    assert!(
        report
            .report_lines
            .iter()
            .any(|l| l.contains("varying per-vertex COLOR_0") && l.contains("preserved, including alpha")),
        "expected a per-vertex COLOR_0 varies report line, got {:?}",
        report.report_lines
    );

    let flat = manifold_core::flatten::flatten_groups(&def).expect("flatten");
    let mat_node = flat
        .nodes
        .iter()
        .find(|n| n.type_id == "node.pbr_material")
        .expect("pbr_material node");
    assert_eq!(mat_node.params.get("color_r"), Some(&float(0.8)));
    assert_eq!(mat_node.params.get("color_g"), Some(&float(0.6)));
    assert_eq!(mat_node.params.get("color_b"), Some(&float(0.4)));
}

/// D9 doctrine ("every import produces a report") applied to G-P5's
/// clearcoat feature set. GLTF_MATERIAL_EXTENSIONS_DESIGN.md E6 (D1
/// revised — full spec surface): a TEXTURED coat is now a real mapping
/// too (no more report-only gap) — an over-featured synthetic material
/// carrying a textured clearcoat together with transmission and a
/// BLEND alphaMode must produce ZERO report lines, and must build a
/// real `Blend` material with the transmission-folded alpha, the
/// clearcoat factor, AND the clearcoatMap wire onto `node.pbr_material`
/// / its group's output.
#[test]
fn over_featured_material_wires_clearcoat_texture_and_maps_transmission_to_blend() {
    let mut m = full_material(0, "Kitchen Sink", 300);
    m.clearcoat_factor = 1.0;
    m.clearcoat_roughness_factor = 0.1;
    m.clearcoat_texture = Some(0);
    m.transmission_factor = 0.9;
    m.was_blend = true;
    m.alpha_mask = false; // a real glTF BLEND material never sets MASK too
    m.base_color_factor = [0.9, 0.95, 1.0, 1.0];
    let summary = GltfImportSummary {
        materials: vec![m],
        bbox_min: [-1.0, -1.0, -1.0],
        bbox_max: [1.0, 1.0, 1.0],
        camera_count: 0,
        default_material_vertex_count: 0,
        animations: Vec::new(),
        animation_report_lines: Vec::new(),
        extension_report_lines: Vec::new(),
        lights: Vec::new(),
        cameras: Vec::new(),
        camera_report_lines: Vec::new(),
        texture_dims: Vec::new(),
    };
    let path = std::path::Path::new("/tmp/synthetic_over_featured.glb");
    let (def, report) = build_import_graph(&summary, path).expect("build graph");
    println!("over-featured report: {:#?}", report.report_lines);
    assert!(
        !report.report_lines.iter().any(|l| l.contains("clearcoat")),
        "clearcoat texture must no longer be report-only: {:?}",
        report.report_lines
    );
    assert!(
        !report.report_lines.iter().any(|l| l.contains("transmission") || l.contains("BLEND")),
        "transmission/BLEND must no longer produce report lines: {:?}",
        report.report_lines
    );

    let flat = manifold_core::flatten::flatten_groups(&def).expect("flatten");
    let mat = flat
        .nodes
        .iter()
        .find(|n| n.type_id == "node.pbr_material")
        .expect("pbr_material node");
    assert_eq!(
        mat.params.get("alpha_mode"),
        Some(&enum_val(2)),
        "transmission/BLEND material must map to alpha_mode Blend (2)"
    );
    // GLTF_MATERIAL_EXTENSIONS_DESIGN.md E2b: color_a is base_color.a
    // alone now (1.0) — transmission's see-through is carried by
    // fs_pbr's shader-side diffuse substitution, not by darkened alpha
    // (the old D8/F-P5 approximation this phase removes).
    let color_a = mat.params.get("color_a").expect("color_a set");
    match color_a {
        SerializedParamValue::Float { value } => assert!(
            (value - 1.0).abs() < 1e-4,
            "color_a must equal base_color.a unchanged, got {value}"
        ),
        other => panic!("expected Float color_a, got {other:?}"),
    }
    assert_eq!(mat.params.get("clearcoat"), Some(&float(1.0)));
    assert_eq!(mat.params.get("clearcoat_roughness"), Some(&float(0.1)));
    // GLTF_MATERIAL_EXTENSIONS_DESIGN.md E6: the textured coat wires
    // `clearcoat_map` from this object's group into its
    // `node.scene_object` bind node through the flattener, same as
    // sheen/iridescence/anisotropy (SCENE_OBJECT_AND_PANEL_V2_DESIGN
    // D1/D3 — render_scene itself only ever sees `object_0`).
    let scene_object = flat
        .nodes
        .iter()
        .find(|n| n.type_id == "node.scene_object")
        .expect("scene_object bind node");
    assert!(
        flat.wires
            .iter()
            .any(|w| w.to_node == scene_object.id && w.to_port == "clearcoat_map"),
        "expected clearcoat_map wired on scene_object"
    );
}

/// D7 sun coherence: each of the Sun X/Y/Z card macros must carry TWO
/// binding targets — the sun `node.light`'s position (unchanged,
/// pre-existing) AND the envmap's new `sun_x`/`sun_y`/`sun_z` disc-
/// direction params — so performing the sun macro moves illumination,
/// shadow, AND the envmap's reflected sun disc together (Peter,
/// 2026-07-15: "place these fake strips and lights in the same
/// positions as the real scene lights").
#[test]
fn sun_macros_bind_both_the_light_and_the_envmap_disc_direction() {
    let summary = GltfImportSummary {
        materials: vec![full_material(0, "Object", 100)],
        bbox_min: [-1.0, -1.0, -1.0],
        bbox_max: [1.0, 1.0, 1.0],
        camera_count: 0,
        default_material_vertex_count: 0,
        animations: Vec::new(),
        animation_report_lines: Vec::new(),
        extension_report_lines: Vec::new(),
        lights: Vec::new(),
        cameras: Vec::new(),
        camera_report_lines: Vec::new(),
        texture_dims: Vec::new(),
    };
    let path = std::path::Path::new("/tmp/synthetic_sun.glb");
    let (def, _report) = build_import_graph(&summary, path).expect("build graph");
    let meta = def.preset_metadata.as_ref().expect("v2 metadata");

    let sun_id = def
        .nodes
        .iter()
        .find(|n| n.type_id == "node.light")
        .map(|n| n.id)
        .expect("sun present");

    for (param, axis) in [("pos_x", "sun_x"), ("pos_y", "sun_y"), ("pos_z", "sun_z")] {
        let macro_id = format!("{sun_id}_{param}");
        let bindings: Vec<_> = meta.bindings.iter().filter(|b| b.id == macro_id).collect();
        assert_eq!(
            bindings.len(),
            2,
            "`{macro_id}` must carry exactly 2 binding targets (sun light + envmap disc), got {}",
            bindings.len()
        );
        let targets_sun = bindings.iter().any(|b| match &b.target {
            BindingTarget::Node { node_id, param: p } => {
                node_id.as_str() == "sun" && p == param
            }
            _ => false,
        });
        let targets_envmap = bindings.iter().any(|b| match &b.target {
            BindingTarget::Node { node_id, param: p } => {
                node_id.as_str() == "envmap" && p == axis
            }
            _ => false,
        });
        assert!(targets_sun, "`{macro_id}` must bind the sun light's `{param}`");
        assert!(
            targets_envmap,
            "`{macro_id}` must ALSO bind the envmap's `{axis}` disc-direction param"
        );
        let defaults: std::collections::HashSet<_> =
            bindings.iter().map(|b| b.default_value.to_bits()).collect();
        assert_eq!(defaults.len(), 1, "`{macro_id}`'s two bindings must share one default value");
    }

    // D7 import defaults: softbox @ 1.0, not the legacy gradient @ 0.
    let flat = manifold_core::flatten::flatten_groups(&def).expect("flatten");
    let envmap = flat.nodes.iter().find(|n| n.type_id == "node.bake_environment").unwrap();
    assert_eq!(envmap.params.get("mode"), Some(&enum_val(1)), "import default mode = Softbox");
    assert_eq!(envmap.params.get("intensity"), Some(&float(1.0)), "import default intensity = 1.0");
    let envmap_id = def
        .nodes
        .iter()
        .find(|n| n.type_id == "node.bake_environment")
        .map(|n| n.id)
        .unwrap();
    let env_intensity_id = format!("{envmap_id}_intensity");
    let env_intensity_param = meta.params.iter().find(|p| p.id == env_intensity_id).unwrap();
    assert_eq!(env_intensity_param.default_value, 1.0, "Environment card default = 1.0");
    assert_eq!(env_intensity_param.min, 0.0);
    assert_eq!(env_intensity_param.max, 4.0, "range stays 0-4 (D7: only the default flips)");

    // F-P7 import defaults: dome fill + strip intensity are now exposed as
    // individual envmap params (P1), not separate curated sliders.
    assert_eq!(
        envmap.params.get("fill"),
        Some(&float(IMPORT_FILL_DEFAULT)),
        "import default fill = IMPORT_FILL_DEFAULT"
    );
    assert_eq!(
        envmap.params.get("emitter_intensity"),
        Some(&float(IMPORT_STRIPS_DEFAULT)),
        "import default strips = IMPORT_STRIPS_DEFAULT"
    );
    assert!(
        meta.params.iter().any(|p| p.id == format!("{envmap_id}_fill")),
        "envmap fill is exposed from ParamDef"
    );
    assert!(
        meta.params.iter().any(|p| p.id == format!("{envmap_id}_emitter_intensity")),
        "envmap emitter_intensity is exposed from ParamDef"
    );
}





/// GLTF_ANIM_RUNTIME_V2_DESIGN.md section 3 invariant, P2 gate: no keyframe
/// payload in ANY def the importer emits. `skeleton_animated.glb` is a
/// real rigged+animated asset (drives `node.gltf_skeleton_pose` +
/// `node.gltf_animation_source` — see the neighboring card-lint and
/// BUG-205 tests) — pre-P2 this asset's def carried the six pose
/// Tables plus the rigid Tables, easily tens of KB per joint/keyframe.
/// Post-P2 the def carries only `path`/`skin_index`/`target_node`
/// selectors, so the whole serialized def stays comfortably under the
/// design's 256 KB budget (the dragon-scale 5.2 GB-RSS pathology this
/// design fixes needs P4's real-asset acceptance measurement; this
/// unit-scale gate proves the STORAGE CLASS is gone, not the exact
/// dragon number).
#[test]
fn imported_def_json_stays_small() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/gltf/skeleton_animated.glb");
    let (def, _report) =
        super::assemble_import_graph(&path).expect("assemble skeleton_animated.glb");
    let json = serde_json::to_string(&def).expect("serialize EffectGraphDef");
    assert!(
        json.len() < 256 * 1024,
        "imported def serialized to {} bytes, budget is 256 KB (GLTF_ANIM_RUNTIME_V2_DESIGN.md D1)",
        json.len()
    );

    let flat = manifold_core::flatten::flatten_groups(&def).expect("flatten import def");
    for n in &flat.nodes {
        if matches!(
            n.type_id.as_str(),
            "node.gltf_skeleton_pose" | "node.gltf_animation_source" | "node.gltf_morph_weights"
        ) {
            for key in [
                "joint_parent_table",
                "joint_root_world_table",
                "inverse_bind_table",
                "translation_tracks",
                "rotation_tracks",
                "scale_tracks",
                "translation_track",
                "rotation_track",
                "scale_track",
                "weight_tracks",
            ] {
                assert!(
                    !n.params.contains_key(key),
                    "{} ({}) still carries the dead keyframe param `{key}`",
                    n.node_id.as_str(),
                    n.type_id
                );
            }
        }
    }
}

/// BUG-205 regression (double-transform half): a SKINNED object must
/// NOT get a `node.gltf_animation_source` wired into its transform_3d.
/// skeleton_animated.glb animates `Bip01` — an ancestor ABOVE the
/// joint tree whose static 0.0254 scale is already inside the joint
/// palette via `joint_root_world` — so the rigid path re-applying that
/// chain shrank the render to 0.0254² of its authored size (a ~12px
/// speck at the framing distance).
#[test]
fn skinned_import_gets_no_rigid_animation_source() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/gltf/skeleton_animated.glb");
    let (def, _report) =
        super::assemble_import_graph(&path).expect("assemble skeleton_animated.glb");
    let flat = manifold_core::flatten::flatten_groups(&def).expect("flatten import def");
    assert!(
        flat.nodes.iter().any(|n| n.type_id == "node.gltf_skeleton_pose"),
        "rigged import must drive its mesh through node.gltf_skeleton_pose"
    );
    let skinned_source = flat
        .nodes
        .iter()
        .find(|n| n.type_id == "node.gltf_skinned_mesh_source")
        .expect("rigged import must contain a skinned mesh source");
    assert_eq!(
        skinned_source.params.get("vertex_colors"),
        Some(&bool_val(true)),
        "new skinned imports must explicitly enable authored vertex colors"
    );
    assert!(
        !flat.nodes.iter().any(|n| n.type_id == "node.gltf_animation_source"),
        "a skinned object's positioning comes entirely from its joint palette — \
         a rigid node.gltf_animation_source on the same object re-applies the \
         ancestor chain a second time (BUG-205)"
    );
}

/// BUG-208: an object with BOTH a skin and morph targets must import
/// with its morph animation COMPOSED, not silently dropped —
/// `node.morph_targets_blend` chained between
/// `node.gltf_skinned_mesh_source` and `node.skin_mesh`'s `in` (glTF
/// applies morph then skin, section 3.7.2), and the deltas source's
/// `skinned` param set so its loaded deltas share the skinned
/// source's untransformed bind-pose space. `skin_morph.glb`: a
/// Blender-authored armature-skinned cylinder with a keyframed
/// "Bulge" shape key, carrying both a skin AND morph targets plus
/// animation channels for each.
#[test]
fn skin_and_morph_combination_composes_instead_of_dropping() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/gltf/hostile/skin_morph.glb");
    let (def, report) = super::assemble_import_graph(&path).expect("assemble skin_morph.glb");

    assert!(
        report.report_lines.iter().any(|l| l.contains("BUG-208")),
        "import report must call out the skin+morph composition explicitly: {:?}",
        report.report_lines
    );

    let flat = manifold_core::flatten::flatten_groups(&def).expect("flatten import def");
    let skinned_source = flat
        .nodes
        .iter()
        .find(|n| n.type_id == "node.gltf_skinned_mesh_source")
        .expect("skin+morph object must still be driven by node.gltf_skinned_mesh_source");
    assert_eq!(
        skinned_source.params.get("vertex_colors"),
        Some(&bool_val(true)),
        "new skin+morph imports must explicitly enable authored vertex colors"
    );
    let blend = flat
        .nodes
        .iter()
        .find(|n| n.type_id == "node.morph_targets_blend")
        .expect("skin+morph object must carry a node.morph_targets_blend — morph animation \
                 dropped silently (BUG-208)");
    let skinmesh = flat
        .nodes
        .iter()
        .find(|n| n.type_id == "node.skin_mesh")
        .expect("skinned object must carry node.skin_mesh");
    assert!(
        flat.wires
            .iter()
            .any(|w| w.from_node == blend.id && w.from_port == "out"
                && w.to_node == skinmesh.id && w.to_port == "in"),
        "node.morph_targets_blend's `out` must feed node.skin_mesh's `in` directly — \
         glTF applies morph before skin (section 3.7.2)"
    );
    let deltas = flat
        .nodes
        .iter()
        .find(|n| n.type_id == "node.gltf_morph_deltas_source")
        .expect("skin+morph object must carry node.gltf_morph_deltas_source");
    assert_eq!(
        deltas.params.get("skinned"),
        Some(&SerializedParamValue::Bool { value: true }),
        "the deltas source must be told this object is skinned — otherwise its loader \
         world-transforms the deltas while the skinned base vertices stay untransformed \
         (a coordinate-space mismatch, BUG-208)"
    );
}








/// BUG-205 regression (bbox-space half): the import summary's bbox for
/// a skinned mesh must live in bind-pose SKINNED space (what
/// `node.skin_mesh` renders), not the mesh node's world space (which
/// glTF skinning ignores). skeleton_animated.glb's two spaces disagree
/// visibly: mesh-node-world y spans 0.36..2.22, bind-skinned y spans
/// -0.57..1.20 — the old bbox recentered/framed a box the skeleton
/// never occupies (feet cropped below frame).
#[test]
fn skinned_import_summary_bbox_is_in_bind_skinned_space() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/gltf/skeleton_animated.glb");
    let summary =
        super::gltf_load::gltf_import_summary(&path).expect("parse skeleton_animated.glb");
    assert!(
        summary.bbox_min[1] < 0.0 && summary.bbox_max[1] < 1.5,
        "bbox must be the bind-pose skinned one (y ≈ -0.57..1.20), got y {:.3}..{:.3} — \
         the mesh-node-world bbox (y ≈ 0.36..2.22) means the summary regressed to \
         treating the skinned mesh as static (BUG-205)",
        summary.bbox_min[1],
        summary.bbox_max[1]
    );
}



/// GRAPH_TOOLING_DESIGN D6: `assemble_import_graph`'s output must be
/// validated through `validate_def` before it reaches the project — the
/// assembler is code and has bugs. This proves the mechanism the
/// `manifold-app` importer hook relies on: a deliberately corrupted
/// assembler-style def (one node's `type_id` rewritten to a type the
/// registry doesn't know) fails `validate_def` with an issue naming that
/// node, never silently. Fixture-free — reuses the synthetic two-material
/// summary from `build_import_graph_groups_each_object_and_flattens_to_flat_wiring`.
#[test]
fn corrupted_assembler_output_fails_validation_naming_the_node() {
    use super::gltf_load::GltfMaterialInfo;
    use manifold_node_engine::validate::{ValidateKind, validate_def};

    let mat = |material_index: u32, name: &str, verts: u32, tex: Option<u32>| GltfMaterialInfo {
        material_index,
        name: Some(name.to_string()),
        base_color_factor: [0.5, 0.5, 0.5, 1.0],
        metallic: 0.0,
        roughness: 0.6,
        emissive: [0.0, 0.0, 0.0],
        alpha_mask: false,
        alpha_cutoff: 0.5,
        base_color_texture: tex,
        normal_texture: None,
        normal_scale: 1.0,
        mr_texture: None,
        occlusion_texture: None,
        occlusion_strength: 1.0,
        emissive_texture: None,
        emissive_strength: 1.0,
        ior: 1.5,
        specular_factor: 1.0,
        legacy_specular_factor: None,
        specular_color_factor: [1.0, 1.0, 1.0],
        specular_texture: None,
        specular_color_texture: None,
        base_color_uv_transform: super::gltf_load::IDENTITY_UV_TRANSFORM,
        normal_uv_transform: super::gltf_load::IDENTITY_UV_TRANSFORM,
        mr_uv_transform: super::gltf_load::IDENTITY_UV_TRANSFORM,
        occlusion_uv_transform: super::gltf_load::IDENTITY_UV_TRANSFORM,
        emissive_uv_transform: super::gltf_load::IDENTITY_UV_TRANSFORM,
        core_tex_coords: [0; 5],
        mr_texture_is_gloss_alpha: false,
        transmission_factor: 0.0,
        transmission_texture: None,
        diffuse_transmission_factor: 0.0,
        diffuse_transmission_color: [1.0, 1.0, 1.0],
        diffuse_transmission_texture: None,
        diffuse_transmission_color_texture: None,
        clearcoat_factor: 0.0,
        clearcoat_roughness_factor: 0.0,
        clearcoat_normal_scale: 1.0,
        clearcoat_texture: None,
        clearcoat_roughness_texture: None,
        clearcoat_normal_texture: None,
        sheen_color_factor: [0.0, 0.0, 0.0],
        sheen_roughness_factor: 0.0,
        sheen_color_texture: None,
        sheen_roughness_texture: None,
        iridescence_factor: 0.0,
        iridescence_ior: 1.3,
        iridescence_thickness_minimum: 100.0,
        iridescence_thickness_maximum: 400.0,
        iridescence_texture: None,
        iridescence_thickness_texture: None,
        anisotropy_strength: 0.0,
        anisotropy_rotation: 0.0,
        anisotropy_texture: None,
        dispersion: 0.0,
        volume_thickness_factor: 0.0,
        volume_attenuation_distance: manifold_node_engine::scene::material::VOLUME_ATTENUATION_DISTANCE_NO_ATTENUATION,
        volume_attenuation_color: [1.0, 1.0, 1.0],
        volume_thickness_texture: None,
        was_blend: false,
        vertex_color_varies: false,
        unlit: false,
        vertex_count: verts,
        base_color_sampler: super::gltf_load::GltfSamplerInfo::default(),
        normal_sampler: super::gltf_load::GltfSamplerInfo::default(),
        mr_sampler: super::gltf_load::GltfSamplerInfo::default(),
        occlusion_sampler: super::gltf_load::GltfSamplerInfo::default(),
        emissive_sampler: super::gltf_load::GltfSamplerInfo::default(),
        extension_maps: [manifold_node_engine::scene::material::MaterialMapInfo::default(); 14],
        animations: Vec::new(),
        skin: None,
        morph: None,
        rigid_multi_node: None,
        own_center: [0.0, 0.0, 0.0],
    };
    let summary = GltfImportSummary {
        materials: vec![mat(0, "Leaf", 1200, Some(0)), mat(1, "Bark", 800, None)],
        bbox_min: [-1.0, -1.0, -1.0],
        bbox_max: [1.0, 1.0, 1.0],
        camera_count: 0,
        default_material_vertex_count: 0,
        animations: Vec::new(),
        animation_report_lines: Vec::new(),
        extension_report_lines: Vec::new(),
        lights: Vec::new(),
        cameras: Vec::new(),
        camera_report_lines: Vec::new(),
        texture_dims: Vec::new(),
    };
    let path = std::path::Path::new("/tmp/synthetic_model.glb");
    let (mut def, _report) = build_import_graph(&summary, path).expect("build graph");

    // Corrupt exactly one node's type_id to something the registry has
    // never heard of — the "assembler wrote a typo" failure class.
    // The producer nodes live inside a group's body (see
    // `build_import_graph_groups_each_object_and_flattens_to_flat_wiring`
    // above), so search recursively rather than only the top level.
    fn find_pbr_material_mut(
        nodes: &mut [manifold_core::effect_graph_def::EffectGraphNode],
    ) -> Option<&mut manifold_core::effect_graph_def::EffectGraphNode> {
        for n in nodes {
            if n.type_id == "node.pbr_material" {
                return Some(n);
            }
            if let Some(group) = n.group.as_mut()
                && let Some(found) = find_pbr_material_mut(&mut group.nodes)
            {
                return Some(found);
            }
        }
        None
    }
    const CORRUPT_TYPE_ID: &str = "node.definitely_not_a_real_type";
    {
        let target = find_pbr_material_mut(&mut def.nodes)
            .expect("assembled graph has a pbr_material node to corrupt");
        target.type_id = CORRUPT_TYPE_ID.to_string();
    }

    let registry = PrimitiveRegistry::with_builtin();
    let device = manifold_node_engine::gpu::context::test_gpu_device("gltf_import tests");
    let report = validate_def(&def, &registry, ValidateKind::Generator, &device);

    assert!(
        !report.is_valid(),
        "a def with an unknown type_id must fail validate_def, not pass silently"
    );
    // Match by type_id, not doc_id: `validate_def` flattens groups before
    // classifying (this def's producers live inside a group), and
    // flattening renumbers doc ids via a fresh `IdAlloc` — the corrupted
    // node's ORIGINAL id doesn't survive to the error, but its (equally
    // corrupted) type_id does, and the reported node_id still names a
    // real node in the flattened def the error is about.
    assert!(
        report
            .errors
            .iter()
            .any(|issue| issue.type_id.as_deref() == Some(CORRUPT_TYPE_ID) && issue.node_id.is_some()),
        "expected an error naming a node with the corrupted type_id; got: {:?}",
        report.errors
    );
}

/// Regression for the glTF-import "unknown parameter 'pos_x_N'" load
/// failure, REWRITTEN for
/// SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md section 2 D3/P2 — the original
/// subject (a per-object param that only existed once `render_scene`
/// reconfigured to a higher object count) no longer exists: per-object
/// TRS is a `transform_n: Transform` PORT now, not a param. The
/// analogous regression is a PORT that only exists once `render_scene`
/// reconfigures — `transform_2`, which a naive loader could reject as
/// "unknown port" if it validated wires against the default 2-object
/// port surface instead of the reconfigured one. A model with >2
/// distinct materials (`objects >= 3`) is exactly the shape that used to
/// trip this; the azalea fixture has only 2 objects, so it never
/// exercised it — the coverage gap that let the original bug ship. This
/// synthetic 3-object def reproduces the shape with no large fixture and
/// must load + wire clean, proving reconfigure runs before wire/port
/// validation for the new port-based surface too.
#[test]
fn render_scene_with_three_objects_loads_object_port() {
    // SCENE_OBJECT_AND_PANEL_V2_DESIGN.md D4/P2: `transform_2` no
    // longer exists as a render_scene
    // port at all — `node.scene_object` owns `transform` now, and
    // render_scene's per-object surface is `object_k` only. The
    // analogous regression under the new shape: `object_2`, a port
    // that only exists once render_scene reconfigures to objects >= 3,
    // must load clean (reconfigure runs before port validation) —
    // same proof, new port.
    use manifold_node_engine::persistence::EffectGraphDefExt;

    let mut render = plain_node(0, "render", "node.render_scene", "render");
    render.params.insert("objects".to_string(), int(3));
    render.params.insert("lights".to_string(), int(1));

    let scene_object_2 = plain_node(1, "object_2", "node.scene_object", "object_2");

    let def = EffectGraphDef {
        version: 1,
        name: None,
        description: None,
        preset_metadata: None,
        scene_modifiers: Vec::new(),
        nodes: vec![render, scene_object_2],
        wires: vec![wire(1, "object", 0, "object_2")],
    };

    // Validate at the `into_graph` layer — the exact place the
    // "unknown parameter 'pos_x_2'" error was raised for the old shape.
    // (A full `from_def` additionally enforces generator-boundary
    // wiring, which this minimal two-node def deliberately omits — out
    // of scope for the port-surface regression.)
    let registry = PrimitiveRegistry::with_builtin();
    let graph = def.into_graph(&registry, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default()).expect(
        "render_scene with objects=3 must accept an object_2 wire at load \
         (reconfigure runs before port validation)",
    );
    assert!(
        graph.wires().iter().any(|w| w.to.1 == "object_2"),
        "the object_2 wire survives into the built graph"
    );
}






























/// CPU-only, in the default sweep: every imported object row is
/// scene_object-shaped (an `object_k` wire whose producer resolves,
/// through at most one group hop, to a `node.scene_object`), and a
/// fresh import needs no migration —
/// `migrate_scene_object_wires` must return `false` on the assembled
/// def, proving the importer emits the target shape natively rather
/// than relying on the load-time migration to paper over legacy wires.
#[test]
fn rosetta_stone_imports_scene_object_shaped_with_no_migration_needed() {
    let path = rosetta_stone_fixture_path();
    if !path.exists() {
        println!(
            "rosetta_stone_imports_scene_object_shaped_with_no_migration_needed: fixture not \
             found at {}, skipping",
            path.display()
        );
        return;
    }

    let (mut def, report) = assemble_import_graph(&path).expect("assemble the_rosetta_stone");
    println!("the_rosetta_stone import report: object_count={}", report.object_count);
    assert!(report.object_count >= 1, "the_rosetta_stone must import at least one object");

    let render = def
        .nodes
        .iter()
        .find(|n| n.type_id == "node.render_scene")
        .expect("render_scene node present");
    let render_id = render.id;
    let objects = report.object_count;
    for k in 0..objects {
        let object_port = format!("object_{k}");
        let producer_id = def
            .wires
            .iter()
            .find(|w| w.to_node == render_id && w.to_port == object_port)
            .unwrap_or_else(|| panic!("object {k}: no wire into render_scene's `{object_port}`"))
            .from_node;
        let producer = def.nodes.iter().find(|n| n.id == producer_id).expect("producer node exists");
        let is_scene_object_shaped = producer.type_id == "node.scene_object"
            || (producer.type_id == GROUP_TYPE_ID
                && producer
                    .group
                    .as_ref()
                    .is_some_and(|g| g.nodes.iter().any(|n| n.type_id == "node.scene_object")));
        assert!(
            is_scene_object_shaped,
            "object {k}: producer (type_id={}) must be node.scene_object or a group containing one",
            producer.type_id
        );
    }

    // Negative: zero legacy per-object port wires anywhere on render_scene.
    for prefix in [
        "mesh_", "material_", "transform_", "base_color_map_", "normal_map_", "mr_map_",
        "occlusion_map_", "emissive_map_", "instances_",
    ] {
        assert!(
            !def.wires.iter().any(|w| w.to_node == render_id && w.to_port.starts_with(prefix)),
            "the_rosetta_stone must wire zero legacy `{prefix}*` ports into render_scene"
        );
    }

    assert!(
        !manifold_core::scene_object_migration::migrate_scene_object_wires(&mut def),
        "a fresh the_rosetta_stone import must already be scene_object-shaped — \
         migrate_scene_object_wires should be a no-op"
    );
}










fn duck_fixture_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/gltf/khronos/Duck.glb")
}

/// BUG-d2qz: Khronos `Duck.glb` carries one embedded perspective camera.
/// The import must add it as an extra, unwired `node.free_camera` card at
/// its authored pose/FOV WITHOUT changing the default active camera — the
/// synthesized bbox-framed `node.orbit_camera` must still be the one wired
/// into `lens`/`render`. Cross-checks the free_camera node's stamped params
/// against an independent parse (`gltf_load::gltf_import_summary`), not
/// just against the assembler's own output.
#[test]
fn duck_import_adds_extra_camera_card_default_camera_unchanged() {
    let path = duck_fixture_path();
    if !path.exists() {
        println!("duck_import_adds_extra_camera_card_default_camera_unchanged: fixture not found, skipping");
        return;
    }

    let expected = super::gltf_load::gltf_import_summary(&path).expect("independent parse for the oracle");
    assert_eq!(expected.cameras.len(), 1, "Duck.glb's one camera is perspective");
    let expected_cam = &expected.cameras[0];

    let (def, report) = assemble_import_graph(&path).expect("assemble Duck");
    assert!(report.camera_synthesized, "the synthesized orbit camera is still the default");
    assert!(
        report.report_lines.iter().any(|l| l.contains("node.free_camera")),
        "report must name the imported camera and its fov: {:?}",
        report.report_lines
    );

    let orbit = def
        .nodes
        .iter()
        .find(|n| n.type_id == "node.orbit_camera")
        .expect("synthesized orbit camera node must still exist");
    let free_cams: Vec<_> = def.nodes.iter().filter(|n| n.type_id == "node.free_camera").collect();
    assert_eq!(free_cams.len(), 1, "Duck.glb carries exactly one embedded camera");

    let get_float = |node: &EffectGraphNode, param: &str| match node.params.get(param) {
        Some(SerializedParamValue::Float { value }) => *value,
        other => panic!("{param} missing or not a Float: {other:?}"),
    };
    assert!((get_float(free_cams[0], "pos_x") - expected_cam.pos[0]).abs() < 1e-4);
    assert!((get_float(free_cams[0], "pos_y") - expected_cam.pos[1]).abs() < 1e-4);
    assert!((get_float(free_cams[0], "pos_z") - expected_cam.pos[2]).abs() < 1e-4);
    assert!((get_float(free_cams[0], "yaw") - expected_cam.yaw).abs() < 1e-5);
    assert!((get_float(free_cams[0], "pitch") - expected_cam.pitch).abs() < 1e-5);
    assert!((get_float(free_cams[0], "roll") - expected_cam.roll).abs() < 1e-5);
    assert!((get_float(free_cams[0], "fov_y") - expected_cam.fov_y).abs() < 1e-5);

    // Default active camera is unchanged: `lens`'s `camera` input still
    // wires from the synthesized orbit camera, never from the imported one.
    let lens_id = def
        .nodes
        .iter()
        .find(|n| n.type_id == "node.camera_lens")
        .expect("lens node present")
        .id;
    let lens_camera_wire = def
        .wires
        .iter()
        .find(|w| w.to_node == lens_id && w.to_port == "camera")
        .expect("lens camera input wired");
    assert_eq!(
        lens_camera_wire.from_node, orbit.id,
        "lens must still read from the synthesized orbit camera, not the imported one"
    );
}

// -----------------------------------------------------------------
// BUG-upfq P1+P2 — scene-derived slider ranges at import
// -----------------------------------------------------------------

/// Build the import graph for a synthetic summary whose bbox is a cube of
/// half-extent `half_extent` centered at the origin. Uses the full importer
/// (`build_import_graph`), which is where the P1/P2 range override runs.
fn scene_import_with_half_extent(half_extent: f32) -> (manifold_core::effect_graph_def::EffectGraphDef, ImportReport) {
    let summary = GltfImportSummary {
        materials: vec![full_material(0, "Hero", 500)],
        bbox_min: [-half_extent, -half_extent, -half_extent],
        bbox_max: [half_extent, half_extent, half_extent],
        camera_count: 0,
        default_material_vertex_count: 0,
        animations: Vec::new(),
        animation_report_lines: Vec::new(),
        extension_report_lines: Vec::new(),
        lights: Vec::new(),
        cameras: Vec::new(),
        camera_report_lines: Vec::new(),
        texture_dims: Vec::new(),
    };
    let path = std::path::Path::new("/tmp/synthetic_scene_scale.glb");
    build_import_graph(&summary, path).expect("build scene-scale import graph")
}

/// Find the stamped card param for `node_id`+`param` on an import def.
fn stamped_card_param<'a>(
    meta: &'a manifold_core::effect_graph_def::PresetMetadata,
    node_id: u32,
    param: &str,
) -> &'a manifold_core::effect_graph_def::ParamSpecDef {
    let id = format!("{node_id}_{param}");
    meta.params.iter().find(|p| p.id == id).unwrap_or_else(|| {
        panic!("no stamped card param `{id}`; have: {:?}", meta.params.iter().map(|p| &p.id).collect::<Vec<_>>())
    })
}

/// BUG-upfq P1: an import's light / focus / light-range slider ranges must
/// scale with the bbox, never stay at the primitive's generic defaults.
/// Uses half-extents 5 and 50 — the import's fixed sun rig (5, 2, 3) sits
/// INSIDE both derived position bands (±2·radius), so neither band gets the
/// widen-for-default distortion and the pure derived ratio is exactly 10.
#[test]
fn scene_range_scales_with_bbox_size() {
    assert!(scene_derived_range_ratio_matches_bbox(5.0, 50.0));
}

/// The workhorse assertion used by the small-vs-large range test.
fn scene_derived_range_ratio_matches_bbox(small_half: f32, large_half: f32) -> bool {
    let (small_def, _) = scene_import_with_half_extent(small_half);
    let (large_def, _) = scene_import_with_half_extent(large_half);

    let small_meta = small_def.preset_metadata.as_ref().unwrap();
    let large_meta = large_def.preset_metadata.as_ref().unwrap();

    let small_sun_id = small_def.nodes.iter().find(|n| n.type_id == "node.light").unwrap().id;
    let large_sun_id = large_def.nodes.iter().find(|n| n.type_id == "node.light").unwrap().id;
    let small_lens_id = small_def.nodes.iter().find(|n| n.type_id == "node.camera_lens").unwrap().id;
    let large_lens_id = large_def.nodes.iter().find(|n| n.type_id == "node.camera_lens").unwrap().id;

    let small_r = super::scene_scale::SceneScale::from_bbox([-small_half; 3], [small_half; 3]).radius;
    let large_r = super::scene_scale::SceneScale::from_bbox([-large_half; 3], [large_half; 3]).radius;
    assert!((large_r / small_r - 10.0).abs() < 1e-3, "bbox radius ratio must be exactly 10");

    // Position slider (sun): ±2·radius, symmetric around 0.
    let small_pos = stamped_card_param(small_meta, small_sun_id, "pos_x");
    let large_pos = stamped_card_param(large_meta, large_sun_id, "pos_x");
    assert!((small_pos.min - -2.0 * small_r).abs() < 1e-3, "small sun pos band is ±2·radius, got min {}", small_pos.min);
    assert!((small_pos.max - 2.0 * small_r).abs() < 1e-3, "small sun pos band is ±2·radius, got max {}", small_pos.max);
    assert!((large_pos.max - 2.0 * large_r).abs() < 1e-3, "large sun pos band is ±2·radius, got max {}", large_pos.max);
    assert!((large_pos.min - -2.0 * large_r).abs() < 1e-3, "large sun pos band is ±2·radius, got min {}", large_pos.min);

    // Focus distance: 0..4·radius, one-sided.
    let small_focus = stamped_card_param(small_meta, small_lens_id, "focus_distance");
    let large_focus = stamped_card_param(large_meta, large_lens_id, "focus_distance");
    assert!(small_focus.min == 0.0, "focus band starts at the hyperfocal 0");
    assert!((small_focus.max - 4.0 * small_r).abs() < 1e-3, "small focus band is 0..4·radius, got max {}", small_focus.max);
    assert!((large_focus.max - 4.0 * large_r).abs() < 1e-3, "large focus band is 0..4·radius, got max {}", large_focus.max);
    // Focus default is the framing distance (~2.2·radius) — inside 0..4·radius.
    assert!(small_focus.default_value >= small_focus.min && small_focus.default_value <= small_focus.max);

    true
}

/// BUG-upfq P4: the unexposed ssao `radius` node default must keep tracking
/// 0.5·scene-radius on big scenes — the old absolute `clamp(0.01, 5.0)`
/// pinned every scene with radius >10 at the same 5.0 AO reach.
#[test]
fn scene_ssao_radius_default_tracks_scene_scale() {
    fn ssao_radius(def: &manifold_core::effect_graph_def::EffectGraphDef) -> f32 {
        def.nodes
            .iter()
            .filter_map(|n| n.group.as_ref())
            .find_map(|g| g.nodes.iter().find(|n| n.type_id == "node.ssao_gtao"))
            .and_then(|n| n.params.get("radius"))
            .map(|v| match v {
                SerializedParamValue::Float { value } => *value,
                other => panic!("ssao radius must be a stamped float, got {other:?}"),
            })
            .expect("import has a grouped ssao_gtao node")
    }

    let (small_def, _) = scene_import_with_half_extent(5.0);
    let (large_def, _) = scene_import_with_half_extent(50.0);
    let small_r = super::scene_scale::SceneScale::from_bbox([-5.0; 3], [5.0; 3]).radius;
    let large_r = super::scene_scale::SceneScale::from_bbox([-50.0; 3], [50.0; 3]).radius;

    let small = ssao_radius(&small_def);
    let large = ssao_radius(&large_def);
    assert!((small - 0.5 * small_r).abs() < 1e-3, "small default is 0.5·radius, got {small}");
    assert!((large - 0.5 * large_r).abs() < 1e-3, "large default is 0.5·radius, got {large}");
    assert!(large > 5.0, "radius-86 scene must not pin at the old 5.0 ceiling, got {large}");
}

/// BUG-upfq P1: the position slider default — the import-stamped value, not
/// the primitive's generic 0.0 — is never moved by the range override, only
/// the min/max metadata.
#[test]
fn scene_range_override_never_moves_defaults() {
    let (def, _) = scene_import_with_half_extent(5.0);
    let meta = def.preset_metadata.as_ref().unwrap();
    let sun_id = def.nodes.iter().find(|n| n.type_id == "node.light").unwrap().id;
    let sun_node = def.nodes.iter().find(|n| n.type_id == "node.light").unwrap();

    let pos = stamped_card_param(meta, sun_id, "pos_x");
    // Default stays whatever the import stamped on the sun node.
    let stamped_default = match sun_node.params.get("pos_x") {
        Some(SerializedParamValue::Float { value }) => *value,
        other => panic!("sun pos_x must be a stamped float, got {other:?}"),
    };
    assert_eq!(pos.default_value, stamped_default, "default value must match the node's stamped value, not the primitive default (BUG-303)");
}

/// BUG-upfq P1: the light attenuation (`range`) slider scales with the
/// bbox and keeps its primitive's 0.01 floor. The sun's shadow range is
/// seeded at 1.5·radius — the band must contain it.
#[test]
fn scene_light_range_scales_and_keeps_zero_one_floor() {
    let (def, _) = scene_import_with_half_extent(2.0);
    let meta = def.preset_metadata.as_ref().unwrap();
    let sun_id = def.nodes.iter().find(|n| n.type_id == "node.light").unwrap().id;
    let range_spec = stamped_card_param(meta, sun_id, "range");

    let r = super::scene_scale::SceneScale::from_bbox([-2.0; 3], [2.0; 3]).radius;
    assert!((range_spec.max - 4.0 * r).abs() < 1e-3, "light range max = 4·radius, got {}", range_spec.max);
    assert_eq!(range_spec.min, 0.01, "light range min stays at the primitive's 0.01 floor");
    // The sun's seeded shadow range (1.5·radius) must stay inside the slide band.
    assert!(range_spec.default_value >= range_spec.min && range_spec.default_value <= range_spec.max);
}

/// BUG-bdwd: an import's CoC node carries `world_to_mm = 1000/scene_radius`
/// (the scene reads as real meter-scale distances in the lens physics) and
/// no card slider is stamped for it — it is plumbing. The f_stop card
/// slider range is scene-derived (`0.5..max(32, 64·radius)`) so a migrated
/// project's `f_stop × R` values stay on-slider.
#[test]
fn import_stamps_world_to_mm_and_scene_f_stop_range() {
    let (def, _) = scene_import_with_half_extent(5.0);
    let meta = def.preset_metadata.as_ref().unwrap();
    let r = super::scene_scale::SceneScale::from_bbox([-5.0; 3], [5.0; 3]).radius;

    // The coc node lives inside the dof group; find it by type recursively.
    fn find_coc(
        nodes: &[manifold_core::effect_graph_def::EffectGraphNode],
    ) -> Option<&manifold_core::effect_graph_def::EffectGraphNode> {
        nodes
            .iter()
            .find(|n| n.type_id == "node.coc_from_depth")
            .or_else(|| {
                nodes
                    .iter()
                    .find_map(|n| n.group.as_ref().and_then(|g| find_coc(&g.nodes)))
            })
    }
    let coc = find_coc(&def.nodes)
        .expect("import must carry a coc_from_depth node (inside the dof group)");
    let w2m = match coc.params.get("world_to_mm") {
        Some(SerializedParamValue::Float { value }) => *value,
        other => panic!("coc world_to_mm must be a stamped float, got {other:?}"),
    };
    assert!(
        (w2m - 1000.0 / r).abs() < 1e-3,
        "coc world_to_mm must be 1000/radius, got {w2m}, radius {r}"
    );
    assert!(w2m <= 100_000.0, "coc world_to_mm must be floored at 100,000, got {w2m}");

    // No card slider surfaced for world_to_mm (plumbing, not a control).
    assert!(
        !meta
            .params
            .iter()
            .any(|p| p.id.starts_with(&coc.id.to_string()) && p.id.contains("world_to_mm")),
        "world_to_mm must not be stamped as a card slider"
    );

    // f_stop card range is scene-derived, one-sided top.
    let lens_list: Vec<_> = def.nodes.iter().filter(|n| n.type_id == "node.camera_lens").collect();
    let lens_id = lens_list[0].id;
    let fstop = stamped_card_param(meta, lens_id, "f_stop");
    assert_eq!(fstop.min, 0.5, "f_stop band keeps the photographic 0.5 floor");
    assert!(
        (fstop.max - (32.0f32).max(64.0 * r)).abs() < 1e-3,
        "f_stop band top must be max(32, 64·radius), got {} radius {r}",
        fstop.max
    );
    assert!(fstop.default_value >= fstop.min && fstop.default_value <= fstop.max);
}

/// BUG-upfq P1: merged object transform sliders get the scene-derived range
/// too (blanket per-object cards) — the merge path stamps them from the
/// incoming bbox, not the existing scene's.
#[test]
fn merge_stamps_scene_ranges_on_transform_cards() {
    let target = scene_def_with_bbox_half_extent(1.0);
    // Merge a 10×-larger asset — its transform pos slider must widen 10×.
    let summary = merge_summary(vec![full_material(0, "Big", 100)], 10.0);
    let path = std::path::Path::new("/tmp/synthetic_merge_scene_range.glb");
    let plan = merge_import_into_graph(&target, &summary, path).expect("merge");

    let transformed = plan
        .new_nodes
        .iter()
        .find_map(|n| n.group.as_ref())
        .and_then(|g| g.nodes.iter().find(|n| n.type_id == "node.transform_3d"))
        .expect("merged object has a transform_3d");
    let r = super::scene_scale::SceneScale::from_bbox([-10.0; 3], [10.0; 3]).radius;
    let pos = plan
        .new_card_params
        .iter()
        .find(|p| p.id == format!("{}_pos_x", transformed.id))
        .expect("merged object's transform pos_x card param");
    assert_eq!(pos.default_value, 0.0);
    assert!((pos.max - 2.0 * r).abs() < 1e-3, "merged transform pos slider must be ±2·incoming-radius, got {}", pos.max);
}

/// BUG-upfq follow-up: orbit camera `distance`/`near`/`far` slider ranges
/// must scale with the bbox radius. Small-bbox import (half-extent 0.137,
/// photoscan branch scale) and large-bbox import (half-extent 50) both get
/// distance max ≈ 6·radius, near max ≈ 2·radius, far max ≈ 20·radius.
#[test]
fn orbit_camera_ranges_scale_with_bbox_radius() {
    let (small_def, _) = scene_import_with_half_extent(0.137);
    let (large_def, _) = scene_import_with_half_extent(50.0);

    let small_meta = small_def.preset_metadata.as_ref().unwrap();
    let large_meta = large_def.preset_metadata.as_ref().unwrap();

    let small_cam_id = small_def.nodes.iter().find(|n| n.type_id == "node.orbit_camera").unwrap().id;
    let large_cam_id = large_def.nodes.iter().find(|n| n.type_id == "node.orbit_camera").unwrap().id;

    let small_r = super::scene_scale::SceneScale::from_bbox([-0.137; 3], [0.137; 3]).radius;
    let large_r = super::scene_scale::SceneScale::from_bbox([-50.0; 3], [50.0; 3]).radius;

    // distance band: 0.01..6·radius
    let small_dist = stamped_card_param(small_meta, small_cam_id, "distance");
    let large_dist = stamped_card_param(large_meta, large_cam_id, "distance");
    assert!((small_dist.max - 6.0 * small_r).abs() < 1e-3, "small orbit distance max = 6·radius, got {}", small_dist.max);
    assert!((large_dist.max - 6.0 * large_r).abs() < 1e-3, "large orbit distance max = 6·radius, got {}", large_dist.max);
    assert_eq!(small_dist.min, 0.01, "distance min stays at the primitive's 0.01 floor");

    // near band: 0.001..2·radius
    let small_near = stamped_card_param(small_meta, small_cam_id, "near");
    let large_near = stamped_card_param(large_meta, large_cam_id, "near");
    assert!((small_near.max - 2.0 * small_r).abs() < 1e-3, "small orbit near max = 2·radius, got {}", small_near.max);
    assert!((large_near.max - 2.0 * large_r).abs() < 1e-3, "large orbit near max = 2·radius, got {}", large_near.max);
    assert_eq!(small_near.min, 0.001, "near min stays at the primitive's 0.001 floor");

    // far band: 1.0..min(20·radius, 10_000), widened to contain the stamped
    // default (the import stamps far at max(DEFAULT_FAR=200, distance+1.5·r),
    // so small scenes get far=200 which is wider than 20·r).
    let small_far = stamped_card_param(small_meta, small_cam_id, "far");
    let large_far = stamped_card_param(large_meta, large_cam_id, "far");
    let small_far_expected = (20.0 * small_r).clamp(200.0, 10_000.0); // widened by stamped default
    let large_far_expected = (20.0 * large_r).min(10_000.0);
    assert!((small_far.max - small_far_expected).abs() < 1e-3, "small orbit far max = min(20·radius, 10000) widened to contain stamped default, got {}", small_far.max);
    assert!((large_far.max - large_far_expected).abs() < 1e-3, "large orbit far max = min(20·radius, 10000), got {}", large_far.max);
    assert_eq!(small_far.min, 1.0, "far min stays at the primitive's 1.0 floor");
}

/// BUG-upfq follow-up: a stamped default that falls outside the derived band
/// must be contained by the widen rule, never moved.
#[test]
fn orbit_camera_range_override_never_moves_defaults() {
    let (def, _) = scene_import_with_half_extent(0.137);
    let meta = def.preset_metadata.as_ref().unwrap();
    let cam_id = def.nodes.iter().find(|n| n.type_id == "node.orbit_camera").unwrap();
    let cam_node = &cam_id;
    let cam_id = cam_id.id;

    for param in &["distance", "near", "far"] {
        let spec = stamped_card_param(meta, cam_id, param);
        let stamped_default = match cam_node.params.get(*param) {
            Some(SerializedParamValue::Float { value }) => *value,
            other => panic!("{param} must be a stamped float, got {other:?}"),
        };
        assert_eq!(spec.default_value, stamped_default, "{param}: default value must match the node's stamped value");
        assert!(spec.default_value >= spec.min, "{param}: default {} must be >= min {}", spec.default_value, spec.min);
        assert!(spec.default_value <= spec.max, "{param}: default {} must be <= max {}", spec.default_value, spec.max);
    }
}

/// KHR_lights_punctual assembly keeps authored photometric values and cone
/// metadata in the graph. An authored light also replaces the synthetic sun
/// slot, so the render_scene port count and wire target stay one-to-one.
#[test]
fn authored_spot_light_preserves_raw_intensity_cone_and_physical_range() {
    use super::gltf_load::{GltfLightKind, GltfPunctualLight};

    let summary = GltfImportSummary {
        materials: vec![full_material(0, "SpotMaterial", 3)],
        bbox_min: [-1.0, -1.0, -1.0],
        bbox_max: [1.0, 1.0, 1.0],
        camera_count: 0,
        default_material_vertex_count: 0,
        animations: Vec::new(),
        animation_report_lines: Vec::new(),
        extension_report_lines: Vec::new(),
        lights: vec![GltfPunctualLight {
            name: Some("Key Spot".to_string()),
            kind: GltfLightKind::Spot {
                inner_cone_angle: 0.2,
                outer_cone_angle: 0.5,
            },
            color: [0.25, 0.5, 0.75],
            intensity: 2400.0,
            range: Some(12.5),
            world_pos: [1.0, 2.0, 3.0],
            world_forward: [0.0, 0.0, -1.0],
        }],
        cameras: Vec::new(),
        camera_report_lines: Vec::new(),
        texture_dims: Vec::new(),
    };

    let path = std::path::Path::new("/tmp/synthetic_authored_spot.glb");
    let (def, report) = build_import_graph(&summary, path).expect("build authored spot graph");
    let render = def
        .nodes
        .iter()
        .find(|n| n.type_id == "node.render_scene")
        .expect("render_scene node");
    assert_eq!(render.params.get("lights"), Some(&int(1)));
    assert!(
        !def.nodes.iter().any(|n| n.node_id == "sun"),
        "authored lights must not get a synthetic sun"
    );

    let light = def
        .nodes
        .iter()
        .find(|n| n.node_id == "light_0")
        .expect("authored light_0 node");
    assert_eq!(light.params.get("mode"), Some(&enum_val(2)));
    assert_eq!(light.params.get("falloff"), Some(&enum_val(1)));
    assert_eq!(light.params.get("intensity"), Some(&float(2400.0)));
    assert_eq!(light.params.get("range"), Some(&float(12.5)));
    assert_eq!(light.params.get("inner_cone_angle"), Some(&float(0.2)));
    assert_eq!(light.params.get("outer_cone_angle"), Some(&float(0.5)));
    assert!(def.wires.iter().any(|wire| {
        wire.from_node == light.id && wire.to_node == render.id && wire.to_port == "light_0"
    }));
    assert!(report.report_lines.iter().any(|line| {
        line.contains("raw") && line.contains("candela") && line.contains("spot cone")
    }));
}
