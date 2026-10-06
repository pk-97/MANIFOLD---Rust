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
    let (mut project, layer_id, _) = super::water_family::water_project();
    let old_size = project.timeline.layers[0].gen_params().unwrap().get_base_param("foam_size");
    let mut editing = manifold_editing::service::EditingService::new();
    editing.execute(Box::new(manifold_editing::commands::effects::ChangeGraphParamCommand::new(
        manifold_core::GraphTarget::Generator(layer_id.clone()),
        "foam_size", old_size, 0.01,
    )), &mut project);
    assert_eq!(editing.take_rejection(), None);
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

    // The failed flow clicked Foam after scrolling down to Whitewater Amount.
    // Selector lookup can find an offscreen node; pointer hit-testing cannot.
    assert!(ui.scene_setup_panel.handle_scroll(-10_000.0));
    let scrolled_tree = build_scene_tree(&mut ui);
    let foam = scrolled_tree.nodes().iter().find(|node| node.text.as_deref() == Some("■ Foam")).unwrap();
    let rect = scrolled_tree.get_bounds(foam.id);
    let center = manifold_ui::Vec2::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
    assert_ne!(scrolled_tree.hit_test(center), Some(foam.id), "clipped Foam cannot receive the click");

    for (name, size_id) in [("Foam", "foam_size"), ("Spray", "spray_size"), ("Bubbles", "bubble_size")] {
        // ScrollTo in the repaired flow brings the outliner target back into view.
        ui.scene_setup_panel.handle_scroll(10_000.0);
        let mut tree = build_scene_tree(&mut ui);
        let label = format!("■ {name}");
        let row = tree.nodes().iter().find(|node| node.text.as_deref() == Some(&label)).unwrap().id;
        let rect = tree.get_bounds(row);
        let center = manifold_ui::Vec2::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
        assert_eq!(tree.hit_test(center), Some(row), "{name} is reachable after scrolling");
        let (consumed, _) = ui.scene_setup_panel.handle_event(&manifold_ui::UIEvent::Click {
            node_id: row, pos: center, modifiers: manifold_ui::Modifiers::default(),
        }, &mut tree);
        assert!(consumed);
        let tree = build_scene_tree(&mut ui);
        let header = tree.nodes().iter().find(|node| tree.name_of(node.id) == Some("scene_setup.properties.name_value")).unwrap();
        assert_eq!(header.text.as_deref(), Some(name));
        let suffix = format!("{size_id}.value");
        let value = tree.nodes().iter().find(|node| tree.name_of(node.id).is_some_and(|id| id.ends_with(&suffix)))
            .expect("selected look renders its manifest-backed Size in Properties");
        if name == "Foam" { assert_eq!(value.text.as_deref(), Some("0.01")); }
        assert!(tree.nodes().iter().any(|node| node.text.as_deref() == Some("Size")));
        assert!(!tree.nodes().iter().any(|node| node.text.as_deref() == Some("Whitewater Amount")));
    }
}
