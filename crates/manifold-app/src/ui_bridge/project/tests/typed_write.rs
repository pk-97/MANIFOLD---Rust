//! The scene panel's one write path keeps each unbound param's declared
//! type, refuses an undeclared param or a non-finite value, and undo
//! restores the exact old value.

use super::*;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::effects::ParamConvert;

/// A root-level node param of `convert`'s shape that no card binds.
fn unbound(def: &EffectGraphDef, convert: ParamConvert) -> (u32, String) {
    def.nodes
        .iter()
        .find_map(|node| {
            manifold_core::param_lookup::node_params(&node.type_id)
                .into_iter()
                .filter(|meta| meta.convert == convert)
                .find(|meta| super::super::scene_write::scene_binding_id(def, &[], node.id, &meta.name).is_none())
                .map(|meta| (node.id, meta.name))
        })
        .unwrap_or_else(|| panic!("the preset has an unbound {convert:?} param"))
}

fn stored(project: &Project, layer_id: &LayerId, node: u32, param: &str) -> Option<SerializedParamValue> {
    effective_def(project, layer_id).nodes.iter().find(|n| n.id == node).and_then(|n| n.params.get(param)).cloned()
}

#[test]
fn scene_write_keeps_each_declared_type_and_undo_restores_the_old_value() {
    let (mut project, layer_id, _) = physics_solids_layer_project();
    let def = effective_def(&project, &layer_id);
    type Declared = fn(&SerializedParamValue) -> bool;
    let cases: [(ParamConvert, f32, Declared); 3] = [
        (ParamConvert::BoolThreshold, 0.0, |value| matches!(value, SerializedParamValue::Bool { value: false })),
        (ParamConvert::EnumRound, 1.0, |value| matches!(value, SerializedParamValue::Enum { value: 1 })),
        (ParamConvert::IntRound, 3.0, |value| matches!(value, SerializedParamValue::Int { value: 3 })),
    ];
    for (convert, value, declared) in cases {
        let (node, param) = unbound(&def, convert);
        let before = stored(&project, &layer_id, node, &param);
        let mut write = apply_scene_param_write(&project, &layer_id, Vec::new(), node, &param, value).expect("a write");
        write.execute(&mut project);
        let after = stored(&project, &layer_id, node, &param).expect("the write lands");
        assert!(declared(&after), "{param} ({convert:?}) wrote {after:?}");
        write.undo(&mut project);
        assert_eq!(stored(&project, &layer_id, node, &param), before, "undo restores {param} exactly");
    }

    // A param the node does not declare would break the layer's graph build,
    // and a NaN or infinity is never written: both are refused.
    let (node, param) = unbound(&def, ParamConvert::BoolThreshold);
    assert!(apply_scene_param_write(&project, &layer_id, Vec::new(), node, "not_declared", 0.25).is_none());
    for value in [f32::NAN, f32::INFINITY] {
        assert!(apply_scene_param_write(&project, &layer_id, Vec::new(), node, &param, value).is_none());
    }
}
