//! Selection receipts for successful content-owned insertions.
use manifold_core::{EffectId, LayerId, project::Project};
use manifold_editing::commands::effect_target::{EffectTarget, with_effects};

#[derive(Clone, Debug)]
pub(crate) enum EditSelection {
    Effects {
        target: EffectTarget,
        ids: Vec<EffectId>,
    },
    Layer(LayerId),
    Light {
        layer_id: LayerId,
        light_id: u32,
    },
    Object {
        layer_id: LayerId,
        object_id: u32,
    },
    /// A placed fluid source, by its role node.
    Source {
        layer_id: LayerId,
        /// The water row it lists under, unfolded so the row shows.
        water: u32,
        node: manifold_core::NodeId,
    },
}

#[derive(Debug)]
pub(crate) enum SelectAfterEdit {
    Effects {
        target: EffectTarget,
        ids: Vec<EffectId>,
    },
    NewLayer,
    NewObject(LayerId),
    NewLight(LayerId),
    NewSource(LayerId),
}

pub(crate) enum PendingSelection {
    Effects {
        target: EffectTarget,
        ids: Vec<EffectId>,
    },
    Layer(Vec<LayerId>),
    Light {
        layer_id: LayerId,
        previous: Vec<u32>,
    },
    Object {
        layer_id: LayerId,
        previous: Vec<u32>,
    },
    Source {
        layer_id: LayerId,
        previous: Vec<(u32, manifold_core::NodeId)>,
    },
}

fn object_ids(project: &Project, layer: &LayerId) -> Vec<u32> {
    let target = manifold_core::GraphTarget::Generator(layer.clone());
    let Some(def) = crate::graph_target::resolve(project, &target) else {
        return Vec::new();
    };
    manifold_nodes_scene::node_graph::scene_vm::SceneVm::from_def(&def)
        .map(|vm| {
            vm.objects
                .into_iter()
                .filter_map(|object| match object {
                    manifold_nodes_scene::node_graph::scene_vm::SceneObjectVm::Known(row) => {
                        Some(row.object_node_id)
                    }
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Each placed source, by its role node, with the object id of the water it lists under.
fn source_nodes(project: &Project, layer: &LayerId) -> Vec<(u32, manifold_core::NodeId)> {
    let target = manifold_core::GraphTarget::Generator(layer.clone());
    crate::graph_target::resolve(project, &target)
        .and_then(|def| manifold_nodes_scene::node_graph::scene_vm::SceneVm::from_def(&def))
        .map(|vm| {
            vm.objects
                .into_iter()
                .filter_map(|object| match object {
                    manifold_nodes_scene::node_graph::scene_vm::SceneObjectVm::Known(row) => {
                        let water = row.object_node_id;
                        Some(row.placed_sources.into_iter().map(move |source| (water, source.node)))
                    }
                    _ => None,
                })
                .flatten()
                .collect()
        })
        .unwrap_or_default()
}

fn light_ids(project: &Project, layer: &LayerId) -> Vec<u32> {
    let target = manifold_core::GraphTarget::Generator(layer.clone());
    crate::graph_target::resolve(project, &target)
    .and_then(|def| manifold_nodes_scene::node_graph::scene_vm::SceneVm::from_def(&def))
    .map(|vm| {
        vm.lights
            .into_iter()
            .filter_map(|light| match light {
                manifold_nodes_scene::node_graph::scene_vm::SceneLightVm::Known(row) => {
                    Some(row.node_doc_id)
                }
                _ => None,
            })
            .collect()
    })
    .unwrap_or_default()
}

impl SelectAfterEdit {
    pub(crate) fn capture(self, project: &Project) -> PendingSelection {
        match self {
            Self::Effects { target, ids } => PendingSelection::Effects { target, ids },
            Self::NewLayer => PendingSelection::Layer(
                project
                    .timeline
                    .layers
                    .iter()
                    .map(|layer| layer.layer_id.clone())
                    .collect(),
            ),
            Self::NewLight(layer_id) => {
                let previous = light_ids(project, &layer_id);
                PendingSelection::Light { layer_id, previous }
            }
            Self::NewObject(layer_id) => {
                let previous = object_ids(project, &layer_id);
                PendingSelection::Object { layer_id, previous }
            }
            Self::NewSource(layer_id) => {
                let previous = source_nodes(project, &layer_id);
                PendingSelection::Source { layer_id, previous }
            }
        }
    }
}

impl PendingSelection {
    pub(crate) fn resolve(self, project: &Project) -> Option<EditSelection> {
        match self {
            Self::Effects { target, ids } => {
                let present = !ids.is_empty()
                    && with_effects(project, &target, |effects, _| {
                        ids.iter()
                            .all(|id| effects.iter().any(|effect| effect.id == *id))
                    })
                    .unwrap_or(false);
                present.then_some(EditSelection::Effects { target, ids })
            }
            Self::Layer(previous) => project
                .timeline
                .layers
                .iter()
                .find(|layer| !previous.contains(&layer.layer_id))
                .map(|layer| EditSelection::Layer(layer.layer_id.clone())),
            Self::Light { layer_id, previous } => light_ids(project, &layer_id)
                .into_iter()
                .find(|id| !previous.contains(id))
                .map(|light_id| EditSelection::Light { layer_id, light_id }),
            Self::Object { layer_id, previous } => object_ids(project, &layer_id)
                .into_iter()
                .find(|id| !previous.contains(id))
                .map(|object_id| EditSelection::Object {
                    layer_id,
                    object_id,
                }),
            Self::Source { layer_id, previous } => source_nodes(project, &layer_id)
                .into_iter()
                .find(|source| !previous.iter().any(|(_, node)| *node == source.1))
                .map(|(water, node)| EditSelection::Source { layer_id, water, node }),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct EditSelectionUpdate {
    pub sequence: u64,
    pub selection: EditSelection,
}

/// Consume a receipt before structural projection so labels, controls and
/// selection are built from the same accepted snapshot.
pub(crate) fn apply_update(
    ui: &mut crate::ui_root::UIRoot,
    project: &Project,
    update: Option<&EditSelectionUpdate>,
    selection: &mut crate::app::SelectionState,
    active_layer: &mut Option<LayerId>,
) -> bool {
    let Some(update) = update else {
        ui.last_edit_selection_sequence = None;
        return false;
    };
    if ui.last_edit_selection_sequence == Some(update.sequence) {
        return false;
    }
    // A later command may already have removed the result. Consume that receipt
    // too, so subsequent frames do not keep resolving a vanished object.
    ui.last_edit_selection_sequence = Some(update.sequence);
    use manifold_ui::panels::InspectorTab;
    match &update.selection {
        EditSelection::Effects { target, ids } => {
            if !with_effects(project, target, |effects, _| {
                ids.iter()
                    .all(|id| effects.iter().any(|effect| effect.id == *id))
            })
            .unwrap_or(false)
            {
                return false;
            }
            let tab = ui.inspector.active_tab();
            let same_scope = match target {
                EffectTarget::Master => tab == InspectorTab::Master,
                EffectTarget::Layer { layer_id } => {
                    tab != InspectorTab::Master
                        && ui.inspector.inspected_layer_id() == Some(layer_id)
                }
            };
            if same_scope {
                ui.inspector.select_effect_ids(tab, ids);
            }
        }
        EditSelection::Layer(id) => {
            if project.timeline.find_layer_by_id(id).is_none() {
                return false;
            }
            selection.select_layer(id.clone());
            selection.pin_scope(InspectorTab::Layer);
            *active_layer = Some(id.clone());
            ui.request_layer_reveal(id.clone());
        }
        EditSelection::Light { layer_id, light_id } => {
            if !light_ids(project, layer_id).contains(light_id) {
                return false;
            }
            focus_scene_tab(selection, active_layer, layer_id);
            ui.scene_setup_panel.set_selection(
                layer_id.clone(),
                manifold_ui::panels::scene_setup_panel::SceneSelection::Lighting,
            );
        }
        EditSelection::Object {
            layer_id,
            object_id,
        } => {
            if !object_ids(project, layer_id).contains(object_id) {
                return false;
            }
            focus_scene_tab(selection, active_layer, layer_id);
            ui.scene_setup_panel.set_selection(
                layer_id.clone(),
                manifold_ui::panels::scene_setup_panel::SceneSelection::Object(*object_id),
            );
        }
        EditSelection::Source { layer_id, water, node } => {
            if !source_nodes(project, layer_id).iter().any(|(_, source)| source == node) {
                return false;
            }
            focus_scene_tab(selection, active_layer, layer_id);
            ui.scene_setup_panel.set_selection(
                layer_id.clone(),
                manifold_ui::panels::scene_setup_panel::SceneSelection::Source(node.clone()),
            );
            ui.scene_setup_panel.unfold(layer_id.clone(), *water);
        }
    }
    true
}

/// A scene item selection shows in the inspector's Scene tab (SCENE_SURFACES
/// D16). The layer is selected only when the inspector is on another layer, so
/// a clip selected on the scene's own layer stays selected.
fn focus_scene_tab(
    selection: &mut crate::app::SelectionState,
    active_layer: &mut Option<LayerId>,
    layer_id: &LayerId,
) {
    let inspected = selection
        .selected_layer_id_for_clip()
        .or(selection.primary_selected_layer_id());
    if inspected != Some(layer_id) {
        selection.select_layer(layer_id.clone());
    }
    *active_layer = Some(layer_id.clone());
    selection.pin_scope(manifold_ui::panels::InspectorTab::Scene);
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::{PresetTypeId, effects::PresetInstance, types::LayerType};
    use manifold_editing::{
        commands::{effect_groups::PasteEffectsCommand, layer::AddLayerCommand},
        service::EditingService,
    };

    #[test]
    fn rejected_paste_has_no_selection_receipt() {
        let mut project = Project::default();
        let original = PresetInstance::new(PresetTypeId::new("Mirror"));
        project.settings.master_effects.push(original);
        let pasted = PresetInstance::new(PresetTypeId::new("Invert"));
        let ids = vec![pasted.id.clone()];
        let pending = SelectAfterEdit::Effects {
            target: EffectTarget::Master,
            ids,
        }
        .capture(&project);
        let before = serde_json::to_vec(&project.settings).unwrap();
        let mut editing = EditingService::new();
        editing.execute(
            Box::new(PasteEffectsCommand::new(
                EffectTarget::Master,
                vec![pasted],
                Vec::new(),
                None,
                Some(manifold_core::EffectGroupId::new("missing-group")),
            )),
            &mut project,
        );
        assert!(editing.take_rejection().is_some());
        assert!(pending.resolve(&project).is_none());
        assert_eq!(serde_json::to_vec(&project.settings).unwrap(), before);
    }

    #[test]
    fn layer_receipt_selects_the_inserted_layer_and_requests_reveal() {
        let mut project = Project::default();
        project
            .timeline
            .add_layer("Existing", LayerType::Video, PresetTypeId::NONE);
        let pending = SelectAfterEdit::NewLayer.capture(&project);
        let mut editing = EditingService::new();
        editing.execute(
            Box::new(AddLayerCommand::new(
                "New generator".into(),
                LayerType::Generator,
                PresetTypeId::PLASMA,
                1,
                None,
            )),
            &mut project,
        );
        let added_id = project.timeline.layers[1].layer_id.clone();
        let update = EditSelectionUpdate {
            sequence: 1,
            selection: pending.resolve(&project).unwrap(),
        };
        let mut ui = crate::ui_root::UIRoot::new();
        let mut selection = crate::app::SelectionState::new();
        let mut active = None;
        assert!(apply_update(
            &mut ui,
            &project,
            Some(&update),
            &mut selection,
            &mut active
        ));
        assert_eq!(active, Some(added_id.clone()));
        assert_eq!(selection.primary_selected_layer_id(), Some(&added_id));
        assert_eq!(ui.pending_layer_reveal(), Some(&added_id));
        assert!(!apply_update(
            &mut ui,
            &project,
            Some(&update),
            &mut selection,
            &mut active
        ));
        assert!(editing.undo(&mut project));
        assert_eq!(project.timeline.layers.len(), 1);
    }

    #[test]
    fn receipt_for_an_already_removed_result_is_consumed() {
        let project = Project::default();
        let update = EditSelectionUpdate {
            sequence: 1,
            selection: EditSelection::Layer(LayerId::new("removed-layer")),
        };
        let mut ui = crate::ui_root::UIRoot::new();
        let mut selection = crate::app::SelectionState::new();
        let mut active = None;
        assert!(!apply_update(
            &mut ui,
            &project,
            Some(&update),
            &mut selection,
            &mut active
        ));
        assert_eq!(ui.last_edit_selection_sequence, Some(1));
        assert!(active.is_none());
        assert!(selection.primary_selected_layer_id().is_none());
        assert!(ui.pending_layer_reveal().is_none());
    }
}
