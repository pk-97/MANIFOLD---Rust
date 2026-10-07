use super::*;

fn string_value(value: &str) -> manifold_core::effect_graph_def::SerializedParamValue {
    manifold_core::effect_graph_def::SerializedParamValue::String {
        value: value.into(),
    }
}

fn mesh_source_graph() -> EffectGraphDef {
    let mut def = graph();
    def.nodes.push(node(4, "mesh", "node.gltf_mesh_source"));
    def.nodes.push(node(5, "role", "node.fluid_role_source"));
    def.nodes.push(node(6, "transform", "node.transform_3d"));
    def.wires.extend([
        wire(4, "source", 5, "mesh_0"),
        wire(6, "transform", 5, "transform"),
        wire(5, "role", 2, "role_0"),
    ]);
    def
}

fn path_spec(id: &str, default_value: &str) -> manifold_core::effect_graph_def::StringParamSpecDef {
    manifold_core::effect_graph_def::StringParamSpecDef {
        id: id.into(),
        name: id.into(),
        default_value: default_value.into(),
        is_file_picker: true,
        use_dropdown: false,
        is_file_path: true,
    }
}

#[test]
fn declared_asset_path_relocation_and_omission_do_not_change_identity() {
    let mut authored = mesh_source_graph();
    authored.nodes[3]
        .params
        .insert("path".into(), string_value("old.glb"));
    let original = digest(&authored);

    let mut relocated = authored.clone();
    relocated.nodes[3]
        .params
        .insert("path".into(), string_value("new.glb"));
    assert_eq!(digest(&relocated), original);

    let mut omitted = authored.clone();
    omitted.nodes[3].params.remove("path");
    assert_eq!(digest(&omitted), original);

    let mut selector = authored.clone();
    selector.nodes[3].params.insert(
        "mesh_index".into(),
        manifold_core::effect_graph_def::SerializedParamValue::Int { value: 2 },
    );
    assert_ne!(
        digest(&selector),
        original,
        "mesh selection remains identity"
    );

    let mut geometry = authored;
    geometry.nodes[3].params.insert(
        "fit".into(),
        manifold_core::effect_graph_def::SerializedParamValue::Enum { value: 1 },
    );
    assert_ne!(
        digest(&geometry),
        original,
        "geometry controls remain identity"
    );
}

#[test]
fn supported_path_binding_and_spec_relocation_is_hidden_from_runtime_strings() {
    let mut authored = mesh_source_graph();
    let mut metadata = string_metadata(vec![string_binding("mesh_path", "mesh", "path")]);
    metadata
        .string_params
        .push(path_spec("mesh_path", "old.glb"));
    authored.preset_metadata = Some(metadata);

    let original = prepare(
        &authored,
        &authored,
        &[],
        &PrimitiveRegistry::with_builtin(),
    )
    .expect("supported path source")
    .pop()
    .expect("fluid source");
    assert!(
        original.string_targets.is_empty(),
        "asset paths are runtime assets"
    );
    assert_eq!(
        original.asset_nodes,
        vec![NodeId::new("mesh"), NodeId::new("role")]
    );

    let mut relocated = authored;
    let metadata = relocated.preset_metadata.as_mut().expect("metadata");
    metadata.string_bindings[0].default_value = "new.glb".into();
    metadata.string_params[0].default_value = "new.glb".into();
    let moved = prepare(
        &relocated,
        &relocated,
        &[],
        &PrimitiveRegistry::with_builtin(),
    )
    .expect("relocated path source")
    .pop()
    .expect("fluid source");
    assert_eq!(moved.digest, original.digest);
    assert!(moved.string_targets.is_empty());
    assert_eq!(
        moved.asset_nodes,
        vec![NodeId::new("mesh"), NodeId::new("role")]
    );
}

#[test]
fn shared_id_fanout_to_selected_non_path_input_keeps_default_in_identity() {
    let mut authored = mesh_source_graph();
    authored.nodes.push(node(7, "text", "node.render_text"));
    authored.nodes.push(node(8, "size", "node.texture_size"));
    authored
        .wires
        .extend([wire(7, "out", 8, "in"), wire(8, "width", 2, "gravity")]);
    let mut metadata = string_metadata(vec![
        string_binding("shared", "mesh", "path"),
        string_binding("shared", "text", "text"),
    ]);
    metadata.string_params.push(path_spec("shared", "old.glb"));
    authored.preset_metadata = Some(metadata);

    let registry = PrimitiveRegistry::with_builtin();
    let original = prepare(&authored, &authored, &[], &registry)
        .expect("fanout source")
        .pop()
        .expect("fluid source");
    assert_eq!(
        original.string_targets,
        vec![(NodeId::new("text"), "text".into())]
    );
    assert_eq!(
        original.asset_nodes,
        vec![NodeId::new("mesh"), NodeId::new("role")]
    );

    let mut changed_default = authored;
    changed_default
        .preset_metadata
        .as_mut()
        .expect("metadata")
        .string_params[0]
        .default_value = "new.glb".into();
    let changed = prepare(&changed_default, &changed_default, &[], &registry)
        .expect("changed fanout source")
        .pop()
        .expect("fluid source");
    assert_ne!(changed.digest, original.digest);
}

#[test]
fn undeclared_asset_loader_path_remains_strictly_hashed() {
    let mut authored = graph();
    authored
        .nodes
        .push(node(4, "texture", "node.gltf_texture_source"));
    authored.nodes.push(node(5, "size", "node.texture_size"));
    authored
        .wires
        .extend([wire(4, "out", 5, "in"), wire(5, "width", 2, "gravity")]);
    authored.nodes[3]
        .params
        .insert("path".into(), string_value("old.glb"));
    let original = digest(&authored);

    let mut relocated = authored.clone();
    relocated.nodes[3]
        .params
        .insert("path".into(), string_value("new.glb"));
    assert_ne!(digest(&relocated), original);

    let mut omitted = authored;
    omitted.nodes[3].params.remove("path");
    assert_ne!(digest(&omitted), original);
}

#[test]
fn coupled_rigid_asset_path_relocation_does_not_change_identity() {
    let mut authored = coupled_graph(false);
    authored.nodes[4]
        .params
        .insert("path".into(), string_value("old.glb"));
    let original = digest(&authored);

    let mut relocated = authored.clone();
    relocated.nodes[4]
        .params
        .insert("path".into(), string_value("new.glb"));
    assert_eq!(digest(&relocated), original);

    let mut omitted = authored.clone();
    omitted.nodes[4].params.remove("path");
    assert_eq!(digest(&omitted), original);

    let mut density = authored;
    density.nodes[4].params.insert(
        "density".into(),
        manifold_core::effect_graph_def::SerializedParamValue::Float { value: 4.0 },
    );
    assert_ne!(digest(&density), original, "rigid controls remain identity");
}
