//! Derived graph wiring for fragment stages.
//!
//! A fragment changes the logical vertex layout. This pass carries that layout
//! as a small provenance chain and inserts remaps at every multi-mesh seam.
//! It runs on the flattened derived graph only; authored preset nodes and wires
//! are never changed.

use std::collections::{BTreeMap, BTreeSet};

use manifold_core::NodeId;
use manifold_core::effect_graph_def::{
    BindingDef, BindingTarget, EffectGraphDef, EffectGraphNode, EffectGraphWire,
    SerializedParamValue,
};

use super::{SceneModifierExpandError, namespace};

type Address = (u32, String);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum RemapKind {
    Mesh,
    Scalar,
}

impl RemapKind {
    fn type_id(self) -> &'static str {
        match self {
            Self::Mesh => "node.remap_mesh_cut",
            Self::Scalar => "node.remap_cut_weights",
        }
    }

    fn stable_name(self) -> &'static str {
        match self {
            Self::Mesh => "mesh",
            Self::Scalar => "scalar",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Lineage {
    source: Address,
    maps: Vec<u32>,
}

const BANDS_PORTS: &[&str] = &[
    "bands",
    "direction_x",
    "direction_y",
    "direction_z",
    "scale",
    "source_offset_x",
    "source_offset_y",
    "source_offset_z",
];
const CELLS_PORTS: &[&str] = &[
    "cell_size",
    "scale",
    "source_offset_x",
    "source_offset_y",
    "source_offset_z",
];

fn invalid(detail: impl Into<String>) -> SceneModifierExpandError {
    SceneModifierExpandError::InvalidRecipe {
        path: "fragmentCuts".into(),
        detail: detail.into(),
    }
}

fn input(def: &EffectGraphDef, node: u32, port: &str) -> Option<EffectGraphWire> {
    def.wires
        .iter()
        .find(|wire| wire.to_node == node && wire.to_port == port)
        .cloned()
}

fn node(def: &EffectGraphDef, id: u32) -> Option<&EffectGraphNode> {
    def.nodes.iter().find(|node| node.id == id)
}

fn generated_node(
    def: &mut EffectGraphDef,
    next_id: &mut u32,
    owner: &NodeId,
    suffix: &str,
    type_id: &str,
    params: BTreeMap<String, SerializedParamValue>,
) -> Result<u32, SceneModifierExpandError> {
    let node_id = namespace::namespace_node_id(&["fragment_cut", owner.as_str(), suffix]);
    if def.nodes.iter().any(|node| node.node_id == node_id) {
        return Err(invalid(format!("generated node id collision for {suffix}")));
    }
    let id = *next_id;
    *next_id = next_id
        .checked_add(1)
        .ok_or_else(|| invalid("numeric node IDs exhausted"))?;
    def.nodes.push(EffectGraphNode {
        id,
        handle: Some(node_id.to_string()),
        node_id,
        type_id: type_id.into(),
        params,
        exposed_params: BTreeSet::new(),
        editor_pos: None,
        wgsl_source: None,
        title: Some("Fragment Cut Map".into()),
        output_formats: BTreeMap::new(),
        output_canvas_scales: BTreeMap::new(),
        group: None,
    });
    Ok(id)
}

fn next_node_id(def: &EffectGraphDef) -> Result<u32, SceneModifierExpandError> {
    def.nodes
        .iter()
        .map(|node| node.id)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| invalid("numeric node IDs exhausted"))
}

fn is_fragment(type_id: &str) -> bool {
    matches!(
        type_id,
        "node.ordered_recon_mesh" | "node.transform_mesh_patches"
    )
}

fn already_cut(def: &EffectGraphDef, fragment: &EffectGraphNode) -> bool {
    let expected =
        namespace::namespace_node_id(&["fragment_cut", fragment.node_id.as_str(), "map"]);
    let Some(map) = def.nodes.iter().find(|node| {
        node.node_id == expected
            && matches!(
                node.type_id.as_str(),
                "node.cut_mesh_bands" | "node.cut_mesh_cells"
            )
    }) else {
        return false;
    };
    ["in", "reference"].iter().all(|port| {
        input(def, fragment.id, port).is_some_and(|wire| {
            node(def, wire.from_node).is_some_and(|remap| remap.type_id == "node.remap_mesh_cut")
                && input(def, wire.from_node, "map")
                    .is_some_and(|wire| wire.from_node == map.id && wire.from_port == "map")
        })
    })
}

/// Also covers frozen legacy graphs whose fragment atoms remain inside groups.
pub(crate) fn contains_fragments(def: &EffectGraphDef) -> bool {
    fn nodes_contain(nodes: &[EffectGraphNode]) -> bool {
        nodes.iter().any(|node| {
            is_fragment(&node.type_id)
                || node
                    .group
                    .as_deref()
                    .is_some_and(|group| nodes_contain(&group.nodes))
        })
    }
    nodes_contain(&def.nodes)
}

fn is_mesh_multi(type_id: &str) -> bool {
    type_id == "node.morph_mesh"
}

fn is_mesh_unary(type_id: &str) -> bool {
    matches!(
        type_id,
        "node.skin_mesh"
            | "node.morph_targets_blend"
            | "node.displace_mesh"
            | "node.rotate_3d"
            | "node.normal_wave_mesh"
            | "node.wave_shear_mesh"
            | "node.facet_normals"
            | "node.push_along_normals"
            | "node.bend_mesh"
            | "node.taper_mesh"
            | "node.twist_mesh"
            | "node.fold_mesh"
            | "node.slice_mesh"
            | "node.shatter_mesh"
            | "node.voxelize_mesh"
            | "node.melt_mesh"
            | "node.noise_displace"
            | "node.glitch_jitter"
            | "node.ripple_mesh"
    )
}

fn is_weight_source(type_id: &str) -> bool {
    matches!(
        type_id,
        "node.mesh_ramp" | "node.mesh_stagger_envelope" | "node.mesh_spatial_mask"
    )
}

fn topo_order(def: &EffectGraphDef, ids: &[u32]) -> Result<Vec<u32>, SceneModifierExpandError> {
    let selected: BTreeSet<u32> = ids.iter().copied().collect();
    let mut indegree = BTreeMap::from_iter(ids.iter().copied().map(|id| (id, 0_u32)));
    let mut outgoing: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for wire in &def.wires {
        let mesh_dependency = node(def, wire.to_node).is_some_and(|node| {
            is_fragment(&node.type_id)
                || is_mesh_unary(&node.type_id)
                || is_mesh_multi(&node.type_id)
                || is_weight_source(&node.type_id)
                || node.type_id == "node.scene_object"
                || node.type_id == "system.mesh_output"
        }) && matches!(
            wire.to_port.as_str(),
            "in" | "reference" | "b" | "weights" | "vertices"
        );
        if mesh_dependency && selected.contains(&wire.from_node) && selected.contains(&wire.to_node)
        {
            *indegree.get_mut(&wire.to_node).expect("selected node") += 1;
            outgoing
                .entry(wire.from_node)
                .or_default()
                .push(wire.to_node);
        }
    }
    let mut ready: BTreeSet<u32> = indegree
        .iter()
        .filter_map(|(id, degree)| (*degree == 0).then_some(*id))
        .collect();
    let mut order = Vec::with_capacity(ids.len());
    while let Some(id) = ready.pop_first() {
        order.push(id);
        for child in outgoing.get(&id).into_iter().flatten() {
            let degree = indegree.get_mut(child).expect("selected child");
            *degree -= 1;
            if *degree == 0 {
                ready.insert(*child);
            }
        }
    }
    if order.len() != ids.len() {
        return Err(invalid("derived mesh graph contains a cycle"));
    }
    Ok(order)
}

fn mesh_input_lineage(lineages: &BTreeMap<Address, Lineage>, wire: &EffectGraphWire) -> Lineage {
    lineages
        .get(&(wire.from_node, wire.from_port.clone()))
        .cloned()
        .unwrap_or_else(|| Lineage {
            source: (wire.from_node, wire.from_port.clone()),
            maps: Vec::new(),
        })
}

fn longest<'a>(values: impl IntoIterator<Item = &'a Lineage>) -> Option<Lineage> {
    values
        .into_iter()
        .max_by_key(|lineage| lineage.maps.len())
        .cloned()
}

fn remap_through_map(
    def: &mut EffectGraphDef,
    next_id: &mut u32,
    source: Address,
    map_id: u32,
    cache: &mut BTreeMap<(Address, u32, RemapKind), Address>,
    kind: RemapKind,
) -> Result<Address, SceneModifierExpandError> {
    let key = (source.clone(), map_id, kind);
    if let Some(cached) = cache.get(&key) {
        return Ok(cached.clone());
    }

    let source_node = source.0.to_string();
    let map_node = map_id.to_string();
    let owner = namespace::namespace_node_id(&[
        "fragment_cut_remap",
        &source_node,
        source.1.as_str(),
        &map_node,
        kind.stable_name(),
    ]);
    let remap = generated_node(
        def,
        next_id,
        &owner,
        "remap",
        kind.type_id(),
        BTreeMap::new(),
    )?;
    def.wires.push(EffectGraphWire {
        from_node: source.0,
        from_port: source.1,
        to_node: remap,
        to_port: "in".into(),
    });
    def.wires.push(EffectGraphWire {
        from_node: map_id,
        from_port: "map".into(),
        to_node: remap,
        to_port: "map".into(),
    });
    let output = (remap, "out".into());
    cache.insert(key, output.clone());
    Ok(output)
}

#[allow(
    clippy::too_many_arguments,
    reason = "Structural remapping needs source and target provenance plus shared graph scratch"
)]
fn align_mesh(
    def: &mut EffectGraphDef,
    next_id: &mut u32,
    mut source_addr: Address,
    source: &Lineage,
    target: &Lineage,
    cache: &mut BTreeMap<(Address, u32, RemapKind), Address>,
    kind: RemapKind,
) -> Result<Address, SceneModifierExpandError> {
    if source == target {
        return Ok(source_addr);
    }
    if source.source != target.source
        || target.maps.len() < source.maps.len()
        || target.maps[..source.maps.len()] != source.maps[..]
    {
        if !target.maps.is_empty() {
            return Err(invalid(format!(
                "cannot align incompatible mesh lineage {:?} to {:?}",
                source.source, target.source
            )));
        }
        return Ok(source_addr);
    }
    for map_id in &target.maps[source.maps.len()..] {
        source_addr = remap_through_map(def, next_id, source_addr, *map_id, cache, kind)?;
    }
    Ok(source_addr)
}

fn wire_to(def: &mut EffectGraphDef, to: u32, port: &str, from: Address) {
    def.wires
        .retain(|wire| !(wire.to_node == to && wire.to_port == port));
    def.wires.push(EffectGraphWire {
        from_node: from.0,
        from_port: from.1,
        to_node: to,
        to_port: port.into(),
    });
}

/// Insert cut maps and propagate layout through downstream morphs, masks,
/// waves, and later fragment stages.
pub(super) fn apply(
    def: &mut EffectGraphDef,
    binding_sources: &mut Vec<Option<super::bindings::SceneModifierBindingSource>>,
) -> Result<(), SceneModifierExpandError> {
    let mut unprepared = false;
    let mut prepared = false;
    for fragment in def.nodes.iter().filter(|node| is_fragment(&node.type_id)) {
        if already_cut(def, fragment) {
            prepared = true;
        } else {
            unprepared = true;
        }
    }
    if !unprepared {
        return Ok(());
    }
    if prepared {
        return Err(invalid(
            "partially prepared cut graph; rebuild from its authored definition",
        ));
    }
    let ids: Vec<u32> = def.nodes.iter().map(|node| node.id).collect();
    let order = topo_order(def, &ids)?;
    let mut next_id = next_node_id(def)?;
    let mut lineages: BTreeMap<Address, Lineage> = BTreeMap::new();
    let mut remap_cache: BTreeMap<(Address, u32, RemapKind), Address> = BTreeMap::new();

    for id in order {
        let Some(current_node) = node(def, id).cloned() else {
            continue;
        };
        if current_node.type_id == "system.mesh_output" {
            if let Some(vertices) = input(def, id, "vertices") {
                lineages.insert(
                    (id, "vertices".into()),
                    mesh_input_lineage(&lineages, &vertices),
                );
            }
            continue;
        }
        let Some(current_wire) = input(def, id, "in") else {
            continue;
        };
        let current_lineage = mesh_input_lineage(&lineages, &current_wire);
        if is_fragment(&current_node.type_id) {
            let Some(reference_wire) = input(def, id, "reference") else {
                return Err(invalid(format!(
                    "{} has no `reference` wire",
                    current_node.type_id
                )));
            };
            let reference_lineage = mesh_input_lineage(&lineages, &reference_wire);
            let base = if current_lineage.maps.len() >= reference_lineage.maps.len() {
                current_lineage.clone()
            } else {
                reference_lineage.clone()
            };
            let aligned_current = align_mesh(
                def,
                &mut next_id,
                (current_wire.from_node, current_wire.from_port.clone()),
                &current_lineage,
                &base,
                &mut remap_cache,
                RemapKind::Mesh,
            )?;
            let aligned_reference = align_mesh(
                def,
                &mut next_id,
                (reference_wire.from_node, reference_wire.from_port.clone()),
                &reference_lineage,
                &base,
                &mut remap_cache,
                RemapKind::Mesh,
            )?;
            let controls = if current_node.type_id == "node.ordered_recon_mesh" {
                BANDS_PORTS
            } else {
                CELLS_PORTS
            };
            let cutter_type = if current_node.type_id == "node.ordered_recon_mesh" {
                "node.cut_mesh_bands"
            } else {
                "node.cut_mesh_cells"
            };
            let params = controls
                .iter()
                .filter_map(|name| {
                    current_node
                        .params
                        .get(*name)
                        .cloned()
                        .map(|value| ((*name).into(), value))
                })
                .collect();
            let map_id = generated_node(
                def,
                &mut next_id,
                &current_node.node_id,
                "map",
                cutter_type,
                params,
            )?;
            fan_out_bindings(
                def,
                binding_sources,
                &current_node.node_id,
                map_id,
                controls,
            )?;
            wire_to(def, map_id, "reference", aligned_reference.clone());
            for control in controls {
                if let Some(control_wire) = input(def, id, control) {
                    def.wires.push(EffectGraphWire {
                        from_node: control_wire.from_node,
                        from_port: control_wire.from_port,
                        to_node: map_id,
                        to_port: (*control).into(),
                    });
                }
            }
            let remap_current = remap_through_map(
                def,
                &mut next_id,
                aligned_current,
                map_id,
                &mut remap_cache,
                RemapKind::Mesh,
            )?;
            let remap_reference = remap_through_map(
                def,
                &mut next_id,
                aligned_reference,
                map_id,
                &mut remap_cache,
                RemapKind::Mesh,
            )?;
            wire_to(def, id, "in", remap_current);
            wire_to(def, id, "reference", remap_reference);
            lineages.insert(
                (id, "out".into()),
                Lineage {
                    source: base.source,
                    maps: base.maps.into_iter().chain([map_id]).collect(),
                },
            );
            continue;
        }
        if is_mesh_multi(&current_node.type_id) {
            let Some(other_wire) = input(def, id, "b") else {
                continue;
            };
            let other_lineage = mesh_input_lineage(&lineages, &other_wire);
            let target = if current_lineage.source == other_lineage.source {
                longest([&current_lineage, &other_lineage]).unwrap_or(current_lineage.clone())
            } else {
                current_lineage.clone()
            };
            let aligned_current = align_mesh(
                def,
                &mut next_id,
                (current_wire.from_node, current_wire.from_port.clone()),
                &current_lineage,
                &target,
                &mut remap_cache,
                RemapKind::Mesh,
            )?;
            let aligned_other = align_mesh(
                def,
                &mut next_id,
                (other_wire.from_node, other_wire.from_port.clone()),
                &other_lineage,
                &target,
                &mut remap_cache,
                RemapKind::Mesh,
            )?;
            wire_to(def, id, "in", aligned_current);
            wire_to(def, id, "b", aligned_other);
            if let Some(weights_wire) = input(def, id, "weights")
                && let Some(weights_lineage) =
                    lineages.get(&(weights_wire.from_node, weights_wire.from_port.clone()))
            {
                let aligned = align_mesh(
                    def,
                    &mut next_id,
                    (weights_wire.from_node, weights_wire.from_port.clone()),
                    weights_lineage,
                    &target,
                    &mut remap_cache,
                    RemapKind::Scalar,
                )?;
                wire_to(def, id, "weights", aligned);
            }
            lineages.insert((id, "out".into()), target);
            continue;
        }
        if is_mesh_unary(&current_node.type_id) {
            if let Some(reference) = input(def, id, "reference") {
                let reference_lineage = mesh_input_lineage(&lineages, &reference);
                let aligned = align_mesh(
                    def,
                    &mut next_id,
                    (reference.from_node, reference.from_port),
                    &reference_lineage,
                    &current_lineage,
                    &mut remap_cache,
                    RemapKind::Mesh,
                )?;
                wire_to(def, id, "reference", aligned);
            }
            if let Some(weights_wire) = input(def, id, "weights")
                && let Some(weights_lineage) =
                    lineages.get(&(weights_wire.from_node, weights_wire.from_port.clone()))
            {
                let aligned = align_mesh(
                    def,
                    &mut next_id,
                    (weights_wire.from_node, weights_wire.from_port.clone()),
                    weights_lineage,
                    &current_lineage,
                    &mut remap_cache,
                    RemapKind::Scalar,
                )?;
                wire_to(def, id, "weights", aligned);
            }
            lineages.insert((id, "out".into()), current_lineage.clone());
        }
        if is_weight_source(&current_node.type_id) {
            if let Some(weights_wire) = input(def, id, "weights")
                && let Some(weights_lineage) =
                    lineages.get(&(weights_wire.from_node, weights_wire.from_port.clone()))
            {
                let aligned = align_mesh(
                    def,
                    &mut next_id,
                    (weights_wire.from_node, weights_wire.from_port.clone()),
                    weights_lineage,
                    &current_lineage,
                    &mut remap_cache,
                    RemapKind::Scalar,
                )?;
                wire_to(def, id, "weights", aligned);
            }
            lineages.insert((id, "weights".into()), current_lineage);
        }
    }

    let objects: Vec<u32> = def
        .nodes
        .iter()
        .filter(|node| node.type_id == "node.scene_object")
        .map(|node| node.id)
        .collect();
    for object in objects {
        let Some(vertices) = input(def, object, "vertices") else {
            continue;
        };
        let Some(lineage) = lineages.get(&(vertices.from_node, vertices.from_port.clone())) else {
            continue;
        };
        let Some(map_id) = lineage.maps.last().copied() else {
            continue;
        };
        if let Some(weights_wire) = input(def, object, "weights")
            && let Some(weights_lineage) =
                lineages.get(&(weights_wire.from_node, weights_wire.from_port.clone()))
        {
            let aligned = align_mesh(
                def,
                &mut next_id,
                (weights_wire.from_node, weights_wire.from_port.clone()),
                weights_lineage,
                lineage,
                &mut remap_cache,
                RemapKind::Scalar,
            )?;
            wire_to(def, object, "weights", aligned);
        }
        wire_to(def, object, "topology", (map_id, "map".into()));
    }
    Ok(())
}

fn fan_out_bindings(
    def: &mut EffectGraphDef,
    binding_sources: &mut Vec<Option<super::bindings::SceneModifierBindingSource>>,
    fragment_id: &NodeId,
    map_id: u32,
    controls: &[&str],
) -> Result<(), SceneModifierExpandError> {
    let Some(map_node) = def.nodes.iter().find(|node| node.id == map_id) else {
        return Err(invalid("generated cutter disappeared"));
    };
    let cutter_id = map_node.node_id.clone();
    let Some(metadata) = def.preset_metadata.as_mut() else {
        return Ok(());
    };
    let additions: Vec<(
        BindingDef,
        Option<super::bindings::SceneModifierBindingSource>,
    )> = metadata
        .bindings
        .iter()
        .enumerate()
        .filter_map(|(index, binding)| {
            let BindingTarget::Node { node_id, param } = &binding.target else {
                return None;
            };
            if node_id != fragment_id || !controls.iter().any(|control| *control == param) {
                return None;
            }
            let mut copy = binding.clone();
            copy.target = BindingTarget::Node {
                node_id: cutter_id.clone(),
                param: param.clone(),
            };
            Some((copy, binding_sources.get(index).cloned().flatten()))
        })
        .collect();
    for (binding, source) in additions {
        metadata.bindings.push(binding);
        binding_sources.push(source);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: u32, node_id: &str, type_id: &str) -> EffectGraphNode {
        EffectGraphNode {
            id,
            node_id: NodeId::new(node_id),
            type_id: type_id.into(),
            handle: None,
            params: BTreeMap::new(),
            exposed_params: BTreeSet::new(),
            editor_pos: None,
            wgsl_source: None,
            title: None,
            output_formats: BTreeMap::new(),
            output_canvas_scales: BTreeMap::new(),
            group: None,
        }
    }

    fn wire(from_node: u32, from_port: &str, to_node: u32, to_port: &str) -> EffectGraphWire {
        EffectGraphWire {
            from_node,
            from_port: from_port.into(),
            to_node,
            to_port: to_port.into(),
        }
    }

    #[test]
    fn fragment_remap_is_reused_by_later_alignment_and_prepare_is_idempotent() {
        let mut def = EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            scene_modifiers: Vec::new(),
            nodes: vec![
                node(1, "source", "system.mesh_input"),
                node(2, "fragment", "node.ordered_recon_mesh"),
                node(3, "morph", "node.morph_mesh"),
                node(4, "object", "node.scene_object"),
            ],
            wires: vec![
                wire(1, "out", 2, "in"),
                wire(1, "out", 2, "reference"),
                wire(1, "out", 3, "in"),
                wire(2, "out", 3, "b"),
                wire(3, "out", 4, "vertices"),
            ],
        };
        let mut binding_sources = Vec::new();

        apply(&mut def, &mut binding_sources).expect("minimal fragment graph prepares");

        let remaps: Vec<_> = def
            .nodes
            .iter()
            .filter(|node| node.type_id == "node.remap_mesh_cut")
            .collect();
        assert_eq!(remaps.len(), 1, "later alignment reuses the fragment remap");
        let current = input(&def, 2, "in").expect("fragment current wire");
        let reference = input(&def, 2, "reference").expect("fragment reference wire");
        let morph_input = input(&def, 3, "in").expect("morph input wire");
        assert_eq!(current.from_node, reference.from_node);
        assert_eq!(current.from_node, morph_input.from_node);

        let prepared = def.clone();
        apply(&mut def, &mut binding_sources).expect("prepared graph is an idempotent no-op");
        assert_eq!(def, prepared);
    }

    #[test]
    fn remap_cache_keeps_mesh_and_scalar_outputs_distinct() {
        let mut def = EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            scene_modifiers: Vec::new(),
            nodes: vec![node(1, "source", "system.mesh_input")],
            wires: Vec::new(),
        };
        let mut next_id = 2;
        let mut cache = BTreeMap::new();
        let mesh = remap_through_map(
            &mut def,
            &mut next_id,
            (1, "out".into()),
            9,
            &mut cache,
            RemapKind::Mesh,
        )
        .expect("mesh remap");
        let scalar = remap_through_map(
            &mut def,
            &mut next_id,
            (1, "out".into()),
            9,
            &mut cache,
            RemapKind::Scalar,
        )
        .expect("scalar remap");
        let mesh_again = remap_through_map(
            &mut def,
            &mut next_id,
            (1, "out".into()),
            9,
            &mut cache,
            RemapKind::Mesh,
        )
        .expect("cached mesh remap");
        let other_source = remap_through_map(
            &mut def,
            &mut next_id,
            (2, "out".into()),
            9,
            &mut cache,
            RemapKind::Mesh,
        )
        .expect("mesh remap for another source");
        let other_port = remap_through_map(
            &mut def,
            &mut next_id,
            (1, "weights".into()),
            9,
            &mut cache,
            RemapKind::Mesh,
        )
        .expect("mesh remap for another source port");
        let other_map = remap_through_map(
            &mut def,
            &mut next_id,
            (1, "out".into()),
            10,
            &mut cache,
            RemapKind::Mesh,
        )
        .expect("mesh remap for another map");

        assert_ne!(mesh, scalar);
        assert_eq!(mesh_again, mesh);
        assert_ne!(mesh, other_source);
        assert_ne!(mesh, other_port);
        assert_ne!(mesh, other_map);
        assert_eq!(cache.len(), 5);
        assert_eq!(def.nodes.len(), 6);
        assert!(
            def.nodes
                .iter()
                .any(|node| node.type_id == "node.remap_mesh_cut")
        );
        assert!(
            def.nodes
                .iter()
                .any(|node| node.type_id == "node.remap_cut_weights")
        );
    }
}
