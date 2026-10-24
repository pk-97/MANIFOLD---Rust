//! Selection receipts for successful content-owned insertions.
use manifold_core::scene_doc::ItemId;
use manifold_core::{EffectId, LayerId, project::Project};
use manifold_editing::commands::effect_target::{EffectTarget, with_effects};

#[derive(Clone, Debug)]
pub(crate) enum EditSelection {
    Effects {
        target: EffectTarget,
        ids: Vec<EffectId>,
    },
    Layer(LayerId),
    /// A scene item the edit added.
    Item {
        layer_id: LayerId,
        item: ItemId,
    },
    /// A click on a scene picture found this item, or empty space
    /// (`scene_pick.rs`).
    Picked(crate::scene_pick::PickedItem),
}

#[derive(Debug)]
pub(crate) enum SelectAfterEdit {
    Effects {
        target: EffectTarget,
        ids: Vec<EffectId>,
    },
    NewLayer,
    NewItem(LayerId, ItemId),
}

pub(crate) enum PendingSelection {
    Effects {
        target: EffectTarget,
        ids: Vec<EffectId>,
    },
    Layer(Vec<LayerId>),
    Item {
        layer_id: LayerId,
        item: ItemId,
    },
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
            Self::NewItem(layer_id, item) => PendingSelection::Item { layer_id, item },
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
            Self::Item { layer_id, item } => project
                .scene_of(&layer_id)
                .is_some_and(|doc| doc.find(&item).is_some())
                .then_some(EditSelection::Item { layer_id, item }),
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
        EditSelection::Item { layer_id, item } => {
            let Some(doc) = project.scene_of(layer_id) else {
                return false;
            };
            if doc.find(item).is_none() {
                return false;
            }
            focus_scene_tab(selection, active_layer, layer_id);
            reveal(ui, doc, layer_id, item);
            ui.scene_setup_panel.set_selection(
                layer_id.clone(),
                manifold_ui::panels::scene_setup_panel::SceneSelection::Item(item.clone()),
            );
        }
        EditSelection::Picked(picked) => {
            let layer_id = &picked.pick.layer;
            let Some(doc) = project.scene_of(layer_id) else {
                return false;
            };
            match picked.item.as_ref().filter(|item| doc.find(item).is_some()) {
                Some(item) => {
                    reveal(ui, doc, layer_id, item);
                    let picked_item =
                        manifold_ui::panels::scene_setup_panel::SceneSelection::Item(item.clone());
                    if picked.pick.extend {
                        ui.scene_setup_panel.toggle_selected(layer_id, picked_item);
                    } else {
                        ui.scene_setup_panel
                            .select_items(layer_id.clone(), vec![picked_item]);
                    }
                }
                // Empty space clears the selection, as in Blender; Shift on
                // empty space keeps it.
                None if !picked.pick.extend => {
                    ui.scene_setup_panel.clear_selected(layer_id.clone())
                }
                None => {}
            }
        }
    }
    true
}

/// Select an import root on the spot, when the UI thread already knows the
/// edit landed — a drop that made its own layer executes that layer locally,
/// so there is no content-thread receipt to wait for (SCENE_DOCUMENT_DESIGN.md
/// section 7.7 (Imports)). The same appearance as a receipt.
pub(crate) fn select_import_root(
    ui: &mut crate::ui_root::UIRoot,
    doc: &manifold_core::scene_doc::SceneDoc,
    selection: &mut crate::app::SelectionState,
    active_layer: &mut Option<LayerId>,
    layer_id: &LayerId,
    item: &ItemId,
) {
    if doc.find(item).is_none() {
        return;
    }
    focus_scene_tab(selection, active_layer, layer_id);
    reveal(ui, doc, layer_id, item);
    ui.scene_setup_panel.set_selection(
        layer_id.clone(),
        manifold_ui::panels::scene_setup_panel::SceneSelection::Item(item.clone()),
    );
}

/// Unfold every group above `item`, so its row shows in the tree.
fn reveal(
    ui: &mut crate::ui_root::UIRoot,
    doc: &manifold_core::scene_doc::SceneDoc,
    layer_id: &LayerId,
    item: &ItemId,
) {
    let mut parents = Vec::new();
    doc.walk(|found, ancestors| {
        if &found.id == item {
            parents = ancestors.iter().map(|a| a.id.clone()).collect();
        }
    });
    for parent in parents {
        ui.scene_setup_panel.unfold(layer_id.clone(), parent);
    }
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
