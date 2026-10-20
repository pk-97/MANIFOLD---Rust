//! The Scene tab's one Add menu (SCENE_SURFACES D8): every group's "+" opens
//! it, starting on that group's entries. Each entry is one insert action: a
//! template for the scene kinds, the existing scene modifier path for forces.

use manifold_foundation::scene_surface::SceneTemplateKind;

use crate::ProjectAction;

use super::PanelAction;
use super::scene_setup_panel::{SceneGroup, SceneSetupVm};

/// One row of the Add menu. `disabled` carries the reason an entry cannot be
/// used right now; a disabled entry has no action.
#[derive(Debug, Clone)]
pub struct AddMenuItem {
    pub label: String,
    pub action: Option<PanelAction>,
    pub disabled: Option<String>,
    /// The last of the clicked group's own entries, when others follow.
    pub separator_after: bool,
}

type Entry = (SceneGroup, String, Option<PanelAction>, Option<String>);

/// The menu for `vm`, in display order: the entries of group `first`, then
/// every other entry in the fixed order Object, Plane, Light, Water, the
/// forces, Import Model.
pub fn add_menu(vm: &SceneSetupVm, first: SceneGroup) -> Vec<AddMenuItem> {
    let layer = || vm.layer_id.clone();
    let insert = |kind| PanelAction::Project(ProjectAction::SceneSetupInsertTemplate { layer_id: layer(), kind });
    let entry = |group: SceneGroup, label: &str, action: PanelAction| -> Entry { (group, label.to_owned(), Some(action), None) };
    let mut entries: Vec<Entry> = vec![
        entry(SceneGroup::Objects, "Object", insert(SceneTemplateKind::Object)),
        entry(SceneGroup::Objects, "Plane", insert(SceneTemplateKind::Plane)),
        entry(SceneGroup::Lighting, "Light", insert(SceneTemplateKind::Light)),
        entry(SceneGroup::Objects, "Water", insert(SceneTemplateKind::Water)),
    ];
    for force in &vm.force_picker {
        let label = format!("Force \u{2014} {}", force.label);
        entries.push(match &force.disabled {
            Some(reason) => (SceneGroup::Forces, label, None, Some(reason.clone())),
            None => entry(
                SceneGroup::Forces,
                &label,
                PanelAction::Project(ProjectAction::SceneModifierApply(layer(), force.preset_id.clone())),
            ),
        });
    }
    entries.push(entry(
        SceneGroup::Objects,
        "Import Model\u{2026}",
        PanelAction::Project(ProjectAction::SceneSetupImportModelClicked(layer(), vm.scene_root_node_id)),
    ));
    let (own, rest): (Vec<Entry>, Vec<Entry>) = entries.into_iter().partition(|(group, ..)| *group == first);
    let split = own.len();
    own.into_iter()
        .chain(rest)
        .enumerate()
        .map(|(index, (_, label, action, disabled))| AddMenuItem {
            label,
            action,
            disabled,
            separator_after: split > 0 && index + 1 == split,
        })
        .collect()
}
