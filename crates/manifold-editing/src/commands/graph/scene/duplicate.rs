//! Undoable scene-object duplication and owned graph cloning.

use super::*;

/// The duplicate-object gesture (D11): one undoable composite edit that
/// deep-clones the source object's `scene_object` (+ its enclosing group,
/// when the object is grouped — the Add/importer shape) with fresh doc ids
/// and fresh [`NodeId`]s throughout, wires the clone's `object` output into
/// the next free `object_k` slot, bumps `objects`, offsets the clone's
/// `node.transform_3d.pos_x` by **+0.5** so it doesn't render exactly inside
/// the original (D11 — deliberate, visible, undoable, tune-by-feel later).
///
/// Ungrouped hand-built objects (a loose `scene_object` whose mesh/
/// transform/material producers are NOT wrapped in a group) share
/// their exclusive upstream chain. Shared producers remain in place and
/// incoming shared-source wires are copied to the cloned chain.
#[derive(Debug)]
pub struct DuplicateSceneObjectCommand {
    target: GraphTarget,
    scope_path: Vec<u32>,
    render_scene_node_id: u32,
    source_index: u32,
    expected_source: Option<NodeId>,
    catalog_default: EffectGraphDef,
    /// The target owner's graph before this edit. `None` is meaningful: the
    /// command must restore an unmaterialized catalog graph on undo.
    prev_graph: Option<Option<EffectGraphDef>>,
    /// Cached successful candidate for redo, preserving all cloned ids and
    /// fluid route boundaries exactly.
    after_graph: Option<EffectGraphDef>,
    /// Live manifest/modulation state before and after the duplicate. A
    /// structural refresh intentionally rebuilds the manifest, so retaining
    /// these snapshots keeps authored values stable across undo/redo too.
    prev_instance: Option<InstanceLayerSnapshot>,
    after_instance: Option<InstanceLayerSnapshot>,
    rejection: Option<String>,
    applied: bool,
}

impl DuplicateSceneObjectCommand {
    pub fn with_expected_source(mut self, source: NodeId) -> Self {
        self.expected_source = Some(source);
        self
    }

    pub fn new(
        target: GraphTarget,
        scope_path: Vec<u32>,
        render_scene_node_id: u32,
        source_index: u32,
        catalog_default: EffectGraphDef,
    ) -> Self {
        Self {
            target,
            scope_path,
            render_scene_node_id,
            source_index,
            expected_source: None,
            catalog_default,
            prev_graph: None,
            after_graph: None,
            prev_instance: None,
            after_instance: None,
            rejection: None,
            applied: false,
        }
    }
}

impl Command for DuplicateSceneObjectCommand {
    fn graph_admission_targets(&self, targets: &mut Vec<GraphTarget>) {
        targets.push(self.target.clone());
    }

    fn execute(&mut self, project: &mut Project) {
        self.rejection = None;
        self.applied = false;
        let scope = self.scope_path.clone();
        let render_id = self.render_scene_node_id;
        let src_k = self.source_index;

        if let (Some(previous_graph), Some(after_graph)) =
            (self.prev_graph.as_ref(), self.after_graph.as_ref())
        {
            let current_graph = project
                .graph_target_owner(&self.target)
                .map(|owner| owner.graph.clone());
            if current_graph.as_ref() != Some(previous_graph) {
                self.rejection =
                    Some("Duplicate Object redo rejected: graph changed since undo".into());
                return;
            }
            if with_target_graph_mut(project, &self.target, &self.catalog_default, true, |def| {
                *def = after_graph.clone();
            })
            .is_none()
            {
                self.rejection = Some("Duplicate Object redo target is unavailable".into());
                return;
            }
            refresh_target_manifest(project, &self.target);
            if let (Some(snapshot), Some(instance)) = (
                self.after_instance.clone(),
                project.graph_target_owner_mut(&self.target),
            ) {
                snapshot.restore(instance);
            }
            self.applied = true;
            return;
        }

        if !scene_object_source_matches(
            project.graph_for_target(&self.target, Some(&self.catalog_default)),
            &scope, render_id, src_k, self.expected_source.as_ref(),
        ) {
            self.rejection = Some("Duplicate Object rejected: selected object changed".into());
            return;
        }
        let physics_match = project
            .graph_for_target(&self.target, Some(&self.catalog_default))
            .and_then(|def| {
                let (nodes, wires) = graph_level(def, &scope)?;
                let source_id = object_producer_id(wires, render_id, src_k)?;
                Some(physics_scene_object_match(
                    nodes, wires, render_id, src_k, source_id,
                ))
            });
        if let Some(PhysicsSceneObjectMatch::Malformed(reason)) = &physics_match {
            self.rejection = Some((*reason).into());
            return;
        }
        if let Some(PhysicsSceneObjectMatch::Valid(physics)) = physics_match.as_ref() {
            if !scope.is_empty() {
                self.rejection =
                    Some("Duplicate Object physics ownership requires a root-level scene".into());
                return;
            }
            if physics.copies {
                self.rejection =
                    Some("Duplicate Object cannot duplicate a Physics World copies object".into());
                return;
            }
            if project
                .graph_for_target(&self.target, Some(&self.catalog_default))
                .and_then(|def| graph_level(def, &scope))
                .and_then(|(_, wires)| first_free_physics_body_slot(wires, physics.world_id))
                .is_none()
            {
                self.rejection =
                    Some("Duplicate Object physics world has no free body slot".into());
                return;
            }
        }

        let Some(baseline) = project
            .graph_for_target(&self.target, Some(&self.catalog_default))
            .cloned()
        else {
            self.rejection = Some("Duplicate Object requires an existing graph".into());
            return;
        };
        let Some(previous_graph) = project
            .graph_target_owner(&self.target)
            .map(|owner| owner.graph.clone())
        else {
            self.rejection = Some("Duplicate Object target is unavailable".into());
            return;
        };
        let baseline_instance = project
            .graph_target_owner_mut(&self.target)
            .map(|instance| InstanceLayerSnapshot::capture(&*instance));
        let append_range = (|| {
            let (nodes, wires) = graph_level(&baseline, &scope)
                .ok_or("Duplicate Object scene scope is unavailable")?;
            let next = scene_object_append_slot(nodes, wires, render_id)?;
            let source = object_producer_id(wires, render_id, src_k)
                .ok_or("Duplicate Object source object is unavailable")?;
            let count = match physics_match.as_ref() {
                Some(PhysicsSceneObjectMatch::Valid(physics)) => physics.render_indices.len(),
                _ => nodes
                    .iter()
                    .find(|node| node.id == source)
                    .filter(|node| node.type_id == "node.scene_object")
                    .map(|_| {
                        wires
                            .iter()
                            .filter(|wire| {
                                wire.from_node == source
                                    && wire.to_node == render_id
                                    && (wire.to_port == "object"
                                        || wire.to_port.starts_with("object_"))
                            })
                            .count()
                    })
                    .unwrap_or_else(|| group_render_indices(wires, render_id, source).len()),
            };
            let count =
                u32::try_from(count).map_err(|_| "Duplicate Object object count is exhausted")?;
            let end = next
                .checked_add(count)
                .filter(|end| *end as f32 as u32 == *end)
                .ok_or("Duplicate Object object count is exhausted")?;
            if count == 0
                || (next..end).any(|index| {
                    wires.iter().any(|wire| {
                        wire.to_node == render_id && wire.to_port == format!("object_{index}")
                    })
                })
            {
                return Err("Duplicate Object destination slots are unavailable");
            }
            Ok((next, end))
        })();
        let (new_k, new_count) = match append_range {
            Ok(range) => range,
            Err(reason) => {
                self.rejection = Some(reason.into());
                return;
            }
        };
        let mut candidate = baseline.clone();
        let mut node_id_map: Vec<(NodeId, NodeId)> = Vec::new();
        let mut cloned_group_id = None;
        let result = (|| {
            let def = &mut candidate;
            // Document ids and handles are global even when the edit is
            // addressed through a nested scope. Seed both allocators
            // from the full tree before borrowing the target level.
            let mut next_id = max_node_id_over(&def.nodes).checked_add(1)?;
            let mut taken = std::collections::HashSet::new();
            collect_all_handles(&def.nodes, &mut taken);
            let (nodes, wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;

            if let Some(PhysicsSceneObjectMatch::Valid(physics)) = physics_match.as_ref() {
                let new_slot = first_free_physics_body_slot(wires, physics.world_id)?;
                append_physics_duplicate(
                    nodes,
                    wires,
                    physics,
                    render_id,
                    &physics.render_indices,
                    new_k,
                    new_slot,
                    &mut node_id_map,
                )?;
                nodes.iter_mut().find(|n| n.id == render_id)?.params.insert(
                    "objects".to_string(),
                    SerializedParamValue::Float {
                        value: new_count as f32,
                    },
                );
            } else {
                let source_id = object_producer_id(wires, render_id, src_k)?;
                let source_node = nodes.iter().find(|n| n.id == source_id)?.clone();
                if source_node.type_id == "node.scene_object" {
                    let owned = loose_scene_object_owned_ids(nodes, wires, source_id);
                    if owned.is_empty() {
                        return None;
                    }
                    let mut source_outputs: Vec<(u32, String)> = wires
                        .iter()
                        .filter_map(|wire| {
                            if wire.from_node != source_id || wire.to_node != render_id {
                                return None;
                            }
                            let index = if wire.to_port == "object" {
                                0
                            } else {
                                wire.to_port.strip_prefix("object_")?.parse().ok()?
                            };
                            Some((index, wire.from_port.clone()))
                        })
                        .collect();
                    source_outputs.sort_unstable();
                    source_outputs.dedup_by_key(|(index, _)| *index);
                    if source_outputs.is_empty() {
                        return None;
                    }

                    let cloned_handle = source_node.handle.as_ref().map(|handle| format!("{handle} 2"));
                    let mut clones = Vec::new();
                    let mut numeric_map = std::collections::HashMap::new();
                    let mut offset_applied = false;
                    for source in nodes.iter().filter(|node| owned.contains(&node.id)) {
                        let mut clone = deep_clone_with_fresh_ids(
                            source,
                            &mut next_id,
                            &mut taken,
                            &mut node_id_map,
                        );
                        if source.id == source_id {
                            clone.handle = cloned_handle.clone();
                            clone.editor_pos = clone.editor_pos.map(|(x, y)| (x + 40.0, y + 40.0));
                        }
                        if clone.type_id == "node.transform_3d" && !offset_applied {
                            offset_applied = true;
                            let cur = match clone.params.get("pos_x") {
                                Some(SerializedParamValue::Float { value }) => *value,
                                _ => 0.0,
                            };
                            clone.params.insert(
                                "pos_x".to_string(),
                                SerializedParamValue::Float { value: cur + 0.5 },
                            );
                        }
                        numeric_map.insert(source.id, clone.id);
                        clones.push(clone);
                    }
                    let clone_id = *numeric_map.get(&source_id)?;
                    let cloned_wires: Vec<_> = wires
                        .iter()
                        .filter_map(|wire| {
                            let to_node = numeric_map.get(&wire.to_node).copied()?;
                            let from_node = numeric_map
                                .get(&wire.from_node)
                                .copied()
                                .unwrap_or(wire.from_node);
                            Some(EffectGraphWire {
                                from_node,
                                from_port: wire.from_port.clone(),
                                to_node,
                                to_port: wire.to_port.clone(),
                            })
                        })
                        .collect();
                    nodes.extend(clones);
                    wires.extend(cloned_wires);
                    for (part, (_, from_port)) in source_outputs.iter().enumerate() {
                        wires.push(scene_build_wire(
                            clone_id,
                            from_port,
                            render_id,
                            &format!("object_{}", new_k + part as u32),
                        ));
                    }
                    nodes.iter_mut().find(|n| n.id == render_id)?.params.insert(
                        "objects".to_string(),
                        SerializedParamValue::Float {
                            value: new_count as f32,
                        },
                    );
                } else {
                let mut source_outputs: Vec<(u32, String)> = wires
                    .iter()
                    .filter_map(|wire| {
                        if wire.from_node != source_id || wire.to_node != render_id {
                            return None;
                        }
                        wire.to_port
                            .strip_prefix("object_")
                            .and_then(|value| value.parse::<u32>().ok())
                            .map(|index| (index, wire.from_port.clone()))
                    })
                    .collect();
                source_outputs.sort_unstable();
                source_outputs.dedup_by_key(|(index, _)| *index);
                if source_outputs.is_empty() {
                    return None;
                }

                let mut clone = deep_clone_with_fresh_ids(
                    &source_node,
                    &mut next_id,
                    &mut taken,
                    &mut node_id_map,
                );
                // D11's exact top-level convention (handle + " 2") overrides
                // whatever `deep_clone_with_fresh_ids`'s generic dedup pass
                // assigned to the TOP node.
                let cloned_handle = source_node.handle.as_ref().map(|h| format!("{h} 2"));
                clone.handle = cloned_handle.clone();
                clone.editor_pos = clone.editor_pos.map(|(x, y)| (x + 40.0, y + 40.0));

                // D6: keep a grouped scene_object's name in sync with its
                // enclosing group, as Add/importer and Rename do.
                if let Some(body) = clone.group.as_deref_mut() {
                    if let Some(inner_object) = body
                        .nodes
                        .iter_mut()
                        .find(|n| n.type_id == "node.scene_object")
                    {
                        inner_object.handle = cloned_handle;
                    }
                    if let Some(transform_node) = body
                        .nodes
                        .iter_mut()
                        .find(|n| n.type_id == "node.transform_3d")
                    {
                        let cur = match transform_node.params.get("pos_x") {
                            Some(SerializedParamValue::Float { value }) => *value,
                            _ => 0.0,
                        };
                        transform_node.params.insert(
                            "pos_x".to_string(),
                            SerializedParamValue::Float { value: cur + 0.5 },
                        );
                    }
                }
                let clone_id = clone.id;
                if source_node.type_id == GROUP_TYPE_ID && scope.is_empty() {
                    cloned_group_id = Some(clone_id);
                }
                if source_node.type_id == GROUP_TYPE_ID {
                    // Group inputs are shared upstream signals, such as World
                    // controls. Keep them connected to the copied boundary.
                    let incoming: Vec<_> = wires.iter()
                        .filter(|wire| wire.to_node == source_id)
                        .map(|wire| EffectGraphWire {
                            to_node: clone_id,
                            ..wire.clone()
                        })
                        .collect();
                    wires.extend(incoming);
                }
                nodes.push(clone);
                for (part, _) in source_outputs.iter().enumerate() {
                    wires.push(scene_build_wire(
                        clone_id,
                        &source_outputs[part].1,
                        render_id,
                        &format!("object_{}", new_k + part as u32),
                    ));
                }
                nodes.iter_mut().find(|n| n.id == render_id)?.params.insert(
                    "objects".to_string(),
                    SerializedParamValue::Float {
                        value: new_count as f32,
                    },
                );
                }
            }

            Some(())
        })();
        if result.is_none() {
            // The clone itself was refused (unresolvable source/level) — no
            // subtree was cloned, so there's nothing to sweep bindings for.
            self.rejection = Some("Duplicate Object source object is unavailable".into());
            return;
        }

        // BUG-212: `deep_clone_with_fresh_ids` mints fresh `NodeId`s for
        // every cloned node (D11 — a stale NodeId would let a card binding
        // silently double-drive both the original and the copy), which
        // makes `string_bindings` entries dangle by the same mechanism —
        // unlike `bindings`/`exposed_params` (D11: performer-facing card
        // exposes, deliberately NOT carried by a duplicate), `string_bindings`
        // is the importer's own "Model File" path plumbing (one entry per
        // file-dependent node, fanned out under a shared outer id) and
        // dropping it silently breaks mesh loading on the clone. Clone every
        // entry whose target falls inside the duplicated subtree, re-targeted
        // at the clone's fresh NodeId, same `id`/`label`/`default_value`.
        // Reached at the same undo-unit boundary `RenameSceneObjectCommand`'s
        // D5 sweep uses (`resolve_target_instance`, outside
        // `with_target_graph_mut`'s narrower graph-only view).
        if let (Some(original_group), Some(cloned_group)) = (
            graph_level(&baseline, &scope)
                .and_then(|(_, wires)| object_producer_id(wires, render_id, src_k)),
            cloned_group_id,
        ) && let Err(reason) = fluid::duplicate_scene_object_fluid_roles(
            &mut candidate,
            original_group,
            cloned_group,
            &node_id_map,
        ) {
            self.rejection = Some(reason);
            return;
        }
        if !node_id_map.is_empty() {
            if let Some(meta) = candidate.preset_metadata.as_mut() {
                let new_entries: Vec<StringBindingDef> = meta
                    .string_bindings
                    .iter()
                    .filter_map(|b| match &b.target {
                        manifold_core::effect_graph_def::BindingTarget::Node { node_id, param } => {
                            node_id_map
                                .iter()
                                .find(|(old, _)| old == node_id)
                                .map(|(_, new_id)| StringBindingDef {
                                    id: b.id.clone(),
                                    label: b.label.clone(),
                                    default_value: b.default_value.clone(),
                                    target: manifold_core::effect_graph_def::BindingTarget::Node {
                                        node_id: new_id.clone(),
                                        param: param.clone(),
                                    },
                                })
                        }
                        manifold_core::effect_graph_def::BindingTarget::Composite { .. } => None,
                        manifold_core::effect_graph_def::BindingTarget::SceneModifier {
                            ..
                        } => None,
                    })
                    .collect();
                meta.string_bindings.extend(new_entries);
            }
            clone_sections::clone_scene_bindings(&mut candidate, &node_id_map);
        }
        let values = match project.graph_target_owner(&self.target)
            .ok_or_else(|| "Duplicate Object parameter owner is unavailable".to_string())
            .and_then(|owner| clone_values::prepare(owner, &self.catalog_default, &self.target, &candidate, &node_id_map))
        {
            Ok(values) => values,
            Err(reason) => { self.rejection = Some(reason); return; }
        };
        let after_graph = candidate.clone();
        if with_target_graph_mut(project, &self.target, &self.catalog_default, true, |def| {
            *def = after_graph.clone();
        })
        .is_none()
        {
            self.rejection = Some("Duplicate Object target is unavailable".into());
            return;
        }
        refresh_target_manifest(project, &self.target);
        self.prev_graph = Some(previous_graph);
        self.after_graph = Some(after_graph);
        self.prev_instance = baseline_instance;
        if let Some(instance) = project.graph_target_owner_mut(&self.target) {
            for (id, value) in values {
                instance.set_base_param_from_automation(&id, value);
            }
        }
        self.after_instance = project
            .graph_target_owner_mut(&self.target)
            .map(|instance| InstanceLayerSnapshot::capture(&*instance));
        self.applied = true;
    }

    fn undo(&mut self, project: &mut Project) {
        let Some(previous_graph) = self.prev_graph.clone() else {
            return;
        };
        if !self.applied {
            return;
        }
        restore_scene_owner_graph(project, &self.target, previous_graph);
        refresh_target_manifest(project, &self.target);
        if let (Some(snapshot), Some(instance)) = (
            self.prev_instance.clone(),
            project.graph_target_owner_mut(&self.target),
        ) {
            snapshot.restore(instance);
        }
        self.applied = false;
    }

    fn description(&self) -> &str {
        "Duplicate Object"
    }

    fn rejection_reason(&self) -> Option<&str> {
        self.rejection.as_deref()
    }

    fn was_applied(&self) -> bool {
        self.applied
    }
}

