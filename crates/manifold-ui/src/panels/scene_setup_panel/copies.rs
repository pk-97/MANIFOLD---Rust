//! The object card's Make Copies action (copy_group_design.md section 5):
//! one button turns the object into a copy group, and the row says why when
//! it cannot. Once the object is a copy group the row is gone and its card
//! carries Count, Layout and Spacing.

use super::*;

const OBJ_OFF_MAKE_COPIES: u64 = 41;
const RUN_LABEL: &str = "Make Copies";

/// The Make Copies button built this frame.
#[derive(Default)]
pub(super) struct MakeCopies {
    run: Option<(NodeId, SceneNodeRef)>,
}

impl MakeCopies {
    pub(super) fn clear(&mut self) {
        self.run = None;
    }
}

impl ScenePanel {
    /// The Make Copies row for a top-level object or group that can become
    /// a copy group, or the reason it cannot.
    pub(super) fn build_make_copies(
        &mut self,
        tree: &mut UITree,
        (inner_x, inner_w): (f32, f32),
        mut cy: f32,
        row: &ObjectKnownRow,
    ) -> f32 {
        let top_level = row.parent.is_none() || row.is_group;
        if row.copy_group || !top_level || !(row.copies_available || row.copies_unavailable_reason.is_some()) {
            return cy;
        }
        let run_w = tree.text_width(RUN_LABEL, color::FONT_LABEL, crate::node::FontWeight::Regular) + 16.0;
        let ordinal = self.row_key(&row.node);
        if row.copies_available {
            tree.add_label(Some(self.content_parent), inner_x, cy, inner_w - run_w - GAP, ROW_H, "Copies", label_style());
            let id = tree.add_button_keyed(
                Some(self.content_parent), inner_x + inner_w - run_w, cy, run_w, ROW_H, btn_style(), RUN_LABEL,
                obj_key(ordinal, OBJ_OFF_MAKE_COPIES),
            );
            tree.set_name(id, "scene_setup.properties.make_copies");
            self.make_copies.run = Some((id, row.node.clone()));
            return cy + ROW_H;
        }
        tree.add_label(Some(self.content_parent), inner_x, cy, inner_w, ROW_H, "Copies", driven_label_style());
        cy += ROW_H;
        if let Some(reason) = row.copies_unavailable_reason.as_deref() {
            let max_chars = ((inner_w / 6.5).floor() as usize).max(8);
            for line in crate::graph_canvas::wrap_text(reason, max_chars) {
                let id = tree.add_label(Some(self.content_parent), inner_x, cy, inner_w, ROW_H, &line, label_style());
                tree.set_name(id, "scene_setup.properties.copies_reason");
                cy += ROW_H;
            }
        }
        cy
    }

    /// Runs Make Copies when its button is clicked.
    pub(super) fn make_copies_click(&mut self, node_id: NodeId) -> Option<Vec<PanelAction>> {
        let (_, object) = self.make_copies.run.clone().filter(|(button, _)| *button == node_id)?;
        let vm = self.state.as_live()?;
        Some(vec![PanelAction::Project(ProjectAction::SceneSetupMakeCopies(vm.layer_id.clone(), object))])
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{azalea_shaped_vm, live};
    use super::*;
    use crate::input::Modifiers;
    use crate::node::Vec2;

    fn named(tree: &UITree, name: &str) -> Vec<NodeId> {
        (0..tree.count()).map(|i| tree.id_at(i)).filter(|&id| tree.name_of(id) == Some(name)).collect()
    }

    fn object_card(edit: impl Fn(&mut ObjectKnownRow)) -> (ScenePanel, UITree) {
        let mut vm = azalea_shaped_vm();
        let mut selected = None;
        for object in &mut vm.objects {
            if let ObjectRowVm::Known(row) = object
                && row.parent.is_none()
                && row.capabilities.duplicate
            {
                edit(row);
                selected.get_or_insert(row.node.clone());
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

    #[test]
    fn make_copies_button_sends_the_object() {
        let (mut panel, mut tree) = object_card(|row| row.copies_available = true);
        let button = named(&tree, "scene_setup.properties.make_copies");
        assert_eq!(button.len(), 1, "an object that can make copies shows the button");
        let (consumed, actions) = panel.handle_event(
            &UIEvent::Click { node_id: button[0], pos: Vec2::ZERO, modifiers: Modifiers::default() },
            &mut tree,
        );
        assert!(consumed);
        assert!(matches!(actions.as_slice(),
            [PanelAction::Project(ProjectAction::SceneSetupMakeCopies(layer, object))]
                if *layer == LayerId::new("layer-1") && *object == panel_object(&panel)));
    }

    fn panel_object(panel: &ScenePanel) -> SceneNodeRef {
        panel.make_copies.run.clone().expect("the button was built").1
    }

    #[test]
    fn an_object_that_cannot_make_copies_says_why_and_has_no_button() {
        let reason = "Copies take single-mesh objects; this import has 3 parts";
        let (_, tree) = object_card(|row| row.copies_unavailable_reason = Some(reason.to_string()));
        assert!(named(&tree, "scene_setup.properties.make_copies").is_empty(), "no button");
        let lines: Vec<String> = named(&tree, "scene_setup.properties.copies_reason")
            .iter()
            .map(|id| tree.get_node(*id).unwrap().text.clone().unwrap())
            .collect();
        assert_eq!(lines.join(" "), reason);
    }

    #[test]
    fn a_copy_group_has_no_make_copies_row() {
        let (_, tree) = object_card(|row| {
            row.copy_group = true;
            row.copies_available = true;
        });
        assert!(named(&tree, "scene_setup.properties.make_copies").is_empty());
        assert!(named(&tree, "scene_setup.properties.copies_reason").is_empty());
    }
}
