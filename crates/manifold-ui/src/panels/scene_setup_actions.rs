//! The Scene tab's one Add menu (SCENE_SURFACES D8): every group's "+" opens
//! it, starting on that group's entries. Each entry is one insert action: a
//! template for the scene kinds, the existing scene modifier path for forces.

use manifold_foundation::NodeId;
use manifold_foundation::scene_surface::SceneTemplateKind;

use crate::ProjectAction;

use super::PanelAction;
use super::scene_setup_panel::{SceneGroup, SceneSelection, SceneSetupVm};

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
/// every other entry in the fixed order Object, Plane, Light, Water, Inflow,
/// Drain, Collider, the forces, Import Model. A fluid source goes into the
/// water `selected` is or sits under, else the scene's only water.
pub fn add_menu(vm: &SceneSetupVm, first: SceneGroup, selected: Option<&SceneSelection>) -> Vec<AddMenuItem> {
    let layer = || vm.layer_id.clone();
    let insert = |kind, water| PanelAction::Project(ProjectAction::SceneSetupInsertTemplate {
        layer_id: layer(),
        render_scene_node_id: vm.scene_root_node_id,
        kind,
        water,
    });
    let entry = |group: SceneGroup, label: &str, action: PanelAction| -> Entry { (group, label.to_owned(), Some(action), None) };
    let mut entries: Vec<Entry> = vec![
        entry(SceneGroup::Objects, "Object", insert(SceneTemplateKind::Object, None)),
        entry(SceneGroup::Objects, "Plane", insert(SceneTemplateKind::Plane, None)),
        entry(SceneGroup::Lighting, "Light", insert(SceneTemplateKind::Light, None)),
        entry(SceneGroup::Objects, "Water", insert(SceneTemplateKind::Water, None)),
    ];
    let water = source_water(vm, selected);
    for (kind, label) in [
        (SceneTemplateKind::Inflow, "Inflow"),
        (SceneTemplateKind::Drain, "Drain"),
        (SceneTemplateKind::Collider, "Collider"),
    ] {
        entries.push(match &water {
            Ok(water) => entry(SceneGroup::Objects, label, insert(kind, Some(water.clone()))),
            Err(reason) => (SceneGroup::Objects, label.to_owned(), None, Some((*reason).to_owned())),
        });
    }
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

/// The liquid domain a fluid source from the menu goes into: the water the
/// selection is or sits under, else the only water, else why there is none.
fn source_water(vm: &SceneSetupVm, selected: Option<&SceneSelection>) -> Result<NodeId, &'static str> {
    let row = match selected {
        Some(SceneSelection::Object(id)) => Some(*id),
        Some(selection @ SceneSelection::Source(_)) => {
            vm.items.iter().find(|item| &item.selection == selection).and_then(|item| item.parent)
        }
        _ => None,
    };
    if let Some(domain) = row.and_then(|row| vm.fluid_domains.iter().find(|domain| domain.rows.contains(&row))) {
        return Ok(domain.node.clone());
    }
    match vm.fluid_domains.as_slice() {
        [] => Err("Add Water first"),
        [only] => Ok(only.node.clone()),
        _ => Err("Select the water this goes into"),
    }
}
