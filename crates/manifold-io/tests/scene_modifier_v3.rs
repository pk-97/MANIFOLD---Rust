use manifold_io::preset_file::{deserialize_preset, serialize_preset};

fn recipe() -> serde_json::Value {
    serde_json::json!({
        "version": 3,
        "nodes": [],
        "wires": [],
        "presetMetadata": {
            "id": "test.sceneModifier",
            "displayName": "Authored modifier",
            "category": "Geometry",
            "oscPrefix": "authored_modifier",
            "params": [{"id": "enabled", "name": "Enabled", "min": 0.0,
                        "max": 1.0, "defaultValue": 1.0, "isToggle": true}],
            "bindings": [],
            "sceneModifier": {"schemaVersion": 1, "singleton": false,
                              "enabledParam": "enabled", "stages": []}
        }
    })
}

#[test]
fn scene_modifier_v3_standalone_recipe_roundtrip() {
    let parsed = deserialize_preset(&recipe().to_string()).unwrap();
    let saved = serialize_preset(&parsed).unwrap();
    assert_eq!(parsed, deserialize_preset(&saved).unwrap());
    assert!(parsed.preset_metadata.unwrap().scene_modifier.is_some());
    assert!(
        !saved.contains("meshFrames"),
        "bare recipes have no host calibration"
    );
}

#[test]
fn scene_modifier_v3_authored_fixture_roundtrips_nondefault_controls_and_stages() {
    let source = include_str!("fixtures/scene_modifier_recipe_v3.json");
    let parsed = deserialize_preset(source).expect("authored v3 fixture parses");
    let metadata = parsed.preset_metadata.as_ref().expect("fixture metadata");
    let recipe = metadata.scene_modifier.as_ref().expect("fixture recipe");
    assert_eq!(parsed.version, 3);
    assert_eq!(recipe.preparation_params, vec!["cellSize".to_string()]);
    assert_eq!(recipe.stages.len(), 2);
    assert_eq!(metadata.params[1].id, "amount");
    assert_eq!(metadata.params[1].default_value, 0.35);

    let saved = serialize_preset(&parsed).expect("authored fixture serializes");
    let reparsed = deserialize_preset(&saved).expect("serialized fixture reparses");
    assert_eq!(parsed, reparsed);
}

#[test]
fn scene_modifier_v3_rejects_future_and_underdeclared_versions() {
    for version in [0, 1, 2, 4] {
        let mut raw = recipe();
        raw["version"] = version.into();
        assert!(
            deserialize_preset(&raw.to_string()).is_err(),
            "version {version}"
        );
    }
    let mut raw = recipe();
    raw["presetMetadata"]["sceneModifier"]["schemaVersion"] = 2.into();
    assert!(deserialize_preset(&raw.to_string()).is_err());
}

#[test]
fn scene_modifier_v3_nested_future_definition_is_rejected_before_catalog_install() {
    let mut future = recipe();
    future["version"] = 999.into();
    let graph = serde_json::json!({
        "version": 3, "nodes": [], "wires": [],
        "sceneModifiers": [{"id": "instance", "scene": {"scope": [], "node": "scene"},
                            "targets": "allObjects", "graph": future}]
    });
    assert!(deserialize_preset(&graph.to_string()).is_err());
    let mut project = serde_json::to_value(manifold_core::project::Project::default()).unwrap();
    project["embeddedPresets"] = serde_json::json!([{
        "kind": "generator", "def": graph, "origin": "Saved"
    }]);
    let registered = std::cell::Cell::new(false);
    let result = manifold_io::loader::load_project_from_json_with(&project.to_string(), |_| {
        registered.set(true)
    });
    let error = result.expect_err("nested future schema must be rejected");
    assert!(
        error
            .to_string()
            .contains("sceneModifiers[0].graph.version"),
        "{error}"
    );
    assert!(
        !registered.get(),
        "invalid schema must not mutate the catalog"
    );
}

#[test]
fn scene_modifier_v3_legacy_standalone_graphs_remain_readable() {
    for version in [1, 2] {
        let raw = serde_json::json!({"version": version, "nodes": [], "wires": []});
        let parsed = deserialize_preset(&raw.to_string()).unwrap();
        assert_eq!(parsed.version, version);
        assert!(parsed.scene_modifiers.is_empty());
    }
}

#[test]
fn scene_modifier_v3_owner_frames_and_local_snapshot_survive_project_reload() {
    let mut raw = serde_json::json!({
        "version": 3, "nodes": [], "wires": [],
        "sceneModifiers": [{
            "id": "modifier/a:b", "scene": {"node": "scene"},
            "targets": {"explicit": {"objects": [{"scope": ["scan/group"], "node": "part:a/b"}]}},
            "meshFrames": [{
                "target": {"scope": ["scan/group"], "node": "part:a/b"},
                "source": {"scope": ["scan/group"], "node": "mesh"},
                "sourceDefinitionHash": "test-source-fingerprint",
                "sourceOffset": [-0.75, 0.2, 0.1], "sceneRadius": 3.5
            }],
            "graph": recipe()
        }]
    });
    let mut host_metadata = recipe()["presetMetadata"].clone();
    host_metadata
        .as_object_mut()
        .unwrap()
        .remove("sceneModifier");
    host_metadata["id"] = "test.owner".into();
    raw["presetMetadata"] = host_metadata;
    let graph = deserialize_preset(&raw.to_string()).expect("owner needs no recipe metadata");
    let saved = serialize_preset(&graph).unwrap();
    assert_eq!(graph, deserialize_preset(&saved).unwrap());

    let mut project = manifold_core::project::Project::default();
    project
        .embedded_presets
        .push(manifold_core::project::EmbeddedPreset {
            kind: manifold_core::preset_def::PresetKind::Generator,
            def: graph.clone(),
            origin: manifold_core::project::EmbeddedOrigin::Saved,
        });
    let back = manifold_io::loader::load_project_from_json_with(
        &serde_json::to_string(&project).unwrap(),
        |_| {},
    )
    .unwrap();
    assert_eq!(back.embedded_presets[0].def, graph);
    assert_eq!(back.project_version, "1.15.0");
}
