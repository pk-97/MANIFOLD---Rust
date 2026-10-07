use manifold_core::effect_graph_def::{EffectGraphDef, EffectGraphNode, EffectGraphWire, GroupDef};
use crate::preset_runtime::gpu_flip_surface::*;
    use manifold_core::effect_graph_def::{GroupInterface, GroupParamDef, SerializedParamValue};
    use manifold_core::params::{Param, ParamManifest};
    use crate::node_graph::{ParamValue, PrimitiveRegistry};
    use crate::preset_runtime::PresetRuntime;

    fn shipped() -> EffectGraphDef {
        serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/generator-presets/WaterDamBreakGpuFlip.json"))).unwrap()
    }

    fn surface(def: &EffectGraphDef) -> &GroupDef {
        // Saved v1160 fixtures predate Water; current presets own it inside Water.
        let nodes = match def.nodes.iter().find(|node| node.node_id.as_str() == "water_family") {
            Some(family) => &family.group.as_ref().expect("Water family body").nodes,
            None => &def.nodes,
        };
        nodes.iter().find(|node| node.node_id.as_str() == "surface")
            .expect("Liquid Surface group").group.as_deref().expect("Liquid Surface body")
    }

    fn surface_node_mut(def: &mut EffectGraphDef) -> &mut EffectGraphNode {
        let family = def.nodes.iter().position(|node| node.node_id.as_str() == "water_family");
        let nodes = match family {
            Some(index) => &mut def.nodes[index].group.as_mut().expect("Water family body").nodes,
            None => &mut def.nodes,
        };
        nodes.iter_mut().find(|node| node.node_id.as_str() == "surface")
            .expect("Liquid Surface group")
    }

    fn volume(group: &GroupDef) -> &EffectGraphNode {
        group.nodes.iter().find(|n| n.node_id.as_str() == "liquid_volume").unwrap()
    }

    fn brick(group: &GroupDef) -> &EffectGraphNode {
        group.nodes.iter().find(|n| n.type_id == "node.lattice_bricks").unwrap()
    }

    fn legacy_shipped() -> EffectGraphDef {
        let mut def = shipped();
        let group = surface_node_mut(&mut def).group.as_deref_mut().unwrap();
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
        *surface_node_mut(&mut restored).group.as_deref_mut().unwrap() = old.clone();
        restored.preset_metadata.as_mut().unwrap().bindings.truncate(
            before.preset_metadata.as_ref().unwrap().bindings.len()
        );
        assert_eq!(&restored, before, "only the schedule and mirrored binding targets are added");
    }

    #[test]
    fn legacy_gpu_flip_surface_saved_fixture_preserves_authored_graph() {
        let mut def: EffectGraphDef = serde_json::from_str(include_str!(
            "../../../../manifold-io/tests/fixtures/water_layer_graph_v1160.json"
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
                3 => surface_node_mut(&mut def).node_id = NodeId::new("custom_surface"),
                4 => surface_node_mut(&mut def).group.as_deref_mut().unwrap().nodes.iter_mut().find(|n| n.node_id.as_str() == "liquid_volume").unwrap().type_id = "node.custom_volume".into(),
                5 => {
                    let group = surface_node_mut(&mut def).group.as_deref_mut().unwrap();
                    let id = volume(group).id;
                    group.wires.retain(|w| w.to_node != id || w.to_port != "solid");
                }
                6 => {
                    let group = surface_node_mut(&mut def).group.as_deref_mut().unwrap();
                    let id = volume(group).id;
                    let wire = group.wires.iter().find(|w| w.to_node == id && w.to_port == "band_extra").unwrap().clone();
                    group.wires.push(wire);
                }
                7 => {
                    let group = surface_node_mut(&mut def).group.as_deref_mut().unwrap();
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
        let group = surface_node_mut(&mut def).group.as_deref_mut().unwrap();
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
        let flat = manifold_core::flatten::flatten_groups(&def).expect("nested fixture flattens");
        assert_eq!(flat.nodes.iter().filter(|n| n.type_id == "node.lattice_bricks").count(), 1);
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
        let surface_node = surface_node_mut(&mut def);
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
        let group = surface_node_mut(&mut def).group.as_deref_mut().unwrap();
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

use manifold_core::{NodeId, effect_graph_def::BindingTarget};
