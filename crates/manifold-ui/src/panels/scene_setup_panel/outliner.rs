//! The Scene tab tree (SCENE_SURFACES D8): fixed groups Camera, Lighting,
//! Objects, Forces, Physics, World. Each row is the item's type icon, its
//! name, and an eye where the item has `visible`. Camera, Lighting and World
//! are single rows; Lighting holds every light, and its add button sits on
//! its row. Objects and Forces own one add button beside their fold header.
//! Rows read `SceneSetupVm.items`.

use super::*;
use crate::icons::scene_item_icon;
use manifold_foundation::scene_surface::SceneItemKind;

/// A fixed group of the Scene tab tree, in display order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SceneGroup {
    Camera,
    Lighting,
    Objects,
    Forces,
    Physics,
    World,
}

impl SceneGroup {
    pub const ORDER: [SceneGroup; 6] = [
        SceneGroup::Camera,
        SceneGroup::Lighting,
        SceneGroup::Objects,
        SceneGroup::Forces,
        SceneGroup::Physics,
        SceneGroup::World,
    ];

    pub fn title(self) -> &'static str {
        match self {
            SceneGroup::Camera => "Camera",
            SceneGroup::Lighting => "Lighting",
            SceneGroup::Objects => "Objects",
            SceneGroup::Forces => "Forces",
            SceneGroup::Physics => "Physics",
            SceneGroup::World => "World",
        }
    }

    pub fn of(kind: SceneItemKind) -> Self {
        match kind {
            SceneItemKind::Camera => SceneGroup::Camera,
            SceneItemKind::Light => SceneGroup::Lighting,
            SceneItemKind::Object(_) | SceneItemKind::Source(_) => SceneGroup::Objects,
            SceneItemKind::Force => SceneGroup::Forces,
            SceneItemKind::Physics => SceneGroup::Physics,
            SceneItemKind::World => SceneGroup::World,
        }
    }

    /// Camera, Lighting and World are one row each, with no fold header.
    fn is_single(self) -> bool {
        matches!(self, SceneGroup::Camera | SceneGroup::Lighting | SceneGroup::World)
    }

    fn has_add(self) -> bool {
        matches!(self, SceneGroup::Lighting | SceneGroup::Objects | SceneGroup::Forces)
    }
}

/// A row's trailing slot. Every row reserves the same width so names line
/// up; only objects carry a `visible` control to put in it.
enum EyeSlot {
    Live(RowValue),
    Empty,
}

fn eye_glyph(value: f32) -> &'static str {
    if value > 0.5 { "\u{1F441}" } else { "\u{2013}" }
}

impl ScenePanel {
    /// The outliner eyes' in-place push (D22): each object's `visible` base
    /// value, read at the row's write address, into the stored rows and the
    /// built eyes. A later build reads the same rows.
    pub fn sync_outliner_eyes(
        &mut self,
        tree: &mut UITree,
        value_of: &mut dyn FnMut(&RowAddr) -> Option<f32>,
    ) {
        if !self.open {
            return;
        }
        let SceneSetupState::Live(vm) = &mut self.state else { return };
        for object in &mut vm.objects {
            if let ObjectRowVm::Known(row) = object
                && let Some(value) = value_of(&row.visible.addr)
            {
                row.visible.value = value;
            }
        }
        for (eye_id, eye) in &mut self.outliner_eye_ids {
            let Some(value) = value_of(&eye.addr) else { continue };
            if (value > 0.5) != (eye.value > 0.5) {
                tree.set_text(*eye_id, eye_glyph(value));
            }
            eye.value = value;
        }
    }
}

const OUTLINER_KEY_BASE: u64 = 90_000_000;
const GROUP_HEADER_KEY_BASE: u64 = 93_000_000;
const GROUP_ADD_KEY_BASE: u64 = 93_000_100;
const KEY_PHYSICS_ENABLE: u64 = 93_000_200;

/// Stable row key from the selection identity: Camera and World are fixed,
/// everything else keys off its stable address.
fn outliner_row_key(sel: &SceneSelection) -> u64 {
    match sel {
        SceneSelection::Camera => OUTLINER_KEY_BASE,
        SceneSelection::World => OUTLINER_KEY_BASE + 1,
        SceneSelection::Lighting => OUTLINER_KEY_BASE + 2,
        SceneSelection::OutlinerFold(group) => GROUP_HEADER_KEY_BASE + *group as u64,
        SceneSelection::Light(id) => crate::param_surface::stable_key(&format!("scene.light.{}", ref_text(id))),
        SceneSelection::Object(id) => crate::param_surface::stable_key(&format!("scene.object.{}", ref_text(id))),
        SceneSelection::Force(id) => crate::param_surface::stable_key(&format!("scene.force.{id}")),
        SceneSelection::Physics(id) => crate::param_surface::stable_key(&format!("scene.physics.{id}")),
        SceneSelection::Source(id) => crate::param_surface::stable_key(&format!("scene.source.{id}")),
    }
}

/// The visibility eye of an object row: the whole hash, so two rows never share a widget.
pub(super) fn outliner_eye_key(object: &SceneNodeRef) -> u64 {
    crate::param_surface::stable_key(&format!("scene.eye.{}", ref_text(object)))
}

fn outliner_group_toggle_key(group: &SceneNodeRef) -> u64 {
    crate::param_surface::stable_key(&format!("scene.group_toggle.{}", ref_text(group)))
}

/// Selected rows take a low-alpha wash of the app-wide selection colour,
/// never a border box (the `layer_header.rs` precedent).
fn outliner_row_style(selected: bool) -> UIStyle {
    let ring = color::SELECTED_LAYER_RING;
    let sel_bg = Color32::new(ring.r, ring.g, ring.b, 40);
    let sel_hover = Color32::new(ring.r, ring.g, ring.b, 60);
    UIStyle {
        bg_color: if selected { sel_bg } else { Color32::TRANSPARENT },
        hover_bg_color: if selected { sel_hover } else { Color32::new(255, 255, 255, 18) },
        text_color: if selected { ring } else { Color32::new(200, 200, 208, 255) },
        font_size: color::FONT_LABEL,
        text_align: TextAlign::Left,
        corner_radius: color::SMALL_RADIUS,
        ..UIStyle::default()
    }
}

/// A group heading reads as a section heading, not an item: the fold header
/// built from `first_slot` takes the darker chrome band and a bold title,
/// while item rows stay transparent with regular grey text.
fn restyle_as_heading(tree: &mut UITree, first_slot: usize, title: &str) {
    for slot in first_slot..tree.count() {
        let id = tree.id_at(slot);
        let Some(node) = tree.get_node(id) else { continue };
        let mut style = node.style;
        if slot == first_slot {
            style.bg_color = color::HEADER_BG;
        } else if node.text.as_deref() == Some(title) {
            style.font_weight = FontWeight::Bold;
        } else {
            continue;
        }
        tree.set_style(id, style);
    }
}

fn icon_style(selected: bool) -> UIStyle {
    UIStyle {
        text_color: if selected { color::SELECTED_LAYER_RING } else { color::TEXT_DIMMED_C32 },
        font_size: color::FONT_LABEL,
        text_align: TextAlign::Center,
        ..UIStyle::default()
    }
}

impl ScenePanel {
    pub(super) fn build_outliner(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        mut cy: f32,
        vm: &SceneSetupVm,
        selected: &[SceneSelection],
    ) -> f32 {
        for group in SceneGroup::ORDER {
            // Lights are listed inside Lighting, never as rows of their own.
            let items: Vec<&SceneItemVm> = vm
                .items
                .iter()
                .filter(|item| SceneGroup::of(item.kind) == group && !matches!(item.selection, SceneSelection::Light(_)))
                .collect();
            if group.is_single() {
                for item in &items {
                    cy = self.build_tree_row(tree, (inner_x, inner_w, 0.0), cy, item, selected, EyeSlot::Empty);
                    if group.has_add() {
                        self.build_add_button(tree, (inner_x, inner_w), cy - ROW_H, group);
                    }
                }
                if !items.is_empty() {
                    self.tree_groups.push(group);
                }
                continue;
            }
            let custom_count = match group {
                SceneGroup::Objects => vm.objects.iter().filter(|o| matches!(o, ObjectRowVm::Custom { .. })).count(),
                _ => 0,
            };
            // Forces has an add button too, so it shows whenever a force can
            // be added; Physics always shows so its empty state can offer one.
            if group == SceneGroup::Forces && items.is_empty() && vm.force_picker.is_empty() {
                continue;
            }
            self.tree_groups.push(group);
            let folded = self.outliner_folded.get(&group).copied().unwrap_or(false);
            cy = self.build_group_header(tree, (inner_x, inner_w), cy, group, folded, items.len() + custom_count);
            if folded {
                continue;
            }
            cy = match group {
                SceneGroup::Objects => self.build_object_rows(tree, (inner_x, inner_w), cy, vm, &items, selected),
                SceneGroup::Physics if items.is_empty() => self.build_physics_empty(tree, inner_x, inner_w, cy, vm),
                _ => items.iter().fold(cy, |cy, item| {
                    self.build_tree_row(tree, (inner_x, inner_w, 0.0), cy, item, selected, EyeSlot::Empty)
                }),
            };
        }
        cy
    }

    /// The shared fold header, with the group's add button beside it.
    fn build_group_header(
        &mut self,
        tree: &mut UITree,
        (inner_x, inner_w): (f32, f32),
        cy: f32,
        group: SceneGroup,
        folded: bool,
        count: usize,
    ) -> f32 {
        let add_w = if group.has_add() { STEP_W + GAP } else { 0.0 };
        let first_slot = tree.count();
        let header = ParamCardPanel::build_section_header(
            tree,
            Some(self.content_parent),
            Rect::new(inner_x, cy, inner_w - add_w, ROW_H),
            group.title(),
            folded,
            count,
            outliner_row_key(&SceneSelection::OutlinerFold(group)),
        );
        tree.set_name(header, format!("scene_tree.group.{}", group.title()));
        restyle_as_heading(tree, first_slot, group.title());
        self.outliner_row_ids.push((header, SceneSelection::OutlinerFold(group)));
        if group.has_add() {
            self.build_add_button(tree, (inner_x, inner_w), cy, group);
        }
        cy + ROW_H
    }

    /// A group's add button, at the right end of the row at `cy`.
    fn build_add_button(&mut self, tree: &mut UITree, (inner_x, inner_w): (f32, f32), cy: f32, group: SceneGroup) {
        let add = tree.add_button_keyed(
            Some(self.content_parent),
            inner_x + inner_w - STEP_W,
            cy,
            STEP_W,
            ROW_H,
            btn_style(),
            "+",
            GROUP_ADD_KEY_BASE + group as u64,
        );
        tree.set_name(add, format!("scene_tree.add.{}", group.title()));
        match group {
            SceneGroup::Lighting => self.add_light_id = Some(add),
            SceneGroup::Objects => self.add_object_id = Some(add),
            _ => self.add_force_id = Some(add),
        }
    }

    /// Object rows as a tree: each row indented by its depth, and each group
    /// at any depth folding what is inside it under a chevron that never
    /// changes selection.
    fn build_object_rows(
        &mut self,
        tree: &mut UITree,
        (inner_x, inner_w): (f32, f32),
        mut cy: f32,
        vm: &SceneSetupVm,
        items: &[&SceneItemVm],
        selected: &[SceneSelection],
    ) -> f32 {
        let expanded = self.expanded_groups.get(&vm.layer_id).cloned().unwrap_or_default();
        for item in items {
            let SceneSelection::Object(id) = &item.selection else { continue };
            let Some(row) = vm.objects.iter().find_map(|object| match object {
                ObjectRowVm::Known(row) if row.node == *id => Some(row.as_ref()),
                _ => None,
            }) else {
                continue;
            };
            let eye = EyeSlot::Live(row.visible.clone());
            let Some(depth) = tree_depth(items, item, &expanded) else { continue };
            let depth_x = depth as f32 * STEP_W;
            let indent = if row.is_group {
                let open = expanded.contains(id);
                let toggle = tree.add_button_keyed(
                    Some(self.content_parent),
                    inner_x + depth_x,
                    cy,
                    STEP_W,
                    ROW_H,
                    btn_style(),
                    if open { "\u{25BE}" } else { "\u{25B8}" },
                    outliner_group_toggle_key(id),
                );
                tree.set_name(toggle, "scene_setup.objects.group_toggle");
                self.group_toggle_ids.push((toggle, id.clone()));
                depth_x + STEP_W
            } else {
                depth_x
            };
            cy = self.build_tree_row(tree, (inner_x, inner_w, indent), cy, item, selected, eye);
            // A water's Inflow, Drain and Collider rows sit under it after its
            // other children, at their indent, and fold with them (D-94).
            let owner = item.parent.clone().unwrap_or_else(|| id.clone());
            let child_of_owner =
                |other: &SceneItemVm| matches!(other.selection, SceneSelection::Object(_)) && other.parent.as_ref() == Some(&owner);
            let last_of_owner = match item.parent {
                Some(_) => !items
                    .iter()
                    .skip_while(|other| !std::ptr::eq(**other, *item))
                    .skip(1)
                    .any(|other| child_of_owner(other)),
                None => (!row.is_group || expanded.contains(id)) && !items.iter().any(|other| child_of_owner(other)),
            };
            if last_of_owner {
                for source in items.iter().filter(|source| matches!(source.selection, SceneSelection::Source(_)) && source.parent.as_ref() == Some(&owner)) {
                    cy = self.build_tree_row(tree, (inner_x, inner_w, STEP_W), cy, source, selected, EyeSlot::Empty);
                }
            }
        }
        for object in &vm.objects {
            if let ObjectRowVm::Custom { index } = object {
                let label = format!("Object {index} — custom (edit in graph)");
                cy = self.build_static_row(tree, (inner_x, inner_w), cy, SceneItemKind::Object(manifold_foundation::scene_surface::ObjectKind::Mesh), &label);
            }
        }
        cy
    }

    /// No physics world yet: say so, and offer to enable physics on the
    /// first object that can take it.
    fn build_physics_empty(&mut self, tree: &mut UITree, inner_x: f32, inner_w: f32, mut cy: f32, vm: &SceneSetupVm) -> f32 {
        let candidate = vm.objects.iter().find_map(|object| match object {
            ObjectRowVm::Known(row)
                if row.physics_available && !row.physics_enabled && (row.parent.is_none() || row.is_group) =>
            {
                Some(row.as_ref())
            }
            _ => None,
        });
        let sentence = if candidate.is_some() { "No physics in this scene." } else { "No physics. Add an object to use physics." };
        let label = tree.add_label(Some(self.content_parent), inner_x + STEP_W, cy, inner_w - STEP_W, ROW_H, sentence, label_style());
        tree.set_name(label, "scene_tree.physics.empty");
        cy += ROW_H;
        if let Some(row) = candidate {
            let button = tree.add_button_keyed(
                Some(self.content_parent),
                inner_x + STEP_W,
                cy,
                inner_w - STEP_W,
                ROW_H,
                btn_style(),
                &format!("Enable Physics on {}", row.name),
                KEY_PHYSICS_ENABLE,
            );
            tree.set_name(button, "scene_tree.physics.enable");
            self.physics_enable_id = Some((button, row.node.clone()));
            cy += ROW_H;
        }
        cy
    }

    /// One selectable row: `[icon | name | eye slot]`, indented by `indent`.
    fn build_tree_row(
        &mut self,
        tree: &mut UITree,
        (inner_x, inner_w, indent): (f32, f32, f32),
        cy: f32,
        item: &SceneItemVm,
        selected: &[SceneSelection],
        eye: EyeSlot,
    ) -> f32 {
        let is_selected = selected.contains(&item.selection);
        let x = inner_x + indent;
        let key = outliner_row_key(&item.selection);
        let icon = tree.add_label(
            Some(self.content_parent),
            x,
            cy,
            STEP_W,
            ROW_H,
            &scene_item_icon(item.kind).text(),
            icon_style(is_selected),
        );
        tree.set_name(icon, "scene_tree.icon");
        let row = tree.add_button_keyed(
            Some(self.content_parent),
            x + STEP_W,
            cy,
            (inner_w - indent - STEP_W * 2.0).max(0.0),
            ROW_H,
            outliner_row_style(is_selected),
            &item.name,
            key,
        );
        tree.set_name(row, "scene_tree.item");
        self.outliner_row_ids.push((row, item.selection.clone()));
        if let EyeSlot::Live(value) = eye {
            let SceneSelection::Object(id) = &item.selection else { return cy + ROW_H };
            let eye_id = tree.add_button_keyed(
                Some(self.content_parent),
                inner_x + inner_w - STEP_W,
                cy,
                STEP_W,
                ROW_H,
                if value.driven { driven_label_style() } else { btn_style() },
                eye_glyph(value.value),
                outliner_eye_key(id),
            );
            tree.set_name(eye_id, format!("scene_setup.object_eye.{}", value.addr.node_doc_id));
            if !value.driven {
                self.outliner_eye_ids.push((eye_id, value));
            }
        }
        cy + ROW_H
    }

    /// A listed row with no address to select (a custom producer).
    fn build_static_row(
        &mut self,
        tree: &mut UITree,
        (inner_x, inner_w): (f32, f32),
        cy: f32,
        kind: SceneItemKind,
        label: &str,
    ) -> f32 {
        tree.add_label(Some(self.content_parent), inner_x, cy, STEP_W, ROW_H, &scene_item_icon(kind).text(), icon_style(false));
        tree.add_label(Some(self.content_parent), inner_x + STEP_W, cy, inner_w - STEP_W * 2.0, ROW_H, label, label_style());
        cy + ROW_H
    }

    /// Tree clicks that are not selection: the group add buttons and the
    /// empty Physics group's enable action.
    pub(super) fn outliner_click(&self, node_id: NodeId, vm: &SceneSetupVm) -> Option<PanelAction> {
        let add = |group| Some(PanelAction::Root(RootAction::SceneSetupAddClicked(vm.layer_id.clone(), group)));
        if self.add_object_id == Some(node_id) {
            add(SceneGroup::Objects)
        } else if self.add_light_id == Some(node_id) {
            add(SceneGroup::Lighting)
        } else if self.add_force_id == Some(node_id) {
            add(SceneGroup::Forces)
        } else if let Some((_, object)) = self.physics_enable_id.as_ref().filter(|(id, _)| *id == node_id) {
            Some(PanelAction::Project(ProjectAction::SceneSetupEnablePhysics(
                vm.layer_id.clone(),
                object.clone(),
            )))
        } else {
            None
        }
    }

    /// The one Add menu for the live scene, starting on `group`'s entries;
    /// empty when not live.
    pub fn add_menu(&self, group: SceneGroup) -> Vec<crate::panels::scene_setup_actions::AddMenuItem> {
        let selected = self.live_selection();
        self.state.as_live().map(|vm| crate::panels::scene_setup_actions::add_menu(vm, group, selected)).unwrap_or_default()
    }

    /// The groups the tree drew last build, in order.
    pub fn tree_groups(&self) -> &[SceneGroup] {
        &self.tree_groups
    }
}

/// How deep `item` sits in the object tree: the number of groups above it.
/// `None` when a group above it is folded, so the row is hidden.
fn tree_depth(items: &[&SceneItemVm], item: &SceneItemVm, expanded: &ahash::AHashSet<SceneNodeRef>) -> Option<usize> {
    let mut depth = 0;
    let mut parent = item.parent.as_ref();
    while let Some(group) = parent {
        if !expanded.contains(group) || depth > items.len() {
            return None;
        }
        depth += 1;
        parent = items
            .iter()
            .find(|other| matches!(&other.selection, SceneSelection::Object(id) if id == group))
            .and_then(|other| other.parent.as_ref());
    }
    Some(depth)
}
