//! Runtime-only sparse preparation for the saved GPU FLIP surface.
//! Authored geometry, wires and card controls remain the source of truth.

use std::collections::{BTreeMap, BTreeSet};

use manifold_core::NodeId;
use manifold_core::effect_graph_def::{
    BindingDef, BindingTarget, EffectGraphDef, EffectGraphNode, EffectGraphWire, GroupDef,
};

const SHARED_INPUTS: &[&str] = &[
    "blobs", "cell_ranges", "solid", "bounds", "cell_size",
    "bins_x", "bins_y", "bins_z", "nodes_x", "nodes_y", "nodes_z",
    "center_x", "center_y", "center_z", "size_x", "size_y", "size_z", "band_extra",
];

fn shared_param(name: &str) -> bool {
    name == "resolution_scale" || SHARED_INPUTS[4..].contains(&name)
}

pub(super) fn prepare(def: &mut EffectGraphDef) {
    if def.preset_metadata.as_ref().is_none_or(|m| m.id.as_str() != "WaterDamBreakGpuFlip")
        || !contains_step(&def.nodes)
    {
        return;
    }
    let mut identities = BTreeSet::new();
    let mut aliased = BTreeSet::new();
    collect(&def.nodes, &mut identities, &mut aliased);
    for modifier in &def.scene_modifiers {
        collect(&modifier.graph.nodes, &mut identities, &mut aliased);
    }
    let bindings = &mut def.preset_metadata.as_mut().expect("metadata checked").bindings;
    prepare_groups(&mut def.nodes, &mut identities, &aliased, bindings);
}

fn contains_step(nodes: &[EffectGraphNode]) -> bool {
    nodes.iter().any(|n| n.type_id == "node.gpu_flip_step"
        || n.group.as_ref().is_some_and(|g| contains_step(&g.nodes)))
}

fn collect(nodes: &[EffectGraphNode], ids: &mut BTreeSet<String>, aliased: &mut BTreeSet<String>) {
    for node in nodes {
        ids.insert(node.node_id.to_string());
        if let Some(group) = &node.group {
            for param in &group.interface.params {
                if shared_param(&param.target_param) {
                    collect_alias_targets(&group.nodes, "", &param.target_handle, aliased);
                }
            }
            collect(&group.nodes, ids, aliased);
        }
    }
}

// Ancestor interfaces can address a nested leaf by its flattened handle path.
// GroupParamDef has one target, so copying this route cannot preserve fan-out.
fn collect_alias_targets(nodes: &[EffectGraphNode], prefix: &str, target: &str, ids: &mut BTreeSet<String>) {
    for node in nodes {
        let handle = format!("{prefix}{}", node.handle.as_deref().unwrap_or_default());
        if handle == target && node.type_id == "node.particle_volume" {
            ids.insert(node.node_id.to_string());
        }
        if let Some(group) = &node.group {
            collect_alias_targets(&group.nodes, &format!("{handle}/"), target, ids);
        }
    }
}

fn prepare_groups(
    nodes: &mut [EffectGraphNode],
    identities: &mut BTreeSet<String>,
    aliased: &BTreeSet<String>,
    bindings: &mut Vec<BindingDef>,
) {
    for node in nodes {
        let Some(group) = &mut node.group else { continue };
        if node.node_id.as_str() == "surface" && node.type_id == "group" {
            prepare_surface(group, identities, aliased, bindings);
        }
        prepare_groups(&mut group.nodes, identities, aliased, bindings);
    }
}

fn prepare_surface(
    group: &mut GroupDef,
    identities: &mut BTreeSet<String>,
    aliased: &BTreeSet<String>,
    bindings: &mut Vec<BindingDef>,
) {
    let volumes: Vec<_> = group.nodes.iter().filter(|n|
        n.node_id.as_str() == "liquid_volume" && n.type_id == "node.particle_volume"
    ).cloned().collect();
    // A repeated stable identity cannot be safely addressed by metadata.
    if volumes.len() != 1 { return; }
    let volume = &volumes[0];
    if aliased.contains(volume.node_id.as_str())
        || group.wires.iter().any(|w| w.to_node == volume.id && w.to_port == "bricks")
        || group.nodes.iter().filter(|n| n.id == volume.id).count() != 1
    {
        return;
    }
    let mut incoming = Vec::new();
    for &port in SHARED_INPUTS {
        let mut wires = group.wires.iter().filter(|w| w.to_node == volume.id && w.to_port == port);
        let wire = wires.next();
        if wires.next().is_some() { return; }
        if let Some(wire) = wire {
            if group.nodes.iter().filter(|n| n.id == wire.from_node).count() != 1 { return; }
            incoming.push(wire.clone());
        } else if matches!(port, "blobs" | "cell_ranges" | "solid") {
            return;
        }
        // Old surfaces predate bounds. wire_blob_bounds supplies one reduction
        // for this same blob endpoint to both consumers during graph loading.
        // Scalar inputs are optional on both primitives, with matching defaults.
    }
    let mut id = 0u32;
    while group.nodes.iter().any(|n| n.id == id) {
        let Some(next) = id.checked_add(1) else { return };
        id = next;
    }
    let mut suffix = 0u32;
    let (node_id, handle) = loop {
        let tail = if suffix == 0 { String::new() } else { format!("_{suffix}") };
        let node_id = NodeId::new(format!("liquid_volume_sparse_bricks{tail}"));
        let handle = format!("Liquid Volume Bricks{tail}");
        if !identities.contains(node_id.as_str())
            && !group.nodes.iter().any(|n| n.handle.as_deref() == Some(handle.as_str()))
        {
            break (node_id, handle);
        }
        let Some(next) = suffix.checked_add(1) else { return };
        suffix = next;
    };
    let mirrored: Vec<_> = bindings.iter().filter_map(|binding| {
        let BindingTarget::Node { node_id: target, param } = &binding.target else { return None };
        if target != &volume.node_id || !shared_param(param) { return None; }
        let mut copy = binding.clone();
        copy.target = BindingTarget::Node { node_id: node_id.clone(), param: param.clone() };
        Some(copy)
    }).collect();
    group.nodes.push(EffectGraphNode {
        id, node_id: node_id.clone(), type_id: "node.lattice_bricks".into(), handle: Some(handle),
        params: volume.params.iter().filter(|(name, _)| shared_param(name))
            .map(|(name, value)| (name.clone(), value.clone())).collect(),
        exposed_params: volume.exposed_params.iter().filter(|name| shared_param(name)).cloned().collect(),
        editor_pos: None, wgsl_source: None, title: None,
        output_formats: BTreeMap::new(), output_canvas_scales: BTreeMap::new(), group: None,
    });
    for mut wire in incoming {
        wire.to_node = id;
        group.wires.push(wire);
    }
    // Interior stays on ParticleVolume: its exterior pass applies both
    // interior and solid even outside the occupied blob bricks.
    group.wires.push(EffectGraphWire {
        from_node: id, from_port: "bricks".into(), to_node: volume.id, to_port: "bricks".into(),
    });
    identities.insert(node_id.to_string());
    bindings.extend(mirrored);
}

#[cfg(test)]
mod testkit;
