//! CPU proof for the real Water family's outliner expansion path.

use super::*;

fn build_scene_tree(ui: &mut crate::ui_root::UIRoot) -> manifold_ui::UITree {
    let mut tree = manifold_ui::UITree::new();
    let rect = manifold_ui::Rect::new(0.0, 0.0, 400.0, 1200.0);
    let region = tree.begin_region(
        rect,
        manifold_ui::ZTier::Base,
        "scene_setup",
        manifold_ui::UIFlags::empty(),
    );
    let content_start = tree.count();
    ui.scene_setup_panel.build_docked(&mut tree, rect);
    tree.end_region(region, content_start);
    tree
}

fn tree_texts(tree: &manifold_ui::UITree) -> Vec<String> {
    tree.nodes()
        .iter()
        .filter_map(|node| node.text.clone())
        .collect()
}

#[test]
fn water_family_group_toggle_expands_children_and_preserves_parent_controls() {
    let (project, layer_id, _) = super::water_family::water_project();
    let (_, _state, mut ui, mut selection, _active_layer, _prefs) = dispatch_harness();

    ui.scene_setup_panel.open();
    selection.select_layer(layer_id.clone());
    crate::ui_bridge::projection::inspector::sync_inspector_data(
        &mut ui,
        &project,
        Some(0),
        &selection,
        &[],
        None,
    );

    let mut collapsed_tree = build_scene_tree(&mut ui);
    let toggle_id = collapsed_tree
        .nodes()
        .iter()
        .find_map(|node| {
            (collapsed_tree.name_of(node.id) == Some("scene_setup.objects.group_toggle"))
                .then_some(node.id)
        })
        .expect("Water parent group toggle");
    let collapsed_texts = tree_texts(&collapsed_tree);
    assert!(collapsed_texts.iter().any(|text| text == "■ Water"));
    for child in ["■ Foam", "■ Spray", "■ Bubbles"] {
        assert!(
            !collapsed_texts.iter().any(|text| text == child),
            "collapsed: {child}"
        );
    }

    let selected_before = ui
        .scene_setup_panel
        .selected_scene_item()
        .expect("Water parent is selected by default");
    let (consumed, actions) = ui.scene_setup_panel.handle_event(
        &manifold_ui::UIEvent::Click {
            node_id: toggle_id,
            pos: manifold_ui::Vec2::ZERO,
            modifiers: manifold_ui::Modifiers::default(),
        },
        &mut collapsed_tree,
    );
    assert!(consumed);
    assert!(matches!(
        actions.as_slice(),
        [manifold_ui::PanelAction::Params(
            manifold_ui::ParamsAction::SectionFoldToggled
        )]
    ));

    let expanded_tree = build_scene_tree(&mut ui);
    let expanded_texts = tree_texts(&expanded_tree);
    for child in ["■ Foam", "■ Spray", "■ Bubbles"] {
        assert!(
            expanded_texts.iter().any(|text| text == child),
            "expanded: {child}"
        );
    }

    let selected_after = ui
        .scene_setup_panel
        .selected_scene_item()
        .expect("parent selection survives expansion");
    assert_eq!(selected_before.layer_id, selected_after.layer_id);
    assert_eq!(selected_before.scene, selected_after.scene);
    assert_eq!(selected_before.index, selected_after.index);
    assert_eq!(selected_before.is_light, selected_after.is_light);

    // Expansion changes only the outliner. The parent's own section and gate
    // controls remain rendered for the same selected Water parent.
    for label in ["Resolution", "Whitewater Amount", "Visible"] {
        assert!(
            collapsed_texts.iter().any(|text| text == label),
            "collapsed parent control missing: {label}"
        );
        assert!(
            expanded_texts.iter().any(|text| text == label),
            "expanded parent control missing: {label}"
        );
    }
}
