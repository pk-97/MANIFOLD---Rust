use super::*;
use super::assembly::*;
use super::merge::*;
use super::object_group::*;
use super::scene::*;
use crate::node_graph::PrimitiveRegistry;
use crate::preset_runtime::PresetRuntime;
use manifold_core::effect_graph_def::BindingTarget;

fn azalea_fixture_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/gltf/cc0__oomurasaki_azalea_r._x_pulchrum.glb")
}

/// Build a minimal, valid `.glb` with `n` distinct materials, each owning
/// exactly one triangle (so every material has geometry and therefore
/// counts toward `ImportReport::material_count`) — hand-rolled binary
/// container (12-byte header + JSON chunk + BIN chunk, no external
/// `.bin`/textures, no `uri` on the buffer so it resolves to the BIN
/// chunk per spec §Binary glTF). GLB_CONFORMANCE_DESIGN.md G-P2: proves
/// the FULL production parse path (`gltf::import` → `gltf_import_summary`
/// → `build_import_graph`) imports every material 1:1, not just the
/// graph-assembly half a synthetic [`GltfImportSummary`] would exercise.
/// Written to the OS temp dir, not committed — a builder fn, not a
/// binary asset (the phase brief's explicit call).
fn write_synthetic_multimaterial_glb(n: usize) -> std::path::PathBuf {
    let mut accessors = Vec::with_capacity(n);
    let mut buffer_views = Vec::with_capacity(n);
    let mut materials = Vec::with_capacity(n);
    let mut primitives = Vec::with_capacity(n);
    let mut bin = Vec::with_capacity(n * 36);

    for i in 0..n {
        // One triangle per material, spread along X so no two overlap —
        // cosmetic, but keeps bbox/normal math non-degenerate.
        let ox = i as f32 * 2.0;
        let tri: [[f32; 3]; 3] = [[ox, 0.0, 0.0], [ox + 1.0, 0.0, 0.0], [ox, 1.0, 0.0]];
        for v in &tri {
            for c in v {
                bin.extend_from_slice(&c.to_le_bytes());
            }
        }
        let byte_offset = i * 36;
        buffer_views.push(serde_json::json!({
            "buffer": 0,
            "byteOffset": byte_offset,
            "byteLength": 36,
        }));
        accessors.push(serde_json::json!({
            "bufferView": i,
            "componentType": 5126, // FLOAT
            "count": 3,
            "type": "VEC3",
            "min": [ox, 0.0, 0.0],
            "max": [ox + 1.0, 1.0, 0.0],
        }));
        materials.push(serde_json::json!({
            "name": format!("Mat{i}"),
            "pbrMetallicRoughness": { "baseColorFactor": [0.5, 0.5, 0.5, 1.0] },
        }));
        // Mode omitted — glTF's default primitive mode is 4 (TRIANGLES).
        primitives.push(serde_json::json!({
            "attributes": { "POSITION": i },
            "material": i,
        }));
    }

    let doc = serde_json::json!({
        "asset": { "version": "2.0" },
        "scene": 0,
        "scenes": [{ "nodes": [0] }],
        "nodes": [{ "mesh": 0 }],
        "meshes": [{ "primitives": primitives }],
        "accessors": accessors,
        "bufferViews": buffer_views,
        "materials": materials,
        "buffers": [{ "byteLength": bin.len() }],
    });
    let json_bytes = serde_json::to_vec(&doc).expect("serialize synthetic glTF JSON");

    // GLB container: header + JSON chunk (space-padded to 4 bytes) + BIN
    // chunk (zero-padded to 4 bytes). Chunk type magics per the Binary
    // glTF spec: 0x4E4F534A = "JSON", 0x004E4942 = "BIN\0".
    let mut json_padded = json_bytes;
    while !json_padded.len().is_multiple_of(4) {
        json_padded.push(b' ');
    }
    let mut bin_padded = bin;
    while !bin_padded.len().is_multiple_of(4) {
        bin_padded.push(0);
    }
    let total_len = 12 + 8 + json_padded.len() + 8 + bin_padded.len();

    let mut glb = Vec::with_capacity(total_len);
    glb.extend_from_slice(b"glTF");
    glb.extend_from_slice(&2u32.to_le_bytes());
    glb.extend_from_slice(&(total_len as u32).to_le_bytes());
    glb.extend_from_slice(&(json_padded.len() as u32).to_le_bytes());
    glb.extend_from_slice(b"JSON");
    glb.extend_from_slice(&json_padded);
    glb.extend_from_slice(&(bin_padded.len() as u32).to_le_bytes());
    glb.extend_from_slice(b"BIN\0");
    glb.extend_from_slice(&bin_padded);

    let path = std::env::temp_dir().join(format!(
        "manifold_synthetic_{n}mat_{}_{}.glb",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::write(&path, &glb).expect("write synthetic glb to temp dir");
    path
}

/// GLB_CONFORMANCE_DESIGN.md D4 / G-P2's named gate test: a 100-material
/// synthetic asset — well past the old (dead) 64-object cap, well under
/// `OBJECT_SAFETY_MAX` (1024) — imports every single material 1:1, no
/// truncation. P1 exposes every material's params, so every object gets
/// card sliders now.
#[test]
fn over_cap_asset_imports_one_to_one() {
    let path = write_synthetic_multimaterial_glb(100);
    let (def, report) = assemble_import_graph(&path).expect("assemble 100-material synthetic glb");
    std::fs::remove_file(&path).ok();

    assert_eq!(report.material_count, 100, "all 100 materials have geometry");
    assert_eq!(
        report.object_count, 100,
        "import is 1:1 — object_count must equal material_count, no truncation (D4)"
    );

    // Every material got its own render_scene wire — object_0..object_99
    // all present, nothing dropped past the old 64-object boundary
    // (SCENE_OBJECT_AND_PANEL_V2_DESIGN D4: render_scene's per-object
    // surface is `object_{i}` only, post-P2).
    for k in 0..100 {
        assert!(
            def.wires.iter().any(|w| w.to_port == format!("object_{k}")),
            "material {k} (past the old 64-object cap) must still wire object_{k}"
        );
    }
    // render_scene's own `objects` param must reflect the true count,
    // not a clamped one.
    let render_node = def
        .nodes
        .iter()
        .find(|n| n.type_id == "node.render_scene")
        .expect("assembled graph has a render_scene node");
    assert_eq!(
        render_node.params.get("objects"),
        Some(&int(100)),
        "render_scene.objects must be the true unclamped count"
    );

    // Structural gate: the assembled graph — 100 objects, well past the
    // dead 64-object UI cap — must still compile through the real
    // registry (catches a bad port/wire before any GPU proof).
    let registry = PrimitiveRegistry::with_builtin();
    PresetRuntime::from_def(def, &registry, None)
        .expect("100-object import graph must compile through PresetRuntime::from_def");
}

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

/// Build a minimal, valid `.glb` with ONE triangle primitive that has NO
/// `material` key at all — glTF's implicit default material
/// (GLB_XFAIL_BURNDOWN_DESIGN.md D4, BUG-171). No `materials` array in
/// the document at all, matching a real asset like `BoxVertexColors.glb`
/// that carries geometry but declares zero materials. Same hand-rolled
/// binary-container shape as `write_synthetic_multimaterial_glb`.
fn write_synthetic_default_material_glb() -> std::path::PathBuf {
    let tri: [[f32; 3]; 3] = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
    let mut bin = Vec::with_capacity(36);
    for v in &tri {
        for c in v {
            bin.extend_from_slice(&c.to_le_bytes());
        }
    }

    let doc = serde_json::json!({
        "asset": { "version": "2.0" },
        "scene": 0,
        "scenes": [{ "nodes": [0] }],
        "nodes": [{ "mesh": 0 }],
        // No "material" key on the primitive — glTF's implicit default
        // material. No "materials" array at all, matching a real asset
        // that declares zero materials.
        "meshes": [{ "primitives": [{ "attributes": { "POSITION": 0 } }] }],
        "accessors": [{
            "bufferView": 0,
            "componentType": 5126,
            "count": 3,
            "type": "VEC3",
            "min": [0.0, 0.0, 0.0],
            "max": [1.0, 1.0, 0.0],
        }],
        "bufferViews": [{ "buffer": 0, "byteOffset": 0, "byteLength": 36 }],
        "buffers": [{ "byteLength": bin.len() }],
    });
    let json_bytes = serde_json::to_vec(&doc).expect("serialize synthetic glTF JSON");

    let mut json_padded = json_bytes;
    while !json_padded.len().is_multiple_of(4) {
        json_padded.push(b' ');
    }
    let mut bin_padded = bin;
    while !bin_padded.len().is_multiple_of(4) {
        bin_padded.push(0);
    }
    let total_len = 12 + 8 + json_padded.len() + 8 + bin_padded.len();

    let mut glb = Vec::with_capacity(total_len);
    glb.extend_from_slice(b"glTF");
    glb.extend_from_slice(&2u32.to_le_bytes());
    glb.extend_from_slice(&(total_len as u32).to_le_bytes());
    glb.extend_from_slice(&(json_padded.len() as u32).to_le_bytes());
    glb.extend_from_slice(b"JSON");
    glb.extend_from_slice(&json_padded);
    glb.extend_from_slice(&(bin_padded.len() as u32).to_le_bytes());
    glb.extend_from_slice(b"BIN\0");
    glb.extend_from_slice(&bin_padded);

    let path = std::env::temp_dir().join(format!(
        "manifold_synthetic_defaultmat_{}_{}.glb",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::write(&path, &glb).expect("write synthetic glb to temp dir");
    path
}

/// GLB_XFAIL_BURNDOWN_DESIGN.md D4 (BUG-171) / §4 invariant ("no silent
/// geometry drop"): a hand-rolled glb whose ONLY primitive has no
/// material must import as exactly ONE object — the synthetic
/// default-material entry — through the FULL production parse path
/// (`gltf_import_summary` → `assemble_import_graph`), not just the
/// graph-assembly half a synthetic `GltfImportSummary` would exercise.
/// Before D4 this asset errored "no materials with geometry — nothing
/// to import" (the geometry was silently uncounted).
#[test]
fn default_material_primitive_imports_as_one_object() {
    let path = write_synthetic_default_material_glb();
    let (def, report) = assemble_import_graph(&path).expect(
        "a materialless-primitive glb must import via the D4 synthetic default material",
    );
    std::fs::remove_file(&path).ok();

    assert_eq!(
        report.object_count, 1,
        "one materialless primitive must yield exactly one render_scene object (D4)"
    );
    assert_eq!(report.default_material_vertex_count, 3, "the one triangle's 3 vertices");

    // The synthetic object's mesh source must carry the D4 sentinel
    // param, not a real (or the colliding "unset") material_index.
    // gltf_mesh_source lives inside the object's group box until load
    // time flattening — same pattern every other nested-node assertion
    // in this test module uses.
    let flat = manifold_core::flatten::flatten_groups(&def).expect("flatten import graph");
    let mesh_node = flat
        .nodes
        .iter()
        .find(|n| n.type_id == "node.gltf_mesh_source")
        .expect("flattened graph has a gltf_mesh_source node");
    assert_eq!(
        mesh_node.params.get("material_index"),
        Some(&int(super::gltf_load::DEFAULT_MATERIAL_MESH_PARAM)),
        "the synthetic object's mesh source must select via the D4 sentinel, not a real \
         material index or the -1 'unset' value"
    );

    // Structural gate: compiles through the real registry.
    let registry = PrimitiveRegistry::with_builtin();
    PresetRuntime::from_def(def, &registry, None)
        .expect("materialless-primitive import graph must compile through PresetRuntime::from_def");
}

/// P1: every material's `color_a` (Opacity) is exposed from the primitive
/// ParamDef, not curated to glass/top-16. Wiring stays 1:1 for all objects
/// and survives a JSON round trip.
#[test]
fn all_materials_expose_opacity_and_wiring_survives_round_trip() {
    let n = 20;
    let materials: Vec<_> = (0..n)
        .map(|k| {
            let mut m = full_material(k as u32, &format!("Glass{k}"), (n - k) as u32 * 100);
            m.was_blend = true;
            m.transmission_factor = 0.5;
            m
        })
        .collect();
    let summary = GltfImportSummary {
        materials,
        bbox_min: [-1.0, -1.0, -1.0],
        bbox_max: [1.0, 1.0, 1.0],
        camera_count: 0,
        default_material_vertex_count: 0,
        animations: Vec::new(),
        animation_report_lines: Vec::new(),
        extension_report_lines: Vec::new(),
    };
    let path = std::path::Path::new("/tmp/synthetic_curation_round_trip.glb");
    let (def, report) = build_import_graph(&summary, path).expect("build 20-object graph");
    assert_eq!(report.object_count, 20, "1:1 — every object gets full wiring");

    let json = serde_json::to_string(&def).expect("serialize EffectGraphDef");
    let reloaded: EffectGraphDef = serde_json::from_str(&json).expect("deserialize EffectGraphDef");
    assert_eq!(def, reloaded, "round trip must be byte-for-byte structurally identical");

    for (def, label) in [(&def, "pre-reload"), (&reloaded, "post-reload")] {
        let meta = def.preset_metadata.as_ref().unwrap_or_else(|| panic!("{label}: v2 metadata"));
        // Every object gets an Opacity slider from the material's ParamDef.
        let opacity_count = meta.params.iter().filter(|p| p.name == "Opacity").count();
        assert_eq!(
            opacity_count, n,
            "{label}: every object gets an Opacity slider"
        );

        // Full graph wiring survives for every object.
        let flat = manifold_core::flatten::flatten_groups(def)
            .unwrap_or_else(|e| panic!("{label}: flatten failed: {e}"));
        let render = flat.nodes.iter().find(|n| n.type_id == "node.render_scene").unwrap();
        for k in 0..n {
            assert!(
                flat.wires.iter().any(|w| w.to_node == render.id && w.to_port == format!("object_{k}")),
                "{label}: object {k} must still wire object_{k}"
            );
        }
    }

    let registry = PrimitiveRegistry::with_builtin();
    PresetRuntime::from_def(reloaded, &registry, None)
        .expect("reloaded 20-object import graph must build through PresetRuntime::from_def");
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
    assert_eq!(report.textures_wired, 2, "both azalea materials carry a base-color texture");
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
    let scene_ambient = meta.params.iter().find(|p| p.id == "scene_ambient").unwrap();
    assert_eq!(scene_ambient.default_value, 0.0, "no ambient fill by default");
    // The Ambient card fans out to every material's `ambient` param.
    let ambient_targets: std::collections::HashSet<String> = meta
        .bindings
        .iter()
        .filter(|b| b.id == "scene_ambient")
        .filter_map(|b| match &b.target {
            BindingTarget::Node { node_id, param } => {
                assert_eq!(param, "ambient");
                Some(node_id.as_str().to_string())
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        ambient_targets,
        ["mat_0", "mat_1"].into_iter().map(String::from).collect(),
        "shared Ambient drives both azalea materials"
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

    // GTAO and the lens are wired into the spine. No motion blur
    // (BUG-136 + fusion cost, see the removal comment in
    // `build_import_graph`), no depth-of-field group (Peter, 2026-07-15:
    // buggy visuals), no atmosphere node (fog + god rays removed,
    // Peter 2026-07-15).
    for present in ["node.ssao_gtao", "node.bilateral_blur", "node.camera_lens"] {
        assert!(
            def.nodes.iter().any(|n| n.type_id == present)
                || def.nodes.iter().filter_map(|n| n.group.as_ref()).any(|g| {
                    g.nodes.iter().any(|inner| inner.type_id == present)
                }),
            "imported graph must carry `{present}`"
        );
    }
    // `node.motion_blur` was removed (BUG-136: no visible effect live,
    // despite correct wiring — see the removal comment above);
    // `node.variable_blur` was P4's superseded DoF blur stage; and the
    // DoF chain itself (`coc_from_depth`/`coc_dilate`/`bokeh_gather`)
    // was removed 2026-07-15 for buggy visuals. None is reintroduced.
    for absent in [
        "node.motion_blur",
        "node.variable_blur",
        "node.coc_from_depth",
        "node.coc_dilate",
        "node.bokeh_gather",
        "node.atmosphere",
    ] {
        assert!(
            !def.nodes.iter().any(|n| n.type_id == absent)
                && !def.nodes.iter().filter_map(|n| n.group.as_ref()).any(|g| {
                    g.nodes.iter().any(|inner| inner.type_id == absent)
                }),
            "`{absent}` should not be in the imported graph"
        );
    }
    // No SSAO/DoF card sliders — the underlying nodes keep their defaults,
    // they're just not auto-exposed on the card.
    for gone_prefix in ["ssao_", "dof_"] {
        assert!(
            !meta.params.iter().any(|p| p.id.starts_with(gone_prefix)),
            "no card param should start with `{gone_prefix}`"
        );
    }
    for gone in [
        "dof_radius", "motion_blur_px", "mb_shutter", "ssao_bias", "fog_density", "god_rays",
    ] {
        assert!(
            !meta.params.iter().any(|p| p.id == gone),
            "unused card param id `{gone}` should not exist"
        );
    }
    // Shadow type default comes from the primitive ParamDef (Soft = 1).
    // The importer still seeds the node param to Hard (0) for the crisp
    // dramatic look, but the card slider metadata follows ParamDef.
    let sun_shadow_id = format!("{sun_id}_shadow_softness");
    let sun_shadow = meta.params.iter().find(|p| p.id == sun_shadow_id).unwrap();
    assert_eq!(sun_shadow.default_value, 1.0, "shadow type ParamDef default is Soft");
}

/// Structural gate (fast, no GPU): the assembled azalea graph must
/// compile through the real `PrimitiveRegistry` — every node type_id
/// resolves, every wire's ports exist and type-check, both boundary
/// nodes are present and wired correctly. Catches a bad port/param
/// name or a missing wire before the (slow, `#[ignore]`d) GPU proof.
#[test]
fn assembled_azalea_graph_passes_from_def_structural_check() {
    let path = azalea_fixture_path();
    if !path.exists() {
        println!(
            "assembled_azalea_graph_passes_from_def_structural_check: fixture not found at {}, skipping",
            path.display()
        );
        return;
    }

    let (def, _report) = assemble_import_graph(&path).expect("assemble azalea");
    let registry = PrimitiveRegistry::with_builtin();
    PresetRuntime::from_def(def, &registry, None)
        .expect("assembled azalea graph must compile through PresetRuntime::from_def");
}

/// Grouping proof, fixture-free: each object's producers must live inside one
/// named, tinted group, and the grouped graph must flatten to the SAME flat
/// wiring the ungrouped assembler produced (compared in node_id space) with
/// every card/string binding still resolving. Uses a synthetic two-material
/// summary — one textured, one not — so it needs no `.glb` on disk.
#[test]
fn build_import_graph_groups_each_object_and_flattens_to_flat_wiring() {
    use super::gltf_load::GltfMaterialInfo;
    use manifold_core::effect_graph_def::GROUP_TYPE_ID;
    use manifold_core::flatten::flatten_groups;

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
        specular_color_factor: [1.0, 1.0, 1.0],
        specular_texture: None,
        specular_color_texture: None,
        base_color_uv_transform: super::gltf_load::IDENTITY_UV_TRANSFORM,
        normal_uv_transform: super::gltf_load::IDENTITY_UV_TRANSFORM,
        mr_uv_transform: super::gltf_load::IDENTITY_UV_TRANSFORM,
        occlusion_uv_transform: super::gltf_load::IDENTITY_UV_TRANSFORM,
        emissive_uv_transform: super::gltf_load::IDENTITY_UV_TRANSFORM,
        uv_tex_coord_override: false,
        mr_texture_is_gloss_alpha: false,
        transmission_factor: 0.0,
        transmission_texture: None,
        clearcoat_factor: 0.0,
        clearcoat_roughness_factor: 0.0,
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
        volume_attenuation_distance: super::gltf_load::VOLUME_ATTENUATION_DISTANCE_NO_ATTENUATION,
        volume_attenuation_color: [1.0, 1.0, 1.0],
        volume_thickness_texture: None,
        was_blend: false,
        vertex_count: verts,
        base_color_sampler: super::gltf_load::GltfSamplerInfo::default(),
        normal_sampler: super::gltf_load::GltfSamplerInfo::default(),
        mr_sampler: super::gltf_load::GltfSamplerInfo::default(),
        occlusion_sampler: super::gltf_load::GltfSamplerInfo::default(),
        emissive_sampler: super::gltf_load::GltfSamplerInfo::default(),
        animations: Vec::new(),
        skin: None,
        morph: None,
        rigid_multi_node: None,
        own_center: [0.0, 0.0, 0.0],
    };
    let summary = GltfImportSummary {
        // Largest-vertex-first sort makes object 0 = Leaf (textured), 1 = Bark.
        materials: vec![mat(0, "Leaf", 1200, Some(0)), mat(1, "Bark", 800, None)],
        bbox_min: [-1.0, -1.0, -1.0],
        bbox_max: [1.0, 1.0, 1.0],
        camera_count: 0,
        default_material_vertex_count: 0,
        animations: Vec::new(),
        animation_report_lines: Vec::new(),
        extension_report_lines: Vec::new(),
    };
    let path = std::path::Path::new("/tmp/synthetic_model.glb");
    let (def, report) = build_import_graph(&summary, path).expect("build grouped graph");
    assert_eq!(report.object_count, 2);
    assert_eq!(report.textures_wired, 1);

    // Top level: two per-object group boxes PLUS the "ao" presentation
    // group, no bare producer
    // nodes.
    let groups: Vec<_> = def.nodes.iter().filter(|n| n.type_id == GROUP_TYPE_ID).collect();
    assert_eq!(groups.len(), 3, "2 object groups + ao");
    assert!(groups.iter().all(|g| g.group.is_some()));
    for bare in [
        "node.gltf_mesh_source",
        "node.pbr_material",
        "node.gltf_texture_source",
        "node.transform_3d",
    ] {
        assert!(
            !def.nodes.iter().any(|n| n.type_id == bare),
            "producer `{bare}` must live inside a group, not at the top level"
        );
    }
    // Only the per-object groups carry a tint (CINEMATIC_POST's ao
    // group is an untinted presentation box, not per-object identity).
    let object_groups: Vec<_> = groups
        .iter()
        .filter(|g| g.group.as_ref().unwrap().tint.is_some())
        .copied()
        .collect();
    assert_eq!(object_groups.len(), 2, "one tinted group per object");
    // Every object group's interface declares a single `object` output
    // (SCENE_OBJECT_AND_PANEL_V2_DESIGN D1/D3 — the transform/material/
    // mesh triplet is bound INSIDE the group by `node.scene_object` now,
    // not exposed as separate interface ports).
    for g in &object_groups {
        let outputs = &g.group.as_ref().unwrap().interface.outputs;
        assert!(
            outputs.iter().any(|o| o.name == "object" && o.port_type == "Object"),
            "every object group exposes a single object output"
        );
    }
    // Distinct tints per object group (legibility).
    let tints: Vec<_> = object_groups.iter().filter_map(|g| g.group.as_ref().unwrap().tint).collect();
    assert_eq!(tints.len(), 2, "every object group gets a tint");
    assert_ne!(tints[0], tints[1], "each object group gets its own tint");

    // Flatten and prove the runtime sees the same flat wiring the ungrouped
    // assembler produced — in node_id space (survives id renumbering + handle
    // prefixing).
    let flat = flatten_groups(&def).expect("grouped import graph flattens");
    let id_of = |doc_id: u32| -> String {
        flat.nodes
            .iter()
            .find(|n| n.id == doc_id)
            .map(|n| n.node_id.as_str().to_string())
            .unwrap_or_default()
    };
    let conn: std::collections::HashSet<(String, String, String, String)> = flat
        .wires
        .iter()
        .map(|w| (id_of(w.from_node), w.from_port.clone(), id_of(w.to_node), w.to_port.clone()))
        .collect();
    // Internal wires into each object's `node.scene_object` bind node —
    // survive flattening in-scope (SCENE_OBJECT_AND_PANEL_V2_DESIGN
    // D1/D3: the mesh/material/transform/map triplet binds to
    // scene_object now, not directly to render_scene).
    for (from_id, from_port, to_id, to_port) in [
        ("mesh_0", "vertices", "object_0_bind", "vertices"),
        ("mat_0", "out", "object_0_bind", "material"),
        ("tex_0", "out", "object_0_bind", "base_color_map"),
        ("mesh_1", "vertices", "object_1_bind", "vertices"),
        ("mat_1", "out", "object_1_bind", "material"),
        ("transform_0", "transform", "object_0_bind", "transform"),
        ("transform_1", "transform", "object_1_bind", "transform"),
    ] {
        assert!(
            conn.contains(&(
                from_id.to_string(),
                from_port.to_string(),
                to_id.to_string(),
                to_port.to_string(),
            )),
            "flattened graph missing wire {from_id}.{from_port} -> {to_id}.{to_port}"
        );
    }
    // Each object's single `object` output reaches render_scene's
    // `object_{k}` port (D4 — render_scene's v2 per-object surface).
    for (from_id, to_port) in [("object_0_bind", "object_0"), ("object_1_bind", "object_1")] {
        assert!(
            conn.contains(&(
                from_id.to_string(),
                "object".to_string(),
                "render".to_string(),
                to_port.to_string(),
            )),
            "flattened graph missing wire {from_id}.object -> render.{to_port}"
        );
    }
    // Bark has no texture — no base_color_map wire into its scene_object.
    assert!(
        !conn.iter().any(|(_, _, to, tp)| to == "object_1_bind" && tp == "base_color_map"),
        "untextured object must not wire a base-color map"
    );
    // No group / boundary nodes survive flattening.
    assert!(
        !flat.nodes.iter().any(|n| {
            n.type_id == GROUP_TYPE_ID || n.type_id.contains("group_output") || n.type_id.contains("group_input")
        }),
        "flattened graph must contain no group or boundary nodes"
    );

    // Every card + string binding still targets a node_id that exists post-flatten.
    let meta = def.preset_metadata.as_ref().expect("v2 metadata");
    let flat_ids: std::collections::HashSet<&str> =
        flat.nodes.iter().map(|n| n.node_id.as_str()).collect();
    for b in &meta.bindings {
        if let BindingTarget::Node { node_id, .. } = &b.target {
            assert!(
                flat_ids.contains(node_id.as_str()),
                "card binding `{}` targets `{}`, gone after flatten",
                b.id,
                node_id.as_str()
            );
        }
    }
    for b in &meta.string_bindings {
        if let BindingTarget::Node { node_id, .. } = &b.target {
            assert!(
                flat_ids.contains(node_id.as_str()),
                "string binding targets `{}`, gone after flatten",
                node_id.as_str()
            );
        }
    }

    // The editor's own data path (`GraphSnapshot::from_def`, which routes a
    // grouped def through the group-preserving structural snapshot) must show
    // all four groups as navigable boxes — each carrying its inner producers —
    // not a flat wall of nodes. This is the legibility payoff, verified at the
    // snapshot layer (the pixels still want Peter's eyes on a real model).
    let snap = crate::node_graph::GraphSnapshot::from_def(&def)
        .expect("editor snapshot builds from the grouped def");
    let snap_groups: Vec<_> =
        snap.nodes.iter().filter(|n| n.group.is_some()).collect();
    assert_eq!(snap_groups.len(), 3, "editor snapshot shows 2 object + ao group boxes");
    let snap_object_groups: Vec<_> = snap_groups
        .iter()
        .filter(|g| g.group.as_ref().unwrap().nodes.iter().any(|inner| inner.type_id == "node.pbr_material"))
        .collect();
    assert_eq!(snap_object_groups.len(), 2, "exactly the two object groups carry a material node");

    // Finally, it must build through the production loader (which flattens).
    let registry = PrimitiveRegistry::with_builtin();
    PresetRuntime::from_def(def, &registry, None)
        .expect("grouped import graph must build through PresetRuntime::from_def");
}

/// A synthetic [`GltfMaterialInfo`] carrying every texture kind F-P4
/// wires, with independent test-controlled fields for the three
/// report-only features (clearcoat/transmission/BLEND). Defaults mirror
/// a "fully-mapped, nothing extra" material — callers override only
/// what a specific test cares about (Rust has no field-update syntax
/// across `..` for `pub(crate)` structs outside the defining module, so
/// this is a plain builder-by-closure, not `..Default::default()`).
fn full_material(material_index: u32, name: &str, verts: u32) -> super::gltf_load::GltfMaterialInfo {
    use super::gltf_load::GltfMaterialInfo;
    GltfMaterialInfo {
        material_index,
        name: Some(name.to_string()),
        base_color_factor: [0.8, 0.8, 0.8, 1.0],
        metallic: 1.0,
        roughness: 0.4,
        emissive: [1.0, 0.5, 0.2],
        alpha_mask: false,
        alpha_cutoff: 0.5,
        base_color_texture: Some(0),
        normal_texture: Some(1),
        normal_scale: 1.0,
        mr_texture: Some(2),
        occlusion_texture: Some(3),
        occlusion_strength: 1.0,
        emissive_texture: Some(4),
        emissive_strength: 2.5,
        ior: 1.5,
        specular_factor: 1.0,
        specular_color_factor: [1.0, 1.0, 1.0],
        specular_texture: None,
        specular_color_texture: None,
        base_color_uv_transform: super::gltf_load::IDENTITY_UV_TRANSFORM,
        normal_uv_transform: super::gltf_load::IDENTITY_UV_TRANSFORM,
        mr_uv_transform: super::gltf_load::IDENTITY_UV_TRANSFORM,
        occlusion_uv_transform: super::gltf_load::IDENTITY_UV_TRANSFORM,
        emissive_uv_transform: super::gltf_load::IDENTITY_UV_TRANSFORM,
        uv_tex_coord_override: false,
        mr_texture_is_gloss_alpha: false,
        transmission_factor: 0.0,
        transmission_texture: None,
        clearcoat_factor: 0.0,
        clearcoat_roughness_factor: 0.0,
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
        volume_attenuation_distance: super::gltf_load::VOLUME_ATTENUATION_DISTANCE_NO_ATTENUATION,
        volume_attenuation_color: [1.0, 1.0, 1.0],
        volume_thickness_texture: None,
        was_blend: false,
        vertex_count: verts,
        base_color_sampler: super::gltf_load::GltfSamplerInfo::default(),
        normal_sampler: super::gltf_load::GltfSamplerInfo::default(),
        mr_sampler: super::gltf_load::GltfSamplerInfo::default(),
        occlusion_sampler: super::gltf_load::GltfSamplerInfo::default(),
        emissive_sampler: super::gltf_load::GltfSamplerInfo::default(),
        animations: Vec::new(),
        skin: None,
        morph: None,
        rigid_multi_node: None,
        own_center: [0.0, 0.0, 0.0],
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

/// BUG-221: composed per-object recenter. The mesh source is shifted
/// by `-own_center` (so local `(0,0,0)` becomes THIS object's own
/// visual center, not wherever the source file authored its local
/// origin) and the user-facing `node.transform_3d`'s `pos` is
/// repositioned to `own_center - scene_center`, so the two compose
/// back to the OLD whole-scene-only `-scene_center` recenter (net
/// world placement at import time is unchanged — only the rotation
/// pivot moves). A synthetic two-material summary with hand-picked
/// `own_center` values (a "minimal fixture constructed
/// programmatically" — no `.glb` on disk needed for this half of the
/// gate) makes every expected number computable by hand; the
/// companion `gpu-proofs` tests below prove the same claim against a
/// real multi-object asset by rendering it.
#[test]
fn bug221_object_transform_recenters_about_own_bbox_center_not_scene_center() {
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
    let own_center = [5.0_f32, 1.0, -0.5];

    for i in 0..3 {
        assert!(
            (translate[i] - (-own_center[i])).abs() < 1e-5,
            "mesh_0.translate_{i} should be -own_center[{i}]: got {translate:?}"
        );
        assert!(
            (pos[i] - (own_center[i] - center[i])).abs() < 1e-5,
            "transform_0.pos_{i} should be own_center[{i}] - center[{i}]: got {pos:?}"
        );
        // The composed net offset must equal the OLD whole-scene-only
        // recenter (-center) — this is the "layout preservation"
        // claim at value level: mesh-side translate and outer pos
        // cancel own_center out entirely, so net world placement is
        // byte-identical to the pre-fix formula regardless of what
        // own_center is.
        assert!(
            (translate[i] + pos[i] - (-center[i])).abs() < 1e-5,
            "mesh_0.translate_{i} + transform_0.pos_{i} must equal -center[{i}] \
             (net world placement unchanged): translate={translate:?} pos={pos:?} center={center:?}"
        );
    }
    // "Position ≈ the object's bounds center", expressed in the SAME
    // recentered-scene coordinate space Position is shown in — a
    // concrete, non-tautological check on top of the formula asserts
    // above.
    assert!((pos[0] - 3.0).abs() < 1e-5 && (pos[1] - 0.0).abs() < 1e-5 && (pos[2] - 0.0).abs() < 1e-5);
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

// -----------------------------------------------------------------
// SCENE_SETUP_PANEL_DESIGN.md P4 — merge_import_into_graph (D5)
// -----------------------------------------------------------------

/// Build a target scene `EffectGraphDef` (as if produced by a PRIOR
/// import) whose bbox is a cube of half-extent `half_extent` centered
/// at the origin, and whose `objects` count on `render_scene` is
/// whatever a single-material summary produces (1). Its synthesized
/// `node.orbit_camera`'s `distance` param is `2.2 * radius` — the exact
/// value [`merge_import_into_graph`]'s scene-reference-radius proxy
/// inverts back out.
fn scene_def_with_bbox_half_extent(half_extent: f32) -> EffectGraphDef {
    let summary = GltfImportSummary {
        materials: vec![full_material(0, "Existing", 500)],
        bbox_min: [-half_extent, -half_extent, -half_extent],
        bbox_max: [half_extent, half_extent, half_extent],
        camera_count: 0,
        default_material_vertex_count: 0,
        animations: Vec::new(),
        animation_report_lines: Vec::new(),
        extension_report_lines: Vec::new(),
    };
    let path = std::path::Path::new("/tmp/synthetic_target_scene.glb");
    let (def, _report) = build_import_graph(&summary, path).expect("build target scene");
    def
}

fn merge_summary(materials: Vec<super::gltf_load::GltfMaterialInfo>, half_extent: f32) -> GltfImportSummary {
    GltfImportSummary {
        materials,
        bbox_min: [-half_extent, -half_extent, -half_extent],
        bbox_max: [half_extent, half_extent, half_extent],
        camera_count: 0,
        default_material_vertex_count: 0,
        animations: Vec::new(),
        animation_report_lines: Vec::new(),
        extension_report_lines: Vec::new(),
    }
}

/// The scene's own render_scene node id + its `objects` count, read the
/// same way a caller would before building the merge summary's expected
/// port range.
fn render_scene_objects(def: &EffectGraphDef) -> (u32, u32) {
    let node = def.nodes.iter().find(|n| n.type_id == "node.render_scene").unwrap();
    let objects = match node.params.get("objects") {
        Some(SerializedParamValue::Int { value }) => *value as u32,
        Some(SerializedParamValue::Float { value }) => *value as u32,
        _ => 0,
    };
    (node.id, objects)
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
    assert_eq!(plan.new_nodes.len(), 3, "one group per incoming material");

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

/// `graph_tool`-equivalent structural gate: a merged def (target scene +
/// the plan's new nodes/wires spliced onto the target's own nodes/
/// wires, `objects` bumped) flattens cleanly and compiles through the
/// real registry — the same proof every import graph gets.
#[test]
fn merged_def_flattens_and_compiles_through_registry() {
    let def = scene_def_with_bbox_half_extent(1.0);
    let (render_id, existing_objects) = render_scene_objects(&def);
    let summary = merge_summary(
        vec![full_material(0, "Merged1", 150), full_material(1, "Merged2", 250)],
        1.0,
    );
    let path = std::path::Path::new("/tmp/synthetic_merge_flatten_compile.glb");
    let plan = merge_import_into_graph(&def, &summary, path).expect("merge two objects");

    let mut merged = def.clone();
    merged.nodes.extend(plan.new_nodes.clone());
    merged.wires.extend(plan.new_wires.clone());
    if let Some(node) = merged.nodes.iter_mut().find(|n| n.id == render_id) {
        node.params.insert(
            "objects".to_string(),
            SerializedParamValue::Int { value: plan.new_objects_count as i32 },
        );
    }
    if let Some(meta) = merged.preset_metadata.as_mut() {
        meta.params.extend(plan.new_card_params.clone());
        meta.bindings.extend(plan.new_card_bindings.clone());
        meta.string_bindings.extend(plan.new_string_bindings.clone());
    }

    let flat = manifold_core::flatten::flatten_groups(&merged)
        .unwrap_or_else(|e| panic!("merged def must flatten cleanly: {e}"));
    // The flattener reassigns EVERY ordinary node (including top-level
    // ones like render_scene) a fresh id (`flatten.rs`'s `clone.id =
    // new_id`) — the pre-flatten `render_id` no longer resolves, so
    // re-find render_scene by type_id in the flattened output.
    let flat_render_id = flat
        .nodes
        .iter()
        .find(|n| n.type_id == "node.render_scene")
        .expect("flattened def keeps its render_scene node")
        .id;
    for k in existing_objects..(existing_objects + 2) {
        assert!(
            flat.wires.iter().any(|w| w.to_node == flat_render_id && w.to_port == format!("object_{k}")),
            "flattened merged def must wire object_{k}"
        );
    }

    let registry = PrimitiveRegistry::with_builtin();
    PresetRuntime::from_def(merged, &registry, None)
        .expect("merged import graph must compile through PresetRuntime::from_def");
}

/// Real-asset merge, using two SMALL fixtures already in this worktree
/// (not the held-out warehouse/skull/rosetta trio, which only exist in
/// the main checkout) — `cc0__oomurasaki_azalea_r._x_pulchrum.glb` as
/// the target scene, Khronos's tiny `Box.glb` merged into it. Writes
/// the merged def to a JSON file so `graph_tool validate`/`fusion` can
/// run against it as a real file, per the phase gate, and doubles as a
/// regression test against real (not hand-built) glTF data.
#[test]
fn merges_a_real_asset_and_writes_merged_def_for_graph_tool() {
    let target_path = azalea_fixture_path();
    let box_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/gltf/khronos/Box.glb");
    if !target_path.exists() || !box_path.exists() {
        println!(
            "merges_a_real_asset_and_writes_merged_def_for_graph_tool: fixture(s) missing, skipping"
        );
        return;
    }

    let (target_def, target_report) =
        assemble_import_graph(&target_path).expect("assemble azalea target scene");
    assert_eq!(target_report.object_count, 2, "azalea has 2 materials with geometry");

    let box_summary =
        gltf_load::gltf_import_summary(&box_path).expect("parse Box.glb summary");
    let plan = merge_import_into_graph(&target_def, &box_summary, &box_path)
        .expect("merge Box.glb into the azalea scene");
    assert_eq!(plan.new_objects_count, 3, "2 azalea objects + 1 Box object");
    assert_eq!(plan.new_nodes.len(), 1, "Box.glb has exactly one material with geometry");

    let mut merged = target_def.clone();
    merged.nodes.extend(plan.new_nodes.clone());
    merged.wires.extend(plan.new_wires.clone());
    if let Some(node) =
        merged.nodes.iter_mut().find(|n| n.id == plan.render_scene_node_id)
    {
        node.params.insert(
            "objects".to_string(),
            SerializedParamValue::Int { value: plan.new_objects_count as i32 },
        );
    }
    if let Some(meta) = merged.preset_metadata.as_mut() {
        meta.params.extend(plan.new_card_params.clone());
        meta.bindings.extend(plan.new_card_bindings.clone());
        meta.string_bindings.extend(plan.new_string_bindings.clone());
    }

    // Structural proof, same as the synthetic test above.
    let flat = manifold_core::flatten::flatten_groups(&merged)
        .unwrap_or_else(|e| panic!("real-asset merged def must flatten cleanly: {e}"));
    let flat_render_id = flat
        .nodes
        .iter()
        .find(|n| n.type_id == "node.render_scene")
        .expect("flattened def keeps its render_scene node")
        .id;
    assert!(
        flat.wires.iter().any(|w| w.to_node == flat_render_id && w.to_port == "object_2"),
        "flattened merged def must wire the new Box object at object_2"
    );
    let registry = PrimitiveRegistry::with_builtin();
    PresetRuntime::from_def(merged.clone(), &registry, None)
        .expect("real-asset merged import graph must compile through PresetRuntime::from_def");

    let json = serde_json::to_string_pretty(&merged).expect("serialize merged def");
    let out_path = std::env::temp_dir().join("scene_setup_p4_merged_azalea_box.json");
    std::fs::write(&out_path, json).expect("write merged def JSON for graph_tool");
    println!(
        "merges_a_real_asset_and_writes_merged_def_for_graph_tool: wrote {}",
        out_path.display()
    );
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
    let summary = GltfImportSummary {
        materials: vec![full_material(0, "Helmet", 1000)],
        bbox_min: [-1.0, -1.0, -1.0],
        bbox_max: [1.0, 1.0, 1.0],
        camera_count: 0,
        default_material_vertex_count: 0,
        animations: Vec::new(),
        animation_report_lines: Vec::new(),
        extension_report_lines: Vec::new(),
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
    for port in ["normal_map", "mr_map", "occlusion_map", "emissive_map"] {
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

/// BUG-036 round-trip gate: the assembled import graph — including the
/// new map-port wires and the D7 sun-coherence dual bindings — must
/// survive a save/reload cycle. `EffectGraphDef` (this function's
/// return type) IS the persisted artifact for a generator layer's
/// override (`ImportModelLayerCommand` stores it verbatim), so a JSON
/// round trip through its own `Serialize`/`Deserialize` impl is the
/// real save/reload path, not a stand-in for it. Asserts the wires and
/// bindings survive AND that the reloaded def still builds through the
/// production loader (`PresetRuntime::from_def`) — proving the ports
/// resolve after reload, not only right after assembly.
#[test]
fn round_trip_preserves_map_wires_and_sun_coherence_bindings() {
    let summary = GltfImportSummary {
        materials: vec![full_material(0, "Helmet", 1000)],
        bbox_min: [-1.0, -1.0, -1.0],
        bbox_max: [1.0, 1.0, 1.0],
        camera_count: 0,
        default_material_vertex_count: 0,
        animations: Vec::new(),
        animation_report_lines: Vec::new(),
        extension_report_lines: Vec::new(),
    };
    let path = std::path::Path::new("/tmp/synthetic_round_trip.glb");
    let (def, _report) = build_import_graph(&summary, path).expect("build graph");

    let json = serde_json::to_string(&def).expect("serialize EffectGraphDef");
    let reloaded: EffectGraphDef = serde_json::from_str(&json).expect("deserialize EffectGraphDef");
    assert_eq!(def, reloaded, "round trip must be byte-for-byte structurally identical");

    let flat = manifold_core::flatten::flatten_groups(&reloaded).expect("flatten reloaded def");
    let scene_object = flat.nodes.iter().find(|n| n.type_id == "node.scene_object").unwrap();
    for port in ["normal_map", "mr_map", "occlusion_map", "emissive_map"] {
        assert!(
            flat.wires.iter().any(|w| w.to_node == scene_object.id && w.to_port == port),
            "reloaded def must still wire `{port}` — maps must stay bound after reload"
        );
    }
    let meta = reloaded.preset_metadata.as_ref().expect("reloaded v2 metadata");
    let sun_id = reloaded
        .nodes
        .iter()
        .find(|n| n.type_id == "node.light")
        .map(|n| n.id)
        .expect("sun present");
    for param in ["pos_x", "pos_y", "pos_z"] {
        let macro_id = format!("{sun_id}_{param}");
        let count = meta.bindings.iter().filter(|b| b.id == macro_id).count();
        assert_eq!(count, 2, "`{macro_id}`'s dual binding must survive reload");
    }

    // Modulation live after reload, not just structurally present: the
    // reloaded def must still build through the production loader.
    let registry = PrimitiveRegistry::with_builtin();
    PresetRuntime::from_def(reloaded, &registry, None)
        .expect("reloaded import graph must build through PresetRuntime::from_def");
}

/// GLTF_ANIMATION_DESIGN.md A1 deliverable 3: a material whose
/// `GltfMaterialInfo::animation` resolved gets one
/// `node.gltf_animation_source` inserted into its group, wired into
/// its OWN `node.transform_3d`'s nine port-shadowed inputs — additive
/// to the static recenter (which stays on `transform_3d`'s own
/// pos_x/y/z param default). A material with no resolved animation
/// gets no such node (never fabricated).
#[test]
fn animated_material_wires_animation_source_into_its_own_transform_3d() {
    use super::gltf_load::{GltfObjectAnimation, QuatTrack, Vec3Track};

    let mut animated = full_material(0, "Inner", 1000);
    animated.animations = vec![Some(GltfObjectAnimation {
        duration_s: 2.0,
        translation: Some(Vec3Track {
            times: vec![0.0, 1.0],
            values: vec![[0.0, 0.0, 0.0], [1.0, 2.0, 3.0]],
            ..Default::default()
        }),
        rotation: Some(QuatTrack {
            times: vec![0.0, 1.0],
            values: vec![
                [0.0, 0.0, 0.0, 1.0],
                [0.0, 0.0, std::f32::consts::FRAC_1_SQRT_2, std::f32::consts::FRAC_1_SQRT_2],
            ],
            ..Default::default()
        }),
        scale: None,
        translation_node: Some(0),
        rotation_node: Some(2),
        scale_node: None,
    })];
    let mut static_obj = full_material(1, "Outer", 500);
    static_obj.animations = Vec::new();

    let summary = GltfImportSummary {
        materials: vec![animated, static_obj],
        bbox_min: [-1.0, -1.0, -1.0],
        bbox_max: [1.0, 1.0, 1.0],
        camera_count: 0,
        default_material_vertex_count: 0,
        animations: Vec::new(),
        animation_report_lines: Vec::new(),
        extension_report_lines: Vec::new(),
    };
    let path = std::path::Path::new("/tmp/synthetic_animation_wiring.glb");
    let (def, _report) = build_import_graph(&summary, path).expect("build graph");
    let flat = manifold_core::flatten::flatten_groups(&def).expect("flatten");

    let anim_nodes: Vec<_> =
        flat.nodes.iter().filter(|n| n.type_id == "node.gltf_animation_source").collect();
    assert_eq!(anim_nodes.len(), 1, "only the animated object gets a source node");
    let anim = anim_nodes[0];

    // GLTF_ANIM_RUNTIME_V2_DESIGN.md P2: no keyframe payload in the
    // def any more — `path` + per-channel node selectors pick the
    // shared cache entries. translation/rotation come from DIFFERENT
    // nodes (0 and 2 respectively — the BoxAnimated.glb shape); scale
    // was never animated -> -1 sentinel.
    assert!(
        !anim.params.contains_key("translation_track"),
        "keyframe payload must never live in the def (P2 D1)"
    );
    assert!(!anim.params.contains_key("rotation_track"));
    assert!(!anim.params.contains_key("scale_track"));
    assert_eq!(anim.params.get("translation_node"), Some(&int(0)));
    assert_eq!(anim.params.get("rotation_node"), Some(&int(2)));
    assert_eq!(anim.params.get("scale_node"), Some(&int(-1)));
    assert_eq!(anim.params.get("duration_s"), Some(&float(2.0)));

    let transform =
        flat.nodes.iter().find(|n| n.type_id == "node.transform_3d" && n.node_id.as_str().contains("transform_0")).expect("transform_0 present");
    for port in
        ["pos_x", "pos_y", "pos_z", "rot_x", "rot_y", "rot_z", "scale_x", "scale_y", "scale_z"]
    {
        assert!(
            flat.wires
                .iter()
                .any(|w| w.from_node == anim.id && w.from_port == port && w.to_node == transform.id && w.to_port == port),
            "animation source must wire `{port}` into transform_0"
        );
    }

    // The registry-facing build path must accept the Table params
    // (proves node.gltf_animation_source is actually registered and
    // its Table param declarations match SerializedParamValue's
    // conversion, not just that JSON round-trips syntactically).
    let registry = PrimitiveRegistry::with_builtin();
    PresetRuntime::from_def(def, &registry, None)
        .expect("import graph with an animated object must build through PresetRuntime::from_def");
}

/// Peter 2026-07-18: animation is a PER-GLB linked control — clips are
/// file-level in glTF, so a multi-object animated import gets ONE
/// "Animation" card section (Rate/Clip/Loop Mode/Retrigger) whose
/// bindings fan out to every animation clock in the file, never one
/// section per object.
#[test]
fn animation_cards_are_one_linked_section_per_glb() {
    use super::gltf_load::{GltfAnimationInfo, GltfObjectAnimation, Vec3Track};

    let track = |node: usize| GltfObjectAnimation {
        duration_s: 2.0,
        translation: Some(Vec3Track {
            times: vec![0.0, 1.0],
            values: vec![[0.0, 0.0, 0.0], [1.0, 2.0, 3.0]],
            ..Default::default()
        }),
        rotation: None,
        scale: None,
        translation_node: Some(node),
        rotation_node: None,
        scale_node: None,
    };
    let mut a = full_material(0, "Inner", 1000);
    a.animations = vec![Some(track(0))];
    let mut b = full_material(1, "Outer", 500);
    b.animations = vec![Some(track(1))];

    let summary = GltfImportSummary {
        materials: vec![a, b],
        bbox_min: [-1.0, -1.0, -1.0],
        bbox_max: [1.0, 1.0, 1.0],
        camera_count: 0,
        default_material_vertex_count: 0,
        animations: vec![GltfAnimationInfo {
            name: Some("Walk".to_string()),
            nodes: Vec::new(),
            skipped_channels: Vec::new(),
        }],
        animation_report_lines: Vec::new(),
        extension_report_lines: Vec::new(),
    };
    let path = std::path::Path::new("/tmp/synthetic_shared_anim_cards.glb");
    let (def, _report) = build_import_graph(&summary, path).expect("build graph");
    let meta = def.preset_metadata.as_ref().expect("preset metadata");

    // Exactly ONE shared param set, leading the card, in one section.
    for (i, id) in ["anim_rate", "anim_clip", "anim_loop_mode", "anim_retrigger"].iter().enumerate() {
        let hits: Vec<_> = meta.params.iter().filter(|p| p.id == *id).collect();
        assert_eq!(hits.len(), 1, "{id} must appear exactly once");
        assert_eq!(hits[0].section.as_deref(), Some("Animation"));
        assert_eq!(meta.params[i].id, *id, "Animation section leads the card");
    }
    assert!(
        !meta.params.iter().any(|p| p.name == "Rate" && p.id != "anim_rate"),
        "no per-object Rate knobs remain"
    );
    let clip = meta.params.iter().find(|p| p.id == "anim_clip").unwrap();
    assert_eq!(clip.value_labels, vec!["Walk".to_string()], "clip detents use file clip names");

    // Fan-out: BOTH objects' animation clocks bind to the shared knobs.
    let rate_bindings: Vec<_> = meta.bindings.iter().filter(|bd| bd.id == "anim_rate").collect();
    assert_eq!(rate_bindings.len(), 2, "one rate binding per animation clock");
    let targets: std::collections::HashSet<_> = rate_bindings
        .iter()
        .map(|bd| match &bd.target {
            BindingTarget::Node { node_id, .. } => node_id.as_str().to_string(),
            other => panic!("unexpected target {other:?}"),
        })
        .collect();
    assert_eq!(targets.len(), 2, "the two bindings target distinct nodes");

    // The fan-out shape must be lint-legal end to end.
    use crate::node_graph::persistence::EffectGraphDefExt;
    let registry = PrimitiveRegistry::with_builtin();
    let graph = def.clone().into_graph(&registry).expect("import graph must build");
    let (errors, _warnings) = crate::node_graph::validate::check_card_lints(&def, Some(&graph));
    assert!(errors.is_empty(), "card lints must accept the shared-anim import: {errors:?}");

    // Merging the SAME animated file into this scene must mint its own
    // linked section under a fresh prefix — never collide with "anim_*".
    let plan = merge_import_into_graph(&def, &summary, path).expect("merge plan");
    let merged_rate: Vec<_> =
        plan.new_card_params.iter().filter(|p| p.id == "anim2_rate").collect();
    assert_eq!(merged_rate.len(), 1, "merge uniquifies the shared anim prefix");
    assert_eq!(
        merged_rate[0].section.as_deref(),
        Some("Animation — synthetic_shared_anim_cards"),
        "merged section is named after the file"
    );
    assert_eq!(
        plan.new_card_bindings.iter().filter(|bd| bd.id == "anim2_rate").count(),
        2,
        "merged bindings fan out under the fresh prefix"
    );
}

/// BUG-204 regression: the A4 Retrigger card param is `is_trigger`
/// and binds to the animation nodes' `trigger_count` — which must be
/// `ParamType::Trigger`, or validate.rs card lint (d) rejects the
/// assembled graph and EVERY animated or rigged glb fails at import
/// (skeleton_animated.glb, 2026-07-17: A4 shipped `trigger_count` as
/// Int four days after the lint landed). Runs the real fixture through
/// the same lint the import path uses.
#[test]
fn animated_and_rigged_import_passes_card_lints() {
    use crate::node_graph::persistence::EffectGraphDefExt;
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/gltf/skeleton_animated.glb");
    let (def, _report) =
        super::assemble_import_graph(&path).expect("assemble skeleton_animated.glb");
    let registry = PrimitiveRegistry::with_builtin();
    let graph = def.clone().into_graph(&registry).expect("import graph must build");
    let (errors, _warnings) =
        crate::node_graph::validate::check_card_lints(&def, Some(&graph));
    assert!(
        errors.is_empty(),
        "card lints must accept the assembled animated+rigged import: {errors:?}"
    );
}

/// GLTF_ANIM_RUNTIME_V2_DESIGN.md §3 invariant, P2 gate: no keyframe
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
/// applies morph then skin, §3.7.2), and the deltas source's
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
    assert!(
        flat.nodes.iter().any(|n| n.type_id == "node.gltf_skinned_mesh_source"),
        "skin+morph object must still be driven by node.gltf_skinned_mesh_source"
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
         glTF applies morph before skin (§3.7.2)"
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

/// The hostile fixture shelf: real-world-shaped assets (Sketchfab FBX
/// conversions, Mixamo rigs, Blender exports) whose traits the Khronos
/// suite doesn't exercise — transform-bearing ancestors above joint
/// trees, unit-conversion scales, animated prefixes. BUG-204 and
/// BUG-205 both shipped through green gates because no oracle ever fed
/// the import pipeline this input class; every glb under
/// `tests/fixtures/gltf/hostile/` runs the full CPU chain here
/// (assemble → graph build → card lints → PresetRuntime build), and
/// the gpu-proofs sibling below renders each one and checks framing
/// invariants. Add assets by dropping a glb in the directory —
/// `scripts/blender/fbx2glb.py` converts FBX-only sources.
fn hostile_fixture_paths() -> Vec<std::path::PathBuf> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/gltf/hostile");
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read hostile fixture dir {}: {e}", dir.display()))
        .filter_map(|entry| {
            let p = entry.ok()?.path();
            (p.extension().and_then(|e| e.to_str()) == Some("glb")).then_some(p)
        })
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "hostile fixture shelf is empty — the sweep is vacuous");
    paths
}

#[test]
fn hostile_fixtures_assemble_validate_and_build() {
    use crate::node_graph::persistence::EffectGraphDefExt;
    for path in hostile_fixture_paths() {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let (def, _report) = super::assemble_import_graph(&path)
            .unwrap_or_else(|e| panic!("{name}: assemble_import_graph failed: {e}"));
        let registry = PrimitiveRegistry::with_builtin();
        let graph = def
            .clone()
            .into_graph(&registry)
            .unwrap_or_else(|e| panic!("{name}: import graph failed to build: {e:?}"));
        let (errors, _warnings) =
            crate::node_graph::validate::check_card_lints(&def, Some(&graph));
        assert!(errors.is_empty(), "{name}: card lints rejected the import: {errors:?}");
        PresetRuntime::from_def(def, &registry, None)
            .unwrap_or_else(|e| panic!("{name}: PresetRuntime::from_def failed: {e:?}"));
    }
}

/// Merge every hostile fixture into a real existing scene (the azalea
/// import) and run the merged def through the same CPU chain — merge
/// reuses `build_object_group`, but that sharing is exactly the kind
/// of claim this shelf exists to prove rather than assume (BUG-204's
/// class: two features correct alone, never composed). Also pins
/// BUG-205 through the merge path: a merged skinned object must get
/// its skeleton pose and must NOT get a rigid animation source.
#[test]
fn hostile_fixtures_merge_into_existing_scene() {
    use crate::node_graph::persistence::EffectGraphDefExt;
    let (target, _report) = super::assemble_import_graph(&azalea_fixture_path())
        .expect("assemble azalea target scene");
    let (render_id, existing_objects) = render_scene_objects(&target);
    for path in hostile_fixture_paths() {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let plan = super::assemble_merge_plan(&target, &path)
            .unwrap_or_else(|e| panic!("{name}: assemble_merge_plan failed: {e}"));

        let had_skin =
            plan.new_nodes.iter().any(|n| contains_type(n, "node.gltf_skeleton_pose"));
        let has_rigid_anim =
            plan.new_nodes.iter().any(|n| contains_type(n, "node.gltf_animation_source"));
        if had_skin {
            assert!(
                !has_rigid_anim,
                "{name}: merged skinned object carries a rigid gltf_animation_source — \
                 BUG-205's double-transform through the merge path"
            );
        }

        let mut merged = target.clone();
        merged.nodes.extend(plan.new_nodes.clone());
        merged.wires.extend(plan.new_wires.clone());
        if let Some(node) = merged.nodes.iter_mut().find(|n| n.id == render_id) {
            node.params.insert(
                "objects".to_string(),
                SerializedParamValue::Int { value: plan.new_objects_count as i32 },
            );
        }
        if let Some(meta) = merged.preset_metadata.as_mut() {
            meta.params.extend(plan.new_card_params.clone());
            meta.bindings.extend(plan.new_card_bindings.clone());
            meta.string_bindings.extend(plan.new_string_bindings.clone());
        }

        let registry = PrimitiveRegistry::with_builtin();
        let graph = merged
            .clone()
            .into_graph(&registry)
            .unwrap_or_else(|e| panic!("{name}: merged graph failed to build: {e:?}"));
        let (errors, _warnings) =
            crate::node_graph::validate::check_card_lints(&merged, Some(&graph));
        assert!(errors.is_empty(), "{name}: card lints rejected the merged def: {errors:?}");
        PresetRuntime::from_def(merged, &registry, None)
            .unwrap_or_else(|e| panic!("{name}: merged PresetRuntime build failed: {e:?}"));
        let _ = existing_objects;
    }
}

/// Group-aware type search: merge plans emit one GROUP node per object
/// with the real producers in its `group.body`.
fn contains_type(node: &EffectGraphNode, type_id: &str) -> bool {
    if node.type_id == type_id {
        return true;
    }
    node.group
        .as_ref()
        .is_some_and(|g| g.nodes.iter().any(|inner| contains_type(inner, type_id)))
}

/// Render every hostile fixture and check framing invariants — the
/// automated form of "does it look plausibly right": enough lit pixels
/// to be a real render (BUG-205's speck fails), not a full-frame
/// blowout, and the lit centroid near frame center (wrong-space
/// framing fails). Edge-contact (object cropped at opposite frame
/// edges) is checked at TWO phases — 0.0 (straight/rest pose, the worst
/// case for an elongated skinned rig) and 0.25 (the original single
/// phase) — against an xfail list. BUG-206 fixed the framing distance
/// (per-axis fit, not bbox-diagonal), so the list is empty; a fixture
/// only goes back on it after investigation confirms the crop is a
/// distinct, unrelated bug (see BUG-206 backlog entry).
#[cfg(feature = "gpu-proofs")]
#[test]
fn hostile_fixtures_render_within_framing_invariants() {
    let (w, h) = (256u32, 256u32);
    const EDGE_XFAIL: &[&str] = &[];
    const PHASES: &[f32] = &[0.0, 0.25];
    for path in hostile_fixture_paths() {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        for &phase in PHASES {
            let (def, _report) = super::assemble_import_graph(&path)
                .unwrap_or_else(|e| panic!("{name}: assemble failed: {e}"));
            let duration_s = skeleton_pose_duration_s_or_static(&def);
            let rgba = render_skinned_import_at_progress(def, w, h, phase, duration_s);

            let mut lit = 0u64;
            let (mut cx, mut cy) = (0.0f64, 0.0f64);
            let (mut top, mut bottom, mut left, mut right) = (false, false, false, false);
            for y in 0..h {
                for x in 0..w {
                    let i = ((y * w + x) * 4) as usize;
                    if rgba[i].max(rgba[i + 1]).max(rgba[i + 2]) > 8 {
                        lit += 1;
                        cx += x as f64;
                        cy += y as f64;
                        top |= y == 0;
                        bottom |= y == h - 1;
                        left |= x == 0;
                        right |= x == w - 1;
                    }
                }
            }
            let fraction = lit as f64 / (w as u64 * h as u64) as f64;
            assert!(
                (0.005..=0.95).contains(&fraction),
                "{name}@phase{phase}: lit fraction {fraction:.4} outside [0.005, 0.95] — \
                 speck (BUG-205 class), black frame, or full-frame blowout"
            );
            let (cx, cy) = (cx / lit as f64 / w as f64, cy / lit as f64 / h as f64);
            assert!(
                (0.2..=0.8).contains(&cx) && (0.2..=0.8).contains(&cy),
                "{name}@phase{phase}: lit centroid ({cx:.2}, {cy:.2}) outside the center \
                 region — wrong-space framing/recenter"
            );
            let cropped = (top && bottom) || (left && right);
            if !EDGE_XFAIL.contains(&name.as_str()) {
                assert!(
                    !cropped,
                    "{name}@phase{phase}: object touches opposite frame edges — default \
                     framing crops it (BUG-206 class)"
                );
            }
        }
    }
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

/// GLTF_ANIMATION_DESIGN.md A1 deliverable 4 / GLTF_ANIM_RUNTIME_V2_
/// DESIGN.md P2: the animation source node (now `path` + per-channel
/// node selectors, no keyframe Tables) survives V1 JSON save→reload —
/// the STANDARD §5 gate must PROVE this, not assume it.
#[test]
fn animation_selectors_survive_json_round_trip() {
    use super::gltf_load::{GltfObjectAnimation, QuatTrack, Vec3Track};

    let mut animated = full_material(0, "Inner", 1000);
    animated.animations = vec![Some(GltfObjectAnimation {
        duration_s: 3.708_33,
        translation: Some(Vec3Track {
            times: vec![0.0, 1.25, 2.5, 3.708_33],
            values: vec![[0.0, 0.0, 0.0], [0.0, 2.52, 0.0], [0.0, 2.52, 0.0], [0.0, 0.0, 0.0]],
            ..Default::default()
        }),
        rotation: Some(QuatTrack {
            times: vec![1.25, 2.5],
            values: vec![[0.0, 0.0, 0.0, 1.0], [1.0, 0.0, 0.0, 0.0]],
            ..Default::default()
        }),
        scale: None,
        translation_node: Some(3),
        rotation_node: Some(3),
        scale_node: None,
    })];
    let summary = GltfImportSummary {
        materials: vec![animated],
        bbox_min: [-1.0, -1.0, -1.0],
        bbox_max: [1.0, 1.0, 1.0],
        camera_count: 0,
        default_material_vertex_count: 0,
        animations: Vec::new(),
        animation_report_lines: Vec::new(),
        extension_report_lines: Vec::new(),
    };
    let path = std::path::Path::new("/tmp/synthetic_animation_round_trip.glb");
    let (def, _report) = build_import_graph(&summary, path).expect("build graph");

    let json = serde_json::to_string(&def).expect("serialize EffectGraphDef");
    let reloaded: EffectGraphDef = serde_json::from_str(&json).expect("deserialize EffectGraphDef");
    assert_eq!(def, reloaded, "round trip must be byte-for-byte structurally identical");

    let flat = manifold_core::flatten::flatten_groups(&reloaded).expect("flatten reloaded def");
    let anim = flat
        .nodes
        .iter()
        .find(|n| n.type_id == "node.gltf_animation_source")
        .expect("animation source survives reload");
    assert!(
        !anim.params.contains_key("translation_track"),
        "keyframe payload must never live in the def (P2 D1)"
    );
    assert!(!anim.params.contains_key("rotation_track"));
    assert_eq!(anim.params.get("translation_node"), Some(&int(3)));
    assert_eq!(anim.params.get("rotation_node"), Some(&int(3)));
    assert_eq!(anim.params.get("scale_node"), Some(&int(-1)));
    assert_eq!(anim.params.get("duration_s"), Some(&float(3.708_33)));

    let registry = PrimitiveRegistry::with_builtin();
    PresetRuntime::from_def(reloaded, &registry, None)
        .expect("reloaded import graph must build through PresetRuntime::from_def");
}

/// IMPORT_FIDELITY_DESIGN.md D8/F-P5 round-trip gate: a `Blend` alpha_mode
/// (from a transmission material) and its performer-facing Opacity card
/// binding must survive save → reload, and stay live (modulatable) after
/// reload — the BUG-036 rule (create-path green is half a gate for
/// stateful features).
#[test]
fn round_trip_preserves_blend_alpha_mode_and_opacity_binding() {
    let mut m = full_material(0, "Windshield", 500);
    m.was_blend = true;
    m.transmission_factor = 0.9;
    let summary = GltfImportSummary {
        materials: vec![m],
        bbox_min: [-1.0, -1.0, -1.0],
        bbox_max: [1.0, 1.0, 1.0],
        camera_count: 0,
        default_material_vertex_count: 0,
        animations: Vec::new(),
        animation_report_lines: Vec::new(),
        extension_report_lines: Vec::new(),
    };
    let path = std::path::Path::new("/tmp/synthetic_glass_round_trip.glb");
    let (def, _report) = build_import_graph(&summary, path).expect("build graph");

    let json = serde_json::to_string(&def).expect("serialize EffectGraphDef");
    let reloaded: EffectGraphDef = serde_json::from_str(&json).expect("deserialize EffectGraphDef");
    assert_eq!(def, reloaded, "round trip must be byte-for-byte structurally identical");

    let flat = manifold_core::flatten::flatten_groups(&reloaded).expect("flatten reloaded def");
    let mat = flat
        .nodes
        .iter()
        .find(|n| n.type_id == "node.pbr_material")
        .expect("pbr_material node");
    assert_eq!(
        mat.params.get("alpha_mode"),
        Some(&enum_val(2)),
        "reloaded def must still carry alpha_mode Blend"
    );

    let meta = reloaded.preset_metadata.as_ref().expect("reloaded v2 metadata");
    assert!(
        meta.bindings.iter().any(|b| b.label == "Opacity"),
        "reloaded def must still carry the glass object's Opacity card binding"
    );

    // Modulation live after reload — not just structurally present.
    let registry = PrimitiveRegistry::with_builtin();
    PresetRuntime::from_def(reloaded, &registry, None)
        .expect("reloaded glass import graph must build through PresetRuntime::from_def");
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
    use crate::node_graph::{ValidateKind, validate_def};
    use manifold_gpu::GpuDevice;

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
        specular_color_factor: [1.0, 1.0, 1.0],
        specular_texture: None,
        specular_color_texture: None,
        base_color_uv_transform: super::gltf_load::IDENTITY_UV_TRANSFORM,
        normal_uv_transform: super::gltf_load::IDENTITY_UV_TRANSFORM,
        mr_uv_transform: super::gltf_load::IDENTITY_UV_TRANSFORM,
        occlusion_uv_transform: super::gltf_load::IDENTITY_UV_TRANSFORM,
        emissive_uv_transform: super::gltf_load::IDENTITY_UV_TRANSFORM,
        uv_tex_coord_override: false,
        mr_texture_is_gloss_alpha: false,
        transmission_factor: 0.0,
        transmission_texture: None,
        clearcoat_factor: 0.0,
        clearcoat_roughness_factor: 0.0,
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
        volume_attenuation_distance: super::gltf_load::VOLUME_ATTENUATION_DISTANCE_NO_ATTENUATION,
        volume_attenuation_color: [1.0, 1.0, 1.0],
        volume_thickness_texture: None,
        was_blend: false,
        vertex_count: verts,
        base_color_sampler: super::gltf_load::GltfSamplerInfo::default(),
        normal_sampler: super::gltf_load::GltfSamplerInfo::default(),
        mr_sampler: super::gltf_load::GltfSamplerInfo::default(),
        occlusion_sampler: super::gltf_load::GltfSamplerInfo::default(),
        emissive_sampler: super::gltf_load::GltfSamplerInfo::default(),
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
    let device = std::sync::Arc::new(GpuDevice::new());
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
/// SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md §2 D3/P2 — the original
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
    use crate::node_graph::persistence::EffectGraphDefExt;

    let mut render = plain_node(0, "render", "node.render_scene", "render");
    render.params.insert("objects".to_string(), int(3));
    render.params.insert("lights".to_string(), int(1));

    let scene_object_2 = plain_node(1, "object_2", "node.scene_object", "object_2");

    let def = EffectGraphDef {
        version: 1,
        name: None,
        description: None,
        preset_metadata: None,
        nodes: vec![render, scene_object_2],
        wires: vec![wire(1, "object", 0, "object_2")],
    };

    // Validate at the `into_graph` layer — the exact place the
    // "unknown parameter 'pos_x_2'" error was raised for the old shape.
    // (A full `from_def` additionally enforces generator-boundary
    // wiring, which this minimal two-node def deliberately omits — out
    // of scope for the port-surface regression.)
    let registry = PrimitiveRegistry::with_builtin();
    let graph = def.into_graph(&registry).expect(
        "render_scene with objects=3 must accept an object_2 wire at load \
         (reconfigure runs before port validation)",
    );
    assert!(
        graph.wires().iter().any(|w| w.to.1 == "object_2"),
        "the object_2 wire survives into the built graph"
    );
}

/// Real held-out proof of Peter's exact case: point `MESH_SNAP_GLB` at a
/// `.glb` with >2 distinct materials (e.g. the japanese-apricot fixture)
/// and confirm the assembled generator loads through the production
/// `PresetRuntime::from_def` without the `unknown parameter 'pos_x_2'`
/// failure. `#[ignore]` + env-gated: the large held-out fixtures aren't in
/// the tree, and this is a targeted manual gate, not CI.
#[test]
#[ignore = "env-gated held-out glTF; set MESH_SNAP_GLB to a >2-material .glb"]
fn held_out_gltf_generator_loads_through_from_def() {
    let Ok(glb) = std::env::var("MESH_SNAP_GLB") else {
        println!("MESH_SNAP_GLB unset — skipping");
        return;
    };
    let (def, report) = assemble_import_graph(std::path::Path::new(&glb))
        .unwrap_or_else(|e| panic!("assemble {glb}: {e}"));
    println!("held-out import report: {report:?}");
    let registry = PrimitiveRegistry::with_builtin();
    PresetRuntime::from_def(def, &registry, None).unwrap_or_else(|e| {
        panic!("held-out glTF generator failed to load through from_def: {e}")
    });
    println!("held-out glTF generator loaded clean ({} objects)", report.object_count);
}

// ========================================================================
// Visual proof — render the assembled azalea graph through the real
// production path (PresetRuntime::from_def_with_device + render()) and
// confirm it's actually lit/textured, not just structurally valid.
// ========================================================================

/// Decode one IEEE-754 binary16 value to f32 (no `half` dependency).
/// Copied from `mesh_snapshot.rs` (test-only, small, not worth a shared
/// module for two call sites).
#[cfg(feature = "gpu-proofs")]
fn half_to_f32(h: u16) -> f32 {
    let sign = if (h >> 15) & 1 == 1 { -1.0f32 } else { 1.0f32 };
    let exp = (h >> 10) & 0x1f;
    let mant = h & 0x3ff;
    let mag = if exp == 0 {
        (mant as f32) * 2f32.powi(-24)
    } else if exp == 0x1f {
        if mant == 0 { f32::INFINITY } else { f32::NAN }
    } else {
        (1.0 + (mant as f32) / 1024.0) * 2f32.powi(exp as i32 - 15)
    };
    sign * mag
}

#[cfg(feature = "gpu-proofs")]
/// Reinhard-tonemap an HDR channel to 8-bit: `out = (v/(1+v)).clamp(0,1)*255`.
fn tonemap_channel(v: f32) -> u8 {
    let ldr = (v / (1.0 + v)).clamp(0.0, 1.0);
    (ldr * 255.0).round() as u8
}

#[cfg(feature = "gpu-proofs")]
/// Render the assembled azalea graph through the PRODUCTION preset
/// runtime (`PresetRuntime::from_def_with_device` + `render()`), the
/// same path a real imported `.manifold` project's generator card
/// takes — not the raw `Graph`/`Executor` harness `mesh_snapshot.rs`
/// uses for its own lower-level proofs. Both `node.gltf_mesh_source`
/// (2 objects) and `node.gltf_texture_source` (2 textures) parse on
/// background threads, so the render is polled (bounded, with a short
/// sleep between attempts) until the frame is actually non-black,
/// mirroring `gltf_textured_azalea_renders_through_render_scene_to_png`'s
/// convergence loop. Ignored by default: needs a GPU, the large
/// fixture, and writes a file. Point `MESH_SNAP_OUT` at an absolute
/// path to control the output location.
#[test]
#[ignore]
fn imported_azalea_renders_faithfully_to_png() {
    use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use crate::preset_context::PresetContext;
    use crate::render_target::RenderTarget;
    use manifold_gpu::GpuTextureFormat;

    // MESH_SNAP_GLB overrides the fixture — the P2 held-out-input gate
    // (SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md §5 P2) points this at a
    // >1-object model NOT the azalea the transform-port swap was
    // developed against, to prove the group-placement wiring generalizes.
    let path = std::env::var("MESH_SNAP_GLB")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| azalea_fixture_path());
    if !path.exists() {
        eprintln!(
            "imported_azalea_renders_faithfully_to_png: fixture not found at {}, skipping",
            path.display()
        );
        return;
    }

    let (def, report) = assemble_import_graph(&path).expect("assemble import graph");
    println!("imported model report: {report:?}");

    let (w, h) = (768u32, 768u32);
    let device = crate::test_device();
    let format = GpuTextureFormat::Rgba16Float;
    let registry = PrimitiveRegistry::with_builtin();

    let mut generator =
        PresetRuntime::from_def_with_device(def, &registry, device.arc(), w, h, format, None)
            .expect("assembled azalea graph must build through PresetRuntime::from_def_with_device");

    let target = RenderTarget::new(&device, w, h, format, "imported-azalea");
    let ctx = PresetContext {
        time: 0.0,
        beat: 0.0,
        dt: 1.0 / 60.0,
        width: w,
        height: h,
        output_width: w,
        output_height: h,
        aspect: 1.0,
        owner_key: 0,
        is_clip_level: false,
        frame_count: 0,
        anim_progress: 0.0,
        trigger_count: 0,
    };

    // BUG-100 (see docs/BUG_BACKLOG.md): `fraction > 0.02` alone is NOT
    // a texture-decode-completion signal — the material's own `ambient`
    // floor (`node.pbr_material`'s 0.18) lights the WHOLE silhouette
    // from frame 1, regardless of whether `node.gltf_texture_source`'s
    // background-thread decode has landed yet, so the old check broke
    // out of this loop on the very first or second attempt for models
    // whose base-color texture takes longer to decode than azalea's —
    // capturing (and, before this fix, permanently asserting on) an
    // under-textured, falsely-"near-black"-looking frame. Confirmed by
    // rendering: forcing extra attempts on the exact same unmodified
    // graph turns a near-black `cc0__japanese_apricot_prunus_mume.glb`/
    // `lowe.glb` capture into a fully lit, richly textured one — the
    // geometry/lighting/material rig was never the problem.
    //
    // Real completion signal: once the texture (and any other async
    // parse) has actually landed, the render is a pure function of a
    // static camera + static geometry + static light, so consecutive
    // frames stop changing. Poll until the readback is byte-identical
    // across `STABLE_STREAK` consecutive frames (not just non-black),
    // so a still-loading frame's mid-decode churn can't look "done".
    const STABLE_STREAK: u32 = 3;
    let max_attempts = 200;
    let mut rgba = Vec::new();
    let mut fraction = 0.0f64;
    let mut prev_rgba: Option<Vec<u8>> = None;
    let mut stable_count = 0u32;
    for attempt in 0..max_attempts {
        {
            let mut enc = device.create_encoder("imported-azalea-render");
            {
                let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
                generator.render(
                    &mut gpu,
                    &target.texture,
                    &ctx,
                    &manifold_core::params::ParamManifest::default(),
                );
            }
            enc.commit_and_wait_completed();
        }

        let bytes_per_row = w * 8;
        let total_bytes = u64::from(h * bytes_per_row);
        let readback_buf = device.create_buffer_shared(total_bytes);
        let mut readback_enc = device.create_encoder("imported-azalea-readback");
        readback_enc.copy_texture_to_buffer(&target.texture, &readback_buf, w, h, bytes_per_row);
        readback_enc.commit_and_wait_completed();

        let ptr = readback_buf.mapped_ptr().expect("shared readback");
        let halves: &[u16] =
            unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };

        rgba = Vec::with_capacity((w * h * 4) as usize);
        let mut non_black = 0usize;
        for px in halves.chunks_exact(4) {
            let r = tonemap_channel(half_to_f32(px[0]));
            let g = tonemap_channel(half_to_f32(px[1]));
            let b = tonemap_channel(half_to_f32(px[2]));
            if r != 0 || g != 0 || b != 0 {
                non_black += 1;
            }
            rgba.push(r);
            rgba.push(g);
            rgba.push(b);
            let a = half_to_f32(px[3]).clamp(0.0, 1.0);
            rgba.push((a * 255.0).round() as u8);
        }
        let total = (w * h) as usize;
        fraction = non_black as f64 / total as f64;

        if fraction > 0.02 && prev_rgba.as_deref() == Some(rgba.as_slice()) {
            stable_count += 1;
        } else {
            stable_count = 0;
        }
        prev_rgba = Some(rgba.clone());

        if stable_count >= STABLE_STREAK {
            println!(
                "imported_azalea_renders_faithfully_to_png: converged on attempt {attempt} \
                 (non-black fraction {fraction:.4}, stable for {STABLE_STREAK} frames)"
            );
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    let total = (w * h) as usize;
    println!(
        "imported_azalea_renders_faithfully_to_png: non-black pixel fraction = {fraction:.4} \
         ({max_attempts} attempts budget, total {total})"
    );
    // PNG is written BEFORE the coverage assert so a failing run still
    // leaves the frame on disk — the assert message alone can't show
    // WHERE the pixels went (tiny vs offset vs black, BUG-205's triage).
    let out_path = std::env::var("MESH_SNAP_OUT")
        .unwrap_or_else(|_| "target/mesh-snap/imported_azalea.png".to_string());
    if let Some(parent) = std::path::Path::new(&out_path).parent() {
        std::fs::create_dir_all(parent).expect("create output dir");
    }
    image::save_buffer(&out_path, &rgba, w, h, image::ExtendedColorType::Rgba8)
        .unwrap_or_else(|e| panic!("save {out_path}: {e}"));
    println!(
        "imported_azalea_renders_faithfully_to_png: wrote {out_path} (fraction {fraction:.4}, report {report:?})"
    );
    assert!(
        fraction > 0.02,
        "expected >2% non-black pixels after polling for both background parses, got \
         {fraction:.4} — likely a broken importer graph, a parse that never landed, or empty geometry"
    );
}

#[cfg(feature = "gpu-proofs")]
/// Stage-4 production-path proof: render the assembled azalea graph the way
/// a real dropped-in generator LAYER renders it — through
/// [`GeneratorRegistry::create_with_override`] with the import graph as the
/// per-layer override and the imported preset id as `gen_type`. This is the
/// exact per-layer call `GeneratorRenderer::render_all` makes for a layer
/// carrying a `generator_graph`, and it differs from
/// `imported_azalea_renders_faithfully_to_png` (which drives
/// `PresetRuntime::from_def_with_device` directly) in one load-bearing way:
/// `is_watched = false` routes through the **on-demand fusion** attempt
/// (`fused_generator_def_for`) that the raw-def path skips. So this closes
/// the last gap — proving the imported graph survives the fuser and renders
/// through the same code an installed timeline layer hits.
///
/// It also proves the registry-gate is a non-issue: the sanitized preset id
/// (`cc0_oomurasaki_...`) is NOT in the bundled catalog, yet the override
/// def renders anyway because `create_with_override` builds from the def.
#[test]
#[ignore]
fn imported_azalea_renders_through_create_with_override_to_png() {
    use crate::generators::registry::GeneratorRegistry;
    use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use crate::preset_context::PresetContext;
    use crate::render_target::RenderTarget;
    use manifold_gpu::GpuTextureFormat;

    let path = azalea_fixture_path();
    if !path.exists() {
        eprintln!(
            "imported_azalea_renders_through_create_with_override_to_png: fixture not found at {}, skipping",
            path.display()
        );
        return;
    }

    let (def, report) = assemble_import_graph(&path).expect("assemble azalea");
    let preset_id = def
        .preset_metadata
        .as_ref()
        .expect("assembled def carries v2 metadata")
        .id
        .clone();
    println!(
        "create_with_override proof: preset id = {preset_id:?} (NOT in bundled catalog), report {report:?}"
    );

    let (w, h) = (768u32, 768u32);
    let device = crate::test_device();
    let format = GpuTextureFormat::Rgba16Float;
    let registry = GeneratorRegistry::new(format);

    // is_watched = false -> production render path, INCLUDING the on-demand
    // fusion attempt the raw from_def proof never exercised.
    let mut generator = registry
        .create_with_override(device.arc(), &preset_id, Some(&def), w, h, false, None, None)
        .expect(
            "create_with_override must build the imported generator from its override def, \
             even though the preset id is not in the bundled catalog",
        );

    let target = RenderTarget::new(&device, w, h, format, "imported-azalea-layer");
    let ctx = PresetContext {
        time: 0.0,
        beat: 0.0,
        dt: 1.0 / 60.0,
        width: w,
        height: h,
        output_width: w,
        output_height: h,
        aspect: 1.0,
        owner_key: 0,
        is_clip_level: false,
        frame_count: 0,
        anim_progress: 0.0,
        trigger_count: 0,
    };

    // BUG-100: same premature-convergence fix as
    // `imported_azalea_renders_faithfully_to_png` — `fraction > 0.02`
    // alone is satisfied by the material's ambient floor before the
    // texture decode lands, so require the readback to be stable
    // across `STABLE_STREAK` consecutive frames too.
    const STABLE_STREAK: u32 = 3;
    let max_attempts = 200;
    let mut rgba = Vec::new();
    let mut fraction = 0.0f64;
    let mut prev_rgba: Option<Vec<u8>> = None;
    let mut stable_count = 0u32;
    for attempt in 0..max_attempts {
        {
            let mut enc = device.create_encoder("imported-azalea-layer-render");
            {
                let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
                generator.render(
                    &mut gpu,
                    &target.texture,
                    &ctx,
                    &manifold_core::params::ParamManifest::default(),
                );
            }
            enc.commit_and_wait_completed();
        }

        let bytes_per_row = w * 8;
        let total_bytes = u64::from(h * bytes_per_row);
        let readback_buf = device.create_buffer_shared(total_bytes);
        let mut readback_enc = device.create_encoder("imported-azalea-layer-readback");
        readback_enc.copy_texture_to_buffer(&target.texture, &readback_buf, w, h, bytes_per_row);
        readback_enc.commit_and_wait_completed();

        let ptr = readback_buf.mapped_ptr().expect("shared readback");
        let halves: &[u16] =
            unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };

        rgba = Vec::with_capacity((w * h * 4) as usize);
        let mut non_black = 0usize;
        for px in halves.chunks_exact(4) {
            let r = tonemap_channel(half_to_f32(px[0]));
            let g = tonemap_channel(half_to_f32(px[1]));
            let b = tonemap_channel(half_to_f32(px[2]));
            if r != 0 || g != 0 || b != 0 {
                non_black += 1;
            }
            rgba.push(r);
            rgba.push(g);
            rgba.push(b);
            let a = half_to_f32(px[3]).clamp(0.0, 1.0);
            rgba.push((a * 255.0).round() as u8);
        }
        fraction = non_black as f64 / (w * h) as f64;

        if fraction > 0.02 && prev_rgba.as_deref() == Some(rgba.as_slice()) {
            stable_count += 1;
        } else {
            stable_count = 0;
        }
        prev_rgba = Some(rgba.clone());

        if stable_count >= STABLE_STREAK {
            println!(
                "create_with_override proof: converged on attempt {attempt} (non-black {fraction:.4}, stable for {STABLE_STREAK} frames)"
            );
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    assert!(
        fraction > 0.02,
        "expected the imported layer to render non-black through create_with_override, got \
         {fraction:.4} — a broken fusion pass, a parse that never landed, or a registry-gate regression"
    );

    let out_path = std::env::var("MESH_SNAP_OUT")
        .unwrap_or_else(|_| "target/mesh-snap/imported_azalea_layer.png".to_string());
    if let Some(parent) = std::path::Path::new(&out_path).parent() {
        std::fs::create_dir_all(parent).expect("create output dir");
    }
    image::save_buffer(&out_path, &rgba, w, h, image::ExtendedColorType::Rgba8)
        .unwrap_or_else(|e| panic!("save {out_path}: {e}"));
    println!("create_with_override proof: wrote {out_path} (fraction {fraction:.4})");
}

#[cfg(feature = "gpu-proofs")]
fn box_animated_fixture_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/gltf/khronos/BoxAnimated.glb")
}

/// GLTF_ANIMATION_DESIGN.md A1's `duration_s`, read straight off the
/// assembled graph's animated `node.gltf_animation_source` — so the
/// render tests below can pick exact `(beats, seconds)` pairs that
/// land on a specific `progress` through the DEFAULT (unwired) beat
/// drive, without wiring a scrub source into the graph.
#[cfg(feature = "gpu-proofs")]
fn box_animated_duration_s() -> f32 {
    let summary = gltf_load::gltf_import_summary(&box_animated_fixture_path())
        .expect("parse BoxAnimated.glb");
    summary
        .materials
        .iter()
        .find_map(|m| m.animations.first().and_then(|a| a.as_ref()))
        .expect("BoxAnimated.glb must resolve an animation on one of its materials")
        .duration_s
}

/// Render-and-look finding (this session, verified with a throwaway
/// probe test before writing this): `BoxAnimated.glb`'s "inner_box"
/// (the only animated object) sits almost entirely INSIDE the
/// stationary "outer_box" shell — under the importer's DEFAULT
/// synthesized camera (a ~17-degree-above-horizon orbit shot tuned
/// for typical hero objects) the inner box is visible only as a
/// sliver of color peeking through a gap at the very top rim. Once
/// its translation lifts it more than ~0.4 world units (well before
/// progress=0.25 in this clip), it moves entirely out of that sliver
/// and the rendered frame goes back to "no visible inner box" —
/// IDENTICAL to every other progress where it's equally absent from
/// the sliver. A default-camera four-phase test would (and, before
/// this fix, DID) come back pixel-identical for 3 of the 4 phases —
/// not a wiring bug, a camera-framing fact about this specific asset
/// discovered by rendering and looking, not assumed. This override
/// re-points the SAME synthesized camera near-vertical (looking down
/// through the shell's open top — confirmed with the probe render:
/// the inner box's full motion and rotation are clearly visible from
/// here) via its own card bindings (`cam_tilt`/`cam_dist` — NOT the
/// raw node params, which the binding evaluation overwrites every
/// frame with its own default_value regardless of what the node
/// param says). Test-only instrumentation — never a change to the
/// production import default.
#[cfg(feature = "gpu-proofs")]
fn point_camera_down_to_see_inner_box(def: &mut manifold_core::effect_graph_def::EffectGraphDef) {
    let meta = def.preset_metadata.as_mut().expect("import graph carries v2 metadata");
    for b in meta.bindings.iter_mut() {
        match b.id.as_str() {
            "cam_tilt" => b.default_value = 1.4,
            "cam_dist" => b.default_value = 6.0,
            _ => {}
        }
    }
}

/// Render the assembled `def` at a chosen `progress` (via the
/// `node.gltf_animation_source` default beat-drive: `progress =
/// wrap(beats * rate / (duration_s * beats_per_second))` with
/// `rate=1.0` and the `beats_per_second=2.0` fallback this test picks
/// by setting `seconds = beats * 0.5` — see
/// `gltf_animation_source::default_progress`), polling for the
/// background mesh-parse to converge (same BUG-100 double-condition
/// convergence loop `imported_azalea_renders_faithfully_to_png` uses).
/// Returns the tonemapped RGBA8 buffer.
#[cfg(feature = "gpu-proofs")]
#[allow(clippy::too_many_arguments)]
fn render_box_animated_at_progress(
    def: manifold_core::effect_graph_def::EffectGraphDef,
    w: u32,
    h: u32,
    progress: f32,
    duration_s: f32,
) -> Vec<u8> {
    use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use crate::preset_context::PresetContext;
    use crate::render_target::RenderTarget;
    use manifold_gpu::GpuTextureFormat;

    let beats = progress * duration_s * 2.0;
    let seconds = (beats * 0.5) as f64;

    let device = crate::test_device();
    let format = GpuTextureFormat::Rgba16Float;
    let registry = PrimitiveRegistry::with_builtin();
    let mut generator =
        PresetRuntime::from_def_with_device(def, &registry, device.arc(), w, h, format, None)
            .expect("BoxAnimated graph must build through PresetRuntime::from_def_with_device");
    let target = RenderTarget::new(&device, w, h, format, "box-animated");
    let ctx = PresetContext {
        time: seconds,
        beat: beats as f64,
        dt: 1.0 / 60.0,
        width: w,
        height: h,
        output_width: w,
        output_height: h,
        aspect: 1.0,
        owner_key: 0,
        is_clip_level: false,
        frame_count: 0,
        anim_progress: 0.0,
        trigger_count: 0,
    };

    const STABLE_STREAK: u32 = 3;
    let max_attempts = 200;
    let mut rgba = Vec::new();
    let mut prev_rgba: Option<Vec<u8>> = None;
    let mut stable_count = 0u32;
    for _attempt in 0..max_attempts {
        {
            let mut enc = device.create_encoder("box-animated-render");
            {
                let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
                generator.render(
                    &mut gpu,
                    &target.texture,
                    &ctx,
                    &manifold_core::params::ParamManifest::default(),
                );
            }
            enc.commit_and_wait_completed();
        }
        let bytes_per_row = w * 8;
        let readback_buf = device.create_buffer_shared(u64::from(h * bytes_per_row));
        let mut readback_enc = device.create_encoder("box-animated-readback");
        readback_enc.copy_texture_to_buffer(&target.texture, &readback_buf, w, h, bytes_per_row);
        readback_enc.commit_and_wait_completed();
        let ptr = readback_buf.mapped_ptr().expect("shared readback");
        let halves: &[u16] =
            unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };
        rgba = Vec::with_capacity((w * h * 4) as usize);
        let mut non_black = 0usize;
        for px in halves.chunks_exact(4) {
            let r = tonemap_channel(half_to_f32(px[0]));
            let g = tonemap_channel(half_to_f32(px[1]));
            let b = tonemap_channel(half_to_f32(px[2]));
            if r != 0 || g != 0 || b != 0 {
                non_black += 1;
            }
            rgba.push(r);
            rgba.push(g);
            rgba.push(b);
            let a = half_to_f32(px[3]).clamp(0.0, 1.0);
            rgba.push((a * 255.0).round() as u8);
        }
        let fraction = non_black as f64 / (w * h) as f64;
        if fraction > 0.02 && prev_rgba.as_deref() == Some(rgba.as_slice()) {
            stable_count += 1;
        } else {
            stable_count = 0;
        }
        prev_rgba = Some(rgba.clone());
        if stable_count >= STABLE_STREAK {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    rgba
}

/// A1 gate item 1 (four-phase PNG goldens): `BoxAnimated.glb`
/// imported and rendered headless at progress 0 / 0.25 / 0.5 / 0.75
/// must produce four visibly distinct frames — the box's translation
/// (and, past progress ~0.34, its rotation too) actually moves it
/// across frame. Written to `tests/fixtures/gltf/goldens/` following
/// the suite's existing `box_animated.png` naming (that file is the
/// STATIC single-frame conformance golden from GLB_CONFORMANCE —
/// this test's four phase-suffixed files are new, additive, and
/// never overwrite it).
#[cfg(feature = "gpu-proofs")]
#[test]
fn box_animated_four_phase_pngs_are_visibly_distinct() {
    let path = box_animated_fixture_path();
    if !path.exists() {
        eprintln!(
            "box_animated_four_phase_pngs_are_visibly_distinct: fixture not found at {}, skipping",
            path.display()
        );
        return;
    }
    let duration_s = box_animated_duration_s();
    let (w, h) = (256u32, 256u32);
    let phases = [0.0f32, 0.25, 0.5, 0.75];
    let mut frames = Vec::new();
    for &p in &phases {
        let (mut def, _report) = assemble_import_graph(&path).expect("assemble BoxAnimated");
        point_camera_down_to_see_inner_box(&mut def);
        frames.push(render_box_animated_at_progress(def, w, h, p, duration_s));
    }

    let out_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/gltf/goldens");
    std::fs::create_dir_all(&out_dir).expect("create goldens dir");
    for (p, rgba) in phases.iter().zip(frames.iter()) {
        let out_path = out_dir.join(format!("box_animated_p{:03}.png", (p * 100.0).round() as u32));
        image::save_buffer(&out_path, rgba, w, h, image::ExtendedColorType::Rgba8)
            .unwrap_or_else(|e| panic!("save {}: {e}", out_path.display()));
    }

    for i in 0..frames.len() {
        for j in (i + 1)..frames.len() {
            assert_ne!(
                frames[i], frames[j],
                "progress {} and progress {} rendered byte-identical frames — the clip isn't animating",
                phases[i], phases[j]
            );
        }
    }
}

/// A1 gate item 2 (round-trip): build the import graph, serialize it
/// through the V1 JSON path, reload, re-render at progress 0.5, and
/// confirm a pixel match against the pre-reload progress-0.5 render —
/// proves `Table` params (the keyframe tracks) and the new
/// `node.gltf_animation_source` node type survive save→reload AND
/// stay live (not just structurally present — STANDARD §5's
/// "modulation live after reload" gate, same doctrine as
/// `round_trip_preserves_map_wires_and_sun_coherence_bindings`).
#[cfg(feature = "gpu-proofs")]
#[test]
fn box_animated_round_trip_preserves_animation_and_renders_identically() {
    let path = box_animated_fixture_path();
    if !path.exists() {
        eprintln!(
            "box_animated_round_trip_preserves_animation_and_renders_identically: \
             fixture not found at {}, skipping",
            path.display()
        );
        return;
    }
    let duration_s = box_animated_duration_s();
    let (w, h) = (256u32, 256u32);

    let (mut def, _report) = assemble_import_graph(&path).expect("assemble BoxAnimated");
    point_camera_down_to_see_inner_box(&mut def);
    let json = serde_json::to_string(&def).expect("serialize EffectGraphDef");
    let reloaded: EffectGraphDef =
        serde_json::from_str(&json).expect("deserialize EffectGraphDef");
    assert_eq!(def, reloaded, "round trip must be byte-for-byte structurally identical");

    let before = render_box_animated_at_progress(def, w, h, 0.5, duration_s);
    let after = render_box_animated_at_progress(reloaded, w, h, 0.5, duration_s);
    assert_eq!(
        before, after,
        "progress-0.5 render must pixel-match before and after a save/reload round trip"
    );
}

// ─── GLTF_ANIMATION_DESIGN.md A2 gate (CesiumMan/Fox skin deformation) ─

#[cfg(feature = "gpu-proofs")]
fn khronos_fixture_path(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/gltf/khronos")
        .join(name)
}

/// Read `duration_s` straight off the assembled graph's
/// `node.gltf_skeleton_pose` node (rather than recomputing it) — the
/// same "read the built graph's own param" convention
/// `box_animated_duration_s` uses for A1's animation source. Every
/// object's producer nodes live INSIDE its group box (`EffectGraphNode::group`),
/// not at the top level, so this recurses. Panics if the asset didn't
/// resolve a skin onto any object (a real bug this test wants to
/// catch loudly, not skip past).
#[cfg(feature = "gpu-proofs")]
fn skeleton_pose_duration_s(def: &manifold_core::effect_graph_def::EffectGraphDef) -> f32 {
    fn find(nodes: &[manifold_core::effect_graph_def::EffectGraphNode]) -> Option<f32> {
        for node in nodes {
            if node.type_id.as_str() == "node.gltf_skeleton_pose"
                && let Some(manifold_core::effect_graph_def::SerializedParamValue::Float { value }) =
                    node.params.get("duration_s")
            {
                return Some(*value);
            }
            if let Some(group) = &node.group
                && let Some(v) = find(&group.nodes)
            {
                return Some(v);
            }
        }
        None
    }
    find(&def.nodes).expect("assembled graph has no node.gltf_skeleton_pose with a duration_s param")
}

/// Like [`skeleton_pose_duration_s`] but for the general hostile shelf
/// (IMPORT_ANYTHING_WAVE_DESIGN.md W1 added a plain unrigged fixture —
/// `webp_texture.glb` — alongside the skinned Mixamo-shaped ones):
/// `0.0` when the asset has no skeleton pose at all, which
/// `render_skinned_import_at_progress` renders as a static rest pose
/// regardless of `progress`. The strict panicking variant stays for
/// call sites that know their fixture is always skinned.
#[cfg(feature = "gpu-proofs")]
fn skeleton_pose_duration_s_or_static(
    def: &manifold_core::effect_graph_def::EffectGraphDef,
) -> f32 {
    fn find(nodes: &[manifold_core::effect_graph_def::EffectGraphNode]) -> Option<f32> {
        for node in nodes {
            if node.type_id.as_str() == "node.gltf_skeleton_pose"
                && let Some(manifold_core::effect_graph_def::SerializedParamValue::Float { value }) =
                    node.params.get("duration_s")
            {
                return Some(*value);
            }
            if let Some(group) = &node.group
                && let Some(v) = find(&group.nodes)
            {
                return Some(v);
            }
        }
        None
    }
    find(&def.nodes).unwrap_or(0.0)
}

/// Render an assembled skinned-import `def` at a chosen `progress`
/// (via `node.gltf_skeleton_pose`'s default beat-drive — identical
/// formula/convergence-polling as `render_box_animated_at_progress`,
/// generalized past the BoxAnimated-specific camera override since
/// CesiumMan/Fox frame fine under the importer's default camera).
#[cfg(feature = "gpu-proofs")]
fn render_skinned_import_at_progress(
    def: manifold_core::effect_graph_def::EffectGraphDef,
    w: u32,
    h: u32,
    progress: f32,
    duration_s: f32,
) -> Vec<u8> {
    use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use crate::preset_context::PresetContext;
    use crate::render_target::RenderTarget;
    use manifold_gpu::GpuTextureFormat;

    let beats = progress * duration_s * 2.0;
    let seconds = (beats * 0.5) as f64;

    let device = crate::test_device();
    let format = GpuTextureFormat::Rgba16Float;
    let registry = PrimitiveRegistry::with_builtin();
    let mut generator =
        PresetRuntime::from_def_with_device(def, &registry, device.arc(), w, h, format, None)
            .expect("skinned import graph must build through PresetRuntime::from_def_with_device");
    let target = RenderTarget::new(&device, w, h, format, "skinned-import");
    let ctx = PresetContext {
        time: seconds,
        beat: beats as f64,
        dt: 1.0 / 60.0,
        width: w,
        height: h,
        output_width: w,
        output_height: h,
        aspect: 1.0,
        owner_key: 0,
        is_clip_level: false,
        frame_count: 0,
        anim_progress: 0.0,
        trigger_count: 0,
    };

    const STABLE_STREAK: u32 = 3;
    let max_attempts = 200;
    let mut rgba = Vec::new();
    let mut prev_rgba: Option<Vec<u8>> = None;
    let mut stable_count = 0u32;
    for _attempt in 0..max_attempts {
        {
            let mut enc = device.create_encoder("skinned-import-render");
            {
                let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
                generator.render(
                    &mut gpu,
                    &target.texture,
                    &ctx,
                    &manifold_core::params::ParamManifest::default(),
                );
            }
            enc.commit_and_wait_completed();
        }
        let bytes_per_row = w * 8;
        let readback_buf = device.create_buffer_shared(u64::from(h * bytes_per_row));
        let mut readback_enc = device.create_encoder("skinned-import-readback");
        readback_enc.copy_texture_to_buffer(&target.texture, &readback_buf, w, h, bytes_per_row);
        readback_enc.commit_and_wait_completed();
        let ptr = readback_buf.mapped_ptr().expect("shared readback");
        let halves: &[u16] =
            unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };
        rgba = Vec::with_capacity((w * h * 4) as usize);
        let mut non_black = 0usize;
        for px in halves.chunks_exact(4) {
            let r = tonemap_channel(half_to_f32(px[0]));
            let g = tonemap_channel(half_to_f32(px[1]));
            let b = tonemap_channel(half_to_f32(px[2]));
            if r != 0 || g != 0 || b != 0 {
                non_black += 1;
            }
            rgba.push(r);
            rgba.push(g);
            rgba.push(b);
            let a = half_to_f32(px[3]).clamp(0.0, 1.0);
            rgba.push((a * 255.0).round() as u8);
        }
        let fraction = non_black as f64 / (w * h) as f64;
        if fraction > 0.02 && prev_rgba.as_deref() == Some(rgba.as_slice()) {
            stable_count += 1;
        } else {
            stable_count = 0;
        }
        prev_rgba = Some(rgba.clone());
        if stable_count >= STABLE_STREAK {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    rgba
}

/// A2 gate: `CesiumMan.glb` and `Fox.glb` — a real rigged, skinned,
/// animated character each — must render four VISIBLY DISTINCT frames
/// across the clip, proving the skin actually deforms (not just a
/// rigid object moving, which A1 already proved for `BoxAnimated`).
/// Written to `tests/fixtures/gltf/goldens/` alongside the A1 goldens.
#[cfg(feature = "gpu-proofs")]
#[test]
fn skinned_characters_render_four_visibly_distinct_deformed_poses() {
    let (w, h) = (256u32, 256u32);
    let out_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/gltf/goldens");
    std::fs::create_dir_all(&out_dir).expect("create goldens dir");

    for asset in ["CesiumMan.glb", "Fox.glb"] {
        let path = khronos_fixture_path(asset);
        if !path.exists() {
            eprintln!(
                "skinned_characters_render_four_visibly_distinct_deformed_poses: \
                 fixture not found at {}, skipping {asset}",
                path.display()
            );
            continue;
        }
        let (def, _report) = assemble_import_graph(&path).expect("assemble skinned import");
        let duration_s = skeleton_pose_duration_s(&def);

        let phases = [0.0f32, 0.25, 0.5, 0.75];
        let mut frames = Vec::new();
        for &p in &phases {
            let (def, _report) = assemble_import_graph(&path).expect("assemble skinned import");
            frames.push(render_skinned_import_at_progress(def, w, h, p, duration_s));
        }

        let stem = asset.trim_end_matches(".glb").to_lowercase();
        for (p, rgba) in phases.iter().zip(frames.iter()) {
            let out_path =
                out_dir.join(format!("{stem}_skin_p{:03}.png", (p * 100.0).round() as u32));
            image::save_buffer(&out_path, rgba, w, h, image::ExtendedColorType::Rgba8)
                .unwrap_or_else(|e| panic!("save {}: {e}", out_path.display()));
        }

        for i in 0..frames.len() {
            for j in (i + 1)..frames.len() {
                assert_ne!(
                    frames[i], frames[j],
                    "{asset}: progress {} and progress {} rendered byte-identical frames — \
                     the skin isn't deforming",
                    phases[i], phases[j]
                );
            }
        }
    }
}

/// GLTF_ANIM_RUNTIME_V2_DESIGN.md D4 (P3) gate: a HELD-OUT multi-node
/// rigid-animated fixture — `CesiumMilkTruck.glb` — must render four
/// pairwise-distinct poses through the full import path, proving the
/// node-slot palette (not a fixture-shaped special case). This asset
/// was NEVER inspected or developed against while building D4; it was
/// found by running the real `gltf_import_summary` resolver over
/// every khronos fixture and grepping its own new report line
/// ("rigid animation composed across N nodes via the node-slot
/// palette") for a hit — `CesiumMilkTruck.glb` material 0 resolves to
/// exactly 2 contributing nodes (the truck body/frame plus a wheel
/// group), at least one animated (the wheel spin), neither skinned —
/// the textbook D4 shape. `BoxAnimated.glb`'s own four-phase gate
/// (`box_animated_four_phase_pngs_are_visibly_distinct`, unchanged by
/// this phase) stays the single-node regression proof: its translation/
/// rotation split across ONE mesh node + ONE ancestor is
/// non-ambiguous, so it still resolves through the pre-D4
/// `GltfObjectAnimation` TRS-track path, not the node-slot palette —
/// D4 only reroutes the genuinely multi-node or ambiguous-ancestor
/// cases.
#[cfg(feature = "gpu-proofs")]
#[test]
fn rigid_multi_node_held_out_fixture_renders_four_distinct_poses() {
    let (w, h) = (256u32, 256u32);
    let out_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/gltf/goldens");
    std::fs::create_dir_all(&out_dir).expect("create goldens dir");

    let asset = "CesiumMilkTruck.glb";
    let path = khronos_fixture_path(asset);
    if !path.exists() {
        eprintln!(
            "rigid_multi_node_held_out_fixture_renders_four_distinct_poses: \
             fixture not found at {}, skipping",
            path.display()
        );
        return;
    }
    let (def, report) = assemble_import_graph(&path).expect("assemble CesiumMilkTruck import");
    assert!(
        report.report_lines.iter().any(|line| line.contains("rigid animation composed across")),
        "CesiumMilkTruck.glb must resolve at least one object through the D4 node-slot \
         palette — if this fails, the fixture no longer exercises the case this test gates \
         (report: {:?})",
        report.report_lines
    );
    let duration_s = skeleton_pose_duration_s_or_static(&def);
    assert!(duration_s > 0.0, "node-slot object must resolve a positive clip duration");

    let phases = [0.0f32, 0.25, 0.5, 0.75];
    let mut frames = Vec::new();
    for &p in &phases {
        let (def, _report) = assemble_import_graph(&path).expect("assemble CesiumMilkTruck import");
        frames.push(render_skinned_import_at_progress(def, w, h, p, duration_s));
    }

    for (p, rgba) in phases.iter().zip(frames.iter()) {
        let out_path =
            out_dir.join(format!("cesium_milk_truck_rigid_p{:03}.png", (p * 100.0).round() as u32));
        image::save_buffer(&out_path, rgba, w, h, image::ExtendedColorType::Rgba8)
            .unwrap_or_else(|e| panic!("save {}: {e}", out_path.display()));
    }

    for i in 0..frames.len() {
        for j in (i + 1)..frames.len() {
            assert_ne!(
                frames[i], frames[j],
                "{asset}: progress {} and progress {} rendered byte-identical frames — the \
                 node-slot palette isn't animating",
                phases[i], phases[j]
            );
        }
    }
}

/// A2 gate: hot-path check (CLAUDE.md content-thread discipline;
/// STANDARD §5's content-thread gate) on the design doc's two NAMED
/// gate fixtures, `CesiumMan.glb` and `Fox.glb`. Substitute for the
/// `MANIFOLD_RENDER_TRACE`-driven `manifold-app` journey-proof harness
/// (`bug035_verify.rs`/`bug037_verify.rs`'s pattern) — wiring a full
/// content-thread project/layer/generator around an imported glTF
/// asset is real additional infrastructure this phase doesn't build;
/// this measures the actual GPU encode+submit wall-clock cost of the
/// exact render path the gate cares about (per-frame CPU skeleton-pose
/// sampling + the skin_mesh dispatch + render_scene), on a warm
/// `PresetRuntime` built once, looped. `CesiumMan.glb` (14016
/// vertices, one skin, 19 joints) is the largest single skinned mesh
/// among the gate fixtures. Asserts no frame exceeds 20ms across a
/// 30-frame warm loop for either asset.
///
/// Re-derived finding (this session, NOT part of this gate):
/// `BrainStem.glb` — the design doc's named joint-count stress case —
/// is actually a MANY-SMALL-SKINS stress case (24 separate skinned
/// objects, each only 18 joints, not one large palette) that measured
/// a flat ~370ms/frame from frame 0 (not a one-time parse cost — see
/// BUG-190). `CesiumMan`'s own skin_mesh dispatch alone measures
/// ~5-6ms, so this reads as a pre-existing many-object `render_scene`
/// scaling cost (shadow/SSAO passes × object count) rather than a
/// skinning-specific regression, but that's not proven — logged as
/// BUG-190 for a dedicated investigation rather than asserted here
/// against a fixture the doc never named as a mandatory gate.
#[cfg(feature = "gpu-proofs")]
#[test]
fn skinned_import_hot_path_stays_under_20ms_per_frame() {
    use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use crate::preset_context::PresetContext;
    use crate::render_target::RenderTarget;
    use manifold_gpu::GpuTextureFormat;

    for asset in ["CesiumMan.glb", "Fox.glb"] {
        let path = khronos_fixture_path(asset);
        if !path.exists() {
            eprintln!("skinned_import_hot_path_stays_under_20ms_per_frame: fixture not found at {}, skipping {asset}", path.display());
            continue;
        }
        let (def, _report) = assemble_import_graph(&path).expect("assemble skinned import");

        let (w, h) = (512u32, 512u32);
        let device = crate::test_device();
        let format = GpuTextureFormat::Rgba16Float;
        let registry = PrimitiveRegistry::with_builtin();
        let mut generator =
            PresetRuntime::from_def_with_device(def, &registry, device.arc(), w, h, format, None)
                .expect("skinned import graph must build");
        let target = RenderTarget::new(&device, w, h, format, "skinned-hot-path");

        const WARMUP: u32 = 10;
        const MEASURED: u32 = 30;
        let mut max_ms = 0.0f64;
        let mut total_ms = 0.0f64;
        for frame in 0..(WARMUP + MEASURED) {
            let beats = frame as f64 * 0.1;
            let ctx = PresetContext {
                time: beats * 0.5,
                beat: beats,
                dt: 1.0 / 60.0,
                width: w,
                height: h,
                output_width: w,
                output_height: h,
                aspect: 1.0,
                owner_key: 0,
                is_clip_level: false,
                frame_count: frame as i64,
                anim_progress: 0.0,
                trigger_count: 0,
            };
            let start = std::time::Instant::now();
            {
                let mut enc = device.create_encoder("skinned-hot-path");
                {
                    let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
                    generator.render(
                        &mut gpu,
                        &target.texture,
                        &ctx,
                        &manifold_core::params::ParamManifest::default(),
                    );
                }
                enc.commit_and_wait_completed();
            }
            let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
            if frame >= WARMUP {
                max_ms = max_ms.max(elapsed_ms);
                total_ms += elapsed_ms;
            }
        }
        let avg_ms = total_ms / MEASURED as f64;
        println!("{asset}: skinned-import hot path — avg {avg_ms:.2}ms, max {max_ms:.2}ms over {MEASURED} frames");
        assert!(
            max_ms < 20.0,
            "{asset}: a frame took {max_ms:.2}ms (> 20ms budget) — skinning dropped a frame"
        );
    }
}

// ─── GLTF_ANIMATION_DESIGN.md A3 gate (AnimatedMorphCube/MorphStressTest) ─

/// Read `duration_s` straight off the assembled graph's
/// `node.gltf_morph_weights` node — same "read the built graph's own
/// param" convention `skeleton_pose_duration_s` uses for A2. Panics if
/// the asset didn't resolve morph targets onto any object (a real bug
/// this test wants to catch loudly, not skip past).
#[cfg(feature = "gpu-proofs")]
fn morph_weights_duration_s(def: &manifold_core::effect_graph_def::EffectGraphDef) -> f32 {
    fn find(nodes: &[manifold_core::effect_graph_def::EffectGraphNode]) -> Option<f32> {
        for node in nodes {
            if node.type_id.as_str() == "node.gltf_morph_weights"
                && let Some(manifold_core::effect_graph_def::SerializedParamValue::Float { value }) =
                    node.params.get("duration_s")
            {
                return Some(*value);
            }
            if let Some(group) = &node.group
                && let Some(v) = find(&group.nodes)
            {
                return Some(v);
            }
        }
        None
    }
    find(&def.nodes).expect("assembled graph has no node.gltf_morph_weights with a duration_s param")
}

/// Render an assembled morphed-import `def` at a chosen `progress` (via
/// `node.gltf_morph_weights`' default beat-drive) — identical
/// formula/convergence-polling as `render_skinned_import_at_progress`,
/// just against the morph weights node's own `duration_s` instead of
/// the skeleton pose's.
#[cfg(feature = "gpu-proofs")]
fn render_morph_import_at_progress(
    def: manifold_core::effect_graph_def::EffectGraphDef,
    w: u32,
    h: u32,
    progress: f32,
    duration_s: f32,
) -> Vec<u8> {
    use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use crate::preset_context::PresetContext;
    use crate::render_target::RenderTarget;
    use manifold_gpu::GpuTextureFormat;

    let beats = progress * duration_s * 2.0;
    let seconds = (beats * 0.5) as f64;

    let device = crate::test_device();
    let format = GpuTextureFormat::Rgba16Float;
    let registry = PrimitiveRegistry::with_builtin();
    let mut generator =
        PresetRuntime::from_def_with_device(def, &registry, device.arc(), w, h, format, None)
            .expect("morphed import graph must build through PresetRuntime::from_def_with_device");
    let target = RenderTarget::new(&device, w, h, format, "morph-import");
    let ctx = PresetContext {
        time: seconds,
        beat: beats as f64,
        dt: 1.0 / 60.0,
        width: w,
        height: h,
        output_width: w,
        output_height: h,
        aspect: 1.0,
        owner_key: 0,
        is_clip_level: false,
        frame_count: 0,
        anim_progress: 0.0,
        trigger_count: 0,
    };

    const STABLE_STREAK: u32 = 3;
    let max_attempts = 200;
    let mut rgba = Vec::new();
    let mut prev_rgba: Option<Vec<u8>> = None;
    let mut stable_count = 0u32;
    for _attempt in 0..max_attempts {
        {
            let mut enc = device.create_encoder("morph-import-render");
            {
                let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
                generator.render(
                    &mut gpu,
                    &target.texture,
                    &ctx,
                    &manifold_core::params::ParamManifest::default(),
                );
            }
            enc.commit_and_wait_completed();
        }
        let bytes_per_row = w * 8;
        let readback_buf = device.create_buffer_shared(u64::from(h * bytes_per_row));
        let mut readback_enc = device.create_encoder("morph-import-readback");
        readback_enc.copy_texture_to_buffer(&target.texture, &readback_buf, w, h, bytes_per_row);
        readback_enc.commit_and_wait_completed();
        let ptr = readback_buf.mapped_ptr().expect("shared readback");
        let halves: &[u16] =
            unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };
        rgba = Vec::with_capacity((w * h * 4) as usize);
        let mut non_black = 0usize;
        for px in halves.chunks_exact(4) {
            let r = tonemap_channel(half_to_f32(px[0]));
            let g = tonemap_channel(half_to_f32(px[1]));
            let b = tonemap_channel(half_to_f32(px[2]));
            if r != 0 || g != 0 || b != 0 {
                non_black += 1;
            }
            rgba.push(r);
            rgba.push(g);
            rgba.push(b);
            let a = half_to_f32(px[3]).clamp(0.0, 1.0);
            rgba.push((a * 255.0).round() as u8);
        }
        let fraction = non_black as f64 / (w * h) as f64;
        if fraction > 0.02 && prev_rgba.as_deref() == Some(rgba.as_slice()) {
            stable_count += 1;
        } else {
            stable_count = 0;
        }
        prev_rgba = Some(rgba.clone());
        if stable_count >= STABLE_STREAK {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    rgba
}

/// A3 gate (positive): `AnimatedMorphCube.glb` and `MorphStressTest.glb`
/// — imported and rendered headless at four chosen progress values —
/// must each produce four visibly distinct frames, proving the morph
/// targets actually blend (the cube's face genuinely bulges/deforms)
/// rather than producing noise or a frozen base mesh. Written to
/// `tests/fixtures/gltf/goldens/` alongside the A1/A2 goldens.
///
/// Deviation from the phase brief's literal "progress 0/0.25/0.5/0.75"
/// (same "re-derive against the real asset" doctrine the brief's own
/// morph_mesh-shape deviation used): re-derived this session by
/// decoding `MorphStressTest.glb`'s `weight_tracks` output accessor —
/// its "Individuals" clip (`animations[0]`, the only clip A3 samples)
/// is eight SEQUENTIAL narrow pulses, one per target, each ramping
/// 0→1→0 within roughly one keyframe interval and sitting in a
/// near-zero valley the rest of the ~9.37s clip (peaks measured at
/// progress ≈0.057/0.181/0.306/0.431/0.555/0.680/0.804/0.929). The
/// evenly-spaced 0/0.25/0.5/0.75 sample points all land in valleys
/// between pulses — a real content property, not a bug (confirmed:
/// `node.gltf_morph_weights` correctly samples ~0 there). `AnimatedMorphCube`
/// keeps the brief's literal four phases (its 2-target animation is a
/// continuous ramp, not a pulse train, so they land on genuinely
/// different weights). For `MorphStressTest`, four phases are chosen at
/// alternating pulse PEAKS (targets 0/2/4/6) instead, preserving the
/// gate's actual intent — proving the blend genuinely differs across
/// the clip — rather than the literal fraction values, which would
/// prove nothing about this asset's blending.
#[cfg(feature = "gpu-proofs")]
#[test]
fn morph_targets_render_four_visibly_distinct_poses() {
    let (w, h) = (256u32, 256u32);
    let out_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/gltf/goldens");
    std::fs::create_dir_all(&out_dir).expect("create goldens dir");

    let assets: [(&str, [f32; 4]); 2] = [
        ("AnimatedMorphCube.glb", [0.0, 0.25, 0.5, 0.75]),
        // Target 0/2/4/6 peak weight-1.0 progress values (see doc
        // comment above) — spreads across the clip landing ON pulses
        // instead of between them.
        ("MorphStressTest.glb", [0.057, 0.306, 0.555, 0.804]),
    ];

    for (asset, phases) in assets {
        let path = khronos_fixture_path(asset);
        if !path.exists() {
            eprintln!(
                "morph_targets_render_four_visibly_distinct_poses: fixture not found at {}, \
                 skipping {asset}",
                path.display()
            );
            continue;
        }
        let (def, _report) = assemble_import_graph(&path).expect("assemble morphed import");
        let duration_s = morph_weights_duration_s(&def);

        let mut frames = Vec::new();
        for &p in &phases {
            let (def, _report) = assemble_import_graph(&path).expect("assemble morphed import");
            frames.push(render_morph_import_at_progress(def, w, h, p, duration_s));
        }

        let stem = asset.trim_end_matches(".glb").to_lowercase();
        for (p, rgba) in phases.iter().zip(frames.iter()) {
            let out_path =
                out_dir.join(format!("{stem}_morph_p{:03}.png", (p * 100.0).round() as u32));
            image::save_buffer(&out_path, rgba, w, h, image::ExtendedColorType::Rgba8)
                .unwrap_or_else(|e| panic!("save {}: {e}", out_path.display()));
        }

        for i in 0..frames.len() {
            for j in (i + 1)..frames.len() {
                assert_ne!(
                    frames[i], frames[j],
                    "{asset}: progress {} and progress {} rendered byte-identical frames — \
                     the morph targets aren't blending",
                    phases[i], phases[j]
                );
            }
        }
    }
}

/// A3 gate (round-trip): build the morphed import graph, serialize it
/// through the V1 JSON path, reload, re-render at progress 0.5, and
/// confirm a pixel match against the pre-reload progress-0.5 render —
/// proves the `weight_tracks` Table plus the three new node types
/// (`node.gltf_morph_weights`, `node.gltf_morph_deltas_source`,
/// `node.morph_targets_blend`) survive save→reload AND stay live, same
/// doctrine as `box_animated_round_trip_preserves_animation_and_renders_identically`.
#[cfg(feature = "gpu-proofs")]
#[test]
fn morph_targets_round_trip_preserves_weights_and_renders_identically() {
    let path = khronos_fixture_path("AnimatedMorphCube.glb");
    if !path.exists() {
        eprintln!(
            "morph_targets_round_trip_preserves_weights_and_renders_identically: fixture not \
             found at {}, skipping",
            path.display()
        );
        return;
    }
    let (w, h) = (256u32, 256u32);

    let (def, _report) = assemble_import_graph(&path).expect("assemble morphed import");
    let duration_s = morph_weights_duration_s(&def);
    let json = serde_json::to_string(&def).expect("serialize EffectGraphDef");
    let reloaded: EffectGraphDef =
        serde_json::from_str(&json).expect("deserialize EffectGraphDef");
    assert_eq!(def, reloaded, "round trip must be byte-for-byte structurally identical");

    let before = render_morph_import_at_progress(def, w, h, 0.5, duration_s);
    let after = render_morph_import_at_progress(reloaded, w, h, 0.5, duration_s);
    assert_eq!(
        before, after,
        "progress-0.5 render must pixel-match before and after a save/reload round trip"
    );
}

// Only used by the `#[cfg(feature = "gpu-proofs")]` render gates below —
// gated the same way to avoid a dead-code warning on the default
// (GPU-free) test sweep.
#[cfg(feature = "gpu-proofs")]
fn damaged_helmet_fixture_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/gltf/DamagedHelmet.glb")
}

/// Build a minimal in-memory glb: one XZ quad whose UVs live ENTIRELY
/// outside [0,1] (V in [1.25, 1.75]) textured by an embedded 2×2 PNG —
/// top row BLUE, bottom row RED. Under the glTF default sampler
/// (REPEAT) V wraps to [0.25, 0.75] and samples the TOP (blue) row;
/// under ClampToEdge every sample pins to the BOTTOM (red) row. The
/// out-of-range-UV regression fixture for
/// `material_maps_repeat_out_of_range_uvs`.
#[cfg(feature = "gpu-proofs")]
fn build_out_of_range_uv_glb() -> Vec<u8> {
    let positions: [[f32; 3]; 4] =
        [[-1.0, 0.0, -1.0], [1.0, 0.0, -1.0], [1.0, 0.0, 1.0], [-1.0, 0.0, 1.0]];
    let normals: [[f32; 3]; 4] = [[0.0, 1.0, 0.0]; 4];
    // V spans [1.05, 1.45]: REPEAT wraps to [0.05, 0.45] — entirely
    // inside the TOP (blue) row of the 2×2 texture. Clamp pins to
    // V = 1.0, the BOTTOM (red) edge row. (First cut used [1.25, 1.75],
    // which wraps across BOTH rows and reads mixed — a fixture bug,
    // not a sampler bug.)
    let uvs: [[f32; 2]; 4] = [[0.25, 1.05], [0.75, 1.05], [0.75, 1.45], [0.25, 1.45]];
    let indices: [u16; 6] = [0, 2, 1, 0, 3, 2];

    // 2×2 PNG: row 0 (top) blue, row 1 red.
    let mut png = Vec::new();
    {
        use image::ImageEncoder;
        let pixels: [u8; 16] =
            [0, 0, 255, 255, 0, 0, 255, 255, 255, 0, 0, 255, 255, 0, 0, 255];
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(&pixels, 2, 2, image::ExtendedColorType::Rgba8)
            .expect("encode fixture png");
    }

    let mut bin: Vec<u8> = Vec::new();
    let pos_off = bin.len();
    bin.extend(positions.iter().flatten().flat_map(|f| f.to_le_bytes()));
    let norm_off = bin.len();
    bin.extend(normals.iter().flatten().flat_map(|f| f.to_le_bytes()));
    let uv_off = bin.len();
    bin.extend(uvs.iter().flatten().flat_map(|f| f.to_le_bytes()));
    let idx_off = bin.len();
    bin.extend(indices.iter().flat_map(|i| i.to_le_bytes()));
    while !bin.len().is_multiple_of(4) {
        bin.push(0);
    }
    let png_off = bin.len();
    bin.extend_from_slice(&png);
    while !bin.len().is_multiple_of(4) {
        bin.push(0);
    }

    let json = serde_json::json!({
        "asset": {"version": "2.0"},
        "scene": 0,
        "scenes": [{"nodes": [0]}],
        "nodes": [{"mesh": 0}],
        "meshes": [{"primitives": [{
            "attributes": {"POSITION": 0, "NORMAL": 1, "TEXCOORD_0": 2},
            "indices": 3,
            "material": 0
        }]}],
        "materials": [{"pbrMetallicRoughness": {
            "baseColorTexture": {"index": 0},
            "metallicFactor": 0.0,
            "roughnessFactor": 1.0
        }}],
        "textures": [{"source": 0, "sampler": 0}],
        "samplers": [{}],
        "images": [{"bufferView": 4, "mimeType": "image/png"}],
        "accessors": [
            {"bufferView": 0, "componentType": 5126, "count": 4, "type": "VEC3",
             "min": [-1.0, 0.0, -1.0], "max": [1.0, 0.0, 1.0]},
            {"bufferView": 1, "componentType": 5126, "count": 4, "type": "VEC3"},
            {"bufferView": 2, "componentType": 5126, "count": 4, "type": "VEC2"},
            {"bufferView": 3, "componentType": 5123, "count": 6, "type": "SCALAR"}
        ],
        "bufferViews": [
            {"buffer": 0, "byteOffset": pos_off, "byteLength": 48},
            {"buffer": 0, "byteOffset": norm_off, "byteLength": 48},
            {"buffer": 0, "byteOffset": uv_off, "byteLength": 32},
            {"buffer": 0, "byteOffset": idx_off, "byteLength": 12},
            {"buffer": 0, "byteOffset": png_off, "byteLength": png.len()}
        ],
        "buffers": [{"byteLength": bin.len()}]
    });
    let mut json_bytes = serde_json::to_vec(&json).expect("fixture json");
    while !json_bytes.len().is_multiple_of(4) {
        json_bytes.push(b' ');
    }

    let total = 12 + 8 + json_bytes.len() + 8 + bin.len();
    let mut glb = Vec::with_capacity(total);
    glb.extend_from_slice(b"glTF");
    glb.extend_from_slice(&2u32.to_le_bytes());
    glb.extend_from_slice(&(total as u32).to_le_bytes());
    glb.extend_from_slice(&(json_bytes.len() as u32).to_le_bytes());
    glb.extend_from_slice(b"JSON");
    glb.extend_from_slice(&json_bytes);
    glb.extend_from_slice(&(bin.len() as u32).to_le_bytes());
    glb.extend_from_slice(b"BIN\0");
    glb.extend_from_slice(&bin);
    glb
}

/// Regression gate for the 2026-07-15 striped-helmet bug: material maps
/// must sample with REPEAT wrapping (the glTF default sampler), not the
/// envmap's clamp-V. The fixture quad's V coords are entirely in
/// [1.25, 1.75]: REPEAT reads the texture's blue top row; the broken
/// clamp pinned every sample to the red bottom edge row. Asserts the
/// rendered quad is blue-dominant. Run deliberately (gpu-proofs).
#[cfg(feature = "gpu-proofs")]
#[test]
fn material_maps_repeat_out_of_range_uvs() {
    use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use crate::preset_context::PresetContext;
    use crate::render_target::RenderTarget;
    use manifold_gpu::GpuTextureFormat;

    let glb = build_out_of_range_uv_glb();
    let path = std::env::temp_dir().join("manifold_uv_wrap_regression.glb");
    std::fs::write(&path, &glb).expect("write temp fixture");

    let (def, _report) = assemble_import_graph(&path).expect("assemble uv-wrap fixture");

    let (w, h) = (128u32, 128u32);
    let device = crate::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let mut generator = PresetRuntime::from_def_with_device(
        def,
        &registry,
        device.arc(),
        w,
        h,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .expect("uv-wrap fixture builds through PresetRuntime");
    let target = RenderTarget::new(&device, w, h, GpuTextureFormat::Rgba16Float, "uv-wrap");
    let ctx = PresetContext {
        time: 0.0,
        beat: 0.0,
        dt: 1.0 / 60.0,
        width: w,
        height: h,
        output_width: w,
        output_height: h,
        aspect: 1.0,
        owner_key: 0,
        is_clip_level: false,
        frame_count: 0,
        anim_progress: 0.0,
        trigger_count: 0,
    };

    // Poll until the background mesh+texture decodes land (byte-stable,
    // non-black — the BUG-100 double condition).
    let mut rgb_sum = [0.0f32; 3];
    let mut prev: Option<Vec<u8>> = None;
    let mut stable = 0u32;
    let mut converged = false;
    for _ in 0..200 {
        {
            let mut enc = device.create_encoder("uv-wrap-render");
            {
                let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
                generator.render(
                    &mut gpu,
                    &target.texture,
                    &ctx,
                    &manifold_core::params::ParamManifest::default(),
                );
            }
            enc.commit_and_wait_completed();
        }
        let bytes_per_row = w * 8;
        let buf = device.create_buffer_shared(u64::from(h * bytes_per_row));
        let mut renc = device.create_encoder("uv-wrap-readback");
        renc.copy_texture_to_buffer(&target.texture, &buf, w, h, bytes_per_row);
        renc.commit_and_wait_completed();
        let ptr = buf.mapped_ptr().expect("shared readback");
        let halves: &[u16] =
            unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };
        rgb_sum = [0.0; 3];
        let mut raw = Vec::with_capacity(halves.len() * 2);
        for px in halves.chunks_exact(4) {
            rgb_sum[0] += half_to_f32(px[0]).max(0.0);
            rgb_sum[1] += half_to_f32(px[1]).max(0.0);
            rgb_sum[2] += half_to_f32(px[2]).max(0.0);
            raw.extend(px.iter().flat_map(|v| v.to_le_bytes()));
        }
        let non_black = rgb_sum.iter().sum::<f32>() > 1.0;
        if non_black && prev.as_deref() == Some(raw.as_slice()) {
            stable += 1;
        } else {
            stable = 0;
        }
        prev = Some(raw);
        if stable >= 3 {
            converged = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(converged, "uv-wrap fixture render never stabilized non-black");
    assert!(
        rgb_sum[2] > rgb_sum[0] * 2.0,
        "out-of-range V must WRAP to the blue top row, not clamp to the red \
         bottom edge: sum RGB = {rgb_sum:?}"
    );
}

#[cfg(feature = "gpu-proofs")]
fn amg_gt3_fixture_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/gltf/mercedes-amg_gt3__www.vecarz.com.glb")
}

/// F-P4 held-out-input gate (DESIGN_DOC_STANDARD.md §5): Khronos
/// `DamagedHelmet.glb` (CC-BY 4.0 — attribution in
/// `tests/fixtures/gltf/README.md`) carries all five glTF PBR map types
/// and was never used to develop this importer — the fixture-overfitting
/// check. Must import with every one of F-P2's four new map ports wired
/// (asserted by port name), render headless through the real
/// `PresetRuntime::from_def_with_device` + `render()` path without error,
/// and produce a non-degenerate frame: mean luminance strictly between
/// 0.02 and 0.98 (catches both an all-black failure — e.g. a stuck
/// background decode — and a blown-out one — e.g. a light-leak from a
/// broken IBL term — without judging the LOOK, which is Peter's L4 call).
/// Needs a GPU device: run deliberately with `--features gpu-proofs`.
#[cfg(feature = "gpu-proofs")]
#[test]
fn damaged_helmet_imports_wires_all_maps_and_renders_non_degenerate() {
    use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use crate::preset_context::PresetContext;
    use crate::render_target::RenderTarget;
    use manifold_core::flatten::flatten_groups;
    use manifold_gpu::GpuTextureFormat;

    let path = damaged_helmet_fixture_path();
    if !path.exists() {
        eprintln!(
            "damaged_helmet_imports_wires_all_maps_and_renders_non_degenerate: fixture not \
             found at {}, skipping",
            path.display()
        );
        return;
    }

    let (def, report) = assemble_import_graph(&path).expect("assemble DamagedHelmet");
    println!("DamagedHelmet import report: {report:?}");
    assert_eq!(report.object_count, 1, "DamagedHelmet is a single-material model");

    // Every one of F-P2's four new map ports must be wired, by name —
    // not just "the import didn't error". SCENE_OBJECT_AND_PANEL_V2_DESIGN
    // D1/D3: these wire into the object's `node.scene_object` bind node
    // now, not directly into `render` — `render` itself only ever sees
    // the single `object_0` port.
    let flat = flatten_groups(&def).expect("DamagedHelmet import graph flattens");
    let scene_object_id =
        flat.nodes.iter().find(|n| n.type_id == "node.scene_object").expect("scene_object bind node").id;
    let scene_object_ports: std::collections::HashSet<String> = flat
        .wires
        .iter()
        .filter(|w| w.to_node == scene_object_id)
        .map(|w| w.to_port.clone())
        .collect();
    for port in ["base_color_map", "normal_map", "mr_map", "occlusion_map", "emissive_map"] {
        assert!(
            scene_object_ports.contains(port),
            "DamagedHelmet must wire `{port}` — carries all five glTF PBR maps; got {scene_object_ports:?}"
        );
    }

    let (w, h) = (512u32, 512u32);
    let device = crate::test_device();
    let format = GpuTextureFormat::Rgba16Float;
    let registry = PrimitiveRegistry::with_builtin();
    let mut generator =
        PresetRuntime::from_def_with_device(def, &registry, device.arc(), w, h, format, None)
            .expect("DamagedHelmet import graph must build through PresetRuntime::from_def_with_device");
    let target = RenderTarget::new(&device, w, h, format, "damaged-helmet");
    let ctx = PresetContext {
        time: 0.0,
        beat: 0.0,
        dt: 1.0 / 60.0,
        width: w,
        height: h,
        output_width: w,
        output_height: h,
        aspect: 1.0,
        owner_key: 0,
        is_clip_level: false,
        frame_count: 0,
        anim_progress: 0.0,
        trigger_count: 0,
    };

    // Same convergence-polling loop as `imported_azalea_renders_faithfully_to_png`
    // — background texture/mesh decodes need to land before the readback
    // means anything. Byte-identical across STABLE_STREAK consecutive
    // frames is the completion signal, but (BUG-100) byte-identical
    // alone isn't enough: DamagedHelmet wires FIVE background texture
    // decodes (base-color/normal/mr/occlusion/emissive, each its own
    // `node.gltf_texture_source` background thread), and
    // `node.gltf_texture_source` emits solid black on every frame until
    // its own decode lands (see that primitive's `run()` step 6) — so a
    // frame where every wired source is STILL mid-decode is *also*
    // byte-stable (three identical black frames) and would falsely read
    // as "converged" before any decode actually finished. Require
    // `fraction > 0.02` (measured, non-black) alongside byte-stability,
    // exactly like the azalea proof's own convergence check.
    const STABLE_STREAK: u32 = 3;
    let max_attempts = 200;
    let mut rgba = Vec::new();
    let mut prev_rgba: Option<Vec<u8>> = None;
    let mut stable_count = 0u32;
    let mut converged = false;
    let mut fraction = 0.0f64;
    for attempt in 0..max_attempts {
        {
            let mut enc = device.create_encoder("damaged-helmet-render");
            {
                let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
                generator.render(
                    &mut gpu,
                    &target.texture,
                    &ctx,
                    &manifold_core::params::ParamManifest::default(),
                );
            }
            enc.commit_and_wait_completed();
        }

        let bytes_per_row = w * 8;
        let total_bytes = u64::from(h * bytes_per_row);
        let readback_buf = device.create_buffer_shared(total_bytes);
        let mut readback_enc = device.create_encoder("damaged-helmet-readback");
        readback_enc.copy_texture_to_buffer(&target.texture, &readback_buf, w, h, bytes_per_row);
        readback_enc.commit_and_wait_completed();

        let ptr = readback_buf.mapped_ptr().expect("shared readback");
        let halves: &[u16] =
            unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };

        rgba = Vec::with_capacity((w * h * 4) as usize);
        let mut non_black = 0usize;
        for px in halves.chunks_exact(4) {
            let r = tonemap_channel(half_to_f32(px[0]));
            let g = tonemap_channel(half_to_f32(px[1]));
            let b = tonemap_channel(half_to_f32(px[2]));
            if r != 0 || g != 0 || b != 0 {
                non_black += 1;
            }
            rgba.push(r);
            rgba.push(g);
            rgba.push(b);
            let a = half_to_f32(px[3]).clamp(0.0, 1.0);
            rgba.push((a * 255.0).round() as u8);
        }
        fraction = non_black as f64 / (w * h) as f64;

        if fraction > 0.02 && prev_rgba.as_deref() == Some(rgba.as_slice()) {
            stable_count += 1;
        } else {
            stable_count = 0;
        }
        prev_rgba = Some(rgba.clone());

        if stable_count >= STABLE_STREAK {
            println!(
                "damaged_helmet_imports_wires_all_maps_and_renders_non_degenerate: converged \
                 on attempt {attempt} (non-black fraction {fraction:.4})"
            );
            converged = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(
        converged,
        "DamagedHelmet render never stabilized non-black after {max_attempts} attempts \
         (last non-black fraction {fraction:.4}) — a background texture decode may be stuck"
    );

    // Mean luminance (Rec. 601 luma over the tonemapped LDR frame),
    // strictly between 0.02 and 0.98 — non-degenerate without judging
    // the look.
    let mut sum_luma = 0.0f64;
    let pixel_count = (w * h) as usize;
    for px in rgba.chunks_exact(4) {
        let (r, g, b) = (px[0] as f64 / 255.0, px[1] as f64 / 255.0, px[2] as f64 / 255.0);
        sum_luma += 0.299 * r + 0.587 * g + 0.114 * b;
    }
    let mean_luminance = sum_luma / pixel_count as f64;
    println!(
        "damaged_helmet_imports_wires_all_maps_and_renders_non_degenerate: mean luminance = {mean_luminance:.4}"
    );

    let out_path = std::env::var("MESH_SNAP_OUT")
        .unwrap_or_else(|_| "target/mesh-snap/damaged_helmet.png".to_string());
    if let Some(parent) = std::path::Path::new(&out_path).parent() {
        std::fs::create_dir_all(parent).expect("create output dir");
    }
    image::save_buffer(&out_path, &rgba, w, h, image::ExtendedColorType::Rgba8)
        .unwrap_or_else(|e| panic!("save {out_path}: {e}"));

    assert!(
        mean_luminance.is_finite() && mean_luminance > 0.02 && mean_luminance < 0.98,
        "expected non-degenerate mean luminance in (0.02, 0.98), got {mean_luminance:.4}"
    );
}

/// Peter-facing look-check sanity, NOT a machine gate (the AMG fixture
/// is untracked, licensing-unverified — vecarz — and absent in a fresh
/// checkout, per the design's explicit "stays untracked" call). Skips
/// cleanly when the file is absent so it never fails CI; when present,
/// only proves the import + render pipeline doesn't error — the actual
/// look (chrome + void + glow) is Peter's in-app L4 check.
#[cfg(feature = "gpu-proofs")]
#[test]
fn amg_gt3_glb_imports_and_renders_without_error_if_present() {
    use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use crate::preset_context::PresetContext;
    use crate::render_target::RenderTarget;
    use manifold_gpu::GpuTextureFormat;

    let path = amg_gt3_fixture_path();
    if !path.exists() {
        println!(
            "amg_gt3_glb_imports_and_renders_without_error_if_present: fixture not tracked, \
             skipping (expected in a fresh checkout — vecarz licensing unverified)"
        );
        return;
    }

    let (def, report) = assemble_import_graph(&path).expect("assemble AMG GT3");
    println!("AMG GT3 import report: {report:?}");
    // GLB_CONFORMANCE_DESIGN.md G-P2 conformance gate: the AMG GT3 has
    // 78 materials with geometry; with the cap dead, ALL of them must be
    // wired.
    assert_eq!(
        report.object_count, 78,
        "AMG GT3 import must be 1:1 — 78 materials, 78 objects, no cap-drop (BUG-163)"
    );
    assert_eq!(report.material_count, 78);

    let (w, h) = (512u32, 512u32);
    let device = crate::test_device();
    let format = GpuTextureFormat::Rgba16Float;
    let registry = PrimitiveRegistry::with_builtin();
    let mut generator =
        PresetRuntime::from_def_with_device(def, &registry, device.arc(), w, h, format, None)
            .expect("AMG GT3 import graph must build through PresetRuntime::from_def_with_device");
    let target = RenderTarget::new(&device, w, h, format, "amg-gt3-sanity");
    let ctx = PresetContext {
        time: 0.0,
        beat: 0.0,
        dt: 1.0 / 60.0,
        width: w,
        height: h,
        output_width: w,
        output_height: h,
        aspect: 1.0,
        owner_key: 0,
        is_clip_level: false,
        frame_count: 0,
        anim_progress: 0.0,
        trigger_count: 0,
    };
    let mut enc = device.create_encoder("amg-gt3-sanity-render");
    {
        let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
        generator.render(&mut gpu, &target.texture, &ctx, &manifold_core::params::ParamManifest::default());
    }
    enc.commit_and_wait_completed();
    println!("amg_gt3_glb_imports_and_renders_without_error_if_present: rendered without error");
}

// ── SCENE_OBJECT_AND_PANEL_V2_DESIGN.md P3 held-out gate: the_rosetta_stone.glb ──
// A fixture v1's briefs never used for emission tests (per the P3 phase
// brief) — proves the importer's NEW scene_object-shaped emission on an
// asset none of the earlier scene_object work was tuned against.

fn rosetta_stone_fixture_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/gltf/the_rosetta_stone.glb")
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

/// GPU render proof: the_rosetta_stone import must actually draw non-
/// degenerate content and be saved to disk for visual inspection.
/// Needs a GPU device: run deliberately with `--features gpu-proofs`.
#[cfg(feature = "gpu-proofs")]
#[test]
fn rosetta_stone_import_renders_gpu_proof() {
    use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use crate::preset_context::PresetContext;
    use crate::render_target::RenderTarget;
    use manifold_gpu::GpuTextureFormat;

    let path = rosetta_stone_fixture_path();
    if !path.exists() {
        eprintln!(
            "rosetta_stone_import_renders_gpu_proof: fixture not found at {}, skipping",
            path.display()
        );
        return;
    }

    let (def, report) = assemble_import_graph(&path).expect("assemble the_rosetta_stone");
    println!("the_rosetta_stone import report: object_count={}", report.object_count);

    let (w, h) = (512u32, 512u32);
    let device = crate::test_device();
    let format = GpuTextureFormat::Rgba16Float;
    let registry = PrimitiveRegistry::with_builtin();
    let mut generator =
        PresetRuntime::from_def_with_device(def, &registry, device.arc(), w, h, format, None)
            .expect("the_rosetta_stone import graph must build through PresetRuntime::from_def_with_device");
    let target = RenderTarget::new(&device, w, h, format, "rosetta-stone");
    let ctx = PresetContext {
        time: 0.0,
        beat: 0.0,
        dt: 1.0 / 60.0,
        width: w,
        height: h,
        output_width: w,
        output_height: h,
        aspect: 1.0,
        owner_key: 0,
        is_clip_level: false,
        frame_count: 0,
        anim_progress: 0.0,
        trigger_count: 0,
    };

    // Same convergence-polling loop as the DamagedHelmet/AMG proofs
    // above — background texture decodes need to land before the
    // readback means anything.
    const STABLE_STREAK: u32 = 3;
    let max_attempts = 200;
    let mut rgba = Vec::new();
    let mut prev_rgba: Option<Vec<u8>> = None;
    let mut stable_count = 0u32;
    let mut converged = false;
    let mut fraction = 0.0f64;
    for attempt in 0..max_attempts {
        {
            let mut enc = device.create_encoder("rosetta-stone-render");
            {
                let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
                generator.render(
                    &mut gpu,
                    &target.texture,
                    &ctx,
                    &manifold_core::params::ParamManifest::default(),
                );
            }
            enc.commit_and_wait_completed();
        }

        let bytes_per_row = w * 8;
        let total_bytes = u64::from(h * bytes_per_row);
        let readback_buf = device.create_buffer_shared(total_bytes);
        let mut readback_enc = device.create_encoder("rosetta-stone-readback");
        readback_enc.copy_texture_to_buffer(&target.texture, &readback_buf, w, h, bytes_per_row);
        readback_enc.commit_and_wait_completed();

        let ptr = readback_buf.mapped_ptr().expect("shared readback");
        let halves: &[u16] =
            unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };

        rgba = Vec::with_capacity((w * h * 4) as usize);
        let mut non_black = 0usize;
        for px in halves.chunks_exact(4) {
            let r = tonemap_channel(half_to_f32(px[0]));
            let g = tonemap_channel(half_to_f32(px[1]));
            let b = tonemap_channel(half_to_f32(px[2]));
            if r != 0 || g != 0 || b != 0 {
                non_black += 1;
            }
            rgba.push(r);
            rgba.push(g);
            rgba.push(b);
            let a = half_to_f32(px[3]).clamp(0.0, 1.0);
            rgba.push((a * 255.0).round() as u8);
        }
        fraction = non_black as f64 / (w * h) as f64;

        if fraction > 0.02 && prev_rgba.as_deref() == Some(rgba.as_slice()) {
            stable_count += 1;
        } else {
            stable_count = 0;
        }
        prev_rgba = Some(rgba.clone());

        if stable_count >= STABLE_STREAK {
            println!(
                "rosetta_stone_import_renders_gpu_proof: converged on attempt {attempt} \
                 (non-black fraction {fraction:.4})"
            );
            converged = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(
        converged,
        "the_rosetta_stone render never stabilized non-black after {max_attempts} attempts \
         (last non-black fraction {fraction:.4})"
    );

    let out_path = std::env::var("MESH_SNAP_OUT")
        .unwrap_or_else(|_| "target/mesh-snap/the_rosetta_stone.png".to_string());
    if let Some(parent) = std::path::Path::new(&out_path).parent() {
        std::fs::create_dir_all(parent).expect("create output dir");
    }
    image::save_buffer(&out_path, &rgba, w, h, image::ExtendedColorType::Rgba8)
        .unwrap_or_else(|e| panic!("save {out_path}: {e}"));
    println!("rosetta_stone_import_renders_gpu_proof: wrote {out_path}");
}

/// One frame, no convergence loop (this is a look-check demo, not a
/// numeric gate) — renders `def` at time 0 into a fresh RGBA buffer.
#[cfg(feature = "gpu-proofs")]
fn render_once(def: EffectGraphDef, w: u32, h: u32, label: &str) -> Vec<u8> {
    use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use crate::preset_context::PresetContext;
    use crate::render_target::RenderTarget;
    use manifold_gpu::GpuTextureFormat;

    let device = crate::test_device();
    let format = GpuTextureFormat::Rgba16Float;
    let registry = PrimitiveRegistry::with_builtin();
    let mut generator =
        PresetRuntime::from_def_with_device(def, &registry, device.arc(), w, h, format, None)
            .unwrap_or_else(|e| panic!("{label}: import graph must build: {e:?}"));
    let target = RenderTarget::new(&device, w, h, format, label);
    let ctx = PresetContext {
        time: 0.0,
        beat: 0.0,
        dt: 1.0 / 60.0,
        width: w,
        height: h,
        output_width: w,
        output_height: h,
        aspect: 1.0,
        owner_key: 0,
        is_clip_level: false,
        frame_count: 0,
        anim_progress: 0.0,
        trigger_count: 0,
    };
    let mut rgba = Vec::new();
    // A few frames for background texture decodes to land (no
    // stability-polling needed for a demo, just enough headroom).
    for _ in 0..30 {
        let mut enc = device.create_encoder(label);
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
            generator.render(&mut gpu, &target.texture, &ctx, &manifold_core::params::ParamManifest::default());
        }
        enc.commit_and_wait_completed();

        let bytes_per_row = w * 8;
        let readback_buf = device.create_buffer_shared(u64::from(h * bytes_per_row));
        let mut readback_enc = device.create_encoder(label);
        readback_enc.copy_texture_to_buffer(&target.texture, &readback_buf, w, h, bytes_per_row);
        readback_enc.commit_and_wait_completed();
        let ptr = readback_buf.mapped_ptr().expect("shared readback");
        let halves: &[u16] =
            unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };
        rgba = Vec::with_capacity((w * h * 4) as usize);
        for px in halves.chunks_exact(4) {
            rgba.push(tonemap_channel(half_to_f32(px[0])));
            rgba.push(tonemap_channel(half_to_f32(px[1])));
            rgba.push(tonemap_channel(half_to_f32(px[2])));
            rgba.push((half_to_f32(px[3]).clamp(0.0, 1.0) * 255.0).round() as u8);
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    rgba
}

/// Demo pair for the P3 gate's L2 artifact: the_rosetta_stone alone,
/// then the_rosetta_stone plus a duplicate of its (sole) object offset
/// by D11's +0.5 on `pos_x` — mirroring `DuplicateSceneObjectCommand`'s
/// shape structurally (deep-clone the producer subtree with fresh doc
/// ids, rewire internal wires, wire the clone's `object` output to the
/// next free `object_k`, bump `objects`) without depending on
/// `manifold-editing` from this crate — the actual command's
/// correctness (fresh-id freshness, undo, D6 handle sync) is proven by
/// its own inverse-pair unit tests in `manifold-editing`; this test
/// proves the RENDER characteristics of the shape it produces.
#[cfg(feature = "gpu-proofs")]
#[test]
fn duplicate_demo_pair_renders_original_then_original_plus_offset_copy() {
    let path = rosetta_stone_fixture_path();
    if !path.exists() {
        eprintln!(
            "duplicate_demo_pair_renders_original_then_original_plus_offset_copy: fixture not \
             found at {}, skipping",
            path.display()
        );
        return;
    }

    let (def, _report) = assemble_import_graph(&path).expect("assemble the_rosetta_stone");
    let (w, h) = (512u32, 512u32);

    let original_rgba = render_once(def.clone(), w, h, "rosetta-original");
    let out_dir = std::env::var("MESH_SNAP_OUT_DIR").unwrap_or_else(|_| "target/mesh-snap".to_string());
    std::fs::create_dir_all(&out_dir).expect("create output dir");
    let original_path = format!("{out_dir}/rosetta_duplicate_demo_original.png");
    image::save_buffer(&original_path, &original_rgba, w, h, image::ExtendedColorType::Rgba8)
        .unwrap_or_else(|e| panic!("save {original_path}: {e}"));

    // Build the "plus a duplicate" def: clone the sole object's producer
    // subtree with fresh doc ids (mirrors
    // manifold_editing::commands::graph::deep_clone_with_fresh_ids /
    // DuplicateSceneObjectCommand), offset transform_3d.pos_x by +0.5,
    // wire to the next object_k slot, bump `objects`.
    let mut plus_def = def.clone();
    let render_id = plus_def
        .nodes
        .iter()
        .find(|n| n.type_id == "node.render_scene")
        .expect("render_scene node")
        .id;
    let producer_id = plus_def
        .wires
        .iter()
        .find(|w| w.to_node == render_id && w.to_port == "object_0")
        .expect("object_0 wire present")
        .from_node;
    let source_index = plus_def.nodes.iter().position(|n| n.id == producer_id).expect("producer node");

    fn max_id(nodes: &[EffectGraphNode]) -> u32 {
        nodes
            .iter()
            .map(|n| n.id.max(n.group.as_ref().map(|g| max_id(&g.nodes)).unwrap_or(0)))
            .max()
            .unwrap_or(0)
    }
    fn collect_all_handles(nodes: &[EffectGraphNode], out: &mut std::collections::HashSet<String>) {
        for n in nodes {
            if let Some(h) = &n.handle {
                out.insert(h.clone());
            }
            if let Some(body) = n.group.as_deref() {
                collect_all_handles(&body.nodes, out);
            }
        }
    }
    // Mirrors `dedup_handle` in
    // `manifold_editing::commands::graph` — `base`, else `base_2`,
    // `base_3`, … (this crate has no dependency on manifold-editing,
    // so the tiny helper is duplicated rather than shared).
    fn dedup_handle(base: &str, taken: &mut std::collections::HashSet<String>) -> String {
        if !taken.contains(base) {
            taken.insert(base.to_string());
            return base.to_string();
        }
        let mut i = 2u32;
        loop {
            let cand = format!("{base}_{i}");
            if !taken.contains(&cand) {
                taken.insert(cand.clone());
                return cand;
            }
            i += 1;
        }
    }
    // Mirrors `deep_clone_with_fresh_ids` in
    // `manifold_editing::commands::graph::DuplicateSceneObjectCommand` —
    // fresh doc id + fresh NodeId + deduped handle on every node,
    // recursively. `Graph::add_node_named` rejects a duplicate handle
    // anywhere in the whole graph, so a clone whose inner nodes kept
    // their source's exact handles (`mesh_0`, `mat_0`, …) fails to
    // build even with fresh ids everywhere else. `node_id_map` (BUG-212)
    // mirrors the production fix: collects every (old, new) stable
    // NodeId pair across the whole subtree so the caller can re-target
    // `string_bindings` entries onto the clone's fresh ids.
    fn clone_fresh(
        src: &EffectGraphNode,
        next_id: &mut u32,
        taken: &mut std::collections::HashSet<String>,
        node_id_map: &mut Vec<(manifold_core::NodeId, manifold_core::NodeId)>,
    ) -> EffectGraphNode {
        let mut node = src.clone();
        node.id = *next_id;
        *next_id += 1;
        let old_node_id = node.node_id.clone();
        node.node_id = manifold_core::NodeId::new(manifold_core::short_id());
        node_id_map.push((old_node_id, node.node_id.clone()));
        node.handle = node.handle.as_deref().map(|h| dedup_handle(h, taken));
        if let Some(group) = node.group.as_deref_mut() {
            let mut id_map: Vec<(u32, u32)> = Vec::new();
            let mut new_nodes = Vec::new();
            for n in &group.nodes {
                let old = n.id;
                let cloned = clone_fresh(n, next_id, taken, node_id_map);
                id_map.push((old, cloned.id));
                new_nodes.push(cloned);
            }
            let remap = |id: u32| id_map.iter().find(|(o, _)| *o == id).map(|(_, n)| *n).unwrap_or(id);
            group.wires = group
                .wires
                .iter()
                .map(|w| EffectGraphWire {
                    from_node: remap(w.from_node),
                    from_port: w.from_port.clone(),
                    to_node: remap(w.to_node),
                    to_port: w.to_port.clone(),
                })
                .collect();
            group.nodes = new_nodes;
        }
        node
    }

    let mut next_id = max_id(&plus_def.nodes) + 1;
    let source_node = plus_def.nodes[source_index].clone();
    let mut taken = std::collections::HashSet::new();
    collect_all_handles(&plus_def.nodes, &mut taken);
    let mut node_id_map: Vec<(manifold_core::NodeId, manifold_core::NodeId)> = Vec::new();
    let mut clone = clone_fresh(&source_node, &mut next_id, &mut taken, &mut node_id_map);
    // D11's exact top-level convention (handle + " 2"), derived from the
    // SOURCE's own handle (not the post-dedup one — see the identical
    // comment on `DuplicateSceneObjectCommand::execute`).
    let cloned_handle = source_node.handle.as_ref().map(|h| format!("{h} 2"));
    clone.handle = cloned_handle.clone();
    if let Some(body) = clone.group.as_deref_mut()
        && let Some(inner_object) = body.nodes.iter_mut().find(|n| n.type_id == "node.scene_object")
    {
        inner_object.handle = cloned_handle;
    }
    clone.editor_pos = clone.editor_pos.map(|(x, y)| (x + 40.0, y + 40.0));

    // `string_bindings` (the "Model File" →
    // mesh-source `path` binding every importer object carries)
    // addresses its target by stable NodeId, which `clone_fresh` just
    // minted fresh for every cloned node (D11). Clone every
    // `string_bindings` entry whose target falls inside the duplicated
    // subtree (per `node_id_map`, collected above), re-targeted at the
    // clone's fresh NodeId, same `id`/`label`/`default_value` — D11's
    // "fresh NodeIds make cloned bindings dangle" is a deliberate
    // tradeoff for CARD exposes (`bindings`/`exposed_params`), not for
    // this non-performer-facing importer plumbing.
    if let Some(meta) = plus_def.preset_metadata.as_mut() {
        let new_entries: Vec<manifold_core::effect_graph_def::StringBindingDef> = meta
            .string_bindings
            .iter()
            .filter_map(|b| match &b.target {
                manifold_core::effect_graph_def::BindingTarget::Node { node_id, param } => node_id_map
                    .iter()
                    .find(|(old, _)| old == node_id)
                    .map(|(_, new_id)| manifold_core::effect_graph_def::StringBindingDef {
                        id: b.id.clone(),
                        label: b.label.clone(),
                        default_value: b.default_value.clone(),
                        target: manifold_core::effect_graph_def::BindingTarget::Node {
                            node_id: new_id.clone(),
                            param: param.clone(),
                        },
                    }),
                manifold_core::effect_graph_def::BindingTarget::Composite { .. } => None,
            })
            .collect();
        meta.string_bindings.extend(new_entries);
    }

    if let Some(body) = clone.group.as_deref_mut()
        && let Some(transform_node) = body.nodes.iter_mut().find(|n| n.type_id == "node.transform_3d")
    {
        let cur = match transform_node.params.get("pos_x") {
            Some(SerializedParamValue::Float { value }) => *value,
            _ => 0.0,
        };
        transform_node
            .params
            .insert("pos_x".to_string(), SerializedParamValue::Float { value: cur + 0.5 });
    }
    let clone_id = clone.id;
    plus_def.nodes.push(clone);
    plus_def
        .wires
        .push(wire(clone_id, "object", render_id, "object_1"));
    plus_def
        .nodes
        .iter_mut()
        .find(|n| n.id == render_id)
        .unwrap()
        .params
        .insert("objects".to_string(), SerializedParamValue::Float { value: 2.0 });

    let plus_rgba = render_once(plus_def, w, h, "rosetta-plus-duplicate");
    let plus_path = format!("{out_dir}/rosetta_duplicate_demo_plus_copy.png");
    image::save_buffer(&plus_path, &plus_rgba, w, h, image::ExtendedColorType::Rgba8)
        .unwrap_or_else(|e| panic!("save {plus_path}: {e}"));

    assert_ne!(
        original_rgba, plus_rgba,
        "the duplicate-plus-offset render must differ from the original (a second, offset copy is visible)"
    );
    println!(
        "duplicate_demo_pair_renders_original_then_original_plus_offset_copy: wrote \
         {original_path} and {plus_path}"
    );
}

// ── BUG-221 render proofs: composed per-object recenter ──
//
// Both tests below reconstruct the PRE-fix graph from the CURRENT
// (post-fix) `assemble_import_graph` output, rather than duplicating
// the old formula by hand: per-object recenter now splits into
// `mesh_k.translate_* = -own_center` and `transform_k.pos_* =
// own_center - center`, and BUG-221's own fix guarantees
// `translate + pos == -center` (the old whole-scene-only recenter) —
// proven independently, at value level, by
// `bug221_object_transform_recenters_about_own_bbox_center_not_scene_center`
// above. Reconstructing pre-fix behavior as `translate=0, pos =
// translate_old + pos_old` is therefore a faithful mechanical inverse
// of the fix, not a hand-typed duplicate of old code that could drift.

#[cfg(feature = "gpu-proofs")]
fn emissive_strength_test_fixture_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/gltf/khronos/EmissiveStrengthTest.glb")
}

/// Rebuild every object group's `mesh_k`/`transform_k` pair back to the
/// PRE-BUG-221 shape: `mesh_k.translate_* = 0`, `transform_k.pos_* =
/// (old mesh translate) + (old transform pos)`. See the module comment
/// above this function for why this reconstruction is faithful.
#[cfg(feature = "gpu-proofs")]
fn reconstruct_pre_bug221_fix(def: &EffectGraphDef) -> EffectGraphDef {
    let mut out = def.clone();
    for node in &mut out.nodes {
        let Some(body) = node.group.as_mut() else { continue };
        let mesh_handles: Vec<String> = body
            .nodes
            .iter()
            .filter_map(|n| n.handle.clone())
            .filter(|h| h.starts_with("mesh_"))
            .collect();
        for mesh_handle in mesh_handles {
            let k = &mesh_handle["mesh_".len()..];
            let transform_handle = format!("transform_{k}");
            let Some(mi) = body.nodes.iter().position(|n| n.handle.as_deref() == Some(mesh_handle.as_str()))
            else {
                continue;
            };
            let Some(ti) =
                body.nodes.iter().position(|n| n.handle.as_deref() == Some(transform_handle.as_str()))
            else {
                continue;
            };
            for (mesh_param, transform_param) in
                [("translate_x", "pos_x"), ("translate_y", "pos_y"), ("translate_z", "pos_z")]
            {
                let t = match body.nodes[mi].params.get(mesh_param) {
                    Some(SerializedParamValue::Float { value }) => *value,
                    _ => 0.0,
                };
                let p = match body.nodes[ti].params.get(transform_param) {
                    Some(SerializedParamValue::Float { value }) => *value,
                    _ => 0.0,
                };
                body.nodes[mi].params.insert(mesh_param.to_string(), float(0.0));
                body.nodes[ti].params.insert(transform_param.to_string(), float(t + p));
            }
        }
    }
    out
}

/// Mean absolute per-channel difference between two same-sized,
/// already-tonemapped RGBA8 buffers (`render_once`'s output format) —
/// same metric shape as `scene_object_migration_round_trip.rs`'s.
#[cfg(feature = "gpu-proofs")]
fn mean_abs_diff_u8(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len());
    let sum: f64 = a.iter().zip(b).map(|(x, y)| (*x as f64 - *y as f64).abs() / 255.0).sum();
    sum / a.len() as f64
}

/// BUG-221 layout-preservation gate: `EmissiveStrengthTest.glb` (6
/// distinct objects laid out in a world-space row, `translation.x`
/// from -6 to +6 — a real multi-object asset whose per-object
/// `own_center` differs substantially from the whole-scene center,
/// confirmed by direct measurement during triage) rendered at default
/// params (no rotation) must look the SAME whether every object's
/// pivot lives at its own visual center (post-fix, current code) or at
/// the shared whole-scene origin (pre-fix, reconstructed) — the fix is
/// a pivot-only change, never a placement change.
#[cfg(feature = "gpu-proofs")]
#[test]
fn bug221_layout_preserved_before_and_after_fix() {
    let path = emissive_strength_test_fixture_path();
    if !path.exists() {
        eprintln!("bug221_layout_preserved_before_and_after_fix: fixture not found at {}, skipping", path.display());
        return;
    }
    let (post_def, _report) = assemble_import_graph(&path).expect("assemble EmissiveStrengthTest");
    let pre_def = reconstruct_pre_bug221_fix(&post_def);

    let (w, h) = (512u32, 512u32);
    let post_rgba = render_once(post_def, w, h, "bug221-post-fix-no-rotation");
    let pre_rgba = render_once(pre_def, w, h, "bug221-pre-fix-no-rotation");

    let out_dir = std::env::var("MESH_SNAP_OUT_DIR").unwrap_or_else(|_| "target/mesh-snap".to_string());
    std::fs::create_dir_all(&out_dir).expect("create output dir");
    let post_path = format!("{out_dir}/bug221_layout_post_fix.png");
    let pre_path = format!("{out_dir}/bug221_layout_pre_fix.png");
    image::save_buffer(&post_path, &post_rgba, w, h, image::ExtendedColorType::Rgba8)
        .unwrap_or_else(|e| panic!("save {post_path}: {e}"));
    image::save_buffer(&pre_path, &pre_rgba, w, h, image::ExtendedColorType::Rgba8)
        .unwrap_or_else(|e| panic!("save {pre_path}: {e}"));

    let diff = mean_abs_diff_u8(&post_rgba, &pre_rgba);
    eprintln!("bug221_layout_preserved_before_and_after_fix: mean_abs_diff = {diff:.6}");
    assert!(
        diff < 0.01,
        "BUG-221's fix must not change net world placement — pre-fix and post-fix renders \
         at default (no-rotation) params should be pixel-comparable, got mean_abs_diff={diff:.6} \
         (wrote {pre_path}, {post_path})"
    );
}

/// BUG-221 pivot-behavior gate: a 45° `rot_y` applied to ONLY the
/// single object whose `own_center` sits farthest from the whole-scene
/// center (found the same way the summary-scan triage did —
/// `EmissiveStrengthTest.glb`'s cubes sit at world X from -6 to +6,
/// each translated away from the origin by its own glTF node, so one
/// of the five cube objects has a large own_center/scene-center
/// offset). Rotating only that one object (not the shared backdrop
/// mesh, which spans the whole scene and would confound the
/// comparison by visibly swinging in BOTH renders regardless of the
/// fix) isolates BUG-221's exact claim: pre-fix, that object's pivot
/// is the shared whole-scene origin, so a 45° spin swings it well out
/// of its original footprint; post-fix, its pivot is its own visual
/// center, so the same 45° spin rotates it in place.
#[cfg(feature = "gpu-proofs")]
#[test]
fn bug221_pivot_spins_in_place_after_fix_but_not_before() {
    let path = emissive_strength_test_fixture_path();
    if !path.exists() {
        eprintln!("bug221_pivot_spins_in_place_after_fix_but_not_before: fixture not found at {}, skipping", path.display());
        return;
    }
    let summary = gltf_load::gltf_import_summary(&path).expect("parse EmissiveStrengthTest for offset scan");
    let center = [
        (summary.bbox_min[0] + summary.bbox_max[0]) * 0.5,
        (summary.bbox_min[1] + summary.bbox_max[1]) * 0.5,
        (summary.bbox_min[2] + summary.bbox_max[2]) * 0.5,
    ];
    // Same largest-vertex-count-first sort `build_import_graph` uses,
    // so this index lines up with the `transform_{k}`/`mesh_{k}`
    // handles in the built def.
    let mut materials = summary.materials.clone();
    materials.sort_by(|a, b| b.vertex_count.cmp(&a.vertex_count));
    let (target_k, target) = materials
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| {
            let mag = |m: &super::gltf_load::GltfMaterialInfo| {
                let d = [m.own_center[0] - center[0], m.own_center[1] - center[1], m.own_center[2] - center[2]];
                d[0] * d[0] + d[1] * d[1] + d[2] * d[2]
            };
            mag(a).partial_cmp(&mag(b)).unwrap()
        })
        .expect("EmissiveStrengthTest has at least one material");
    let offset = [
        target.own_center[0] - center[0],
        target.own_center[1] - center[1],
        target.own_center[2] - center[2],
    ];
    let offset_mag = (offset[0] * offset[0] + offset[1] * offset[1] + offset[2] * offset[2]).sqrt();
    eprintln!(
        "bug221_pivot_spins_in_place_after_fix_but_not_before: rotating object {target_k} \
         (own_center={:?}, scene center={center:?}, offset magnitude={offset_mag:.3})",
        target.own_center
    );
    assert!(
        offset_mag > 1.0,
        "fixture must actually exercise a far-from-scene-center object for this gate to mean \
         anything, got offset magnitude {offset_mag:.3}"
    );

    let (post_def, _report) = assemble_import_graph(&path).expect("assemble EmissiveStrengthTest");
    let pre_def = reconstruct_pre_bug221_fix(&post_def);

    fn rotate_one_object_y(mut def: EffectGraphDef, k: usize, radians: f32) -> EffectGraphDef {
        let target_handle = format!("transform_{k}");
        for node in &mut def.nodes {
            let Some(body) = node.group.as_mut() else { continue };
            for n in &mut body.nodes {
                if n.type_id == "node.transform_3d" && n.handle.as_deref() == Some(target_handle.as_str()) {
                    n.params.insert("rot_y".to_string(), float(radians));
                }
            }
        }
        def
    }

    let rot = std::f32::consts::FRAC_PI_4; // 45 degrees
    let post_rot_def = rotate_one_object_y(post_def, target_k, rot);
    let pre_rot_def = rotate_one_object_y(pre_def, target_k, rot);

    let (w, h) = (512u32, 512u32);
    let post_rot_rgba = render_once(post_rot_def, w, h, "bug221-post-fix-45deg");
    let pre_rot_rgba = render_once(pre_rot_def, w, h, "bug221-pre-fix-45deg");

    let out_dir = std::env::var("MESH_SNAP_OUT_DIR").unwrap_or_else(|_| "target/mesh-snap".to_string());
    std::fs::create_dir_all(&out_dir).expect("create output dir");
    let post_path = format!("{out_dir}/bug221_pivot_post_fix_45deg.png");
    let pre_path = format!("{out_dir}/bug221_pivot_pre_fix_45deg.png");
    image::save_buffer(&post_path, &post_rot_rgba, w, h, image::ExtendedColorType::Rgba8)
        .unwrap_or_else(|e| panic!("save {post_path}: {e}"));
    image::save_buffer(&pre_path, &pre_rot_rgba, w, h, image::ExtendedColorType::Rgba8)
        .unwrap_or_else(|e| panic!("save {pre_path}: {e}"));

    assert_ne!(
        post_rot_rgba, pre_rot_rgba,
        "pre-fix and post-fix 45deg-rotated renders must differ — different pivots must produce \
         visibly different results, or the fix changed nothing"
    );
    println!(
        "bug221_pivot_spins_in_place_after_fix_but_not_before: wrote {pre_path} and {post_path} \
         — look at both: pre-fix should show cube(s) swung away from their unrotated footprint, \
         post-fix should show every cube still occupying roughly its unrotated footprint, just \
         with rotated faces"
    );
}
