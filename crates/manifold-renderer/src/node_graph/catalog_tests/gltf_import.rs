use manifold_nodes_scene::node_graph::gltf_load;
use manifold_nodes_scene::node_graph::gltf_import::testkit::*;
use manifold_nodes_scene::node_graph::gltf_import::*;
use manifold_nodes_scene::node_graph::gltf_import::assembly::*;
use manifold_nodes_scene::node_graph::gltf_import::merge::*;
use manifold_nodes_scene::node_graph::gltf_import::scene::*;
use manifold_node_engine::persistence::PrimitiveRegistry;
use manifold_node_engine::scene::boundary_nodes::FINAL_OUTPUT_TYPE_ID;
use manifold_nodes_scene::node_graph::gltf_load::GltfImportSummary;
use manifold_node_engine::runtime::PresetRuntime;
#[cfg(feature = "gpu-proofs")]
use manifold_node_engine::runtime::frame_status::FrameRenderStatus;
use manifold_core::NodeId;
#[cfg(feature = "gpu-proofs")]
use manifold_core::WarmupBudget;
use manifold_core::effect_graph_def::BindingTarget;
use manifold_core::effect_graph_def::{EffectGraphDef, EffectGraphNode, GROUP_TYPE_ID, SerializedParamValue};


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

/// GLB_XFAIL_BURNDOWN_DESIGN.md D4 (BUG-171) / section 4 invariant ("no silent
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
        Some(&int(manifold_nodes_scene::node_graph::gltf_load::DEFAULT_MATERIAL_MESH_PARAM)),
        "the synthetic object's mesh source must select via the D4 sentinel, not a real \
         material index or the -1 'unset' value"
    );
    assert_eq!(
        mesh_node.params.get("vertex_colors"),
        Some(&bool_val(true)),
        "new imports must explicitly enable authored vertex colors"
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
        lights: Vec::new(),
        cameras: Vec::new(),
        camera_report_lines: Vec::new(),
        texture_dims: Vec::new(),
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

#[test]
fn legacy_compound_import_migrates_to_editable_children_once() {
    use manifold_node_engine::persistence::EffectGraphDefExt;
    let path = azalea_fixture_path();
    let (mut def, _) = assemble_import_graph(&path).unwrap();
    let group = def.nodes.iter_mut().find(|node| node.node_id.as_str().starts_with("object_") && node.group.is_some()).unwrap().group.as_mut().unwrap();
    let locals: std::collections::HashSet<_> = group.nodes.iter().filter(|n| n.node_id.as_str().starts_with("part_transform_")).map(|n| n.id).collect();
    let local_ids: std::collections::HashSet<_> = group.nodes.iter().filter(|n| locals.contains(&n.id)).map(|n| n.node_id.clone()).collect();
    group.nodes.retain(|n| !locals.contains(&n.id));
    group.wires.retain(|w| !locals.contains(&w.from_node));
    for w in &mut group.wires {
        if w.to_port == "parent_transform" { w.to_port = "transform".into(); }
    }
    let metadata = def.preset_metadata.as_mut().unwrap();
    let local_bindings: std::collections::HashSet<_> = metadata.bindings.iter().filter_map(|b| match &b.target {
        BindingTarget::Node { node_id, .. } if local_ids.contains(node_id) => Some(b.id.clone()),
        _ => None,
    }).collect();
    metadata.params.retain(|p| !local_bindings.contains(&p.id));
    metadata.bindings.retain(|b| !local_bindings.contains(&b.id));
    for binding in &mut metadata.bindings {
        if let BindingTarget::Node { param, .. } = &mut binding.target && param == "parent_visible" { *param = "visible".into(); }
    }
    assert!(manifold_nodes_scene::node_graph::scene_exposure::migrate_scene_exposures(&mut def));
    let saved = def.clone();
    assert!(!manifold_nodes_scene::node_graph::scene_exposure::migrate_scene_exposures(&mut def));
    assert_eq!(def, saved);
    let vm = manifold_nodes_scene::node_graph::scene_vm::SceneVm::from_def(&def).unwrap();
    assert_eq!(vm.header.object_count, 1);
    assert_eq!(vm.objects.len(), 3);
    def.into_graph(&PrimitiveRegistry::with_builtin(), &Default::default()).unwrap();
}

/// Grouping proof, fixture-free: each object's producers must live inside one
/// named, tinted group, and the grouped graph must flatten to the SAME flat
/// wiring the ungrouped assembler produced (compared in node_id space) with
/// every card/string binding still resolving. Uses a synthetic two-material
/// summary — one textured, one not — so it needs no `.glb` on disk.
#[test]
fn build_import_graph_groups_each_object_and_flattens_to_flat_wiring() {
    use manifold_nodes_scene::node_graph::gltf_load::GltfMaterialInfo;
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
        legacy_specular_factor: None,
        specular_color_factor: [1.0, 1.0, 1.0],
        specular_texture: None,
        specular_color_texture: None,
        base_color_uv_transform: manifold_nodes_scene::node_graph::gltf_load::IDENTITY_UV_TRANSFORM,
        normal_uv_transform: manifold_nodes_scene::node_graph::gltf_load::IDENTITY_UV_TRANSFORM,
        mr_uv_transform: manifold_nodes_scene::node_graph::gltf_load::IDENTITY_UV_TRANSFORM,
        occlusion_uv_transform: manifold_nodes_scene::node_graph::gltf_load::IDENTITY_UV_TRANSFORM,
        emissive_uv_transform: manifold_nodes_scene::node_graph::gltf_load::IDENTITY_UV_TRANSFORM,
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
        base_color_sampler: manifold_nodes_scene::node_graph::gltf_load::GltfSamplerInfo::default(),
        normal_sampler: manifold_nodes_scene::node_graph::gltf_load::GltfSamplerInfo::default(),
        mr_sampler: manifold_nodes_scene::node_graph::gltf_load::GltfSamplerInfo::default(),
        occlusion_sampler: manifold_nodes_scene::node_graph::gltf_load::GltfSamplerInfo::default(),
        emissive_sampler: manifold_nodes_scene::node_graph::gltf_load::GltfSamplerInfo::default(),
        extension_maps: [manifold_node_engine::scene::material::MaterialMapInfo::default(); 14],
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
        lights: Vec::new(),
        cameras: Vec::new(),
        camera_report_lines: Vec::new(),
        texture_dims: Vec::new(),
    };
    let path = std::path::Path::new("/tmp/synthetic_model.glb");
    let (def, report) = build_import_graph(&summary, path).expect("build grouped graph");
    assert_eq!(report.object_count, 2);
    assert_eq!(report.textures_wired, 1);

    // Top level: one compound object group PLUS the "ao" presentation group
    // PLUS the "dof" group (CINEMATIC_SCENE_TAIL D1/section 3 —
    // coc_from_depth → bokeh_gather), no bare producer nodes.
    let groups: Vec<_> = def.nodes.iter().filter(|n| n.type_id == GROUP_TYPE_ID).collect();
    assert_eq!(groups.len(), 3, "compound object group + ao + dof");
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
    // Only the compound object group carries a tint (CINEMATIC_POST's ao
    // group is an untinted presentation box, not object identity).
    let object_groups: Vec<_> = groups
        .iter()
        .filter(|g| g.group.as_ref().unwrap().tint.is_some())
        .copied()
        .collect();
    assert_eq!(object_groups.len(), 1, "one tinted compound group");
    // The compound group's interface exposes one Object output per material;
    // all parts share the group's one transform and visibility binding.
    let outputs = &object_groups[0].group.as_ref().unwrap().interface.outputs;
    assert_eq!(outputs.len(), 2, "one Object output per material");
    assert!(outputs.iter().all(|o| o.port_type == "Object"));
    assert_eq!(outputs[0].name, "object");
    assert_eq!(outputs[1].name, "object_1");
    // A compound asset has one shared tint (legibility).
    let tints: Vec<_> = object_groups.iter().filter_map(|g| g.group.as_ref().unwrap().tint).collect();
    assert_eq!(tints.len(), 1, "compound group gets a tint");

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
        ("transform_0", "transform", "object_0_bind", "parent_transform"),
        ("transform_0", "transform", "object_1_bind", "parent_transform"),
        ("part_transform_0", "transform", "object_0_bind", "transform"),
        ("part_transform_1", "transform", "object_1_bind", "transform"),
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
    // all three groups as navigable boxes — the compound group carries both
    // material producers while AO/DOF remain separate presentation boxes —
    // not a flat wall of nodes. This is the legibility payoff, verified at the
    // snapshot layer (the pixels still want Peter's eyes on a real model).
    let snap = manifold_node_engine::snapshot::GraphSnapshot::from_def(&def)
        .expect("editor snapshot builds from the grouped def");
    let snap_groups: Vec<_> =
        snap.nodes.iter().filter(|n| n.group.is_some()).collect();
    assert_eq!(snap_groups.len(), 3, "editor snapshot shows compound object + ao + dof group boxes");
    let snap_object_groups: Vec<_> = snap_groups
        .iter()
        .filter(|g| g.group.as_ref().unwrap().nodes.iter().any(|inner| inner.type_id == "node.pbr_material"))
        .collect();
    assert_eq!(snap_object_groups.len(), 1, "the compound object group carries both material nodes");

    // Finally, it must build through the production loader (which flattens).
    let registry = PrimitiveRegistry::with_builtin();
    PresetRuntime::from_def(def, &registry, None)
        .expect("grouped import graph must build through PresetRuntime::from_def");
}

/// CINEMATIC_SCENE_TAIL I2 (D4): every surfaced lens param — the ones the
/// Scene Setup panel's Lens rows bind (`focus_distance` / `f_stop` /
/// `shutter_angle`, surfaced on the import card as `{lens_doc_id}_…`) — has
/// a directed consumer path to `final` in an import-assembled graph. Dead
/// sliders are the bug D4 kills by construction: once the tail exists,
/// focus_distance/f_stop reach final via lens → coc_from_depth
/// → bokeh_gather → motion_blur, and shutter_angle via lens →
/// motion_blur.camera.
#[test]
fn scene_lens_params_have_consumers() {
    // Synthetic single-material import — fast and fixture-free, same shape
    // every real import takes (the dof/motion_blur tail is added regardless).
    let mut mat = full_material(0, "Mat", 100);
    mat.own_center = [0.0, 0.0, 0.0];
    let summary = GltfImportSummary {
        materials: vec![mat],
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
    let path = std::path::Path::new("/tmp/synthetic_lens_consumers_test.glb");
    let (def, _report) = build_import_graph(&summary, path).expect("build import graph");

    // The lens node must exist (the tail reuses it, never adds a second).
    let lens_nid = "lens";
    let lens_list: Vec<_> = def.nodes.iter().filter(|n| n.node_id == lens_nid).collect();
    assert_eq!(lens_list.len(), 1, "exactly one camera_lens node, reused by the tail");
    let lens_id = lens_list[0].id;

    // Every surfaced lens binding targets lens.<param> and has a path to final.
    let meta = def.preset_metadata.as_ref().expect("v2 metadata");
    let binds: Vec<_> = meta
        .bindings
        .iter()
        .filter(|b| matches!(&b.target, BindingTarget::Node { node_id, .. } if node_id == lens_nid))
        .collect();
    let lens_params: Vec<&str> = binds
        .iter()
        .map(|b| match &b.target {
            BindingTarget::Node { param, .. } => param.as_str(),
            _ => unreachable!("filtered above"),
        })
        .collect();
    for want in ["focus_distance", "f_stop", "shutter_angle"] {
        assert!(
            lens_params.contains(&want),
            "lens binding for `{want}` must exist (surfaced on the card)"
        );
    }

    // Directed-graph reachability: node id -> outgoing (to_node, to_port).
    let mut out_edges: std::collections::HashMap<u32, Vec<(u32, &str)>> =
        std::collections::HashMap::new();
    for w in &def.wires {
        out_edges
            .entry(w.from_node)
            .or_default()
            .push((w.to_node, w.to_port.as_str()));
    }
    let final_id = def
        .nodes
        .iter()
        .find(|n| n.type_id == FINAL_OUTPUT_TYPE_ID)
        .expect("import graph has a final node")
        .id;
    // BFS over node ids (not flattened): groups are wired at the top level,
    // so a group input node's consumer is found via the group node's outward
    // wires. The tail's inner-chain connectivity is pinned by a direct look
    // below; reachability here proves the OUTER path from lens to final.
    fn reaches_final(
        start: u32,
        final_id: u32,
        out_edges: &std::collections::HashMap<u32, Vec<(u32, &str)>>,
    ) -> bool {
        let mut seen = std::collections::HashSet::new();
        let mut stack = vec![start];
        while let Some(n) = stack.pop() {
            if n == final_id {
                return true;
            }
            if !seen.insert(n) {
                continue;
            }
            if let Some(edges) = out_edges.get(&n) {
                for (to, _port) in edges {
                    stack.push(*to);
                }
            }
        }
        false
    }
    for param in lens_params {
        assert!(
            reaches_final(lens_id, final_id, &out_edges),
            "surfaced lens param `{param}`'s target node lense must have a consumer path to final"
        );
    }

    // The chain itself, node-for-node on CinematicScene's dof group +
    // motion_blur tail (CINEMATIC_SCENE_TAIL section 3). The dof group exists
    // as ONE top-level group node; its inner wiring is asserted by reading
    // the group's own node/wire lists.
    let dof_group = def
        .nodes
        .iter()
        .find(|n| n.type_id == GROUP_TYPE_ID && n.node_id == NodeId::new("dof"))
        .expect("dof group present in the import graph");
    let inner = dof_group.group.as_ref().expect("dof group has inner nodes");
    let bokeh = inner.nodes.iter().find(|node| node.type_id == "node.bokeh_gather").unwrap();
    assert_eq!(bokeh.params["enabled"], bool_val(true));
    assert_eq!(bokeh.params["aperture"], enum_val(0));
    assert_eq!(bokeh.params["quality"], enum_val(1));
    for node in [bokeh, def.nodes.iter().find(|node| node.type_id == "node.motion_blur").unwrap()] {
        let binding = meta.bindings.iter().find(|binding| matches!(
            &binding.target,
            BindingTarget::Node { node_id, param } if *node_id == node.node_id && param == "enabled"
        )).expect("cinematic enabled control is exposed");
        let spec = meta.params.iter().find(|spec| spec.id == binding.id).unwrap();
        assert!(spec.is_toggle);
        assert_eq!(spec.default_value, 1.0);
    }
    let inner_types: Vec<&str> = inner.nodes.iter().map(|n| n.type_id.as_str()).collect();
    for want in ["node.coc_from_depth", "node.bokeh_gather"] {
        assert!(
            inner_types.contains(&want),
            "dof group must contain `{want}` (got {inner_types:?})"
        );
    }
    let coc = inner
        .nodes
        .iter()
        .find(|node| node.type_id == "node.coc_from_depth")
        .expect("dof group CoC source present");
    assert!(
        inner.nodes.iter().all(|node| node.type_id != "node.coc_dilate"),
        "import DoF tail must not retain the obsolete CoC dilation node"
    );
    assert!(
        inner.wires.iter().any(|wire| {
            wire.from_node == coc.id
                && wire.from_port == "out"
                && wire.to_node == bokeh.id
                && wire.to_port == "width"
        }),
        "import DoF tail must feed original CoC directly into bokeh width"
    );
    let mb = def
        .nodes
        .iter()
        .find(|n| n.type_id == "node.motion_blur")
        .expect("motion_blur tail present in the import graph");
    let mb_fed: Vec<(&str, &str)> = def
        .wires
        .iter()
        .filter(|w| w.to_node == mb.id)
        .map(|w| {
            (
                def.nodes.iter().find(|n| n.id == w.from_node).map(|n| n.node_id.as_str()).unwrap_or("?"),
                w.from_port.as_str(),
            )
        })
        .collect();
    for (from, port) in [("dof", "out"), ("render", "velocity"), ("lens", "out")] {
        assert!(
            mb_fed.contains(&(from, port)),
            "motion_blur must be fed by `{from}.{port}` (got {mb_fed:?})"
        );
    }
}

/// BUG-303, runtime proof: the def-level test above shows the exposure
/// DEFAULTS are seeded right; this one proves the stamped placement
/// SURVIVES instantiation. `PresetRuntime::from_def` runs the exact code
/// that caused the bug — `BoundGraph::new` → `apply_binding_defaults`
/// plants every binding's `default_value` onto its target param at build —
/// so if any exposure over a `transform_k` ever again carries a
/// manifest-default 0.0, this assert catches the clobber at the live-graph
/// level, where the def-level test cannot see it. Mock executor, no GPU.
#[test]
fn bug303_stamped_transform_survives_preset_runtime_instantiation() {
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
    let expected = [0.0_f32, 0.0, 0.0];

    let path = std::path::Path::new("/tmp/synthetic_bug303_runtime_test.glb");
    let (def, _report) = build_import_graph(&summary, path).expect("build import graph");

    let registry = PrimitiveRegistry::with_builtin();
    let runtime =
        PresetRuntime::from_def(def, &registry, None).expect("instantiate imported def");

    let node_id = manifold_core::NodeId::new("transform_0");
    let inst = runtime
        .graph
        .instance_by_node_id(&node_id)
        .expect("shared transform_0 present in the live graph");
    for (axis, (param, want)) in ["pos_x", "pos_y", "pos_z"].iter().zip(expected).enumerate() {
            let got = runtime
                .graph
                .get_node(inst)
                .and_then(|n| n.params.get(*param).cloned())
                .unwrap_or_else(|| panic!("transform_0.{param} readable post-build"));
            let manifold_node_engine::parameters::ParamValue::Float(got) = got else {
                panic!("transform_0.{param} is a Float param, got {got:?}");
            };
            assert!(
                (got - want).abs() < 1e-5,
                "transform_0.{param} must survive instantiation at shared origin \
                 (axis {axis}); got {got}"
            );
        }
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
        lights: Vec::new(),
        cameras: Vec::new(),
        camera_report_lines: Vec::new(),
        texture_dims: Vec::new(),
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
    use manifold_nodes_scene::node_graph::gltf_load::{GltfObjectAnimation, QuatTrack, Vec3Track};

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
        lights: Vec::new(),
        cameras: Vec::new(),
        camera_report_lines: Vec::new(),
        texture_dims: Vec::new(),
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
    use manifold_nodes_scene::node_graph::gltf_load::{GltfAnimationInfo, GltfObjectAnimation, Vec3Track};

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
        lights: Vec::new(),
        cameras: Vec::new(),
        camera_report_lines: Vec::new(),
        texture_dims: Vec::new(),
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
    use manifold_node_engine::persistence::EffectGraphDefExt;
    let registry = PrimitiveRegistry::with_builtin();
    let graph = def.clone().into_graph(&registry, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default()).expect("import graph must build");
    let (errors, _warnings) = manifold_node_engine::validate::check_card_lints(&def, Some(&graph));
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
    use manifold_node_engine::persistence::EffectGraphDefExt;
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/gltf/skeleton_animated.glb");
    let (def, _report) =
        manifold_nodes_scene::node_graph::gltf_import::assemble_import_graph(&path).expect("assemble skeleton_animated.glb");
    let registry = PrimitiveRegistry::with_builtin();
    let graph = def.clone().into_graph(&registry, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default()).expect("import graph must build");
    let (errors, _warnings) =
        manifold_node_engine::validate::check_card_lints(&def, Some(&graph));
    assert!(
        errors.is_empty(),
        "card lints must accept the assembled animated+rigged import: {errors:?}"
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
    use manifold_node_engine::persistence::EffectGraphDefExt;
    for path in hostile_fixture_paths() {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let (def, _report) = manifold_nodes_scene::node_graph::gltf_import::assemble_import_graph(&path)
            .unwrap_or_else(|e| panic!("{name}: assemble_import_graph failed: {e}"));
        let registry = PrimitiveRegistry::with_builtin();
        let graph = def
            .clone()
            .into_graph(&registry, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default())
            .unwrap_or_else(|e| panic!("{name}: import graph failed to build: {e:?}"));
        let (errors, _warnings) =
            manifold_node_engine::validate::check_card_lints(&def, Some(&graph));
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
    use manifold_node_engine::persistence::EffectGraphDefExt;
    let (target, _report) = manifold_nodes_scene::node_graph::gltf_import::assemble_import_graph(&azalea_fixture_path())
        .expect("assemble azalea target scene");
    let (render_id, existing_objects) = render_scene_objects(&target);
    for path in hostile_fixture_paths() {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let plan = manifold_nodes_scene::node_graph::gltf_import::assemble_merge_plan(&target, &path)
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
            .into_graph(&registry, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default())
            .unwrap_or_else(|e| panic!("{name}: merged graph failed to build: {e:?}"));
        let (errors, _warnings) =
            manifold_node_engine::validate::check_card_lints(&merged, Some(&graph));
        assert!(errors.is_empty(), "{name}: card lints rejected the merged def: {errors:?}");
        PresetRuntime::from_def(merged, &registry, None)
            .unwrap_or_else(|e| panic!("{name}: merged PresetRuntime build failed: {e:?}"));
        let _ = existing_objects;
    }
}

/// Pre-existing merge-path bug (surfaced, not introduced, by BUG-w5wv):
/// `build_object_group`'s `local_k` numbers a merged object's OWN inner
/// STRING handles ("mat_{k}", "mesh_{k}", …), a SEPARATE identifier system
/// from the numeric `EffectGraphNode.id` — those handles are what a card
/// binding's `NodeId` addresses, and they're never renamed downstream. A
/// fresh import's `local_k` always starts at 0, so merging a SECOND glTF
/// (whose own `local_k` ALSO starts at 0) into a target scene that already
/// has its own "mat_0" collides on that bare handle — `check_card_lints`/
/// `graph.instance_by_node_id` then resolve the incoming material's OWN
/// binding against the TARGET's colliding node instead of its own. Silently
/// harmless when every colliding node happened to be identically
/// `node.pbr_material` with similar-looking values (nobody could tell which
/// of the two a binding actually landed on); this test proves the fix
/// (`max_local_k_recursive`'s offset in `merge.rs`) with DISTINGUISHABLE
/// values on each side, fully decoupled from the unlit-routing feature
/// (BUG-w5wv's own `azalea + cubicspline_interp.glb` real-fixture merge,
/// which DOES put two different primitive types behind the collision, is
/// covered by `hostile_fixtures_merge_into_existing_scene`'s general sweep).
#[test]
fn merge_local_k_offset_avoids_colliding_with_the_targets_own_material_handle() {
    use manifold_node_engine::persistence::EffectGraphDefExt;
    // Target: one object, its own material's color_r = 0.8 (full_material's
    // default) — this is what a colliding resolution would WRONGLY return.
    let target = scene_def_with_bbox_half_extent(1.0);
    let (render_id, _existing_objects) = render_scene_objects(&target);

    // Incoming: one object, color_r overridden to a clearly distinct value —
    // pre-fix, this material's OWN `local_k` would ALSO be 0 (restarting
    // independently of the target), colliding on the bare handle "mat_0".
    let mut incoming = full_material(0, "Incoming", 300);
    incoming.base_color_factor[0] = 0.15;
    let summary = merge_summary(vec![incoming], 1.0);
    let path = std::path::Path::new("/tmp/synthetic_merge_local_k_collision.glb");
    let plan = merge_import_into_graph(&target, &summary, path).expect("merge incoming material");

    // The offset must have moved the incoming material's handle off "mat_0"
    // (the target's own material already claims it).
    let incoming_mat_handle = plan
        .new_nodes
        .iter()
        .find_map(|n| find_handle_by_type(n, "node.pbr_material"))
        .expect("incoming material must build node.pbr_material");
    assert_ne!(
        incoming_mat_handle, "mat_0",
        "incoming material's handle must not collide with the target's own mat_0"
    );

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
    let graph = merged.clone().into_graph(&registry, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default()).expect("merged graph must build");
    let (errors, _warnings) = manifold_node_engine::validate::check_card_lints(&merged, Some(&graph));
    assert!(errors.is_empty(), "card lints rejected the merged def: {errors:?}");

    // The incoming material's own "color_r" binding must resolve to ITS OWN
    // node — proven by the VALUE (0.15), not the target's colliding 0.8.
    let color_r_binding = merged
        .preset_metadata
        .as_ref()
        .unwrap()
        .bindings
        .iter()
        .find(|b| matches!(&b.target, BindingTarget::Node { node_id, param } if node_id.as_str() == incoming_mat_handle && param == "color_r"))
        .expect("incoming material's color_r binding must exist");
    let BindingTarget::Node { node_id, .. } = &color_r_binding.target else { unreachable!() };
    let instance = graph
        .instance_by_node_id(node_id)
        .and_then(|idx| graph.get_node(idx))
        .expect("color_r binding must resolve to a real node instance");
    assert!(
        matches!(
            instance.params.get("color_r"),
            Some(manifold_node_engine::parameters::ParamValue::Float(v)) if (*v - 0.15).abs() < 1e-6
        ),
        "color_r binding must resolve to the INCOMING material's own node (0.15), not the \
         target's colliding one (0.8) — got {:?}",
        instance.params.get("color_r")
    );
}

/// Group-aware handle search, mirroring [`contains_type`]: the first
/// node of `type_id` anywhere in `node` (including inside its own group
/// body), returning its OWN `handle`.
fn find_handle_by_type<'a>(node: &'a EffectGraphNode, type_id: &str) -> Option<&'a str> {
    if node.type_id == type_id {
        return node.handle.as_deref();
    }
    node.group.as_ref()?.nodes.iter().find_map(|inner| find_handle_by_type(inner, type_id))
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
            let (def, _report) = manifold_nodes_scene::node_graph::gltf_import::assemble_import_graph(&path)
                .unwrap_or_else(|e| panic!("{name}: assemble failed: {e}"));
            let duration_s = skeleton_pose_duration_s_or_static(&def);
            let rgba = render_import_def_at_progress(def, w, h, phase, duration_s, &name);

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

/// GLTF_ANIMATION_DESIGN.md A1 deliverable 4 / GLTF_ANIM_RUNTIME_V2_
/// DESIGN.md P2: the animation source node (now `path` + per-channel
/// node selectors, no keyframe Tables) survives V1 JSON save→reload —
/// the STANDARD section 5 gate must PROVE this, not assume it.
#[test]
fn animation_selectors_survive_json_round_trip() {
    use manifold_nodes_scene::node_graph::gltf_load::{GltfObjectAnimation, QuatTrack, Vec3Track};

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
        lights: Vec::new(),
        cameras: Vec::new(),
        camera_report_lines: Vec::new(),
        texture_dims: Vec::new(),
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
        lights: Vec::new(),
        cameras: Vec::new(),
        camera_report_lines: Vec::new(),
        texture_dims: Vec::new(),
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

/// Override an inner node param on an assembled def the way the runtime will
/// actually see it. Card bindings SHADOW node params — `apply_binding_defaults`
/// stamps every binding's `default_value` onto its target param at build time —
/// so writing `node.params` alone is silently reverted for any exposed param,
/// and the test renders the untouched scene while looking like it configured
/// one. Addressed by `node_id`/param, never by binding id: ids are
/// `{node_doc_id}_{param}` and are reassigned whenever exposure stamping
/// changes. Panics on a missing node, a missing param, or a card response that
/// is not pass-through, so a stale address fails loudly.
#[cfg(feature = "gpu-proofs")]
fn set_bound_param(def: &mut EffectGraphDef, node_id: &str, param: &str, value: f32) {
    fn set_node_param(
        nodes: &mut [EffectGraphNode],
        node_id: &str,
        param: &str,
        value: f32,
    ) -> bool {
        let mut found = false;
        for node in nodes.iter_mut() {
            if node.node_id.as_str() == node_id {
                assert!(
                    node.params.contains_key(param)
                        || manifold_nodes_scene::node_graph::scene_exposure::metadata_for_node_type(&node.type_id)
                            .iter()
                            .any(|m| m.name == param),
                    "node `{node_id}` ({}) has no param `{param}`",
                    node.type_id
                );
                node.params.insert(param.to_string(), float(value));
                found = true;
            }
            if let Some(body) = node.group.as_mut() {
                found |= set_node_param(&mut body.nodes, node_id, param, value);
            }
        }
        found
    }
    assert!(
        set_node_param(&mut def.nodes, node_id, param, value),
        "assembled def has no node with node_id `{node_id}`"
    );

    let Some(meta) = def.preset_metadata.as_mut() else { return };
    let manifold_core::effect_graph_def::PresetMetadata { params, bindings, .. } = meta;
    for binding in bindings.iter_mut() {
        let BindingTarget::Node { node_id: target_node, param: target_param } = &binding.target
        else {
            continue;
        };
        if target_node.as_str() != node_id || target_param != param {
            continue;
        }
        let pass_through = (binding.scale - 1.0).abs() < 1e-6
            && binding.offset.abs() < 1e-6
            && params.iter().find(|p| p.id == binding.id).is_none_or(|p| {
                !p.invert && p.curve == manifold_core::macro_bank::MacroCurve::Linear
            });
        assert!(
            pass_through,
            "binding `{}` remaps its target — set_bound_param only handles pass-through cards",
            binding.id
        );
        binding.default_value = value;
    }
}

/// Every phase pair must differ by at least this mean-abs diff (0–255
/// scale) — catches a frozen animation. The floor sits 3x under the
/// smallest genuine phase delta in the suite (CesiumMilkTruck's wheel
/// spin, measured 0.032); a single stray hot pixel in a 256x256 frame
/// is ~0.004, so one-pixel flicker no longer counts as "distinct"
/// (BUG-gkaw, write-only goldens: `assert_ne!` proved almost nothing).
#[cfg(feature = "gpu-proofs")]
const PHASE_DISTINCT_FLOOR: f64 = 0.01;

/// Golden compare tolerance, same value and units as
/// `glb_conformance.rs`'s `check_golden` (mean-abs over all RGBA bytes,
/// 0–255 scale, manifest.json's `mean_abs_tol: 2.0`).
#[cfg(feature = "gpu-proofs")]
const GOLDEN_MEAN_ABS_TOL: f64 = 2.0;

/// Mean absolute difference between two same-sized RGBA8 buffers, 0–255.
#[cfg(feature = "gpu-proofs")]
fn mean_abs_diff(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len(), "mean_abs_diff: buffer size mismatch");
    let sum: f64 = a.iter().zip(b).map(|(x, y)| (f64::from(*x) - f64::from(*y)).abs()).sum();
    sum / a.len() as f64
}

/// Snapshot a phase sequence and gate it three ways:
/// 1. every frame clears a non-black floor — a blank render fails as
///    "black frame", never as a confusing pair-compare message;
/// 2. every frame pair differs by [`PHASE_DISTINCT_FLOOR`] — catches a
///    frozen animation;
/// 3. every frame matches its committed golden within
///    [`GOLDEN_MEAN_ABS_TOL`] — catches a wrong-but-moving render the
///    pairwise gates can't (BUG-gkaw: the goldens were write-only).
///
/// Frames always land in `MESH_SNAP_OUT_DIR` (default `target/mesh-snap`,
/// gitignored) so every run is inspectable. The committed
/// `tests/fixtures/gltf/goldens/` copies are refreshed only under
/// `MANIFOLD_REBASELINE_GOLDENS=1` (which skips gate 3), only once the
/// other assertions pass — a failing run must never overwrite the goldens
/// it just contradicted — and only for a human who LOOKED at the PNGs
/// before committing them.
#[cfg(feature = "gpu-proofs")]
fn assert_phase_sequence_distinct(
    stem: &str,
    subject: &str,
    phases: &[f32],
    frames: &[Vec<u8>],
    w: u32,
    h: u32,
) {
    let name = |p: f32| format!("{stem}_p{:03}.png", (p * 100.0).round() as u32);
    let write_into = |dir: &std::path::Path| {
        std::fs::create_dir_all(dir).unwrap_or_else(|e| panic!("create {}: {e}", dir.display()));
        for (p, rgba) in phases.iter().zip(frames) {
            let out = dir.join(name(*p));
            image::save_buffer(&out, rgba, w, h, image::ExtendedColorType::Rgba8)
                .unwrap_or_else(|e| panic!("save {}: {e}", out.display()));
        }
    };

    let snap_dir = std::path::PathBuf::from(
        std::env::var("MESH_SNAP_OUT_DIR").unwrap_or_else(|_| "target/mesh-snap".to_string()),
    );
    write_into(&snap_dir);
    eprintln!("{subject}: wrote {} phase frames to {}", frames.len(), snap_dir.display());

    for (p, rgba) in phases.iter().zip(frames) {
        let non_black =
            rgba.chunks_exact(4).filter(|px| px[0] != 0 || px[1] != 0 || px[2] != 0).count();
        let fraction = non_black as f64 / f64::from(w * h);
        assert!(
            fraction > 0.02,
            "{subject}: frame at progress {p} is (nearly) black — non-black fraction \
             {fraction:.4}. Blank headless render; see BUG-cs6 (skinned all-black headless, \
             environment fault) before suspecting the shader."
        );
    }

    for i in 0..frames.len() {
        for j in (i + 1)..frames.len() {
            let diff = mean_abs_diff(&frames[i], &frames[j]);
            assert!(
                diff > PHASE_DISTINCT_FLOOR,
                "{subject}: progress {} and progress {} rendered near-identical frames \
                 (mean-abs diff {diff:.4} <= floor {PHASE_DISTINCT_FLOOR}) — frozen animation \
                 or a stuck sampler",
                phases[i], phases[j]
            );
        }
    }

    let goldens = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/gltf/goldens");
    if std::env::var("MANIFOLD_REBASELINE_GOLDENS").is_ok() {
        write_into(&goldens);
        eprintln!("{subject}: re-baselined {} goldens in {}", frames.len(), goldens.display());
        return;
    }
    for (p, rgba) in phases.iter().zip(frames) {
        let golden_path = goldens.join(name(*p));
        assert!(
            golden_path.exists(),
            "{subject}: golden missing at {} — run once with MANIFOLD_REBASELINE_GOLDENS=1, \
             look at the PNG, then commit it",
            golden_path.display()
        );
        let golden = image::open(&golden_path)
            .unwrap_or_else(|e| panic!("decode golden {}: {e}", golden_path.display()))
            .to_rgba8();
        assert!(
            golden.width() == w && golden.height() == h,
            "{subject}: golden {} is {}x{}, expected {w}x{h} — regenerate with \
             MANIFOLD_REBASELINE_GOLDENS=1",
            golden_path.display(),
            golden.width(),
            golden.height()
        );
        let diff = mean_abs_diff(rgba, golden.as_raw());
        assert!(
            diff <= GOLDEN_MEAN_ABS_TOL,
            "{subject}: progress {p} diverged from committed golden {} (mean-abs diff \
             {diff:.4} > tol {GOLDEN_MEAN_ABS_TOL}) — compare against {} and re-baseline ONLY \
             after looking at both PNGs",
            golden_path.display(),
            snap_dir.join(name(*p)).display()
        );
    }
}

#[cfg(feature = "gpu-proofs")]
fn box_animated_fixture_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/gltf/khronos/BoxAnimated.glb")
}

/// BoxAnimated has an animated ancestor, so the importer uses the whole
/// hierarchy pose path. Read its authored duration through the existing pose
/// helper, rather than the now-empty single-node animation summary.
#[cfg(feature = "gpu-proofs")]
fn box_animated_duration_s() -> f32 {
    let (def, _) = assemble_import_graph(&box_animated_fixture_path())
        .expect("assemble BoxAnimated.glb");
    skeleton_pose_duration_s(&def)
}

/// `BoxAnimated.glb`'s "inner_box" (the only animated object) sits almost
/// entirely INSIDE the stationary "outer_box" shell. Under the importer's
/// default synthesized camera (~17-degree-above-horizon orbit) it is visible
/// only as a sliver through a gap at the top rim, and once its translation
/// lifts it past ~0.4 world units — well before progress=0.25 — it leaves that
/// sliver and every later phase renders identically. That is a framing fact
/// about this asset, not a wiring bug, so the four-phase gate re-points the
/// same synthesized camera near-vertical, down through the shell's open top.
/// Test-only instrumentation — never a change to the production import default.
#[cfg(feature = "gpu-proofs")]
fn point_camera_down_to_see_inner_box(def: &mut manifold_core::effect_graph_def::EffectGraphDef) {
    set_bound_param(def, "camera", "tilt", 1.4);
    set_bound_param(def, "camera", "distance", 6.0);
}

/// Render an assembled import `def` at a chosen `progress` through the
/// default beat-drive all three glTF samplers share
/// (`node.gltf_animation_source` / `node.gltf_skeleton_pose` /
/// `node.gltf_morph_weights`): `progress = wrap(beats * rate /
/// (duration_s * beats_per_second))` with `rate=1.0` and the
/// `beats_per_second=2.0` fallback picked by setting
/// `seconds = beats * 0.5`. Convergence requires finished warmup, a complete
/// frame, a non-black floor, and byte-stable pixels. Stable partial scenes
/// must not become golden candidates while another object is still loading.
#[cfg(feature = "gpu-proofs")]
fn render_import_def_at_progress(
    def: manifold_core::effect_graph_def::EffectGraphDef,
    w: u32,
    h: u32,
    progress: f32,
    duration_s: f32,
    label: &str,
) -> Vec<u8> {
    use manifold_node_engine::gpu::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use manifold_node_engine::runtime::preset_context::PresetContext;
    use manifold_node_engine::gpu::render_target::RenderTarget;
    use manifold_gpu::GpuTextureFormat;

    let beats = progress * duration_s * 2.0;
    let seconds = (beats * 0.5) as f64;

    let device = manifold_gpu::testkit::test_device();
    let format = GpuTextureFormat::Rgba16Float;
    let registry = PrimitiveRegistry::with_builtin();
    let mut generator =
        PresetRuntime::from_def_with_device(def, &registry, device.arc(), w, h, format, None)
            .unwrap_or_else(|e| {
                panic!("{label}: import graph failed PresetRuntime::from_def_with_device: {e:?}")
            });
    let target = RenderTarget::new(&device, w, h, format, label);
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
    let warmup_budget = WarmupBudget::default();
    let warmup_start = std::time::Instant::now();
    let mut attempts = 0u32;
    let mut prev_rgba: Option<Vec<u8>> = None;
    let mut stable_count = 0u32;
    let mut last_fraction = 0.0f64;
    let mut last_warmup_pending = true;
    let mut last_frame_status =
        FrameRenderStatus::PendingGeometry;
    while warmup_start.elapsed() < warmup_budget.per_layer {
        attempts += 1;
        let frame_status;
        {
            let mut enc = device.create_encoder("import-def-render");
            {
                let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
                generator.render(
                    &mut gpu,
                    &target.texture,
                    &ctx,
                    &manifold_core::params::ParamManifest::default(),
                );
                frame_status = gpu.frame_status();
            }
            enc.commit_and_wait_completed();
        }
        let bytes_per_row = w * 8;
        let readback_buf = device.create_buffer_shared(u64::from(h * bytes_per_row));
        let mut readback_enc = device.create_encoder("import-def-readback");
        readback_enc.copy_texture_to_buffer(&target.texture, &readback_buf, w, h, bytes_per_row);
        readback_enc.commit_and_wait_completed();
        let ptr = readback_buf.mapped_ptr().expect("shared readback");
        let halves: &[u16] =
            unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
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
        last_fraction = non_black as f64 / (w * h) as f64;
        last_warmup_pending = generator.warmup_pending();
        last_frame_status = frame_status;
        if !last_warmup_pending
            && frame_status == FrameRenderStatus::Complete
            && last_fraction > 0.02
            && prev_rgba.as_deref() == Some(rgba.as_slice())
        {
            stable_count += 1;
        } else {
            stable_count = 0;
        }
        let converged = stable_count >= STABLE_STREAK;
        prev_rgba = Some(rgba);
        if converged {
            return prev_rgba.expect("frame stored above");
        }
        let remaining = warmup_budget.per_layer.saturating_sub(warmup_start.elapsed());
        if remaining.is_zero() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20).min(remaining));
    }
    panic!(
        "{label}: render at progress {progress} never converged after {attempts} attempts \
         in {:?} (last non-black fraction {last_fraction:.4}, \
         warmup_pending={last_warmup_pending}, frame_status={last_frame_status:?}) — \
         blank, incomplete, or unstable render; \
         node errors: {:?}", warmup_start.elapsed(), generator.errors()
    );
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
        frames.push(render_import_def_at_progress(def, w, h, p, duration_s, "box-animated"));
    }

    assert_phase_sequence_distinct(
        "box_animated",
        "BoxAnimated.glb rigid clip",
        &phases,
        &frames,
        w,
        h,
    );
}

/// A1 gate item 2 (round-trip): build the import graph, serialize it
/// through the V1 JSON path, reload, re-render at progress 0.5, and
/// confirm a pixel match against the pre-reload progress-0.5 render —
/// proves `Table` params (the keyframe tracks) and the new
/// `node.gltf_animation_source` node type survive save→reload AND
/// stay live (not just structurally present — STANDARD section 5's
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

    let before = render_import_def_at_progress(def, w, h, 0.5, duration_s, "box-animated");
    let after = render_import_def_at_progress(reloaded, w, h, 0.5, duration_s, "box-animated");
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
/// `render_import_def_at_progress` renders as a static rest pose
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

/// A2 gate: `CesiumMan.glb` and `Fox.glb` — a real rigged, skinned,
/// animated character each — must render four VISIBLY DISTINCT frames
/// across the clip, proving the skin actually deforms (not just a
/// rigid object moving, which A1 already proved for `BoxAnimated`).
/// Written to `tests/fixtures/gltf/goldens/` alongside the A1 goldens.
#[cfg(feature = "gpu-proofs")]
#[test]
fn skinned_characters_render_four_visibly_distinct_deformed_poses() {
    let (w, h) = (256u32, 256u32);

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
            frames.push(render_import_def_at_progress(def, w, h, p, duration_s, asset));
        }

        let stem = format!("{}_skin", asset.trim_end_matches(".glb").to_lowercase());
        assert_phase_sequence_distinct(
            &stem,
            &format!("{asset} skin deformation"),
            &phases,
            &frames,
            w,
            h,
        );
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
    let flat = manifold_core::flatten::flatten_groups(&def).expect("flatten import def");
    let rigid_source = flat
        .nodes
        .iter()
        .find(|n| n.type_id == "node.gltf_skinned_mesh_source")
        .expect("rigid multi-node import must contain a skinned mesh source");
    assert_eq!(
        rigid_source.params.get("vertex_colors"),
        Some(&bool_val(true)),
        "new rigid multi-node imports must explicitly enable authored vertex colors"
    );
    let duration_s = skeleton_pose_duration_s_or_static(&def);
    assert!(duration_s > 0.0, "node-slot object must resolve a positive clip duration");

    let phases = [0.0f32, 0.25, 0.5, 0.75];
    let mut frames = Vec::new();
    for &p in &phases {
        let (def, _report) = assemble_import_graph(&path).expect("assemble CesiumMilkTruck import");
        frames.push(render_import_def_at_progress(def, w, h, p, duration_s, asset));
    }

    assert_phase_sequence_distinct(
        "cesium_milk_truck_rigid",
        &format!("{asset} node-slot palette"),
        &phases,
        &frames,
        w,
        h,
    );
}

/// A2 gate: hot-path check (CLAUDE.md content-thread discipline;
/// STANDARD section 5's content-thread gate) on the design doc's two NAMED
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
    use manifold_node_engine::gpu::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use manifold_node_engine::runtime::preset_context::PresetContext;
    use manifold_node_engine::gpu::render_target::RenderTarget;
    use manifold_gpu::GpuTextureFormat;

    for asset in ["CesiumMan.glb", "Fox.glb"] {
        let path = khronos_fixture_path(asset);
        if !path.exists() {
            eprintln!("skinned_import_hot_path_stays_under_20ms_per_frame: fixture not found at {}, skipping {asset}", path.display());
            continue;
        }
        let (def, _report) = assemble_import_graph(&path).expect("assemble skinned import");

        let (w, h) = (512u32, 512u32);
        let device = manifold_gpu::testkit::test_device();
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
            frames.push(render_import_def_at_progress(def, w, h, p, duration_s, asset));
        }

        let stem = format!("{}_morph", asset.trim_end_matches(".glb").to_lowercase());
        assert_phase_sequence_distinct(
            &stem,
            &format!("{asset} morph blending"),
            &phases,
            &frames,
            w,
            h,
        );
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

    let before = render_import_def_at_progress(def, w, h, 0.5, duration_s, "morph-import");
    let after = render_import_def_at_progress(reloaded, w, h, 0.5, duration_s, "morph-import");
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
    use manifold_node_engine::gpu::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use manifold_node_engine::runtime::preset_context::PresetContext;
    use manifold_node_engine::gpu::render_target::RenderTarget;
    use manifold_gpu::GpuTextureFormat;

    let glb = build_out_of_range_uv_glb();
    let path = std::env::temp_dir().join("manifold_uv_wrap_regression.glb");
    std::fs::write(&path, &glb).expect("write temp fixture");

    let (def, _report) = assemble_import_graph(&path).expect("assemble uv-wrap fixture");

    let (w, h) = (128u32, 128u32);
    let device = manifold_gpu::testkit::test_device();
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
    let mut last_warmup_pending = true;
    let mut last_frame_status =
        FrameRenderStatus::PendingGeometry;
    let warmup_budget = WarmupBudget::default();
    let warmup_start = std::time::Instant::now();
    let mut attempts = 0u32;
    while warmup_start.elapsed() < warmup_budget.per_layer {
        attempts += 1;
        let frame_status;
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
                frame_status = gpu.frame_status();
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
        last_warmup_pending = generator.warmup_pending();
        last_frame_status = frame_status;
        if !last_warmup_pending
            && frame_status == FrameRenderStatus::Complete
            && non_black
            && prev.as_deref() == Some(raw.as_slice())
        {
            stable += 1;
        } else {
            stable = 0;
        }
        prev = Some(raw);
        if stable >= 3 {
            converged = true;
            break;
        }
        let remaining = warmup_budget.per_layer.saturating_sub(warmup_start.elapsed());
        if remaining.is_zero() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50).min(remaining));
    }
    assert!(
        converged,
        "uv-wrap fixture render never stabilized non-black after {attempts} attempts in {:?} \
         (warmup_pending={last_warmup_pending}, frame_status={last_frame_status:?})",
        warmup_start.elapsed()
    );
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

/// F-P4 held-out-input gate (DESIGN_DOC_STANDARD.md section 5): Khronos
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
    use manifold_node_engine::gpu::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use manifold_node_engine::runtime::preset_context::PresetContext;
    use manifold_node_engine::gpu::render_target::RenderTarget;
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
    let device = manifold_gpu::testkit::test_device();
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
    let mut rgba = Vec::new();
    let mut prev_rgba: Option<Vec<u8>> = None;
    let mut stable_count = 0u32;
    let mut converged = false;
    let mut fraction = 0.0f64;
    let mut last_warmup_pending = true;
    let mut last_frame_status =
        FrameRenderStatus::PendingGeometry;
    let warmup_budget = WarmupBudget::default();
    let warmup_start = std::time::Instant::now();
    let mut attempts = 0u32;
    while warmup_start.elapsed() < warmup_budget.per_layer {
        attempts += 1;
        let frame_status;
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
                frame_status = gpu.frame_status();
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

        last_warmup_pending = generator.warmup_pending();
        last_frame_status = frame_status;
        if !last_warmup_pending
            && frame_status == FrameRenderStatus::Complete
            && fraction > 0.02
            && prev_rgba.as_deref() == Some(rgba.as_slice())
        {
            stable_count += 1;
        } else {
            stable_count = 0;
        }
        prev_rgba = Some(rgba.clone());

        if stable_count >= STABLE_STREAK {
            println!(
                "damaged_helmet_imports_wires_all_maps_and_renders_non_degenerate: converged \
                 on attempt {attempts} (non-black fraction {fraction:.4})"
            );
            converged = true;
            break;
        }
        let remaining = warmup_budget.per_layer.saturating_sub(warmup_start.elapsed());
        if remaining.is_zero() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50).min(remaining));
    }
    assert!(
        converged,
        "DamagedHelmet render never stabilized non-black after {attempts} attempts in {:?} \
         (last non-black fraction {fraction:.4}, warmup_pending={last_warmup_pending}, \
         frame_status={last_frame_status:?}) — a background texture decode may be stuck",
        warmup_start.elapsed()
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
    use manifold_node_engine::gpu::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use manifold_node_engine::runtime::preset_context::PresetContext;
    use manifold_node_engine::gpu::render_target::RenderTarget;
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
    let device = manifold_gpu::testkit::test_device();
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

/// GPU render proof: the_rosetta_stone import must actually draw non-
/// degenerate content and be saved to disk for visual inspection.
/// Needs a GPU device: run deliberately with `--features gpu-proofs`.
#[cfg(feature = "gpu-proofs")]
#[test]
fn rosetta_stone_import_renders_gpu_proof() {
    use manifold_node_engine::gpu::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use manifold_node_engine::runtime::preset_context::PresetContext;
    use manifold_node_engine::gpu::render_target::RenderTarget;
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
    let device = manifold_gpu::testkit::test_device();
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
    let mut rgba = Vec::new();
    let mut prev_rgba: Option<Vec<u8>> = None;
    let mut stable_count = 0u32;
    let mut converged = false;
    let mut fraction = 0.0f64;
    let mut last_warmup_pending = true;
    let mut last_frame_status =
        FrameRenderStatus::PendingGeometry;
    let warmup_budget = WarmupBudget::default();
    let warmup_start = std::time::Instant::now();
    let mut attempts = 0u32;
    while warmup_start.elapsed() < warmup_budget.per_layer {
        attempts += 1;
        let frame_status;
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
                frame_status = gpu.frame_status();
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

        last_warmup_pending = generator.warmup_pending();
        last_frame_status = frame_status;
        if !last_warmup_pending
            && frame_status == FrameRenderStatus::Complete
            && fraction > 0.02
            && prev_rgba.as_deref() == Some(rgba.as_slice())
        {
            stable_count += 1;
        } else {
            stable_count = 0;
        }
        prev_rgba = Some(rgba.clone());

        if stable_count >= STABLE_STREAK {
            println!(
                "rosetta_stone_import_renders_gpu_proof: converged on attempt {attempts} \
                 (non-black fraction {fraction:.4})"
            );
            converged = true;
            break;
        }
        let remaining = warmup_budget.per_layer.saturating_sub(warmup_start.elapsed());
        if remaining.is_zero() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50).min(remaining));
    }
    assert!(
        converged,
        "the_rosetta_stone render never stabilized non-black after {attempts} attempts in {:?} \
         (last non-black fraction {fraction:.4}, warmup_pending={last_warmup_pending}, \
         frame_status={last_frame_status:?})",
        warmup_start.elapsed()
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

/// Render at fixed time zero until the import is ready and non-black.
/// Reject incomplete or blank output before comparing the demo images.
#[cfg(feature = "gpu-proofs")]
fn render_once(def: EffectGraphDef, w: u32, h: u32, label: &str) -> Vec<u8> {
    use manifold_node_engine::gpu::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use manifold_node_engine::runtime::preset_context::PresetContext;
    use manifold_node_engine::gpu::render_target::RenderTarget;
    use manifold_gpu::GpuTextureFormat;

    let device = manifold_gpu::testkit::test_device();
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
    let mut non_black_fraction = 0.0f64;
    let mut last_warmup_pending = true;
    let mut last_frame_status =
        FrameRenderStatus::PendingGeometry;
    let warmup_budget = WarmupBudget::default();
    let warmup_start = std::time::Instant::now();
    let mut attempts = 0u32;
    while warmup_start.elapsed() < warmup_budget.per_layer {
        attempts += 1;
        let frame_status;
        let mut enc = device.create_encoder(label);
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
            generator.render(&mut gpu, &target.texture, &ctx, &manifold_core::params::ParamManifest::default());
            frame_status = gpu.frame_status();
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
            rgba.push((half_to_f32(px[3]).clamp(0.0, 1.0) * 255.0).round() as u8);
        }
        non_black_fraction = non_black as f64 / (w * h) as f64;
        last_warmup_pending = generator.warmup_pending();
        last_frame_status = frame_status;
        if !last_warmup_pending
            && last_frame_status == FrameRenderStatus::Complete
            && non_black_fraction > 0.02
        {
            return rgba;
        }
        let remaining = warmup_budget.per_layer.saturating_sub(warmup_start.elapsed());
        if remaining.is_zero() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20).min(remaining));
    }
    assert!(
        !last_warmup_pending
            && last_frame_status == FrameRenderStatus::Complete
            && non_black_fraction > 0.02,
        "{label}: render_once exhausted its warmup budget after {attempts} attempts in {:?} \
         without a ready, non-black frame (non-black fraction {non_black_fraction:.4}, \
         warmup_pending={last_warmup_pending}, frame_status={last_frame_status:?})",
        warmup_start.elapsed()
    );
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
    use manifold_core::effect_graph_def::EffectGraphWire;
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
                manifold_core::effect_graph_def::BindingTarget::Composite { .. } | manifold_core::effect_graph_def::BindingTarget::SceneModifier { .. } => None,
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

/// The transform that owns part `k`'s own placement: `part_transform_{k}`
/// inside a static compound group (whose `transform_{k}` is the shared
/// asset transform), else the per-object `transform_{k}`.
#[cfg(feature = "gpu-proofs")]
fn find_part_transform(
    body: &manifold_core::effect_graph_def::GroupDef,
    k: impl std::fmt::Display,
) -> Option<&EffectGraphNode> {
    let part = format!("part_transform_{k}");
    let object = format!("transform_{k}");
    body.nodes
        .iter()
        .find(|n| n.handle.as_deref() == Some(part.as_str()))
        .or_else(|| body.nodes.iter().find(|n| n.handle.as_deref() == Some(object.as_str())))
}

/// Rebuild every object group's `mesh_k`/`transform_k` pair back to the
/// PRE-BUG-221 shape: `mesh_k.translate_* = 0`, `transform_k.pos_* =
/// (old mesh translate) + (old transform pos)`. See the module comment
/// above this function for why this reconstruction is faithful.
#[cfg(feature = "gpu-proofs")]
fn reconstruct_pre_bug221_fix(def: &EffectGraphDef) -> EffectGraphDef {
    const AXES: [(&str, &str); 3] =
        [("translate_x", "pos_x"), ("translate_y", "pos_y"), ("translate_z", "pos_z")];
    fn float_of(node: &EffectGraphNode, param: &str) -> f32 {
        match node.params.get(param) {
            Some(SerializedParamValue::Float { value }) => *value,
            _ => 0.0,
        }
    }

    let mut out = def.clone();
    // Collected first, then written through `set_bound_param` — `transform_3d`
    // params carry card bindings that re-stamp their `default_value` over any
    // direct node-param write.
    let mut moves: Vec<(String, String, [f32; 3])> = Vec::new();
    for node in &out.nodes {
        let Some(body) = node.group.as_ref() else { continue };
        for mesh in &body.nodes {
            let Some(k) = mesh.handle.as_deref().and_then(|h| h.strip_prefix("mesh_")) else {
                continue;
            };
            let Some(transform) = find_part_transform(body, k) else {
                continue;
            };
            let mut summed = [0.0f32; 3];
            for (i, (mesh_param, transform_param)) in AXES.iter().enumerate() {
                summed[i] = float_of(mesh, mesh_param) + float_of(transform, transform_param);
            }
            moves.push((
                mesh.node_id.as_str().to_string(),
                transform.node_id.as_str().to_string(),
                summed,
            ));
        }
    }
    for (mesh_id, transform_id, summed) in moves {
        for (i, (mesh_param, transform_param)) in AXES.iter().enumerate() {
            set_bound_param(&mut out, &mesh_id, mesh_param, 0.0);
            set_bound_param(&mut out, &transform_id, transform_param, summed[i]);
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
            let mag = |m: &manifold_nodes_scene::node_graph::gltf_load::GltfMaterialInfo| {
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
        let node_id = def
            .nodes
            .iter()
            .filter_map(|n| n.group.as_ref())
            .find_map(|body| find_part_transform(body, k))
            .filter(|n| n.type_id == "node.transform_3d")
            .unwrap_or_else(|| panic!("assembled def has no transform for part {k}"))
            .node_id
            .as_str()
            .to_string();
        set_bound_param(&mut def, &node_id, "rot_y", radians);
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
