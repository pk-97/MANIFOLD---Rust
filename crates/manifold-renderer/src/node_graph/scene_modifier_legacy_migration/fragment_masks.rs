//! Repair the first stock fragment-mask snapshots after the sampling bug fix.
//!
//! This migration is deliberately narrower than a general-purpose parameter
//! migration.  A saved scene-modifier graph is eligible only when its stable
//! stock id and complete topology still match the bundled recipe.  That keeps
//! authored rewires, custom recipes, and intentionally exposed mask controls
//! untouched while allowing ordinary tuned values to survive the repair.

use manifold_core::effect_graph_def::{
    BindingTarget, EffectGraphDef, EffectGraphNode, GroupDef, SerializedParamValue,
};
use manifold_core::preset_type_id::PresetTypeId;

use crate::node_graph::bundled_preset_def;

const REPAIRED_STOCK_IDS: &[&str] = &[
    "MaskedPeel",
    "SurfacePeel",
    "VortexFragments",
    "OrderedRecon",
    "OrderedReconHit",
];

/// Repair legacy stock fragment-mask sampling in a project graph.
///
/// Returns `true` when at least one nested scene-modifier snapshot changed.
/// The host graph itself is also checked so the helper remains useful for a
/// saved standalone stock recipe in tests and tools.
pub(super) fn migrate_stock_fragment_masks(def: &mut EffectGraphDef) -> bool {
    let mut changed = repair_graph(def);
    for instance in &mut def.scene_modifiers {
        changed |= migrate_stock_fragment_masks(&mut instance.graph);
    }
    changed
}

fn repair_graph(def: &mut EffectGraphDef) -> bool {
    let Some(metadata) = def.preset_metadata.as_ref() else {
        return false;
    };
    let id = metadata.id.as_str();
    if !REPAIRED_STOCK_IDS.contains(&id) {
        return false;
    }

    let Some(stock) = bundled_preset_def(&PresetTypeId::from_string(id.to_owned())) else {
        return false;
    };
    if !same_topology(def, stock) || !qualified_fragment_nodes(def, id) {
        return false;
    }

    repair_modes(&mut def.nodes)
}

/// Compare the graph's structural identity while ignoring tuned parameter
/// values and editor-only presentation. Stable document ids, node ids/types,
/// group interfaces, and every wire remain part of the qualification.
fn same_topology(a: &EffectGraphDef, b: &EffectGraphDef) -> bool {
    let same_recipe = a
        .preset_metadata
        .as_ref()
        .and_then(|metadata| metadata.scene_modifier.as_ref())
        == b.preset_metadata
            .as_ref()
            .and_then(|metadata| metadata.scene_modifier.as_ref());
    same_recipe
        && a.nodes.len() == b.nodes.len()
        && a.wires == b.wires
        && a.nodes
            .iter()
            .zip(&b.nodes)
            .all(|(a, b)| same_node_topology(a, b))
}

fn same_node_topology(a: &EffectGraphNode, b: &EffectGraphNode) -> bool {
    a.id == b.id
        && a.node_id == b.node_id
        && a.type_id == b.type_id
        && (!matches!(
            a.type_id.as_str(),
            "system.group_input" | "system.group_output"
        ) || a.handle == b.handle)
        && a.wgsl_source == b.wgsl_source
        && a.output_formats == b.output_formats
        && a.output_canvas_scales == b.output_canvas_scales
        && match (&a.group, &b.group) {
            (None, None) => true,
            (Some(a), Some(b)) => same_group_topology(a, b),
            _ => false,
        }
}

fn same_group_topology(a: &GroupDef, b: &GroupDef) -> bool {
    a.interface == b.interface
        && a.wires == b.wires
        && a.nodes.len() == b.nodes.len()
        && a.nodes
            .iter()
            .zip(&b.nodes)
            .all(|(a, b)| same_node_topology(a, b))
}

/// The topology comparison above is intentionally broad. These identity and
/// mode checks state the stock continuity contract without duplicating the
/// wire walk already covered by the structural comparison.
fn qualified_fragment_nodes(def: &EffectGraphDef, id: &str) -> bool {
    let Some(mask) = find_node(&def.nodes, "mask") else {
        return false;
    };
    if mask.type_id != "node.mesh_spatial_mask"
        || !legacy_mode(mask)
        || is_exposed_or_bound(def, &mask.node_id, "sample_mode")
    {
        return false;
    }
    let Some(morph) = find_node(&def.nodes, "mask_morph") else {
        return false;
    };
    if morph.type_id != "node.morph_mesh" {
        return false;
    }
    if matches!(id, "OrderedRecon" | "OrderedReconHit") {
        let Some(stagger) = find_node(&def.nodes, "stagger") else {
            return false;
        };
        stagger.type_id == "node.mesh_stagger_envelope"
            && legacy_mode(stagger)
            && !is_exposed_or_bound(def, &stagger.node_id, "sample_mode")
    } else {
        find_node(&def.nodes, "stagger").is_none()
    }
}

fn legacy_mode(node: &EffectGraphNode) -> bool {
    matches!(
        node.params.get("sample_mode"),
        Some(SerializedParamValue::Enum { value: 1 })
    )
}

fn is_exposed_or_bound(def: &EffectGraphDef, node_id: &manifold_core::NodeId, param: &str) -> bool {
    find_node(&def.nodes, node_id.as_str()).is_some_and(|node| node.exposed_params.contains(param))
        || def.preset_metadata.as_ref().is_some_and(|metadata| {
            metadata.bindings.iter().any(|binding| {
                matches!(&binding.target, BindingTarget::Node { node_id: target, param: target_param }
                    if target == node_id && target_param == param)
            })
        })
}

fn find_node<'a>(nodes: &'a [EffectGraphNode], node_id: &str) -> Option<&'a EffectGraphNode> {
    for node in nodes {
        if node.node_id.as_str() == node_id {
            return Some(node);
        }
        if let Some(group) = node.group.as_deref()
            && let Some(found) = find_node(&group.nodes, node_id)
        {
            return Some(found);
        }
    }
    None
}

fn repair_modes(nodes: &mut [EffectGraphNode]) -> bool {
    let mut changed = false;
    for node in nodes {
        if matches!(node.node_id.as_str(), "mask" | "stagger")
            && matches!(
                node.params.get("sample_mode"),
                Some(SerializedParamValue::Enum { value: 1 })
            )
        {
            node.params.insert(
                "sample_mode".into(),
                SerializedParamValue::Enum { value: 0 },
            );
            changed = true;
        }
        if let Some(group) = node.group.as_deref_mut() {
            changed |= repair_modes(&mut group.nodes);
        }
    }
    changed
}
