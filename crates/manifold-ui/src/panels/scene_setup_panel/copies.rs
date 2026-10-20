//! The object card's copy count (PHYSICS_ENGINE_BOUNDARY_DESIGN.md P6a):
//! "Copies [−] N [+] [Duplicate]" makes N copies in one undo step, each with
//! its own body when the source has physics. The header's copy button stays
//! one copy. When the source's Physics World has fewer free body slots than
//! N, Duplicate is dimmed and the reason shows under the row.
//!
//! The count is UI-local, like Maya's Duplicate Special: it is remembered
//! across objects and never saved.

use super::*;
use manifold_foundation::scene_surface::MAX_DUPLICATE_COPIES;

const OBJ_OFF_COPIES_LESS: u64 = 41;
const OBJ_OFF_COPIES_MORE: u64 = 42;
const OBJ_OFF_COPIES_RUN: u64 = 43;
const RUN_LABEL: &str = "Duplicate";

/// The copy count and the buttons built this frame.
pub(super) struct CopyCount {
    count: u32,
    less: Option<NodeId>,
    more: Option<NodeId>,
    run: Option<(NodeId, usize)>,
}

impl Default for CopyCount {
    fn default() -> Self {
        Self { count: 1, less: None, more: None, run: None }
    }
}

impl CopyCount {
    pub(super) fn clear(&mut self) {
        self.less = None;
        self.more = None;
        self.run = None;
    }
}

/// Why `count` copies cannot be made, when the world is short of slots.
fn shortfall(count: u32, free: Option<u32>) -> Option<String> {
    let free = free.filter(|free| *free < count)?;
    let slots = if free == 1 { "slot" } else { "slots" };
    Some(format!("The Physics World has {free} free body {slots}; {count} copies need {count}"))
}

impl ScenePanel {
    /// The copies row for a top-level object or group that can be copied.
    pub(super) fn build_copy_count(
        &mut self,
        tree: &mut UITree,
        (inner_x, inner_w): (f32, f32),
        mut cy: f32,
        row: &ObjectKnownRow,
    ) -> f32 {
        if !row.capabilities.duplicate || (row.parent_group_id.is_some() && !row.is_group) {
            return cy;
        }
        let count = self.copy_count.count;
        let run_w = tree.text_width(RUN_LABEL, color::FONT_LABEL, crate::node::FontWeight::Regular) + 16.0;
        let value_w = STEP_W * 1.5;
        let controls_w = STEP_W * 2.0 + value_w + GAP + run_w;
        let key = |offset| obj_key(row.object_node_id as usize, offset);
        tree.add_label(Some(self.content_parent), inner_x, cy, inner_w - controls_w - GAP, ROW_H, "Copies", label_style());
        let mut x = inner_x + inner_w - controls_w;
        let less = tree.add_button_keyed(Some(self.content_parent), x, cy, STEP_W, ROW_H, btn_style(), "\u{2212}", key(OBJ_OFF_COPIES_LESS));
        tree.set_name(less, "scene_setup.properties.copies_less");
        x += STEP_W;
        let value_style = UIStyle { text_align: TextAlign::Center, ..label_style() };
        let value = tree.add_label(Some(self.content_parent), x, cy, value_w, ROW_H, &count.to_string(), value_style);
        tree.set_name(value, "scene_setup.properties.copies");
        x += value_w;
        let more = tree.add_button_keyed(Some(self.content_parent), x, cy, STEP_W, ROW_H, btn_style(), "+", key(OBJ_OFF_COPIES_MORE));
        tree.set_name(more, "scene_setup.properties.copies_more");
        x += STEP_W + GAP;
        self.copy_count.less = (count > 1).then_some(less);
        self.copy_count.more = (count < MAX_DUPLICATE_COPIES).then_some(more);
        let reason = shortfall(count, row.physics_free_body_slots);
        let run = if reason.is_none() {
            let id = tree.add_button_keyed(Some(self.content_parent), x, cy, run_w, ROW_H, btn_style(), RUN_LABEL, key(OBJ_OFF_COPIES_RUN));
            self.copy_count.run = Some((id, row.index));
            id
        } else {
            let style = UIStyle { text_align: TextAlign::Center, ..driven_label_style() };
            tree.add_label(Some(self.content_parent), x, cy, run_w, ROW_H, RUN_LABEL, style)
        };
        tree.set_name(run, "scene_setup.properties.copies_run");
        cy += ROW_H;
        if let Some(reason) = reason {
            let max_chars = ((inner_w / 6.5).floor() as usize).max(8);
            for line in crate::graph_canvas::wrap_text(&reason, max_chars) {
                let id = tree.add_label(Some(self.content_parent), inner_x, cy, inner_w, ROW_H, &line, label_style());
                tree.set_name(id, "scene_setup.properties.copies_reason");
                cy += ROW_H;
            }
        }
        cy
    }

    /// Steps the count (a planner input, so the row redraws) or runs the
    /// duplicate.
    pub(super) fn copy_count_click(&mut self, node_id: NodeId) -> Option<Vec<PanelAction>> {
        let step = if self.copy_count.less == Some(node_id) {
            -1
        } else if self.copy_count.more == Some(node_id) {
            1
        } else {
            let (_, index) = self.copy_count.run.filter(|(button, _)| *button == node_id)?;
            let vm = self.state.as_live()?;
            return Some(vec![PanelAction::Project(ProjectAction::SceneSetupDuplicateObject(
                vm.layer_id.clone(),
                vm.scene_root_node_id,
                index as u32,
                self.copy_count.count,
            ))]);
        };
        self.copy_count.count = self.copy_count.count.saturating_add_signed(step).clamp(1, MAX_DUPLICATE_COPIES);
        self.view_version += 1;
        Some(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::tests::{azalea_shaped_vm, live};
    use crate::input::Modifiers;
    use crate::node::Vec2;

    fn named(tree: &UITree, name: &str) -> Vec<NodeId> {
        (0..tree.count()).map(|i| tree.id_at(i)).filter(|&id| tree.name_of(id) == Some(name)).collect()
    }

    fn object_card(free_slots: Option<u32>) -> (ScenePanel, UITree) {
        let mut vm = azalea_shaped_vm();
        let mut selected = None;
        for object in &mut vm.objects {
            if let ObjectRowVm::Known(row) = object
                && row.parent_group_id.is_none()
                && row.capabilities.duplicate
            {
                row.physics_free_body_slots = free_slots;
                selected.get_or_insert(row.object_node_id);
            }
        }
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(live(vm));
        panel.selection.insert(LayerId::new("layer-1"), SceneSelection::Object(selected.expect("a copyable object")));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 1400.0));
        (panel, tree)
    }

    fn click(panel: &mut ScenePanel, tree: &mut UITree, name: &str) -> Vec<PanelAction> {
        let id = named(tree, name)[0];
        let (consumed, actions) = panel.handle_event(
            &UIEvent::Click { node_id: id, pos: Vec2::ZERO, modifiers: Modifiers::default() },
            tree,
        );
        assert!(consumed, "{name} takes the click");
        *tree = UITree::new();
        panel.build_docked(tree, Rect::new(0.0, 0.0, 400.0, 1400.0));
        actions
    }

    fn shown_count(tree: &UITree) -> String {
        let value = named(tree, "scene_setup.properties.copies")[0];
        tree.get_node(value).unwrap().text.clone().unwrap()
    }

    #[test]
    fn copies_step_and_duplicate_sends_the_count() {
        let (mut panel, mut tree) = object_card(None);
        assert_eq!(shown_count(&tree), "1");
        click(&mut panel, &mut tree, "scene_setup.properties.copies_more");
        click(&mut panel, &mut tree, "scene_setup.properties.copies_more");
        assert_eq!(shown_count(&tree), "3");
        click(&mut panel, &mut tree, "scene_setup.properties.copies_less");
        assert_eq!(shown_count(&tree), "2");
        let actions = click(&mut panel, &mut tree, "scene_setup.properties.copies_run");
        assert!(matches!(actions.as_slice(),
            [PanelAction::Project(ProjectAction::SceneSetupDuplicateObject(layer, _, _, 2))]
                if *layer == LayerId::new("layer-1")));
    }

    #[test]
    fn copies_beyond_free_body_slots_dim_duplicate_with_the_reason() {
        let (mut panel, mut tree) = object_card(Some(1));
        assert!(panel.copy_count.run.is_some(), "one copy fits one free slot");
        assert!(named(&tree, "scene_setup.properties.copies_reason").is_empty());
        click(&mut panel, &mut tree, "scene_setup.properties.copies_more");
        assert!(panel.copy_count.run.is_none(), "two copies cannot fit");
        let reason = named(&tree, "scene_setup.properties.copies_reason");
        let text: Vec<String> = reason.iter().map(|id| tree.get_node(*id).unwrap().text.clone().unwrap()).collect();
        assert_eq!(text.join(" "), "The Physics World has 1 free body slot; 2 copies need 2");
    }

    #[test]
    fn copies_stay_within_one_and_the_world_size() {
        assert_eq!(shortfall(3, None), None);
        assert_eq!(shortfall(3, Some(3)), None);
        let (mut panel, _) = object_card(None);
        assert!(panel.copy_count.less.is_none(), "one copy is the floor");
        panel.copy_count.count = MAX_DUPLICATE_COPIES;
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 1400.0));
        assert!(panel.copy_count.more.is_none(), "one copy per body slot is the ceiling");
    }
}
