use super::*;
use crate::input::Modifiers;
use crate::panels::TrimKind;
use crate::panels::param_slider_shared::{AudioSendChoice, AudioRowState, TrimHandleIds};

fn fixture(kind: TrimKind) -> (ScenePanel, UITree, ParamSurface) {
    let (vm, mut surface) = tests::world_transform_vm();
    match kind {
        TrimKind::Driver => {
            surface.rows[0].modulation.driver_active = true;
            surface.rows[0].modulation.trim_min = 0.2;
            surface.rows[0].modulation.trim_max = 0.8;
        }
        TrimKind::Ableton => surface.rows[0].mapping.ableton_range = Some((0.2, 0.8)),
        TrimKind::Audio => surface.rows[0].audio = crate::panels::param_slider_shared::AudioRowState {
            active: true,
            range_min: 0.2,
            range_max: 0.8,
            ..Default::default()
        },
    }
    let mut panel = ScenePanel::new();
    panel.open();
    panel.configure(SceneSetupState::Live(Box::new(vm)));
    panel.configure_params(Some(surface.clone()));
    panel
        .selection
        .insert(LayerId::new("layer-1"), SceneSelection::World);
    let tree = rebuild(&mut panel);
    (panel, tree, surface)
}

fn rebuild(panel: &mut ScenePanel) -> UITree {
    let mut tree = UITree::new();
    panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 500.0, 1000.0));
    tree
}

#[test]
fn filtered_scene_config_keeps_audio_with_reordered_visible_rows() {
    let (mut vm, mut surface) = tests::world_transform_vm();
    vm.world_sections = vec!["Transform".into()];
    let mut hidden = surface.rows[0].clone();
    hidden.id = manifold_foundation::ParamId::from("hidden");
    hidden.spec.section = Some("Hidden".into());
    hidden.audio = AudioRowState { active: true, send_id: Some(manifold_foundation::AudioSendId::new("hidden-send")), ..Default::default() };
    let mut first = surface.rows[0].clone();
    first.id = manifold_foundation::ParamId::from("first");
    first.spec.section = Some("Transform".into());
    first.audio = AudioRowState { active: true, send_id: Some(manifold_foundation::AudioSendId::new("first-send")), ..Default::default() };
    let mut second = surface.rows[0].clone();
    second.id = manifold_foundation::ParamId::from("second");
    second.spec.section = Some("Transform".into());
    second.audio = AudioRowState { active: true, send_id: Some(manifold_foundation::AudioSendId::new("second-send")), ..Default::default() };
    surface.rows = vec![hidden, second, first];
    surface.audio_sends = vec![
        AudioSendChoice { id: manifold_foundation::AudioSendId::new("first-send"), label: "First".into() },
        AudioSendChoice { id: manifold_foundation::AudioSendId::new("second-send"), label: "Second".into() },
    ];
    let mut panel = ScenePanel::new();
    panel.open();
    panel.configure(SceneSetupState::Live(Box::new(vm)));
    panel.configure_params(Some(surface));
    panel.selection.insert(LayerId::new("layer-1"), SceneSelection::World);
    let _tree = rebuild(&mut panel);
    assert_eq!(panel.properties_card.rows.iter().map(|row| row.id.as_ref()).collect::<Vec<_>>(), ["second", "first"]);
    assert_eq!(panel.properties_card.mod_state.audio_rows.iter().map(|row| row.send_id.as_ref().unwrap().as_str()).collect::<Vec<_>>(), ["second-send", "first-send"]);
}

fn handles(panel: &ScenePanel, kind: TrimKind, row: usize) -> TrimHandleIds {
    let host = &panel.properties_card.row_host;
    match kind {
        TrimKind::Driver => host.trim_ids[row],
        TrimKind::Ableton => host.ableton_trim_ids[row],
        TrimKind::Audio => host.audio_trim_ids[row],
    }
    .expect("every drawn trim pair must be retained for routing")
}

fn address(kind: TrimKind) -> ValueRef {
    ValueRef::Trim(
        kind,
        GraphParamTarget::GeneratorOf(LayerId::new("layer-1")),
        "translate_x".into(),
    )
}

fn press(panel: &mut ScenePanel, tree: &UITree, node_id: NodeId, kind: TrimKind) {
    let bounds = tree.get_bounds(node_id);
    let pos = Vec2::new(
        bounds.x + bounds.width * 0.5,
        bounds.y + bounds.height * 0.5,
    );
    assert_eq!(
        tree.hit_test(pos),
        Some(node_id),
        "trim bar must receive the pointer hit"
    );
    let (consumed, actions) = panel.handle_event(
        &UIEvent::PointerDown {
            node_id,
            pos,
            modifiers: Modifiers::NONE,
        },
        tree,
    );
    assert!(consumed);
    assert!(
        matches!(actions.as_slice(), [PanelAction::Scrub(value, ScrubPhase::Begin)] if *value == address(kind)),
        "{actions:?}"
    );
    let (consumed, actions) = panel.handle_event(
        &UIEvent::DragBegin {
            node_id: Some(node_id),
            pos,
            origin: pos,
            modifiers: Modifiers::NONE,
        },
        tree,
    );
    assert!(consumed, "trim must retain drag ownership");
    assert!(actions.is_empty());
}

fn move_to(
    panel: &mut ScenePanel,
    tree: &UITree,
    row: usize,
    norm: f32,
    kind: TrimKind,
) -> (f32, f32) {
    let track = panel.properties_card.row_host.slider_ids[row]
        .unwrap()
        .track;
    let rect = tree.get_bounds(track);
    let (consumed, actions) = panel.handle_event(
        &UIEvent::Drag {
            node_id: None,
            pos: Vec2::new(rect.x + rect.width * norm, rect.y),
            delta: Vec2::ZERO,
            modifiers: Modifiers::NONE,
        },
        tree,
    );
    assert!(consumed);
    match actions.as_slice() {
        [PanelAction::Scrub(value, ScrubPhase::Move(ScrubValue::Range(min, max)))]
            if *value == address(kind) =>
        {
            (*min, *max)
        }
        other => panic!("expected a trim move on its original address: {other:?}"),
    }
}

fn release(panel: &mut ScenePanel, tree: &UITree, kind: TrimKind, drag_end: bool) {
    let event = if drag_end {
        UIEvent::DragEnd {
            node_id: None,
            pos: Vec2::ZERO,
        }
    } else {
        UIEvent::PointerUp {
            node_id: None,
            pos: Vec2::ZERO,
        }
    };
    let (consumed, actions) = panel.handle_event(&event, tree);
    assert!(consumed);
    assert!(
        matches!(actions.as_slice(), [PanelAction::Scrub(value, ScrubPhase::Commit)] if *value == address(kind)),
        "{actions:?}"
    );
    let (_, again) = panel.handle_event(
        &UIEvent::PointerUp {
            node_id: None,
            pos: Vec2::ZERO,
        },
        tree,
    );
    assert!(again.is_empty(), "release must commit once");
}

#[test]
fn scene_trim_both_edges_dispatch_clamp_and_commit_for_every_kind() {
    for kind in [TrimKind::Driver, TrimKind::Audio, TrimKind::Ableton] {
        for is_min in [true, false] {
            let (mut panel, tree, _) = fixture(kind);
            let trim = handles(&panel, kind, 0);
            press(
                &mut panel,
                &tree,
                if is_min {
                    trim.min_bar_id
                } else {
                    trim.max_bar_id
                },
                kind,
            );
            let (min, max) = move_to(&mut panel, &tree, 0, if is_min { 0.4 } else { 0.6 }, kind);
            let expected = if is_min { (0.4, 0.8) } else { (0.2, 0.6) };
            assert!((min - expected.0).abs() < 0.02 && (max - expected.1).abs() < 0.02);
            assert_eq!(
                move_to(&mut panel, &tree, 0, if is_min { 2.0 } else { -1.0 }, kind),
                if is_min { (0.8, 0.8) } else { (0.2, 0.2) }
            );
            assert_eq!(
                move_to(&mut panel, &tree, 0, if is_min { -1.0 } else { 2.0 }, kind),
                if is_min { (0.0, 0.8) } else { (0.2, 1.0) }
            );
            release(&mut panel, &tree, kind, is_min);
        }
    }
}

#[test]
fn scene_trim_driver_proximity_uses_live_track_after_scroll() {
    let kind = TrimKind::Driver;
    let (mut panel, mut tree, _) = fixture(kind);
    for i in 0..tree.count() {
        let id = tree.id_at(i);
        let mut bounds = tree.get_bounds(id);
        bounds.y += 71.0;
        tree.set_bounds(id, bounds);
    }
    let track = panel.properties_card.row_host.slider_ids[0].unwrap().track;
    let bar = tree.get_bounds(handles(&panel, kind, 0).min_bar_id);
    let (_, actions) = panel.handle_event(
        &UIEvent::PointerDown {
            node_id: track,
            pos: Vec2::new(bar.x + bar.width * 0.5 + 5.0, bar.y),
            modifiers: Modifiers::NONE,
        },
        &tree,
    );
    assert!(
        matches!(actions.as_slice(), [PanelAction::Scrub(value, ScrubPhase::Begin)] if *value == address(kind)),
        "{actions:?}"
    );
    let (min, max) = move_to(&mut panel, &tree, 0, 0.45, kind);
    assert!((min - 0.45).abs() < 0.02 && max == 0.8);
    release(&mut panel, &tree, kind, true);
}

#[test]
fn scene_trim_rebuild_preserves_parameter_identity_and_range() {
    let kind = TrimKind::Driver;
    let (mut panel, tree, mut surface) = fixture(kind);
    let trim = handles(&panel, kind, 0);
    press(&mut panel, &tree, trim.min_bar_id, kind);
    move_to(&mut panel, &tree, 0, 0.4, kind);
    let mut another = surface.rows[0].clone();
    another.id = "translate_y".into();
    another.modulation.trim_max = 0.95;
    surface.rows.insert(0, another);
    panel.configure_params(Some(surface));
    let tree = rebuild(&mut panel);
    assert_eq!(panel.properties_card.rows[1].id.as_ref(), "translate_x");
    let bar = tree.get_bounds(handles(&panel, kind, 1).min_bar_id);
    let track = tree.get_bounds(panel.properties_card.row_host.slider_ids[1].unwrap().track);
    let drawn_min = (bar.x + bar.width * 0.5 - track.x) / track.width;
    assert!(
        (drawn_min - 0.4).abs() < 0.02,
        "rebuild must retain the live trim range"
    );
    let (min, max) = move_to(&mut panel, &tree, 1, 0.5, kind);
    assert!((min - 0.5).abs() < 0.02 && max == 0.8);
    release(&mut panel, &tree, kind, true);
}

#[test]
fn scene_trim_release_after_layer_change_commits_original_address() {
    let kind = TrimKind::Audio;
    let (mut panel, tree, surface) = fixture(kind);
    let trim = handles(&panel, kind, 0);
    press(&mut panel, &tree, trim.min_bar_id, kind);
    let (mut vm, _) = tests::world_transform_vm();
    vm.layer_id = LayerId::new("layer-2");
    panel.configure(SceneSetupState::Live(Box::new(vm)));
    panel.configure_params(Some(surface));
    panel
        .selection
        .insert(LayerId::new("layer-2"), SceneSelection::World);
    let tree = rebuild(&mut panel);
    let (_, actions) = panel.handle_event(
        &UIEvent::Drag {
            node_id: None,
            pos: Vec2::ZERO,
            delta: Vec2::ZERO,
            modifiers: Modifiers::NONE,
        },
        &tree,
    );
    assert!(
        actions.is_empty(),
        "a replacement layer must not supply drag geometry"
    );
    release(&mut panel, &tree, kind, false);
}
