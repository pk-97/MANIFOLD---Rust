//! The data the Scene tab is built from: the scene document's items as a flat
//! preorder list, their field rows, and the selection type. Pure data; the
//! panel draws from it and never reads the project.

use super::*;
use manifold_foundation::ItemId;
use manifold_foundation::scene_surface::{FieldPick, ItemKind};

use crate::param_surface::MaterialFeature;

/// One value a pick-list field row offers.
#[derive(Clone, Debug, PartialEq)]
pub struct FieldOptionVm {
    pub label: String,
    /// What a click on it sets.
    pub pick: FieldPick,
    /// The value the field holds now (a set field checks every member).
    pub checked: bool,
    /// Why this choice cannot be picked now: drawn disabled with the reason.
    pub disabled: Option<String>,
}

/// How a field row picks its value, with the choices resolved to names.
#[derive(Clone, Debug, PartialEq)]
pub enum FieldControlVm {
    /// Pick from `options`; `current` is the text on the row's button.
    Pick { current: String, options: Vec<FieldOptionVm> },
    /// A file picker; `path` is the file now, empty for none.
    File { path: String, extensions: Vec<String> },
    /// On or off, drawn as a switch; a click sends `toggle`.
    Switch { on: bool, toggle: FieldPick },
}

/// One structural-field row of an item's card. The panel draws it as a row of
/// the shared param surface; a pick sends `ProjectAction::SceneFieldSet`.
#[derive(Clone, Debug, PartialEq)]
pub struct FieldRowVm {
    pub id: String,
    pub label: String,
    pub section: ControlGroup,
    pub control: FieldControlVm,
    /// Why the row cannot be used right now: drawn disabled, the reason on
    /// hover.
    pub disabled: Option<String>,
    /// The card header carries it instead of a row (a Modifier's Type and On).
    pub header: bool,
}

/// One Add menu entry: what a click on it adds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddEntryVm {
    pub key: String,
    /// The menu section it sits under.
    pub group: String,
    pub label: String,
    /// Whether it can sit at the top of the scene.
    pub top_level: bool,
    /// The kinds it can sit inside.
    pub holders: Vec<ItemKind>,
    /// Why it cannot be added right now, for the current selection: drawn
    /// disabled with the reason.
    pub disabled: Option<String>,
}

/// The Material an item draws with, as its card shows it inline.
#[derive(Clone, Debug, PartialEq)]
pub struct MaterialBlockVm {
    pub item: ItemId,
    pub name: String,
    /// How many items bind this material.
    pub used_by: usize,
    /// The features added to it, in the inspector's order.
    pub features: Vec<MaterialFeature>,
    pub parameter_ids: Vec<String>,
    /// The Material's own field rows (its texture source).
    pub fields: Vec<FieldRowVm>,
    /// The starting look its features and values match, if any.
    pub look: Option<crate::param_surface::MaterialLook>,
    /// Whether it shades PBR. Looks and features are PBR, so an Unlit or Cel
    /// Material offers neither.
    pub pbr: bool,
}

/// One item of the Scene tab tree, in document preorder.
#[derive(Clone, Debug, PartialEq)]
pub struct SceneItemVm {
    pub id: ItemId,
    pub kind: ItemKind,
    pub name: String,
    /// How many groups sit above it.
    pub depth: usize,
    pub parent: Option<ItemId>,
    pub has_children: bool,
    /// The manifest params this item owns (`<item id>.<property>`), in
    /// manifest order.
    pub parameter_ids: Vec<String>,
    pub fields: Vec<FieldRowVm>,
    /// The Material block shown inline: this Material itself, or the one an
    /// Object or Group draws with.
    pub material: Option<MaterialBlockVm>,
    /// Whether the item can be removed and duplicated.
    pub removable: bool,
    /// An Object whose Shape is a plane: it gets the plane mark in the tree.
    pub plane: bool,
    /// A Camera whose rig the editor view can be copied into.
    pub copy_view: bool,
    /// The Environment light: Lighting lists it before the other lights.
    pub environment: bool,
    /// An Environment light in HDRI mode: the file "Aim Sun at HDRI" aims at.
    pub aim_sun_file: Option<String>,
    /// The World of a layer that stopped drawing after runaway GPU frames:
    /// its header offers Draw Again.
    pub draw_again: bool,
    /// A Scene Loop card in a scene with no Loop camera: it offers Add Loop
    /// Camera.
    pub add_loop_camera: bool,
    /// Why a manifest row cannot be used right now, by param id: drawn
    /// dimmed with the reason on hover.
    pub row_reasons: Vec<(String, String)>,
    /// Its physics is Body: the tree marks it when it sits inside a Group.
    pub body: bool,
    /// A Swarm drawn as a scene item: that item and its name, linked from
    /// the card as its look.
    pub look: Option<(ItemId, String)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SceneSetupVm {
    pub layer_id: LayerId,
    /// Every item of the scene, depth-first.
    pub items: Vec<SceneItemVm>,
    pub add_entries: Vec<AddEntryVm>,
}

impl SceneSetupVm {
    pub fn item(&self, id: &ItemId) -> Option<&SceneItemVm> {
        self.items.iter().find(|item| &item.id == id)
    }
}

/// The outliner selection: the one scene item whose controls the properties
/// region shows. UI-local workspace state, like fold state, never saved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SceneSelection {
    Item(ItemId),
    /// All lighting in one place: every light, the environment and its HDRI.
    Lighting,
}

impl SceneSelection {
    /// What the tab shows for this selection: a light shows as Lighting.
    pub fn shown(self, vm: &SceneSetupVm) -> Self {
        match self {
            SceneSelection::Item(id) if vm.item(&id).is_some_and(|item| item.kind == ItemKind::Light) => {
                SceneSelection::Lighting
            }
            other => other,
        }
    }

    /// The item this selection names, if it is one.
    pub fn item(&self) -> Option<&ItemId> {
        match self {
            SceneSelection::Item(id) => Some(id),
            SceneSelection::Lighting => None,
        }
    }
}

/// The empty and live states for the selected layer.
#[derive(Clone, Debug, PartialEq)]
pub enum SceneSetupState {
    /// Nothing selected, or the selection isn't a generator layer.
    NoSelection(String),
    /// A generator layer with no scene.
    NoScene { layer_id: LayerId },
    /// The full panel.
    Live(Box<SceneSetupVm>),
}

impl Default for SceneSetupState {
    fn default() -> Self {
        SceneSetupState::NoSelection("Select a layer to set up its scene.".to_string())
    }
}

impl SceneSetupState {
    /// Extract the `Live` VM if present, None otherwise.
    pub fn as_live(&self) -> Option<&SceneSetupVm> {
        match self {
            SceneSetupState::Live(vm) => Some(vm),
            _ => None,
        }
    }
}
