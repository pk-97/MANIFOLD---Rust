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
mod tests {
    use super::*;
    use manifold_core::effect_graph_def::{GroupInterface, GroupParamDef, SerializedParamValue};
    use manifold_core::params::{Param, ParamManifest};
    use crate::node_graph::{ParamValue, PrimitiveRegistry};
    use crate::preset_runtime::PresetRuntime;

    fn shipped() -> EffectGraphDef {
        serde_json::from_str(include_str!("../../assets/generator-presets/WaterDamBreakGpuFlip.json")).unwrap()
    }

    fn surface(def: &EffectGraphDef) -> &GroupDef {
        def.nodes.iter().find(|n| n.node_id.as_str() == "surface").unwrap().group.as_deref().unwrap()
    }

    fn surface_mut(def: &mut EffectGraphDef) -> &mut GroupDef {
        def.nodes.iter_mut().find(|n| n.node_id.as_str() == "surface").unwrap().group.as_deref_mut().unwrap()
    }

    fn volume(group: &GroupDef) -> &EffectGraphNode {
        group.nodes.iter().find(|n| n.node_id.as_str() == "liquid_volume").unwrap()
    }

    fn brick(group: &GroupDef) -> &EffectGraphNode {
        group.nodes.iter().find(|n| n.type_id == "node.lattice_bricks").unwrap()
    }

    fn legacy_shipped() -> EffectGraphDef {
        let mut def = shipped();
        let group = surface_mut(&mut def);
        let old = brick(group).clone();
        group.nodes.retain(|n| n.id != old.id);
        group.wires.retain(|w| w.from_node != old.id && w.to_node != old.id);
        def.preset_metadata.as_mut().unwrap().bindings.retain(|b|
            !matches!(&b.target, BindingTarget::Node { node_id, .. } if node_id == &old.node_id)
        );
        def
    }

    fn assert_preserved(before: &EffectGraphDef, after: &EffectGraphDef) {
        let old = surface(before);
        let new = surface(after);
        assert_eq!(&new.nodes[..old.nodes.len()], old.nodes.as_slice());
        assert_eq!(&new.wires[..old.wires.len()], old.wires.as_slice());
        assert_eq!(new.nodes.len(), old.nodes.len() + 1);
        assert_eq!(new.interface, old.interface);
        assert_eq!(new.tint, old.tint);
        let mut restored = after.clone();
        *surface_mut(&mut restored) = old.clone();
        restored.preset_metadata.as_mut().unwrap().bindings.truncate(
            before.preset_metadata.as_ref().unwrap().bindings.len()
        );
        assert_eq!(&restored, before, "only the schedule and mirrored binding targets are added");
    }

    #[test]
    fn legacy_gpu_flip_surface_saved_fixture_preserves_authored_graph() {
        let mut def: EffectGraphDef = serde_json::from_str(include_str!(
            "../../../manifold-io/tests/fixtures/water_layer_graph_v1160.json"
        )).unwrap();
        let before = def.clone();
        prepare(&mut def);
        assert_preserved(&before, &def);
        let group = surface(&def);
        assert_eq!(brick(group).params, volume(group).params);
        assert!(!group.wires.iter().any(|w| w.to_node == brick(group).id && w.to_port == "bounds"));
        assert_eq!(group.wires.len(), surface(&before).wires.len() + 17);
        let prepared = def.clone();
        prepare(&mut def);
        assert_eq!(def, prepared);
    }

    #[test]
    fn legacy_gpu_flip_surface_current_preset_is_byte_unchanged() {
        let mut def = shipped();
        let before = serde_json::to_vec(&def).unwrap();
        prepare(&mut def);
        prepare(&mut def);
        assert_eq!(serde_json::to_vec(&def).unwrap(), before);
    }

    #[test]
    fn legacy_gpu_flip_surface_scope_and_unsupported_inputs_stay_dense() {
        for case in 0..8 {
            let mut def = legacy_shipped();
            match case {
                0 => def.preset_metadata = None,
                1 => def.preset_metadata.as_mut().unwrap().id = manifold_core::PresetTypeId::new("WaterDamBreak"),
                2 => {
                    fn remove_step(nodes: &mut [EffectGraphNode]) {
                        for node in nodes {
                            if node.type_id == "node.gpu_flip_step" { node.type_id = "node.flip_step".into(); }
                            if let Some(group) = &mut node.group { remove_step(&mut group.nodes); }
                        }
                    }
                    remove_step(&mut def.nodes);
                }
                3 => def.nodes.iter_mut().find(|n| n.node_id.as_str() == "surface").unwrap().node_id = NodeId::new("custom_surface"),
                4 => surface_mut(&mut def).nodes.iter_mut().find(|n| n.node_id.as_str() == "liquid_volume").unwrap().type_id = "node.custom_volume".into(),
                5 => {
                    let group = surface_mut(&mut def);
                    let id = volume(group).id;
                    group.wires.retain(|w| w.to_node != id || w.to_port != "solid");
                }
                6 => {
                    let group = surface_mut(&mut def);
                    let id = volume(group).id;
                    let wire = group.wires.iter().find(|w| w.to_node == id && w.to_port == "band_extra").unwrap().clone();
                    group.wires.push(wire);
                }
                7 => {
                    let group = surface_mut(&mut def);
                    let target_handle = volume(group).handle.clone().unwrap();
                    group.interface.params.push(GroupParamDef {
                        name: "custom_detail".into(), target_handle, target_param: "resolution_scale".into(),
                        default: Some(SerializedParamValue::Int { value: 3 }),
                    });
                }
                _ => unreachable!(),
            }
            let before = def.clone();
            prepare(&mut def);
            assert_eq!(def, before, "case {case}");
        }
    }

    #[test]
    fn legacy_gpu_flip_surface_copies_exact_sources_params_and_binding_records() {
        let mut def = legacy_shipped();
        let binding = def.preset_metadata.as_mut().unwrap().bindings.iter_mut().find(|b|
            matches!(&b.target, BindingTarget::Node { node_id, param } if node_id.as_str() == "liquid_volume" && param == "resolution_scale")
        ).unwrap();
        binding.offset = 2.0;
        binding.scale = 0.5;
        binding.user_added = true;
        binding.default_mirrors_node_param = true;
        let original_binding = binding.clone();
        let group = surface_mut(&mut def);
        let volume_id = volume(group).id;
        let mut source = volume(group).clone();
        source.id = group.nodes.iter().map(|n| n.id).max().unwrap() + 1;
        source.node_id = NodeId::new("custom_band_source");
        source.type_id = "node.scalar".into();
        source.handle = Some("Liquid Volume Bricks".into());
        group.wires.retain(|w| w.to_node != volume_id || w.to_port != "band_extra");
        group.wires.push(EffectGraphWire { from_node: source.id, from_port: "out".into(), to_node: volume_id, to_port: "band_extra".into() });
        group.wires.push(EffectGraphWire { from_node: source.id, from_port: "interior".into(), to_node: volume_id, to_port: "interior".into() });
        group.nodes.push(source);
        let volume_node = group.nodes.iter_mut().find(|n| n.id == volume_id).unwrap();
        for &name in &SHARED_INPUTS[4..] {
            volume_node.params.insert(name.into(), SerializedParamValue::Float { value: 3.25 });
        }
        volume_node.params.insert("resolution_scale".into(), SerializedParamValue::Int { value: 2 });
        // An unrelated global identity and a local handle both collide with
        // the first candidate; preparation must choose a different identity.
        let mut collision = def.nodes[0].clone();
        collision.id = def.nodes.iter().map(|n| n.id).max().unwrap() + 1;
        collision.node_id = NodeId::new("liquid_volume_sparse_bricks");
        collision.group = None;
        def.nodes.push(collision);
        let before = def.clone();
        prepare(&mut def);
        assert_preserved(&before, &def);
        let group = surface(&def);
        let brick = brick(group);
        assert_ne!(brick.node_id.as_str(), "liquid_volume_sparse_bricks");
        assert_eq!(brick.params, volume(group).params);
        for &port in SHARED_INPUTS {
            let endpoint = |id| group.wires.iter().filter(|w| w.to_node == id && w.to_port == port)
                .map(|w| (w.from_node, w.from_port.as_str())).collect::<Vec<_>>();
            assert_eq!(endpoint(brick.id), endpoint(volume_id), "{port}");
        }
        assert!(!group.wires.iter().any(|w| w.to_node == brick.id && w.to_port == "interior"));
        let mut expected = original_binding;
        expected.target = BindingTarget::Node { node_id: brick.node_id.clone(), param: "resolution_scale".into() };
        assert!(def.preset_metadata.as_ref().unwrap().bindings.contains(&expected));
    }

    #[test]
    fn legacy_gpu_flip_surface_nested_group_is_prepared() {
        let mut def = legacy_shipped();
        let mut wrapper = def.nodes[0].clone();
        wrapper.id = 0;
        wrapper.node_id = NodeId::new("wrapper");
        wrapper.type_id = "group".into();
        wrapper.handle = Some("Wrapper".into());
        wrapper.params.clear();
        wrapper.group = Some(Box::new(GroupDef {
            interface: GroupInterface { inputs: Vec::new(), outputs: Vec::new(), params: Vec::new() },
            nodes: std::mem::take(&mut def.nodes), wires: std::mem::take(&mut def.wires), tint: None,
        }));
        def.nodes.push(wrapper);
        prepare(&mut def);
        let inner = &def.nodes[0].group.as_ref().unwrap().nodes;
        let group = inner.iter().find(|n| n.node_id.as_str() == "surface").unwrap().group.as_ref().unwrap();
        assert_eq!(group.nodes.iter().filter(|n| n.type_id == "node.lattice_bricks").count(), 1);
        let prepared = def.clone();
        prepare(&mut def);
        assert_eq!(def, prepared);
    }

    #[test]
    fn legacy_gpu_flip_surface_runtime_defaults_and_live_binding_fan_out() {
        let mut def = legacy_shipped();
        // Preserve an explicitly saved coarse surface with its old support,
        // independently of the bundled preset's fresh defaults.
        let metadata = def.preset_metadata.as_mut().unwrap();
        for param in &mut metadata.params {
            match param.id.as_str() {
                "surface_detail" => param.default_value = 0.0,
                "surface_particle_scale" => param.default_value = 3.0,
                _ => {}
            }
        }
        for binding in &mut metadata.bindings {
            match binding.id.as_str() {
                "surface_detail" => binding.default_value = 0.0,
                "surface_particle_scale" => binding.default_value = 3.0,
                _ => {}
            }
        }
        let surface_node = def.nodes.iter_mut().find(|n| n.node_id.as_str() == "surface").unwrap();
        surface_node.params.insert("particle_scale".into(), SerializedParamValue::Float { value: 3.0 });
        for node in &mut surface_node.group.as_deref_mut().unwrap().nodes {
            match node.node_id.as_str() {
                "liquid_volume" | "liquid_mesh" => {
                    node.params.insert("resolution_scale".into(), SerializedParamValue::Int { value: 1 });
                }
                "liquid_blobs" => {
                    node.params.insert("particle_scale".into(), SerializedParamValue::Float { value: 3.0 });
                }
                _ => {}
            }
        }
        def.preset_metadata.as_mut().unwrap().bindings.iter_mut().find(|b|
            matches!(&b.target, BindingTarget::Node { node_id, param } if node_id.as_str() == "liquid_volume" && param == "resolution_scale")
        ).unwrap().offset = 2.0;
        // Exercise the old missing-bounds path through the real loader too.
        let group = surface_mut(&mut def);
        let volume_id = volume(group).id;
        group.wires.retain(|w| w.to_node != volume_id || w.to_port != "bounds");
        let mut params = ParamManifest::from_params(def.preset_metadata.as_ref().unwrap().params.iter().cloned().map(Param::bundled).collect());
        let mut expected = def.clone();
        prepare(&mut expected);
        let brick_id = brick(surface(&expected)).node_id.clone();
        let mut runtime = PresetRuntime::from_def(def, &PrimitiveRegistry::with_builtin(), None).expect("prepared surface builds without a device");
        let targets = [NodeId::new("liquid_volume"), brick_id];
        let assert_scale = |runtime: &PresetRuntime, value| {
            for target in &targets {
                let id = runtime.graph.instance_by_node_id(target).unwrap();
                assert_eq!(runtime.graph.get_node(id).unwrap().params.get("resolution_scale"), Some(&ParamValue::Float(value)));
            }
        };
        assert_scale(&runtime, 2.0);
        let blobs = runtime.graph.instance_by_node_id(&NodeId::new("liquid_blobs")).unwrap();
        assert_eq!(runtime.graph.get_node(blobs).unwrap().params.get("particle_scale"), Some(&ParamValue::Float(3.0)));
        let target_nodes: Vec<_> = targets.iter().map(|id| runtime.graph.instance_by_node_id(id).unwrap()).collect();
        let source = |id, port: &str| runtime.graph.wires_into(id).find(|w| w.to.1 == port).unwrap().from;
        assert_eq!(source(target_nodes[0], "bounds"), source(target_nodes[1], "bounds"));
        assert_eq!(source(target_nodes[0], "blobs"), source(target_nodes[1], "blobs"));
        let detail = params.get_mut("surface_detail").unwrap();
        detail.value = 1.0;
        detail.base = 1.0;
        runtime.apply_param_values(&params);
        assert_scale(&runtime, 3.0);
    }

    #[test]
    fn gpu_flip_surface_fresh_defaults_and_live_bindings_reach_nested_nodes() {
        let def = shipped();
        let mut params = ParamManifest::from_params(def.preset_metadata.as_ref().unwrap().params.iter().cloned().map(Param::bundled).collect());
        let mut runtime = PresetRuntime::from_def(def, &PrimitiveRegistry::with_builtin(), None).unwrap();
        let assert_param = |runtime: &PresetRuntime, node: &str, name: &str, value| {
            let id = runtime.graph.instance_by_node_id(&NodeId::new(node)).unwrap();
            assert_eq!(runtime.graph.get_node(id).unwrap().params.get(name), Some(&ParamValue::Float(value)), "{node}.{name}");
        };
        for node in ["liquid_volume", "liquid_mesh", "liquid_bricks"] {
            assert_param(&runtime, node, "resolution_scale", 1.0);
        }
        assert_param(&runtime, "liquid_blobs", "particle_scale", 3.0);
        assert!(runtime.shadowed_def_params().next().is_none());
        for (name, value) in [("surface_detail", 1.0), ("surface_particle_scale", 2.2)] {
            let param = params.get_mut(name).unwrap();
            param.value = value;
            param.base = value;
        }
        runtime.apply_param_values(&params);
        for node in ["liquid_volume", "liquid_mesh", "liquid_bricks"] {
            assert_param(&runtime, node, "resolution_scale", 2.0);
        }
        assert_param(&runtime, "liquid_blobs", "particle_scale", 2.2);
    }
}
