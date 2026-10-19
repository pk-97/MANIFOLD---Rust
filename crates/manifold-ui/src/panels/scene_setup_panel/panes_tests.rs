use super::super::tests::{azalea_shaped_vm, live, set_item_ids, world_transform_vm};
use super::*;
use crate::input::Modifiers;

/// Tall enough for the split to move 40px, short enough that both the tree
/// and World's 24 rows overflow their panes.
const RECT: Rect = Rect { x: 0.0, y: 0.0, width: 400.0, height: 400.0 };

fn built() -> (ScenePanel, UITree) {
    let (mut vm, mut surface) = world_transform_vm();
    let template = surface.rows[0].clone();
    let ids: Vec<String> = (0..24).map(|i| format!("row_{i}")).collect();
    surface.rows = ids
        .iter()
        .map(|id| ParamRow { id: id.clone().into(), ..template.clone() })
        .collect();
    set_item_ids(&mut vm, SceneSelection::World, &ids.iter().map(String::as_str).collect::<Vec<_>>());
    let mut panel = ScenePanel::new();
    panel.open();
    panel.configure(live(vm));
    panel.configure_params(Some(surface));
    panel.selection.insert(LayerId::new("layer-1"), SceneSelection::World);
    let tree = rebuild(&mut panel);
    (panel, tree)
}

fn rebuild(panel: &mut ScenePanel) -> UITree {
    let mut tree = UITree::new();
    panel.build_docked(&mut tree, RECT);
    tree
}

fn first_row(panel: &ScenePanel, tree: &UITree) -> Rect {
    tree.get_bounds(panel.outliner_row_ids[0].0)
}

fn inside(outer: Rect, r: Rect) -> bool {
    r.y >= outer.y && r.y + r.height <= outer.y + outer.height
}

#[test]
fn scrolling_the_settings_leaves_the_tree_in_view() {
    let (mut panel, tree) = built();
    let row = first_row(&panel, &tree);
    assert!(inside(panel.panes.tree.viewport(), row));
    assert!(panel.scroll.can_scroll(), "the fixture's settings must overflow their pane");

    assert!(panel.handle_scroll(-10_000.0));
    let tree = rebuild(&mut panel);
    assert!(panel.scroll.scroll_offset() > 0.0);
    assert_eq!(first_row(&panel, &tree), row, "the tree row must not move when the settings scroll");
}

#[test]
fn the_wheel_scrolls_the_pane_under_the_pointer() {
    let (mut panel, tree) = built();
    assert!(panel.panes.tree.can_scroll(), "the fixture's tree must overflow its pane at this height");
    let over_tree = panel.panes.tree.viewport();
    let row = panel.outliner_row_ids[0].0;
    assert_eq!(panel.scroll_viewport_of(&tree, Some(row)), over_tree, "a tree row scrolls in the tree pane");
    let setting = tree.all_nodes().find(|n| tree.name_of(n.id).is_some_and(|s| s.starts_with("param_row.row_0"))).map(|n| n.id);
    assert!(setting.is_some());
    assert_eq!(panel.scroll_viewport_of(&tree, setting), panel.scroll.viewport());
    let pos = Vec2::new(over_tree.x + 20.0, over_tree.y + 5.0);

    assert!(panel.handle_scroll_at(pos, -10_000.0));
    assert!(panel.panes.tree.scroll_offset() > 0.0);
    assert_eq!(panel.scroll.scroll_offset(), 0.0, "the settings pane must not scroll");

    let over_settings = panel.scroll.viewport();
    let pos = Vec2::new(over_settings.x + 20.0, over_settings.y + 5.0);
    let tree_offset = panel.panes.tree.scroll_offset();
    assert!(panel.handle_scroll_at(pos, -10_000.0));
    assert!(panel.scroll.scroll_offset() > 0.0);
    assert_eq!(panel.panes.tree.scroll_offset(), tree_offset);
}

#[test]
fn dragging_the_split_resizes_both_panes_and_a_double_click_resets_it() {
    let (mut panel, mut tree) = built();
    let tree_h = panel.panes.tree.viewport().height;
    let settings_h = panel.scroll.viewport().height;
    let strip = panel.panes.strip_id.expect("a live scene builds the split");
    let at = panel.panes.strip;
    let start = Vec2::new(at.x + 50.0, at.y + 2.0);
    assert!(panel.split_cursor(start));
    let modifiers = Modifiers::default();

    let (consumed, _) = panel.handle_event(&UIEvent::PointerDown { node_id: strip, pos: start, modifiers }, &mut tree);
    assert!(consumed);
    let to = Vec2::new(start.x, start.y + 40.0);
    let drag = UIEvent::Drag { node_id: None, pos: to, delta: Vec2::new(0.0, 40.0), modifiers };
    let version = panel.view_version();
    assert!(panel.handle_event(&drag, &mut tree).0);
    assert_ne!(panel.view_version(), version, "a drag is a planner input");
    assert!(panel.handle_event(&UIEvent::DragEnd { node_id: None, pos: to }, &mut tree).0);
    let mut tree = rebuild(&mut panel);
    assert!((panel.panes.tree.viewport().height - (tree_h + 40.0)).abs() < 0.5);
    assert!((panel.scroll.viewport().height - (settings_h - 40.0)).abs() < 0.5);

    let strip = panel.panes.strip_id.expect("split");
    let pos = Vec2::new(panel.panes.strip.x + 50.0, panel.panes.strip.y + 2.0);
    assert!(panel.handle_event(&UIEvent::DoubleClick { node_id: strip, pos, modifiers }, &mut tree).0);
    rebuild(&mut panel);
    assert_eq!(panel.panes.tree.viewport().height, tree_h);
}

#[test]
fn a_short_tree_takes_only_its_rows() {
    let mut panel = ScenePanel::new();
    panel.open();
    panel.configure(live(azalea_shaped_vm()));
    let mut tree = UITree::new();
    panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 2000.0));
    assert!(!panel.panes.tree.can_scroll());
    assert!(panel.panes.tree.viewport().height < 2000.0 * DEFAULT_SPLIT);
}

#[test]
fn an_empty_state_has_one_column_and_no_split() {
    let mut panel = ScenePanel::new();
    panel.open();
    panel.configure(SceneSetupState::NoSelection("Select a layer.".to_string()));
    let mut tree = UITree::new();
    panel.build_docked(&mut tree, RECT);
    assert!(panel.panes.strip_id.is_none());
    assert!(!panel.split_cursor(Vec2::new(10.0, 10.0)));
    let anywhere = Vec2::new(20.0, 40.0);
    assert_eq!(panel.scroll_viewport_of(&tree, None), panel.scroll.viewport());
    assert!(!panel.handle_scroll_at(anywhere, -10.0), "nothing to scroll in a one-line sentence");
}
