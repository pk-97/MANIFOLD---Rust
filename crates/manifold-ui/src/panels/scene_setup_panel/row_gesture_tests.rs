use super::*;
use crate::input::Modifiers;
use crate::panels::param_slider_shared::{AbletonMappingDisplay, AbletonMappingStatus};
use crate::panels::MappingAction;

fn fixture() -> (ScenePanel, UITree, ParamSurface) {
    let (vm, mut surface) = tests::world_transform_vm();
    surface.supports_envelopes = true;
    surface.rows[0].modulation.envelope_active = true;
    surface.rows[0].modulation.target_norm = 0.8;

    let mut panel = ScenePanel::new();
    panel.open();
    panel.configure(SceneSetupState::Live(Box::new(vm)));
    panel.configure_params(Some(surface.clone()));
    panel
        .selection
        .insert(LayerId::new("layer-1"), SceneSelection::World);
    let mut tree = UITree::new();
    panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 500.0, 1000.0));
    (panel, tree, surface)
}

fn rebuild(panel: &mut ScenePanel) -> UITree {
    let mut tree = UITree::new();
    panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 500.0, 1000.0));
    tree
}

fn center(tree: &UITree, node: NodeId) -> Vec2 {
    let bounds = tree.get_bounds(node);
    Vec2::new(bounds.x + bounds.width * 0.5, bounds.y + bounds.height * 0.5)
}

fn target_address() -> ValueRef {
    ValueRef::EnvelopeTarget(
        GraphParamTarget::GeneratorOf(LayerId::new("layer-1")),
        "translate_x".into(),
    )
}

#[test]
fn envelope_target_drag_survives_reorder_and_commits_once() {
    let (mut panel, mut tree, mut surface) = fixture();
    let target_id = panel.properties_card.row_host.target_ids[0]
        .as_ref()
        .expect("envelope target is rendered")
        .target_bar_id;
    let target_pos = center(&tree, target_id);
    assert_eq!(tree.hit_test(target_pos), Some(target_id));

    let (_, begin) = panel.handle_event(
        &UIEvent::PointerDown {
            node_id: target_id,
            pos: target_pos,
            modifiers: Modifiers::NONE,
        },
        &mut tree,
    );
    assert!(matches!(begin.as_slice(),
        [PanelAction::Scrub(address, ScrubPhase::Begin)]
        if address == &target_address()
    ));

    let track = panel.properties_card.row_host.slider_ids[0].unwrap().track;
    let track_rect = tree.get_bounds(track);
    let moved_norm = 0.35;
    let (_, move_before_rebuild) = panel.handle_event(
        &UIEvent::Drag {
            node_id: Some(target_id),
            pos: Vec2::new(track_rect.x + track_rect.width * moved_norm, track_rect.y),
            delta: Vec2::ZERO,
            modifiers: Modifiers::NONE,
        },
        &mut tree,
    );
    assert!(matches!(move_before_rebuild.as_slice(),
        [PanelAction::Scrub(address, ScrubPhase::Move(ScrubValue::Scalar(value)))]
        if address == &target_address() && (*value - moved_norm).abs() < 0.02
    ));

    let mut inserted = surface.rows[0].clone();
    inserted.id = "translate_y".into();
    inserted.modulation.envelope_active = false;
    surface.rows.insert(0, inserted);
    panel.configure_params(Some(surface));
    let mut rebuilt = rebuild(&mut panel);
    assert_eq!(panel.properties_card.rows[1].id.as_ref(), "translate_x");

    let retained_target = panel.properties_card.row_host.target_ids[1]
        .as_ref()
        .expect("reordered row keeps its envelope target")
        .target_bar_id;
    let retained_track = panel.properties_card.row_host.slider_ids[1].unwrap().track;
    let retained_track_rect = rebuilt.get_bounds(retained_track);
    let retained_target_rect = rebuilt.get_bounds(retained_target);
    let retained_norm = (retained_target_rect.x + retained_target_rect.width * 0.5
        - retained_track_rect.x)
        / retained_track_rect.width;
    assert!((retained_norm - moved_norm).abs() < 0.02, "live target was lost across reorder");

    let (_, move_after_rebuild) = panel.handle_event(
        &UIEvent::Drag {
            node_id: Some(retained_target),
            pos: Vec2::new(
                retained_track_rect.x + retained_track_rect.width * 0.65,
                retained_track_rect.y,
            ),
            delta: Vec2::ZERO,
            modifiers: Modifiers::NONE,
        },
        &mut rebuilt,
    );
    assert!(matches!(move_after_rebuild.as_slice(),
        [PanelAction::Scrub(address, ScrubPhase::Move(ScrubValue::Scalar(value)))]
        if address == &target_address() && (*value - 0.65).abs() < 0.02
    ));

    let (_, commit) = panel.handle_event(
        &UIEvent::DragEnd {
            node_id: None,
            pos: Vec2::ZERO,
        },
        &mut rebuilt,
    );
    assert!(matches!(commit.as_slice(), [PanelAction::Scrub(address, ScrubPhase::Commit)]
        if address == &target_address()));
    let (_, duplicate) = panel.handle_event(
        &UIEvent::PointerUp {
            node_id: None,
            pos: Vec2::ZERO,
        },
        &mut rebuilt,
    );
    assert!(duplicate.is_empty(), "release must commit exactly once");
}

#[test]
fn ableton_mapping_drawer_click_is_registered_and_typed() {
    let (mut panel, _, mut surface) = fixture();
    surface.rows[0].modulation.envelope_active = false;
    surface.rows[0].mapping.ableton_range = Some((0.2, 0.8));
    surface.rows[0].mapping.ableton_display = Some(AbletonMappingDisplay {
        macro_name: "Macro 1".into(),
        track_name: "Track".into(),
        device_name: "Device".into(),
        status: AbletonMappingStatus::Active,
        inverted: false,
    });
    panel.configure_params(Some(surface));
    let mut tree = rebuild(&mut panel);
    let drawer = panel.properties_card.row_host.ableton_config_ids[0]
        .as_ref()
        .expect("Ableton mapping drawer is rendered");
    let button = drawer.button_ids()[0];
    let pos = center(&tree, button);
    assert_eq!(tree.hit_test(pos), Some(button));
    let (consumed, actions) = panel.handle_event(
        &UIEvent::Click {
            node_id: button,
            pos,
            modifiers: Modifiers::NONE,
        },
        &mut tree,
    );
    assert!(consumed);
    assert!(matches!(actions.as_slice(),
        [PanelAction::Mapping(MappingAction::AbletonInvertToggle(target, id))]
        if *target == GraphParamTarget::GeneratorOf(LayerId::new("layer-1"))
            && id.as_ref() == "translate_x"
    ));
}
