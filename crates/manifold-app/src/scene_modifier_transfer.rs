//! Clipboard and generator replacement share one atomic modifier transfer.
use std::borrow::Cow;

use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef, EffectGraphNode};
use manifold_core::effects::PresetInstance;
use manifold_core::project::Project;
use manifold_core::scene_modifier_preset::{SceneNodeRef, SceneTargetSelection};
use manifold_core::{GraphTarget, LayerId, NodeId};
use manifold_editing::command::Command;

#[derive(Debug, Clone)]
pub(crate) struct ModifierClipboard {
    host: Box<PresetInstance>,
    graph: Box<EffectGraphDef>,
    selected: Vec<NodeId>,
}

impl ModifierClipboard {
    pub(crate) fn capture(
        project: &Project,
        layer: &LayerId,
        selected: &[NodeId],
    ) -> Result<Self, String> {
        let target = GraphTarget::Generator(layer.clone());
        let host = project
            .graph_target_owner(&target)
            .ok_or("Generator is no longer available")?;
        let graph = crate::graph_target::resolve(project, &target)
            .ok_or("Generator graph is unavailable")?;
        if selected.is_empty()
            || selected
                .iter()
                .any(|id| !graph.scene_modifiers.iter().any(|item| &item.id == id))
        {
            return Err("Select scene modifiers to copy".into());
        }
        Ok(Self {
            host: Box::new(host.clone()),
            graph: Box::new(graph.clone()),
            selected: selected.to_vec(),
        })
    }

    pub(crate) fn count(&self) -> usize {
        self.selected.len()
    }
}

pub(crate) fn build_paste(
    project: &Project,
    layer: LayerId,
    clipboard: ModifierClipboard,
) -> Result<Box<dyn Command>, String> {
    let target = GraphTarget::Generator(layer.clone());
    let before = project
        .graph_target_owner(&target)
        .ok_or("Scene modifiers require a generator layer")?;
    let graph =
        crate::graph_target::resolve(project, &target).ok_or("Generator graph is unavailable")?;
    let mut after = before.clone();
    transfer(
        &clipboard.host,
        &clipboard.graph,
        &mut after,
        graph,
        &clipboard.selected,
        false,
    )?;
    // Singleton recipes allow one instance per scene (the same rule the picker
    // and the add/duplicate actions enforce); reject a paste that grows any
    // (scene, singleton recipe) count, while leaving already-migrated
    // multi-instance scenes pasteable for everything else.
    let before_counts = singleton_counts(before);
    let after_counts = singleton_counts(&after);
    for (key, after_n) in &after_counts {
        // One instance per scene is allowed; the paste is rejected only when
        // it pushes a scene past that.
        if *after_n > before_counts.get(key).copied().unwrap_or(0).max(1) {
            return Err(format!(
                "{} is already applied to this scene",
                key.1.as_str()
            ));
        }
    }
    Ok(Box::new(
        crate::generator_change::ReplaceGeneratorStateCommand::new(
            layer,
            before.clone(),
            after,
            "Paste Scene Modifiers",
        ),
    ))
}

/// Per-(scene, recipe) instance counts for singleton recipes.
fn singleton_counts(
    host: &manifold_core::effects::PresetInstance,
) -> std::collections::BTreeMap<
    (
        manifold_core::scene_modifier_preset::SceneNodeRef,
        String,
    ),
    usize,
> {
    let mut counts: std::collections::BTreeMap<_, usize> = std::collections::BTreeMap::new();
    let Some(graph) = host.graph.as_ref() else {
        return counts;
    };
    for instance in &graph.scene_modifiers {
        let Some(metadata) = instance.graph.preset_metadata.as_ref() else {
            continue;
        };
        if metadata
            .scene_modifier
            .as_ref()
            .is_some_and(|recipe| recipe.singleton)
        {
            *counts
                .entry((instance.scene.clone(), metadata.id.as_str().to_string()))
                .or_default() += 1;
        }
    }
    counts
}

fn scenes(nodes: &[EffectGraphNode], scope: &mut Vec<NodeId>, out: &mut Vec<SceneNodeRef>) {
    for node in nodes {
        if node.type_id == "node.render_scene" {
            out.push(SceneNodeRef {
                scope: scope.clone(),
                node: node.node_id.clone(),
            });
        }
        if let Some(group) = &node.group {
            scope.push(node.node_id.clone());
            scenes(&group.nodes, scope, out);
            scope.pop();
        }
    }
}

/// Transfer authored snapshots, not catalog defaults. Destination-specific mesh
/// frames are resolved afresh; the normal command admission checks the complete
/// stack before any project state is changed.
pub(crate) fn transfer(
    source_host: &PresetInstance,
    source_graph: &EffectGraphDef,
    destination_host: &mut PresetInstance,
    destination_graph: &EffectGraphDef,
    selected: &[NodeId],
    preserve_ids: bool,
) -> Result<(), String> {
    if selected.is_empty() {
        return Ok(());
    }
    let mut available = Vec::new();
    scenes(&destination_graph.nodes, &mut Vec::new(), &mut available);
    if available.is_empty() {
        return Err("This generator has no compatible scene. Remove its scene modifiers before changing generators, or paste onto a scene generator.".into());
    }

    // Reuse duplication's identity and host-macro cloning, including customized
    // binding conversions and shared-macro rejection.
    let duplicated =
        manifold_core::scene_modifier_edit::duplicate_scene_modifiers(source_graph, selected)
            .map_err(|error| error.to_string())?;
    let (snapshot, ids, remaps) = if preserve_ids {
        let metadata = source_graph
            .preset_metadata
            .as_ref()
            .ok_or("Source metadata is unavailable")?;
        let mut remaps: Vec<_> = metadata.bindings.iter().map(|b| (&b.id, &b.target))
            .chain(metadata.string_bindings.iter().map(|b| (&b.id, &b.target)))
            .filter(|(_, target)| matches!(target, BindingTarget::SceneModifier { modifier_id, .. } if selected.contains(modifier_id)))
            .map(|(id, _)| (id.clone(), id.clone())).collect();
        remaps.sort();
        remaps.dedup();
        (source_graph, selected.to_vec(), remaps)
    } else {
        let ids = duplicated
            .graph
            .scene_modifiers
            .iter()
            .filter(|item| {
                !source_graph
                    .scene_modifiers
                    .iter()
                    .any(|old| old.id == item.id)
            })
            .map(|item| item.id.clone())
            .collect();
        (
            &duplicated.graph,
            ids,
            duplicated.parameter_id_remaps.clone(),
        )
    };
    let mut graph = destination_graph.clone();
    for source in snapshot
        .scene_modifiers
        .iter()
        .filter(|item| ids.contains(&item.id))
    {
        if graph
            .scene_modifiers
            .iter()
            .any(|item| item.id == source.id)
        {
            return Err("Destination already contains this scene modifier identity".into());
        }
        let mut instance = source.clone();
        instance.scene = if available.contains(&source.scene) {
            source.scene.clone()
        } else if available.len() == 1 {
            available[0].clone()
        } else {
            return Err(
                "Destination has multiple scenes; the modifier scene cannot be matched".into(),
            );
        };
        if let SceneTargetSelection::Explicit { objects } = &instance.targets {
            let reachable =
                manifold_renderer::node_graph::scene_modifier_authoring::scene_modifier_objects(
                    &graph,
                    &instance.scene,
                )
                .map_err(|error| error.to_string())?;
            if objects.iter().any(|object| !reachable.contains(object)) {
                return Err("A modifier targets objects that do not exist in the destination scene. Set its targets to All Objects or remove it before transferring.".into());
            }
        }
        instance.mesh_frames.clear();
        instance.mesh_frames =
            manifold_renderer::node_graph::scene_modifier_expand::resolve_modifier_mesh_frames(
                &graph, &instance,
            )
            .map_err(|error| {
                format!("Scene modifier is incompatible with the destination: {error}")
            })?;
        graph.scene_modifiers.push(instance);
    }
    let source_metadata = snapshot
        .preset_metadata
        .as_ref()
        .ok_or("Source metadata is unavailable")?;
    let metadata = graph
        .preset_metadata
        .as_mut()
        .ok_or("Destination metadata is unavailable")?;
    let owns = |target: &BindingTarget| matches!(target, BindingTarget::SceneModifier { modifier_id, .. } if ids.contains(modifier_id));
    let macro_ids: Vec<_> = source_metadata
        .bindings
        .iter()
        .filter(|b| owns(&b.target))
        .map(|b| &b.id)
        .chain(
            source_metadata
                .string_bindings
                .iter()
                .filter(|b| owns(&b.target))
                .map(|b| &b.id),
        )
        .collect();
    if macro_ids.iter().any(|id| {
        metadata.params.iter().any(|p| &p.id == *id)
            || metadata.string_params.iter().any(|p| &p.id == *id)
            || metadata.bindings.iter().any(|b| &b.id == *id)
            || metadata.string_bindings.iter().any(|b| &b.id == *id)
    }) {
        return Err("Scene modifier controls collide with destination controls".into());
    }
    metadata.params.extend(
        source_metadata
            .params
            .iter()
            .filter(|p| macro_ids.contains(&&p.id))
            .cloned(),
    );
    metadata.string_params.extend(
        source_metadata
            .string_params
            .iter()
            .filter(|p| macro_ids.contains(&&p.id))
            .cloned(),
    );
    metadata.bindings.extend(
        source_metadata
            .bindings
            .iter()
            .filter(|b| owns(&b.target))
            .cloned(),
    );
    metadata.string_bindings.extend(
        source_metadata
            .string_bindings
            .iter()
            .filter(|b| owns(&b.target))
            .cloned(),
    );
    graph.version = graph.version.max(3);
    manifold_core::scene_modifier_preset::validate_scene_modifier_schema(&graph)
        .map_err(|e| e.to_string())?;

    let mut host = destination_host.clone();
    host.graph = Some(graph);
    host.refresh_manifest_from_graph();
    // If base tracking becomes active, initialize destination bases first so
    // unrelated controls do not suddenly use stale untracked base values.
    if source_host.base_tracked && !host.base_tracked {
        for param in host.params.iter_mut() {
            param.base = param.value;
        }
        host.base_tracked = true;
    }
    for (source, destination) in &remaps {
        if let Some(param) = source_host.params.get(source)
            && let Some(target) = host.params.get_mut(destination)
        {
            let spec = target.spec.clone();
            *target = param.clone();
            target.spec = spec;
            if !source_host.base_tracked {
                target.base = target.value;
            }
        }
    }
    macro_rules! copy_routes {
        ($field:ident) => {
            if let Some(entries) = &source_host.$field {
                let copies: Vec<_> = entries
                    .iter()
                    .flat_map(|entry| {
                        remaps
                            .iter()
                            .filter(move |(source, _)| entry.param_id.as_ref() == source)
                            .map(move |(_, destination)| {
                                let mut copy = entry.clone();
                                copy.param_id = Cow::Owned(destination.clone());
                                copy
                            })
                    })
                    .collect();
                if !copies.is_empty() {
                    let destination_entries = host.$field.get_or_insert_with(Vec::new);
                    destination_entries.retain(|entry| {
                        !remaps
                            .iter()
                            .any(|(_, destination)| entry.param_id.as_ref() == destination)
                    });
                    destination_entries.extend(copies);
                }
            }
        };
    }
    copy_routes!(drivers);
    copy_routes!(envelopes);
    copy_routes!(audio_mods);
    copy_routes!(automation_lanes);
    // Hardware mappings stay attached when changing the same generator owner;
    // a clipboard paste must not duplicate external control bindings.
    if preserve_ids {
        copy_routes!(ableton_mappings);
    }
    host.bump_graph_structure_version();
    *destination_host = host;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::PresetTypeId;
    use manifold_core::layer::Layer;
    use manifold_editing::service::EditingService;

    fn fixture() -> (Project, LayerId, NodeId, String) {
        let mut project = Project::default();
        let mut layer = Layer::new_generator("Source".into(), PresetTypeId::new("WaveGrid"), 0);
        let id = layer.layer_id.clone();
        let graph =
            manifold_renderer::node_graph::bundled_preset_def(&PresetTypeId::new("WaveGrid"))
                .unwrap()
                .clone();
        layer.gen_params_or_init().graph = Some(graph);
        layer.gen_params_or_init().refresh_manifest_from_graph();
        project.timeline.layers.push(layer);
        let mut command = crate::scene_modifier_edit::build_action(
            &project,
            crate::scene_modifier_edit::SceneModifierAction::Add(id.clone(), "SceneFog".into()),
        )
        .unwrap();
        command.execute(&mut project);
        assert!(command.was_applied());
        let host = project
            .graph_target_owner_mut(&GraphTarget::Generator(id.clone()))
            .unwrap();
        let graph = host.graph.as_ref().unwrap();
        let modifier = graph.scene_modifiers[0].id.clone();
        let amount = graph.preset_metadata.as_ref().unwrap().bindings.iter().find(|b|
            matches!(&b.target, BindingTarget::SceneModifier { param_id, .. } if param_id == "amount")).unwrap().id.clone();
        host.params.get_mut(&amount).unwrap().value = 0.42;
        host.params.get_mut(&amount).unwrap().base = 0.37;
        host.base_tracked = true;
        host.drivers = Some(vec![manifold_core::effects::ParameterDriver::new(
            amount.clone(),
            Default::default(),
            Default::default(),
        )]);
        (project, id, modifier, amount)
    }

    #[test]
    fn clipboard_paste_preserves_snapshot_remaps_controls_and_undo_redo() {
        let (mut project, source, modifier, source_amount) = fixture();
        let clipboard =
            ModifierClipboard::capture(&project, &source, std::slice::from_ref(&modifier)).unwrap();
        project
            .graph_target_owner_mut(&GraphTarget::Generator(source))
            .unwrap()
            .params
            .get_mut(&source_amount)
            .unwrap()
            .value = 0.9;
        let destination =
            Layer::new_generator("Destination".into(), PresetTypeId::new("WaveRing"), 1);
        let id = destination.layer_id.clone();
        project.timeline.layers.push(destination);
        let target = GraphTarget::Generator(id.clone());
        let before = serde_json::to_value(project.graph_target_owner(&target).unwrap()).unwrap();
        let command = build_paste(&project, id, clipboard).unwrap();
        let mut service = EditingService::new();
        service.execute(
            crate::scene_modifier_edit::with_admission(command),
            &mut project,
        );
        assert!(service.take_rejection().is_none());
        let host = project.graph_target_owner(&target).unwrap();
        let graph = host.graph.as_ref().unwrap();
        assert_ne!(graph.scene_modifiers[0].id, modifier);
        let amount = graph.preset_metadata.as_ref().unwrap().bindings.iter().find(|b|
            matches!(&b.target, BindingTarget::SceneModifier { param_id, .. } if param_id == "amount")).unwrap().id.clone();
        assert_ne!(amount, source_amount);
        assert_eq!(host.params.get(&amount).unwrap().value, 0.42);
        assert_eq!(host.params.get(&amount).unwrap().base, 0.37);
        assert_eq!(host.drivers.as_ref().unwrap()[0].param_id.as_ref(), amount);
        let after = serde_json::to_value(host).unwrap();
        assert!(service.undo(&mut project));
        assert_eq!(
            serde_json::to_value(project.graph_target_owner(&target).unwrap()).unwrap(),
            before
        );
        assert!(service.redo(&mut project));
        assert!(service.take_rejection().is_none());
        assert_eq!(
            serde_json::to_value(project.graph_target_owner(&target).unwrap()).unwrap(),
            after
        );
    }

    #[test]
    fn clipboard_paste_remaps_math_view_carrier_and_roundtrips_undo_redo() {
        let (imported, report) = manifold_renderer::node_graph::gltf_import::assemble_import_graph(
            std::path::Path::new(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/fixtures/gltf/cc0__japanese_thistle_cirsium_japonicum.glb"
            )),
        )
        .unwrap();
        assert!(report.object_count > 0);
        let mut available_scenes = Vec::new();
        scenes(&imported.nodes, &mut Vec::new(), &mut available_scenes);
        let scene = available_scenes.into_iter().next().expect("imported scene");

        let mut source_graph = imported.clone();
        let carrier_recipe = manifold_renderer::node_graph::bundled_preset_def(
            &PresetTypeId::new("VortexFragments"),
        )
        .unwrap();
        let view_recipe = manifold_renderer::node_graph::bundled_preset_def(
            &PresetTypeId::new("MathView"),
        )
        .unwrap();
        let carrier_a = NodeId::new("carrier_a");
        let carrier_b = NodeId::new("carrier_b");
        let view_id = NodeId::new("math_view");
        let carrier = |id: NodeId, graph: &EffectGraphDef| {
            manifold_renderer::node_graph::scene_modifier_authoring::prepare_new_scene_modifier(
                graph,
                carrier_recipe,
                id,
                scene.clone(),
                SceneTargetSelection::AllObjects,
            )
            .unwrap()
        };
        let carrier_a_instance = carrier(carrier_a.clone(), &source_graph);
        source_graph = manifold_core::scene_modifier_edit::insert_scene_modifier(
            &source_graph,
            0,
            carrier_a_instance,
        )
        .unwrap()
        .graph;
        let carrier_b_instance = carrier(carrier_b.clone(), &source_graph);
        source_graph = manifold_core::scene_modifier_edit::insert_scene_modifier(
            &source_graph,
            1,
            carrier_b_instance,
        )
        .unwrap()
        .graph;
        let mut view =
            manifold_renderer::node_graph::scene_modifier_authoring::prepare_new_scene_modifier(
                &source_graph,
                view_recipe,
                view_id.clone(),
                scene.clone(),
                SceneTargetSelection::AllObjects,
            )
            .unwrap();
        view.legacy_math_view_carrier = Some(carrier_b.clone());
        source_graph = manifold_core::scene_modifier_edit::insert_scene_modifier(
            &source_graph,
            2,
            view,
        )
        .unwrap()
        .graph;

        let mut project = Project::default();
        let mut source_layer = Layer::new_generator(
            "Source".into(),
            PresetTypeId::new("PhotoscanBaseline"),
            0,
        );
        let source = source_layer.layer_id.clone();
        source_layer.gen_params_or_init().graph = Some(source_graph);
        source_layer.gen_params_or_init().refresh_manifest_from_graph();
        let mut destination_layer = Layer::new_generator(
            "Destination".into(),
            PresetTypeId::new("PhotoscanBaseline"),
            1,
        );
        let destination = destination_layer.layer_id.clone();
        destination_layer.gen_params_or_init().graph = Some(imported);
        destination_layer
            .gen_params_or_init()
            .refresh_manifest_from_graph();
        project.timeline.layers.push(source_layer);
        project.timeline.layers.push(destination_layer);

        let selected = [carrier_a, carrier_b.clone(), view_id];
        let clipboard = ModifierClipboard::capture(&project, &source, &selected).unwrap();
        let target = GraphTarget::Generator(destination.clone());
        let before = serde_json::to_value(project.graph_target_owner(&target).unwrap()).unwrap();
        let command = build_paste(&project, destination.clone(), clipboard).unwrap();
        let mut service = EditingService::new();
        service.execute(
            crate::scene_modifier_edit::with_admission(command),
            &mut project,
        );
        assert!(service.take_rejection().is_none());

        let host = project.graph_target_owner(&target).unwrap();
        let graph = host.graph.as_ref().unwrap();
        let pasted_view = graph
            .scene_modifiers
            .iter()
            .find(|instance| {
                instance
                    .graph
                    .preset_metadata
                    .as_ref()
                    .is_some_and(|metadata| metadata.id.as_str() == "MathView")
            })
            .expect("pasted Math View");
        let pasted_carrier = graph
            .scene_modifiers
            .iter()
            .find(|instance| Some(&instance.id) == pasted_view.legacy_math_view_carrier.as_ref())
            .expect("pasted Math View carrier");
        assert_ne!(pasted_carrier.id, carrier_b);
        let reloaded: EffectGraphDef = serde_json::from_value(serde_json::to_value(graph).unwrap()).unwrap();
        assert_eq!(&reloaded, graph, "pasted association survives serialization");
        assert_eq!(
            manifold_core::scene_modifier_math_view::math_view_connect_support(
                graph,
                &pasted_view.id,
            ),
            Ok(())
        );
        let after = serde_json::to_value(host).unwrap();
        assert!(service.undo(&mut project));
        assert_eq!(
            serde_json::to_value(project.graph_target_owner(&target).unwrap()).unwrap(),
            before
        );
        assert!(service.redo(&mut project));
        assert!(service.take_rejection().is_none());
        assert_eq!(
            serde_json::to_value(project.graph_target_owner(&target).unwrap()).unwrap(),
            after
        );
    }

    #[test]
    fn incompatible_scene_and_missing_explicit_objects_leave_destination_unchanged() {
        let (project, source, modifier, _) = fixture();
        let source_target = GraphTarget::Generator(source);
        let host = project.graph_target_owner(&source_target).unwrap();
        let mut source_graph = host.graph.clone().unwrap();
        let mut destination = PresetInstance::new_generator(PresetTypeId::new("Plasma"));
        let graph = manifold_renderer::node_graph::bundled_preset_def(&PresetTypeId::new("Plasma"))
            .unwrap();
        let before = serde_json::to_value(&destination).unwrap();
        assert!(
            transfer(
                host,
                &source_graph,
                &mut destination,
                graph,
                std::slice::from_ref(&modifier),
                false
            )
            .unwrap_err()
            .contains("no compatible scene")
        );
        assert_eq!(serde_json::to_value(&destination).unwrap(), before);
        source_graph.scene_modifiers[0].targets = SceneTargetSelection::Explicit {
            objects: vec![SceneNodeRef {
                scope: Vec::new(),
                node: NodeId::new("missing-object"),
            }],
        };
        let graph =
            manifold_renderer::node_graph::bundled_preset_def(&PresetTypeId::new("WaveRing"))
                .unwrap();
        assert!(
            transfer(
                host,
                &source_graph,
                &mut destination,
                graph,
                &[modifier],
                false
            )
            .unwrap_err()
            .contains("objects that do not exist")
        );
        assert_eq!(serde_json::to_value(&destination).unwrap(), before);
    }

    #[test]
    fn generator_change_preserves_modifier_identity_and_rejects_incompatible_type() {
        let (mut project, id, modifier, amount) = fixture();
        let target = GraphTarget::Generator(id.clone());
        let mut service = EditingService::new();
        let command = crate::generator_change::build_change(
            &project,
            id.clone(),
            PresetTypeId::new("WaveRing"),
        )
        .unwrap();
        service.execute(
            crate::scene_modifier_edit::with_admission(command),
            &mut project,
        );
        assert!(service.take_rejection().is_none());
        let host = project.graph_target_owner(&target).unwrap();
        assert_eq!(host.graph.as_ref().unwrap().scene_modifiers[0].id, modifier);
        assert_eq!(host.params.get(&amount).unwrap().base, 0.37);
        assert_eq!(host.drivers.as_ref().unwrap().len(), 1);
        assert_eq!(host.drivers.as_ref().unwrap()[0].param_id.as_ref(), amount);
        assert!(
            crate::generator_change::build_change(&project, id, PresetTypeId::new("Plasma"))
                .is_err()
        );
        assert_eq!(
            project
                .graph_target_owner(&target)
                .unwrap()
                .generator_type()
                .as_str(),
            "WaveRing"
        );
        assert!(service.undo(&mut project));
        assert_eq!(
            project
                .graph_target_owner(&target)
                .unwrap()
                .generator_type()
                .as_str(),
            "WaveGrid"
        );
        assert!(service.redo(&mut project));
        assert_eq!(
            project
                .graph_target_owner(&target)
                .unwrap()
                .generator_type()
                .as_str(),
            "WaveRing"
        );
    }
}
