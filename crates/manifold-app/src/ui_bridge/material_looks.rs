//! Starter looks change existing factors through the material batch command.
use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef};
use manifold_core::effects::PresetInstance;
use manifold_core::scene_modifier_preset::SceneNodeRef;
use manifold_ui::panels::actions::{MaterialLook, MaterialParamWrite};

pub(super) fn recipe(look: MaterialLook) -> &'static [(&'static str, f32)] {
    match look {
        MaterialLook::Matte => &[("metallic", 0.0), ("roughness", 0.8)],
        MaterialLook::Coated => &[
            ("metallic", 0.0),
            ("roughness", 0.35),
            ("coat_mode", 2.0),
            ("clearcoat", 1.0),
            ("clearcoat_roughness", 0.1),
        ],
        MaterialLook::BrushedMetal => &[
            ("metallic", 1.0),
            ("roughness", 0.3),
            ("anisotropy_mode", 2.0),
            ("anisotropy_strength", 0.6),
        ],
        MaterialLook::Glass => &[
            ("metallic", 0.0),
            ("roughness", 0.05),
            ("glass_mode", 2.0),
            ("transmission", 1.0),
            ("ior", 1.5),
        ],
    }
}

pub(super) fn writes(
    inst: &PresetInstance,
    def: &EffectGraphDef,
    material: &SceneNodeRef,
    look: MaterialLook,
) -> Result<Vec<MaterialParamWrite>, String> {
    let meta = def
        .preset_metadata
        .as_ref()
        .ok_or("Material has no parameter bindings")?;
    let mut factors = vec![
        ("coat_mode", 1.0),
        ("iridescence_mode", 1.0),
        ("emission_mode", 1.0),
        ("glass_mode", 1.0),
        ("sheen_mode", 1.0),
        ("anisotropy_mode", 1.0),
        ("translucency_mode", 1.0),
    ];
    for &(name, value) in recipe(look) {
        if let Some(pair) = factors.iter_mut().find(|(n, _)| *n == name) {
            pair.1 = value;
        } else {
            factors.push((name, value));
        }
    }
    factors.into_iter().map(|(param,value)| {
        let mut found = meta.bindings.iter().filter(|binding|
            matches!(&binding.target,BindingTarget::Node {node_id,param:p} if *node_id == material.node && p == param));
        let binding = found.next().ok_or_else(|| format!("Material parameter {param} is not exposed"))?;
        if found.next().is_some() { return Err(format!("Material parameter {param} has ambiguous bindings")); }
        if !inst.params.contains(&binding.id) { return Err(format!("Material parameter {param} is unavailable")); }
        Ok(MaterialParamWrite { param_id: binding.id.clone().into(), value })
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::effect_graph_def::{EffectGraphDef, EffectGraphNode, EffectGraphWire};
    use manifold_core::effects::ParameterDriver;
    use manifold_core::id::NodeId;
    use manifold_core::project::Project;
    use manifold_core::scene_modifier_preset::SceneNodeRef;
    use manifold_core::types::{BeatDivision, DriverWaveform, LayerType};
    use manifold_editing::commands::material::{
        ChangeMaterialParamsCommand, MaterialEditContext, MaterialParamChange,
    };
    use manifold_editing::service::EditingService;
    use manifold_ui::panels::actions::MaterialLook;

    struct MaterialFixture {
        project: Project,
        target: manifold_core::GraphTarget,
        object: SceneNodeRef,
        material: SceneNodeRef,
        catalog_default: EffectGraphDef,
    }

    fn physics_solids_fixture() -> MaterialFixture {
        let mut project = Project::default();
        let index = project.timeline.add_layer(
            "Scene",
            LayerType::Generator,
            manifold_core::PresetTypeId::from_string("PhysicsSolids".to_string()),
        );
        let layer_id = project.timeline.layers[index].layer_id.clone();
        let target = manifold_core::GraphTarget::Generator(layer_id);
        let mut def = manifold_renderer::node_graph::bundled_preset_def(
            project.preset_instance(&target).unwrap().effect_type(),
        )
        .expect("PhysicsSolids is bundled")
        .clone();
        manifold_renderer::node_graph::scene_exposure::migrate_scene_exposures(&mut def);
        let (object, material) =
            material_object_refs(&def).expect("PhysicsSolids has a PBR object");
        project.with_preset_graph_mut(&target, |instance| {
            instance.graph = Some(def.clone());
            instance.refresh_manifest_from_graph();
        });
        MaterialFixture {
            project,
            target,
            object,
            material,
            catalog_default: def,
        }
    }

    fn material_object_refs(def: &EffectGraphDef) -> Option<(SceneNodeRef, SceneNodeRef)> {
        fn walk(
            nodes: &[EffectGraphNode],
            wires: &[EffectGraphWire],
            scope: &[NodeId],
        ) -> Option<(SceneNodeRef, SceneNodeRef)> {
            for material in nodes
                .iter()
                .filter(|node| node.type_id == "node.pbr_material")
            {
                for object in nodes
                    .iter()
                    .filter(|node| node.type_id == "node.scene_object")
                {
                    if wires.iter().any(|wire| {
                        wire.from_node == material.id
                            && wire.to_node == object.id
                            && wire.to_port == "material"
                    }) {
                        return Some((
                            SceneNodeRef {
                                scope: scope.to_vec(),
                                node: object.node_id.clone(),
                            },
                            SceneNodeRef {
                                scope: scope.to_vec(),
                                node: material.node_id.clone(),
                            },
                        ));
                    }
                }
            }
            for group_node in nodes.iter().filter(|node| node.group.is_some()) {
                let group = group_node.group.as_deref()?;
                let mut child_scope = scope.to_vec();
                child_scope.push(group_node.node_id.clone());
                if let Some(found) = walk(&group.nodes, &group.wires, &child_scope) {
                    return Some(found);
                }
            }
            None
        }
        walk(&def.nodes, &def.wires, &[])
    }

    fn with_scope_mut(
        def: &mut EffectGraphDef,
        scope: &[NodeId],
        edit: &mut dyn FnMut(&mut Vec<EffectGraphNode>, &mut Vec<EffectGraphWire>),
    ) -> bool {
        let Some((head, tail)) = scope.split_first() else {
            edit(&mut def.nodes, &mut def.wires);
            return true;
        };
        let Some(group) = def
            .nodes
            .iter_mut()
            .find(|node| node.node_id == *head)
            .and_then(|node| node.group.as_deref_mut())
        else {
            return false;
        };
        let mut nested = EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            scene_modifiers: Vec::new(),
            nodes: std::mem::take(&mut group.nodes),
            wires: std::mem::take(&mut group.wires),
        };
        let found = with_scope_mut(&mut nested, tail, edit);
        group.nodes = nested.nodes;
        group.wires = nested.wires;
        found
    }

    fn add_selected_map_source(fixture: &mut MaterialFixture, type_id: &str, port: &str) {
        let object = fixture.object.clone();
        let material = fixture.material.clone();
        fixture
            .project
            .with_preset_graph_mut(&fixture.target, |instance| {
                let graph = instance.graph.as_mut().unwrap();
                let mut edit = |nodes: &mut Vec<EffectGraphNode>,
                                wires: &mut Vec<EffectGraphWire>| {
                    let object_doc = nodes
                        .iter()
                        .find(|node| node.node_id == object.node)
                        .unwrap()
                        .id;
                    let mut source = nodes
                        .iter()
                        .find(|node| node.node_id == material.node)
                        .unwrap()
                        .clone();
                    source.id = nodes.iter().map(|node| node.id).max().unwrap_or(0) + 100;
                    source.node_id = NodeId::new("material_look_source");
                    source.type_id = type_id.to_owned();
                    source.group = None;
                    nodes.push(source.clone());
                    wires.push(EffectGraphWire {
                        from_node: source.id,
                        from_port: "out".to_owned(),
                        to_node: object_doc,
                        to_port: port.to_owned(),
                    });
                };
                assert!(with_scope_mut(graph, &object.scope, &mut edit));
            });
    }

    fn build_command(
        fixture: &MaterialFixture,
        look: MaterialLook,
    ) -> (ChangeMaterialParamsCommand, Vec<MaterialParamChange>) {
        let instance = fixture.project.preset_instance(&fixture.target).unwrap();
        let def = instance.graph.as_ref().unwrap();
        let writes = writes(instance, def, &fixture.material, look).unwrap();
        let changes = writes
            .iter()
            .map(|write| MaterialParamChange {
                param_id: write.param_id.clone(),
                expected: instance.get_base_param(&write.param_id),
                value: write.value,
            })
            .collect::<Vec<_>>();
        let context = MaterialEditContext {
            object: fixture.object.clone(),
            material: fixture.material.clone(),
            kind: manifold_editing::commands::material::MaterialEditKind::Look,
            expected_preset_id: instance.effect_type().clone(),
        };
        (
            ChangeMaterialParamsCommand::new(
                fixture.target.clone(),
                context,
                changes.clone(),
                format!("Apply {look:?} material look"),
                Some(fixture.catalog_default.clone()),
            ),
            changes,
        )
    }

    fn assert_look_result(look: MaterialLook) {
        let mut fixture = physics_solids_fixture();
        let before_graph = serde_json::to_string(
            &fixture
                .project
                .preset_instance(&fixture.target)
                .unwrap()
                .graph,
        )
        .unwrap();
        let (command, changes) = build_command(&fixture, look);
        let baseline = changes
            .iter()
            .map(|change| (change.param_id.clone(), change.expected))
            .collect::<Vec<_>>();
        let changed_ids = changes
            .iter()
            .map(|change| change.param_id.to_string())
            .collect::<std::collections::BTreeSet<_>>();
        let untouched = fixture
            .project
            .preset_instance(&fixture.target)
            .unwrap()
            .params
            .iter()
            .filter(|param| !changed_ids.contains(param.id()))
            .map(|param| (param.id().to_owned(), param.base))
            .collect::<Vec<_>>();
        let mut service = EditingService::new();
        service.execute(Box::new(command), &mut fixture.project);
        assert!(service.take_rejection().is_none());
        let after = fixture.project.preset_instance(&fixture.target).unwrap();
        for change in &changes {
            assert_eq!(after.get_base_param(&change.param_id), change.value);
        }
        assert_eq!(
            serde_json::to_string(&after.graph).unwrap(),
            before_graph,
            "a look only changes bound-slot bases"
        );
        for (id, value) in &untouched {
            assert_eq!(after.get_base_param(id), *value);
        }
        assert!(service.undo(&mut fixture.project));
        let undone = fixture.project.preset_instance(&fixture.target).unwrap();
        for (id, value) in &baseline {
            assert_eq!(undone.get_base_param(id), *value);
        }
        assert!(service.redo(&mut fixture.project));
        let redone = fixture.project.preset_instance(&fixture.target).unwrap();
        for change in &changes {
            assert_eq!(redone.get_base_param(&change.param_id), change.value);
        }

        let saved = serde_json::to_string(&fixture.project).unwrap();
        let mut loaded: Project = serde_json::from_str(&saved).unwrap();
        loaded.reconcile_param_manifests();
        let loaded_instance = loaded.preset_instance(&fixture.target).unwrap();
        for change in &changes {
            assert_eq!(
                loaded_instance.get_base_param(&change.param_id),
                change.value
            );
        }
        assert_eq!(
            serde_json::to_string(&loaded_instance.graph).unwrap(),
            before_graph
        );
    }

    #[test]
    fn material_inspector_placement_six_slots_undo_exactly() {
        let mut fixture = physics_solids_fixture();
        let inst = fixture.project.preset_instance(&fixture.target).unwrap();
        let metadata = inst
            .graph
            .as_ref()
            .unwrap()
            .preset_metadata
            .as_ref()
            .unwrap();
        let names = ["uv_m00", "uv_m01", "uv_m10", "uv_m11", "uv_tx", "uv_ty"];
        let values = [0.0, -1.0, 1.0, 0.0, 0.2, 0.3];
        let changes: Vec<_> = names.into_iter().zip(values).map(|(name,value)| {
            let binding = metadata.bindings.iter().find(|b| matches!(&b.target,
                BindingTarget::Node {node_id,param} if node_id == &fixture.material.node && param == name)).unwrap();
            MaterialParamChange {param_id:binding.id.clone().into(),expected:inst.get_base_param(&binding.id),value}
        }).collect();
        let context = MaterialEditContext {
            object: fixture.object.clone(),
            material: fixture.material.clone(),
            expected_preset_id: inst.effect_type().clone(),
            kind: manifold_editing::commands::material::MaterialEditKind::Placement,
        };
        let mut service = EditingService::new();
        service.execute(
            Box::new(ChangeMaterialParamsCommand::new(
                fixture.target.clone(),
                context,
                changes.clone(),
                "Rotate texture".into(),
                Some(fixture.catalog_default),
            )),
            &mut fixture.project,
        );
        assert!(service.take_rejection().is_none());
        assert!(changes.iter().all(|change| {
            fixture
                .project
                .preset_instance(&fixture.target)
                .unwrap()
                .get_base_param(&change.param_id)
                == change.value
        }));
        assert!(service.undo(&mut fixture.project));
        assert!(changes.iter().all(|change| {
            fixture
                .project
                .preset_instance(&fixture.target)
                .unwrap()
                .get_base_param(&change.param_id)
                .to_bits()
                == change.expected.to_bits()
        }));
        assert!(
            !service.undo(&mut fixture.project),
            "one transform gesture creates one undo unit"
        );
    }

    #[test]
    fn material_inspector_looks_round_trip_through_one_undo_unit() {
        for look in [
            MaterialLook::Matte,
            MaterialLook::Coated,
            MaterialLook::BrushedMetal,
            MaterialLook::Glass,
        ] {
            assert_look_result(look);
        }
    }

    #[test]
    fn material_inspector_look_conflicts_reject_without_writes() {
        let mut fixture = physics_solids_fixture();
        let target = fixture.target.clone();
        let touched = {
            let instance = fixture.project.preset_instance(&target).unwrap();
            let def = instance.graph.as_ref().unwrap();
            writes(instance, def, &fixture.material, MaterialLook::Matte).unwrap()
        };
        let baseline = fixture
            .project
            .preset_instance(&target)
            .unwrap()
            .get_base_param(&touched[0].param_id);
        fixture
            .project
            .preset_instance_mut(&target)
            .unwrap()
            .drivers = Some(vec![ParameterDriver::new(
            touched[0].param_id.clone(),
            BeatDivision::Quarter,
            DriverWaveform::Sine,
        )]);
        let (command, changes) = build_command(&fixture, MaterialLook::Matte);
        let mut service = EditingService::new();
        service.execute(Box::new(command), &mut fixture.project);
        assert!(service.take_rejection().is_some());
        assert_eq!(
            fixture
                .project
                .preset_instance(&target)
                .unwrap()
                .get_base_param(&changes[0].param_id),
            baseline
        );
    }

    #[test]
    fn material_inspector_selected_mr_map_rejects_without_writes() {
        let mut fixture = physics_solids_fixture();
        add_selected_map_source(&mut fixture, "node.gltf_texture_source", "mr_map");
        let before = fixture
            .project
            .preset_instance(&fixture.target)
            .unwrap()
            .get_base_param("metallic");
        let (command, _) = build_command(&fixture, MaterialLook::Matte);
        let mut service = EditingService::new();
        service.execute(Box::new(command), &mut fixture.project);
        assert!(service.take_rejection().is_some());
        assert_eq!(
            fixture
                .project
                .preset_instance(&fixture.target)
                .unwrap()
                .get_base_param("metallic"),
            before
        );
    }

    #[test]
    fn material_inspector_emissive_skin_rejects_without_writes() {
        let mut fixture = physics_solids_fixture();
        add_selected_map_source(&mut fixture, "node.layer_source", "emissive_map");
        let before = fixture
            .project
            .preset_instance(&fixture.target)
            .unwrap()
            .get_base_param("roughness");
        let (command, _) = build_command(&fixture, MaterialLook::Matte);
        let mut service = EditingService::new();
        service.execute(Box::new(command), &mut fixture.project);
        assert!(service.take_rejection().is_some());
        assert_eq!(
            fixture
                .project
                .preset_instance(&fixture.target)
                .unwrap()
                .get_base_param("roughness"),
            before
        );
    }
}
