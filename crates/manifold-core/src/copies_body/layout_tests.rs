//! Old copy piles become live copy groups resting exactly where they did:
//! the transform's rotation turns each copy, Spacing counts copy sizes, and
//! each show's saved cards convert with that show's own values.

use super::*;
use crate::effect_graph_def::SerializedParamValue;
use crate::preset_def::PresetKind;
use crate::project::{EmbeddedOrigin, EmbeddedPreset, Project};

fn float(value: f32) -> serde_json::Value {
    serde_json::json!({"type": "Float", "value": value})
}

fn binding(id: &str, node: &str, param: &str, default: f32) -> serde_json::Value {
    serde_json::json!({"id": id, "label": id, "defaultValue": default,
        "target": {"kind": "node", "nodeId": node, "param": param}, "convert": {"type": "Float"}})
}

fn spec(id: &str, min: f32, max: f32, default: f32) -> serde_json::Value {
    serde_json::json!({"id": id, "name": id, "min": min, "max": max, "defaultValue": default})
}

/// A turned pile from before copy groups: the spawner on the world in
/// metres, a body wired into `copies`, its transform turned 0.08/0.12/0.04
/// at scale 0.8, and the cards `cards` adds.
fn old_pile(cards: &[(&str, &str, &str, f32)]) -> EffectGraphDef {
    let bindings: Vec<_> = cards.iter().map(|(id, node, param, default)| binding(id, node, param, *default)).collect();
    let params: Vec<_> = cards.iter().map(|(id, _, _, default)| spec(id, 0.01, 100.0, *default)).collect();
    serde_json::from_value(serde_json::json!({
        "version": 2,
        "name": "Pile",
        "presetMetadata": {"id": "Pile", "displayName": "Pile", "category": "Geometry", "oscPrefix": "p",
            "params": params, "bindings": bindings},
        "nodes": [
            {"id": 40, "nodeId": "world", "typeId": "node.physics_world",
                "params": {"copy_count": float(64.0), "copy_spacing": float(1.85), "copy_columns": float(8.0),
                    "copy_layout": {"type": "Enum", "value": 1}}},
            {"id": 120, "nodeId": "box_xf", "typeId": "node.transform_3d",
                "params": {"pos_y": float(9.0), "rot_x": float(0.08), "rot_y": float(0.12), "rot_z": float(0.04),
                    "scale_x": float(0.8), "scale_y": float(0.8), "scale_z": float(0.8)}},
            {"id": 121, "nodeId": "box_body", "typeId": "node.rigid_body", "params": {"shape": {"type": "Enum", "value": 1}}},
            {"id": 5, "nodeId": "lfo", "typeId": "node.lfo"},
            {"id": 7, "nodeId": "mesh", "typeId": "node.scene_object"}
        ],
        "wires": [
            {"fromNode": 120, "fromPort": "transform", "toNode": 121, "toPort": "transform"},
            {"fromNode": 121, "fromPort": "body", "toNode": 40, "toPort": "copies"}
        ]
    }))
    .expect("old pile graph")
}

fn node<'a>(def: &'a EffectGraphDef, stable: &str) -> &'a EffectGraphNode {
    def.nodes.iter().find(|n| n.node_id == stable).unwrap_or_else(|| panic!("{stable}"))
}

fn value(def: &EffectGraphDef, stable: &str, param: &str) -> Option<f32> {
    node(def, stable).params.get(param).and_then(scalar_value)
}

fn target<'a>(def: &'a EffectGraphDef, id: &str) -> (&'a str, &'a str) {
    let binding = def.preset_metadata.as_ref().unwrap().bindings.iter().find(|b| b.id == id).expect(id);
    match &binding.target {
        BindingTarget::Node { node_id, param } => (node_id.as_str(), param.as_str()),
        other => panic!("{other:?}"),
    }
}

/// The metres one Spacing step was for this pile (Scatter, cube, 0.8).
fn pile_unit() -> f32 {
    spacing_unit(true, platonic_width(1, 0.8))[0]
}

#[test]
fn a_stored_turn_moves_onto_copy_turn_and_spacing_counts_copy_sizes() {
    let mut def = old_pile(&[]);
    let report = migrate_world_copies(&mut def);
    assert_eq!((report.converted, report.kept.len()), (1, 0));
    assert_eq!(["rot_x", "rot_y", "rot_z"].map(|p| value(&def, "box_xf", p)), [Some(0.0); 3], "the group is unturned");
    assert_eq!(
        COPY_TURN.map(|p| value(&def, "box_body", p)),
        [Some(0.08), Some(0.12), Some(0.04)],
        "each copy keeps its turn"
    );
    let spacing = value(&def, "box_body", "copy_size_spacing").unwrap();
    assert_eq!(spacing * pile_unit(), 1.85, "the same metres, exactly");
    assert_eq!(value(&def, "box_body", "copy_spacing"), None);

    let migrated = def.clone();
    assert_eq!(migrate_world_copies(&mut def), WorldCopiesMigration::default(), "idempotent");
    assert_eq!(def, migrated);
}

#[test]
fn a_bound_turn_and_a_wired_turn_follow_onto_copy_turn() {
    let mut def = old_pile(&[("120_rot_x", "box_xf", "rot_x", 0.08), ("40_copy_layout", "world", "copy_layout", 1.0)]);
    def.wires.push(EffectGraphWire { from_node: 5, from_port: "out".into(), to_node: 120, to_port: "rot_y".into() });
    migrate_world_copies(&mut def);
    assert_eq!(target(&def, "120_rot_x"), ("box_body", "copy_turn_x"), "the card keeps its id and drives the copies");
    assert!(
        def.wires.iter().any(|w| w.from_node == 5 && w.to_node == 121 && w.to_port == "copy_turn_y"),
        "the wire moves onto Copy Turn"
    );
    assert!(!def.wires.iter().any(|w| w.to_node == 120 && w.to_port.starts_with("rot_")));
    let layout = def.preset_metadata.as_ref().unwrap().params.iter().find(|p| p.id == "40_copy_layout").unwrap();
    assert_eq!((layout.max, layout.value_labels.clone()), ((COPY_LAYOUT_LABELS.len() - 1) as f32, COPY_LAYOUT_LABELS.map(String::from).to_vec()));
}

#[test]
fn a_transform_that_feeds_more_than_the_body_is_split() {
    let mut def = old_pile(&[("120_scale_x", "box_xf", "scale_x", 0.8)]);
    def.wires.push(EffectGraphWire { from_node: 120, from_port: "transform".into(), to_node: 7, to_port: "transform".into() });
    def.wires.push(EffectGraphWire { from_node: 5, from_port: "out".into(), to_node: 120, to_port: "pos_x".into() });
    migrate_world_copies(&mut def);
    assert_eq!(value(&def, "box_xf", "rot_x"), Some(0.08), "the other node keeps its turned transform");
    let copy = node(&def, "box_xf_copies");
    assert_eq!(value(&def, "box_xf_copies", "rot_x"), Some(0.0));
    assert!(def.wires.iter().any(|w| w.from_node == copy.id && w.to_node == 121 && w.to_port == "transform"));
    assert!(def.wires.iter().any(|w| w.from_node == 120 && w.to_node == 7));
    assert!(def.wires.iter().any(|w| w.from_node == 5 && w.to_node == copy.id && w.to_port == "pos_x"), "drivers fan out");
    let scales: Vec<_> = def
        .preset_metadata
        .as_ref()
        .unwrap()
        .bindings
        .iter()
        .filter(|b| b.id == "120_scale_x")
        .map(|b| match &b.target {
            BindingTarget::Node { node_id, .. } => node_id.to_string(),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(scales, ["box_xf", "box_xf_copies"], "the card drives both");
}

#[test]
fn shapes_the_load_cannot_rebuild_are_left_as_they_were_and_named() {
    let wired = |port: &str| {
        let mut def = old_pile(&[]);
        def.wires.push(EffectGraphWire { from_node: 5, from_port: "out".into(), to_node: 120, to_port: port.into() });
        def
    };
    let mut stretched = old_pile(&[]);
    stretched.nodes.iter_mut().find(|n| n.id == 120).unwrap().params.insert("scale_y".into(), SerializedParamValue::Float { value: 2.0 });
    let mut sourced = old_pile(&[]);
    sourced.wires.push(EffectGraphWire { from_node: 5, from_port: "out".into(), to_node: 121, to_port: "source".into() });
    let mut foreign = old_pile(&[]);
    foreign.wires.retain(|w| w.to_port != "transform");
    foreign.wires.push(EffectGraphWire { from_node: 5, from_port: "out".into(), to_node: 121, to_port: "transform".into() });
    // Where the saved graph says the copy's size, Spacing keeps its metres;
    // where it does not, it takes the default multiple.
    let pile_metres = Some(1.85);
    for (mut def, why, metres) in [
        (wired("scale_x"), "Scale is wired", pile_metres),
        (stretched, "differs between axes", pile_metres),
        (sourced, "imported mesh", None),
        (foreign, "Transform node", None),
    ] {
        let report = migrate_world_copies(&mut def);
        assert_eq!(report.converted, 0, "{why}");
        assert!(report.kept.iter().any(|line| line.contains(why)), "{why}: {:?}", report.kept);
        assert_eq!(value(&def, "box_xf", "rot_x"), Some(0.08), "{why}: left as it was");
        assert_eq!(value(&def, "box_body", "copy_spacing"), None, "{why}: the param has no metres form");
        let spacing = value(&def, "box_body", "copy_size_spacing").unwrap();
        match metres {
            Some(metres) => assert!((spacing * pile_unit() - metres).abs() < 1e-6, "{why}: {spacing}"),
            None => assert_eq!(spacing, SIZE_SPACING_DEFAULT, "{why}"),
        }
    }
}

#[test]
fn a_body_with_nothing_on_its_transform_converts_as_an_unturned_copy_at_scale_one() {
    let mut def = old_pile(&[]);
    def.wires.retain(|w| w.to_port != "transform");
    let report = migrate_world_copies(&mut def);
    assert_eq!((report.converted, report.kept.len()), (1, 0));
    let spacing = value(&def, "box_body", "copy_size_spacing").unwrap();
    assert!((spacing * spacing_unit(true, platonic_width(1, 1.0))[0] - 1.85).abs() < 1e-6);
    assert_eq!(COPY_TURN.map(|p| value(&def, "box_body", p)), [None; 3]);
    assert_eq!(value(&def, "box_xf", "rot_x"), Some(0.08), "an unwired transform is not the copies'");
}

#[test]
fn a_spacing_card_of_unknown_size_takes_the_default_multiple() {
    let mut def = old_pile(&[("40_copy_spacing", "world", "copy_spacing", 1.85)]);
    def.wires.push(EffectGraphWire { from_node: 5, from_port: "out".into(), to_node: 121, to_port: "source".into() });
    let report = migrate_world_copies(&mut def);
    assert_eq!(target(&def, "40_copy_spacing"), ("box_body", "copy_size_spacing"));
    let meta = def.preset_metadata.as_ref().unwrap();
    let spec = meta.params.iter().find(|p| p.id == "40_copy_spacing").unwrap();
    assert_eq!((spec.min, spec.max, spec.default_value), (SIZE_SPACING_RANGE.0, SIZE_SPACING_RANGE.1, SIZE_SPACING_DEFAULT));
    let card = meta.bindings.iter().find(|b| b.id == "40_copy_spacing").unwrap();
    assert_eq!(card.default_value, SIZE_SPACING_DEFAULT);
    assert_eq!(report.spacing_cards, [SpacingCard { card: "40_copy_spacing".into(), size: None }]);
    assert_eq!(report.spacing_cards[0].convert(3.0, |_| None), SIZE_SPACING_DEFAULT, "a show's saved metres too");
}

#[test]
fn a_split_transform_s_copy_takes_a_handle_no_node_has() {
    let mut def = old_pile(&[]);
    for (id, handle) in [(120, "Box Transform"), (7, "Box Transform copies")] {
        def.nodes.iter_mut().find(|n| n.id == id).unwrap().handle = Some(handle.into());
    }
    def.wires.push(EffectGraphWire { from_node: 120, from_port: "transform".into(), to_node: 7, to_port: "transform".into() });
    migrate_world_copies(&mut def);
    assert_eq!(node(&def, "box_xf").handle.as_deref(), Some("Box Transform"));
    assert_eq!(node(&def, "box_xf_copies").handle.as_deref(), Some("Box Transform copies 2"));
}

/// A pile saved with its Spacing and Scale on cards, in a layer graph of
/// its own and in a layer that tracks the file's copy of the preset.
fn show(own_graph: bool) -> Project {
    let def = old_pile(&[("40_copy_spacing", "world", "copy_spacing", 1.85), ("120_scale_x", "box_xf", "scale_x", 0.8)]);
    let mut project = Project::default();
    let mut layer = crate::layer::Layer::new_generator(
        "Pile".into(),
        crate::preset_type_id::PresetTypeId::from_string("Pile".into()),
        0,
    );
    let fx = layer.gen_params_mut().unwrap();
    if own_graph {
        fx.graph = Some(def);
        fx.refresh_manifest_from_graph();
    } else {
        project.embedded_presets.push(EmbeddedPreset { kind: PresetKind::Generator, def, origin: EmbeddedOrigin::Snapshot });
    }
    project.timeline.layers.push(layer);
    project
}

#[test]
fn a_show_s_saved_spacing_converts_with_its_own_scale() {
    let mut project = show(true);
    let fx = project.timeline.layers[0].gen_params_mut().unwrap();
    assert!(fx.set_base_param("40_copy_spacing", 3.0) && fx.set_base_param("120_scale_x", 0.5));
    project.on_after_deserialize();

    let fx = project.timeline.layers[0].gen_params().unwrap();
    let unit = spacing_unit(true, platonic_width(1, 0.5))[0];
    assert_eq!(fx.get_base_param("40_copy_spacing"), size_multiple(3.0, unit), "this show's 0.5 scale, not the graph's 0.8");
    assert!((fx.get_base_param("40_copy_spacing") * unit - 3.0).abs() < 1e-6);
    assert_eq!(target(fx.graph.as_ref().unwrap(), "40_copy_spacing"), ("box_body", "copy_size_spacing"));

    let converted = fx.get_base_param("40_copy_spacing");
    project.on_after_deserialize();
    assert_eq!(project.timeline.layers[0].gen_params().unwrap().get_base_param("40_copy_spacing"), converted, "idempotent");
}

/// The stock preset now reads Spacing and the transform's rotation
/// differently, so a layer that tracked it takes the file's converted copy
/// as its own graph and plays what it played when saved.
#[test]
fn a_tracking_layer_takes_the_file_s_converted_preset_as_its_own() {
    let mut project = show(false);
    project.on_after_deserialize();
    let fx = project.timeline.layers[0].gen_params().unwrap();
    let def = fx.graph.as_ref().expect("the layer plays the file's copy");
    assert_eq!(value(def, "box_body", "copy_turn_x"), Some(0.08));
    assert_eq!(target(def, "40_copy_spacing"), ("box_body", "copy_size_spacing"));
    assert!(project.load_report.migration_notes.iter().any(|n| n.contains("no longer follows")), "{:?}", project.load_report.migration_notes);
}

/// Count, Layout and Columns used to wait for Reset, and a transform change
/// moved the copies at frame time keeping their speed. A show that modulates
/// one of them now plays it live, and the load report says so.
#[test]
fn a_modulated_count_card_is_named_as_now_live() {
    use crate::effects::ParameterDriver;
    use crate::types::{BeatDivision, DriverWaveform};

    let def = old_pile(&[("40_copy_count", "world", "copy_count", 64.0), ("120_pos_x", "box_xf", "pos_x", 0.0)]);
    let mut project = Project::default();
    let mut layer =
        crate::layer::Layer::new_generator("Pile".into(), crate::preset_type_id::PresetTypeId::from_string("Pile".into()), 0);
    let fx = layer.gen_params_mut().unwrap();
    fx.graph = Some(def);
    fx.refresh_manifest_from_graph();
    fx.drivers = Some(vec![ParameterDriver::new("40_copy_count", BeatDivision::Quarter, DriverWaveform::Sine)]);
    project.timeline.layers.push(layer);
    project.on_after_deserialize();

    let notes = &project.load_report.migration_notes;
    assert!(notes.iter().any(|n| n.contains("40_copy_count") && n.contains("wait for Reset")), "{notes:?}");
    assert!(!notes.iter().any(|n| n.contains("120_pos_x")), "an unmodulated card is not named: {notes:?}");
}
