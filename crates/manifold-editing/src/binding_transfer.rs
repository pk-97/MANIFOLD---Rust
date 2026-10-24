//! The one card-metadata copier, used by Duplicate and Paste (typed
//! identity D5, D-117).
//!
//! A copy is independent, like Blender's Shift+D: every card row the copied
//! nodes are bound to, Model File included, gets a fresh id and starts at
//! the source's value, so changing the copy never changes the source.
//! Linked copies (Blender's Alt+D) are a later feature.

use std::collections::{BTreeMap, HashSet};

use manifold_core::NodeId;
use manifold_core::effect_graph_def::{BindingTarget, PresetMetadata};

/// Copy every binding of `source` whose target node was copied (`node_map`
/// pairs source and copy) into `dest`, with its spec. A source id becomes
/// `<id>_duplicate` (then `_2`, `_3`…) once, and every copied target under
/// it shares that one new id. User-added cards copy too. `relabel` renames a
/// section, label or name for the copy, given the source node it belongs to.
/// Returns `(source id, copy id)` pairs, numeric and string, sorted.
pub fn transfer_bindings(
    source: &PresetMetadata,
    dest: &mut PresetMetadata,
    node_map: &[(NodeId, NodeId)],
    relabel: &dyn Fn(&str, &NodeId) -> String,
) -> Vec<(String, String)> {
    let mut taken: HashSet<String> = dest
        .params
        .iter()
        .map(|spec| spec.id.clone())
        .chain(dest.string_params.iter().map(|spec| spec.id.clone()))
        .chain(dest.bindings.iter().map(|binding| binding.id.clone()))
        .chain(
            dest.string_bindings
                .iter()
                .map(|binding| binding.id.clone()),
        )
        .collect();
    let copy_of = |target: &BindingTarget| match target {
        BindingTarget::Node { node_id, param } => node_map
            .iter()
            .find(|(old, _)| old == node_id)
            .map(|(_, new)| {
                (
                    node_id.clone(),
                    BindingTarget::Node {
                        node_id: new.clone(),
                        param: param.clone(),
                    },
                )
            }),
        _ => None,
    };
    let mut remaps: BTreeMap<String, String> = BTreeMap::new();
    let mut owners: BTreeMap<String, NodeId> = BTreeMap::new();
    let mut fresh = |id: &str, owner: &NodeId, remaps: &mut BTreeMap<String, String>| {
        owners
            .entry(id.to_string())
            .or_insert_with(|| owner.clone());
        remaps
            .entry(id.to_string())
            .or_insert_with(|| unique(&mut taken, &format!("{id}_duplicate")))
            .clone()
    };

    for binding in &source.bindings {
        let Some((owner, target)) = copy_of(&binding.target) else {
            continue;
        };
        let mut copy = binding.clone();
        copy.id = fresh(&binding.id, &owner, &mut remaps);
        copy.target = target;
        copy.label = relabel(&copy.label, &owner);
        dest.bindings.push(copy);
    }
    for binding in &source.string_bindings {
        let Some((owner, target)) = copy_of(&binding.target) else {
            continue;
        };
        let mut copy = binding.clone();
        copy.id = fresh(&binding.id, &owner, &mut remaps);
        copy.target = target;
        copy.label = relabel(&copy.label, &owner);
        dest.string_bindings.push(copy);
    }
    for spec in &source.params {
        let (Some(id), Some(owner)) = (remaps.get(&spec.id), owners.get(&spec.id)) else {
            continue;
        };
        let mut copy = spec.clone();
        copy.id = id.clone();
        copy.section = copy.section.map(|section| relabel(&section, owner));
        dest.params.push(copy);
    }
    for spec in &source.string_params {
        let (Some(id), Some(owner)) = (remaps.get(&spec.id), owners.get(&spec.id)) else {
            continue;
        };
        let mut copy = spec.clone();
        copy.id = id.clone();
        copy.name = relabel(&copy.name, owner);
        dest.string_params.push(copy);
    }
    remaps.into_iter().collect()
}

fn unique(taken: &mut HashSet<String>, base: &str) -> String {
    let mut value = base.to_string();
    let mut suffix = 2;
    while !taken.insert(value.clone()) {
        value = format!("{base}_{suffix}");
        suffix += 1;
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn meta() -> PresetMetadata {
        serde_json::from_value(json!({
            "id": "scene", "displayName": "Scene", "category": "Geometry", "oscPrefix": "scene",
            "params": [
                {"id": "7_scale_x", "name": "Scale X", "min": 0.0, "max": 10.0, "defaultValue": 1.0, "section": "moss — Transform"},
                {"id": "user_glow", "name": "Glow", "min": 0.0, "max": 1.0, "defaultValue": 0.5, "section": "moss — Material"},
            ],
            "bindings": [
                {"id": "7_scale_x", "label": "", "defaultValue": 1.0, "target": {"kind": "node", "nodeId": "transform_1", "param": "scale_x"}},
                {"id": "user_glow", "label": "", "defaultValue": 0.5, "userAdded": true, "target": {"kind": "node", "nodeId": "mat_1", "param": "glow"}},
            ],
            "stringParams": [{"id": "model_file_2", "name": "Model File — moss", "defaultValue": "/m/moss.glb", "isFilePicker": true}],
            "stringBindings": [
                {"id": "model_file_2", "label": "Model File", "defaultValue": "/m/moss.glb", "target": {"kind": "node", "nodeId": "mesh_1", "param": "path"}},
                {"id": "model_file_2", "label": "Model File", "defaultValue": "/m/moss.glb", "target": {"kind": "node", "nodeId": "tex_1", "param": "path"}},
            ],
        }))
        .unwrap()
    }

    #[test]
    fn a_copy_gets_its_own_rows_and_model_file_at_the_same_path() {
        let source = meta();
        let mut dest = source.clone();
        let map: Vec<(NodeId, NodeId)> = ["transform_1", "mat_1", "mesh_1", "tex_1"]
            .iter()
            .map(|id| (NodeId::new(*id), NodeId::new(format!("{id}_copy"))))
            .collect();
        let remaps = transfer_bindings(&source, &mut dest, &map, &|text, _| {
            text.replace("moss", "moss 2")
        });
        assert_eq!(
            remaps,
            vec![
                ("7_scale_x".to_string(), "7_scale_x_duplicate".to_string()),
                (
                    "model_file_2".to_string(),
                    "model_file_2_duplicate".to_string()
                ),
                ("user_glow".to_string(), "user_glow_duplicate".to_string()),
            ]
        );
        let file = dest
            .string_params
            .iter()
            .find(|spec| spec.id == "model_file_2_duplicate")
            .unwrap();
        assert_eq!(
            (file.default_value.as_str(), file.name.as_str()),
            ("/m/moss.glb", "Model File — moss 2")
        );
        assert_eq!(
            dest.string_bindings
                .iter()
                .filter(|b| b.id == "model_file_2_duplicate")
                .count(),
            2
        );
        assert_eq!(
            dest.params
                .iter()
                .find(|spec| spec.id == "user_glow_duplicate")
                .unwrap()
                .section
                .as_deref(),
            Some("moss 2 — Material")
        );
        assert!(manifold_core::binding_identity::check(&dest).is_empty());
        let again = transfer_bindings(&source, &mut dest, &map, &|text, _| text.to_string());
        assert_eq!(
            again[0].1, "7_scale_x_duplicate_2",
            "a second copy takes the next free id"
        );
    }
}
