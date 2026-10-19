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
    assert!(panel.split_cursor(start, RECT));
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
    assert!(!panel.split_cursor(Vec2::new(10.0, 10.0), RECT));
    let anywhere = Vec2::new(20.0, 40.0);
    assert_eq!(panel.scroll_viewport_of(&tree, None), panel.scroll.viewport());
    assert!(!panel.handle_scroll_at(anywhere, -10.0), "nothing to scroll in a one-line sentence");
}

#[test]
fn a_drag_held_at_the_clamp_rebuilds_nothing() {
    let (mut panel, mut tree) = built();
    let strip = panel.panes.strip_id.expect("a live scene builds the split");
    let at = panel.panes.strip;
    let start = Vec2::new(at.x + 50.0, at.y + 2.0);
    let modifiers = Modifiers::default();
    panel.handle_event(&UIEvent::PointerDown { node_id: strip, pos: start, modifiers }, &mut tree);
    let drag = |y: f32| UIEvent::Drag { node_id: None, pos: Vec2::new(start.x, y), delta: Vec2::ZERO, modifiers };

    // Far above the tree: the pane is at its smallest.
    let version = panel.view_version();
    assert!(panel.handle_event(&drag(start.y - 300.0), &mut tree).0);
    assert_ne!(panel.view_version(), version, "the first move shrinks the tree and holds the strip");
    let tree_h = panel.panes.tree_h(panel.panes.split);
    assert_eq!(tree_h, MIN_TREE_H);

    let held = panel.view_version();
    for y in [start.y - 320.0, start.y - 400.0, start.y - 250.0] {
        assert!(panel.handle_event(&drag(y), &mut tree).0);
    }
    assert_eq!(panel.view_version(), held, "moves that leave the panes as drawn are not planner input");
    assert_eq!(panel.panes.tree_h(panel.panes.split), tree_h);

    panel.handle_event(&UIEvent::DragEnd { node_id: None, pos: Vec2::new(start.x, start.y - 400.0) }, &mut tree);
    assert_ne!(panel.view_version(), held, "the release drops the held colour");
}

#[test]
fn a_hidden_scene_tab_never_shows_the_resize_cursor() {
    let (panel, _tree) = built();
    let at = panel.panes.strip;
    let over = Vec2::new(at.x + 50.0, at.y + 2.0);
    assert!(panel.split_cursor(over, RECT));
    // The tab is hidden: the inspector reports no Scene rect, and the strip
    // rect from the last build is stale.
    assert!(!panel.split_cursor(over, Rect::ZERO));
    // A Scene rect that no longer covers the old strip.
    assert!(!panel.split_cursor(over, Rect::new(0.0, 0.0, 400.0, at.y - 1.0)));
}
