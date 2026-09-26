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
    Object {
        layer_id: LayerId,
        object_id: u32,
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
}

pub(crate) enum PendingSelection {
    Effects {
        target: EffectTarget,
        ids: Vec<EffectId>,
    },
    Layer(Vec<LayerId>),
    Object {
        layer_id: LayerId,
        previous: Vec<u32>,
    },
}

fn object_ids(project: &Project, layer: &LayerId) -> Vec<u32> {
    let Some(def) = crate::graph_target::resolve(
        project,
        &manifold_core::GraphTarget::Generator(layer.clone()),
    ) else {
        return Vec::new();
    };
    manifold_renderer::node_graph::scene_vm::SceneVm::from_def(def)
        .map(|vm| {
            vm.objects
                .into_iter()
                .filter_map(|object| match object {
                    manifold_renderer::node_graph::scene_vm::SceneObjectVm::Known(row) => {
                        Some(row.object_node_id)
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
            Self::NewObject(layer_id) => {
                let previous = object_ids(project, &layer_id);
                PendingSelection::Object { layer_id, previous }
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
            Self::Object { layer_id, previous } => object_ids(project, &layer_id)
                .into_iter()
                .find(|id| !previous.contains(id))
                .map(|object_id| EditSelection::Object {
                    layer_id,
                    object_id,
                }),
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
            ui.pending_layer_reveal = Some(id.clone());
        }
        EditSelection::Object {
            layer_id,
            object_id,
        } => {
            if !object_ids(project, layer_id).contains(object_id) {
                return false;
            }
            ui.scene_setup_panel.set_selection(
                layer_id.clone(),
                manifold_ui::panels::scene_setup_panel::SceneSelection::Object(*object_id),
            );
        }
    }
    true
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
        assert_eq!(selection.primary_selected_layer_id, Some(added_id.clone()));
        assert_eq!(ui.pending_layer_reveal, Some(added_id));
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
        assert!(selection.primary_selected_layer_id.is_none());
        assert!(ui.pending_layer_reveal.is_none());
    }
}
