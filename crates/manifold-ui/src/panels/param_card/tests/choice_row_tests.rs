//! Pick-list and switch rows: the kind each declared row draws as, their
//! modulation lane, the Scene tab hosts and the right-click mapping menu.

use super::*;

/// `gen_config` with row 0 a three-way choice (Sun, Point, Spot; showing
/// Spot) and row 2 the Off/On float a light's Cast Shadows is.
fn gen_config_with_choice_and_float_switch() -> ParamSurface {
    let mut c = gen_config();
    let choice = &mut c.rows[0];
    choice.spec.whole_numbers = true;
    (choice.spec.min, choice.spec.max, choice.spec.default) = (0.0, 2.0, 2.0);
    choice.spec.value_labels = Some(vec!["Sun".into(), "Point".into(), "Spot".into()]);
    let switch = &mut c.rows[2];
    (switch.spec.min, switch.spec.max, switch.spec.default) = (0.0, 1.0, 1.0);
    switch.spec.value_labels = Some(vec!["Off".into(), "On".into()]);
    c
}

/// The one kind decision (`ParamRow::kind`): what each declared kind
/// draws as on a scene surface and on any other card.
#[test]
fn row_kind_follows_the_declared_kind() {
    use crate::param_surface::RowKind;
    let c = gen_config_with_choice_and_float_switch();
    let (choice, toggle, float_switch) = (&c.rows[0], &c.rows[1], &c.rows[2]);
    assert_eq!(choice.kind(true), RowKind::Choice);
    assert_eq!(float_switch.kind(true), RowKind::Switch);
    assert_eq!(toggle.kind(true), RowKind::Switch);
    assert_eq!(toggle.kind(false), RowKind::Switch, "a toggle is a switch on every card");
    assert_eq!(choice.kind(false), RowKind::Number, "off a scene a labelled row keeps its slider");
    assert_eq!(float_switch.kind(false), RowKind::Number);
    let mut trigger = toggle.clone();
    (trigger.spec.is_toggle, trigger.spec.is_trigger) = (false, true);
    assert_eq!(trigger.kind(true), RowKind::Trigger);
    let mut driven = choice.clone();
    driven.modulation.driver_active = true;
    assert_eq!(driven.kind(true), RowKind::Number, "a driven choice keeps the slider that shows its driver");
    let mut three_label_float = float_switch.clone();
    three_label_float.spec.value_labels = Some(vec!["Low".into(), "Mid".into(), "High".into()]);
    assert_eq!(three_label_float.kind(true), RowKind::Number);
    assert!(!choice.grows_drawer(true) && !float_switch.grows_drawer(true));
    assert!(choice.grows_drawer(false), "the slider it stays off a scene keeps its drawer");
}

/// A scene card draws a choice as a pick list showing its label and an
/// Off/On float as the switch; a pick-list click opens the shared
/// dropdown at the current label, a switch click toggles, and the value
/// sync redraws both. The same rows on an ordinary card stay sliders.
#[test]
fn scene_card_draws_choices_as_pick_lists_and_switches() {
    use crate::param_surface::RowKind;
    let config = gen_config_with_choice_and_float_switch();
    let rect = Rect::new(0.0, 0.0, 340.0, 300.0);

    let mut tree = UITree::new();
    let mut ordinary = ParamCardPanel::new();
    ordinary.configure(&config);
    ordinary.build(&mut tree, rect);
    assert!(ordinary.row_host.slider_ids[0].is_some() && ordinary.row_host.toggle_ids[0].is_none());
    assert!(ordinary.row_host.slider_ids[2].is_some() && ordinary.row_host.toggle_ids[2].is_none());

    let mut tree = UITree::new();
    let mut panel = ParamCardPanel::new();
    panel.configure(&config);
    panel.set_scene_sections(Some(&[]));
    panel.set_host(CardHost::SceneCard);
    panel.build(&mut tree, rect);
    let text = |tree: &UITree, id| tree.get_node(id).unwrap().text.clone();

    assert!(panel.row_host.slider_ids[0].is_none());
    let pick = panel.row_host.toggle_ids[0].as_ref().expect("the choice is a button row");
    let pick_button = pick.button_id;
    assert_eq!(pick.kind, RowKind::Choice);
    assert_eq!(text(&tree, pick_button).as_deref(), Some("Spot"));
    assert_eq!(tree.name_of(pick_button), Some("param_row.speed"));

    let switch = panel.row_host.toggle_ids[2].as_ref().expect("the Off/On float is a button row");
    let switch_button = switch.button_id;
    assert_eq!(switch.kind, RowKind::Switch);
    assert_eq!(text(&tree, switch_button).as_deref(), Some("ON"));

    use crate::view::UiParamSlot as Slot;
    let choice_slot = Slot { min: 0.0, max: 2.0, ..Slot::exposed(1.0) };
    panel.sync_values_positional(&mut tree, &[choice_slot, Slot::exposed(1.0), Slot::exposed(0.0)]);
    assert_eq!(text(&tree, pick_button).as_deref(), Some("Point"));
    assert_eq!(text(&tree, switch_button).as_deref(), Some("OFF"));

    match panel.handle_click(pick_button, &tree).as_slice() {
        [PanelAction::Root(RootAction::ParamEnumDropdown { param_id, labels, current_index, cell_node_id, .. })] => {
            assert_eq!(param_id.as_ref(), "speed");
            assert_eq!(labels, &["Sun", "Point", "Spot"]);
            assert_eq!(*current_index, 1, "checked at the live value, Point");
            assert_eq!(*cell_node_id, pick_button, "the list opens under the pick list");
        }
        other => panic!("expected the shared dropdown, got {other:?}"),
    }

    match panel.handle_click(switch_button, &tree).as_slice() {
        [PanelAction::Params(ParamsAction::ParamToggle(_, param_id))] => assert_eq!(param_id.as_ref(), "scale"),
        other => panic!("expected ParamToggle, got {other:?}"),
    }
}

/// A pick list on a scene-hosted card (an inspector modifier card, the
/// scene card) keeps the slider's T, D and A lane: each arms its
/// modulator, and the attached row draws as the slider whose drawer
/// shows it.
#[test]
fn pick_list_rows_keep_their_modulation_lane() {
    use crate::param_surface::RowKind;
    let mut config = gen_config_with_choice_and_float_switch();
    let rect = Rect::new(0.0, 0.0, 340.0, 300.0);
    let build = |config: &ParamSurface| {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(config);
        panel.set_host(CardHost::SceneCard);
        panel.build(&mut tree, rect);
        (panel, tree)
    };
    let (mut panel, tree) = build(&config);
    assert_eq!(panel.row_host.toggle_ids[0].as_ref().map(|ids| ids.kind), Some(RowKind::Choice));
    let driver = panel.row_host.driver_btn_ids[0].expect("the pick list has a D button");
    assert_eq!(tree.name_of(driver), Some("param_row.speed.driver_btn"));
    match panel.handle_click(driver, &tree).as_slice() {
        [PanelAction::Modulation(ModulationAction::DriverToggle(_, id))] => assert_eq!(id.as_ref(), "speed"),
        other => panic!("expected DriverToggle, got {other:?}"),
    }
    let envelope = panel.row_host.envelope_btn_ids[0].expect("the pick list has a T button");
    assert!(matches!(panel.handle_click(envelope, &tree).as_slice(),
        [PanelAction::Modulation(ModulationAction::EnvelopeToggle(_, id))] if id.as_ref() == "speed"));
    assert!(panel.row_host.audio_btn_ids[0].is_some(), "the pick list has an A button");
    assert!(panel.row_host.driver_btn_ids[2].is_none(), "a switch keeps no lane");

    config.rows[0].modulation.driver_active = true;
    let (panel, _) = build(&config);
    assert!(panel.row_host.slider_ids[0].is_some(), "the driven row is the slider that shows its driver");
    assert!(config.rows[0].grows_drawer(true));
}

/// Every Scene tab card draws by kind, through the effect body the real
/// cards build with: an object modifier card (Bend's Axis) and a force
/// card show the pick list and the switch, and a pick opens the shared
/// dropdown aimed at the owning layer's generator. An inspector modifier
/// card, same rows and same target, keeps its sliders.
#[test]
fn scene_tab_cards_draw_choices_as_pick_lists() {
    use crate::param_surface::RowKind;
    let mut config = gen_config_with_choice_and_float_switch();
    config.kind = ParamCardKind::Effect;
    config.modifier = Some(crate::param_surface::ModifierCardInfo {
        instance_id: manifold_foundation::NodeId::new("modifier-1"),
        layer_id: LayerId::new("layer-1"),
        params_owner: Default::default(),
        enabled_label: "Enabled".into(),
        stack_index: 0,
        stack_len: 1,
        targets_all: true,
        objects: Vec::new(),
    });
    let rect = Rect::new(0.0, 0.0, 340.0, 300.0);
    let built = |card: &mut ParamCardPanel| {
        let mut tree = UITree::new();
        card.build(&mut tree, rect);
        let kinds = [0, 2].map(|row| card.row_host.toggle_ids[row].as_ref().map(|ids| ids.kind));
        (tree, kinds)
    };
    let layer = GraphParamTarget::GeneratorOf(LayerId::new("layer-1"));

    let mut object_modifier = ParamCardPanel::new();
    let mut object_config = config.clone();
    object_config.modifier = None;
    object_modifier.configure_object_modifier(
        &object_config,
        crate::param_surface::ObjectModifierCardInfo {
            layer_id: LayerId::new("layer-1"),
            object: manifold_foundation::SceneNodeRef { scope: Vec::new(), node: manifold_foundation::NodeId::new("object7") },
            owner: manifold_foundation::SceneNodeRef { scope: Vec::new(), node: manifold_foundation::NodeId::new("group42") },
            node_doc_id: 70,
        },
    );
    object_modifier.set_host(CardHost::SceneTab);
    let (tree, kinds) = built(&mut object_modifier);
    assert_eq!(kinds, [Some(RowKind::Choice), Some(RowKind::Switch)]);
    let pick = object_modifier.row_host.toggle_ids[0].as_ref().unwrap().button_id;
    match object_modifier.handle_click(pick, &tree).as_slice() {
        [PanelAction::Root(RootAction::ParamEnumDropdown { target, param_id, .. })] => {
            assert_eq!(target, &layer, "the pick writes the owning layer's generator");
            assert_eq!(param_id.as_ref(), "speed");
        }
        other => panic!("expected the shared dropdown, got {other:?}"),
    }

    let mut force = ParamCardPanel::new();
    force.configure(&config);
    force.set_item_hosted(true);
    force.set_host(CardHost::SceneTab);
    assert_eq!(built(&mut force).1, [Some(RowKind::Choice), Some(RowKind::Switch)]);

    let mut inspector = ParamCardPanel::new();
    inspector.configure(&config);
    assert_eq!(inspector.param_target(), layer);
    assert_eq!(built(&mut inspector).1, [None, None], "an inspector modifier card keeps its sliders");
    assert!(inspector.row_host.slider_ids[0].is_some() && inspector.row_host.slider_ids[2].is_some());
}

/// A switch row and a pick-list row keep the right-click mapping menu
/// their slider had: label and button both open `ParamLabelRightClick`
/// for that row's param, the menu a slider row's label opens.
#[test]
fn button_rows_open_the_mapping_menu_on_right_click() {
    let mut tree = UITree::new();
    let mut panel = ParamCardPanel::new();
    panel.configure(&gen_config_with_choice_and_float_switch());
    panel.set_scene_sections(Some(&[]));
    panel.set_host(CardHost::SceneCard);
    panel.build(&mut tree, Rect::new(0.0, 0.0, 340.0, 300.0));
    let mut reg = crate::intent::IntentRegistry::new();
    panel.register_intents(&mut reg);
    let menu_for = |node| match reg.resolve(&tree, Some(node), crate::intent::Gesture::RightClick) {
        Some(PanelAction::Params(ParamsAction::ParamLabelRightClick(_, param_id))) => Some(param_id.to_string()),
        _ => None,
    };
    for (row, param) in [(0, "speed"), (1, "invert"), (2, "scale")] {
        let ids = panel.row_host.toggle_ids[row].as_ref().expect("a button row");
        assert_eq!(menu_for(ids.button_id).as_deref(), Some(param), "{param} button");
        assert_eq!(menu_for(ids.label_id.expect("label")).as_deref(), Some(param), "{param} label");
    }
}
