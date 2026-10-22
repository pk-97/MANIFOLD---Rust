use super::*;
use crate::input::Modifiers;
use manifold_foundation::scene_surface::FieldPick;

fn item(id: &str, kind: ItemKind, name: &str, depth: usize, parent: Option<&str>) -> SceneItemVm {
    SceneItemVm {
        id: ItemId::new(id),
        kind,
        name: name.to_string(),
        depth,
        parent: parent.map(ItemId::new),
        has_children: false,
        parameter_ids: Vec::new(),
        fields: Vec::new(),
        material: None,
        removable: kind != ItemKind::World,
        plane: false,
        environment: false,
        copy_view: false,
        aim_sun_file: None,
        row_reasons: Vec::new(),
        modifiers: Vec::new(),
    }
}

fn physics_field() -> FieldRowVm {
    FieldRowVm {
        id: "physics".into(),
        label: "Physics".into(),
        section: ControlGroup::Body,
        control: FieldControlVm::Pick {
            current: "None".into(),
            options: vec![
                FieldOptionVm { label: "None".into(), pick: FieldPick::Choice("none".into()), checked: true },
                FieldOptionVm { label: "Body".into(), pick: FieldPick::Choice("body".into()), checked: false },
            ],
        },
        disabled: None,
    }
}

fn live_panel(fields: Vec<FieldRowVm>) -> (ScenePanel, UITree) {
    let mut cube = item("cube", ItemKind::Object, "Cube", 0, None);
    cube.fields = fields;
    let vm = SceneSetupVm {
        layer_id: LayerId::new("layer-1"),
        items: vec![item("world", ItemKind::World, "World", 0, None), cube],
        add_entries: vec![AddEntryVm { key: "object.cube".into(), group: "Object".into(), label: "Cube".into() }],
    };
    let surface = ParamSurface {
        kind: crate::ParamCardKind::Generator,
        title: "Scene".into(),
        collapsed: false,
        enabled: true,
        effect_index: 0,
        effect_id: manifold_foundation::EffectId::new(""),
        supports_envelopes: false,
        has_graph_mod: false,
        layer_id: Some(LayerId::new("layer-1")),
        rows: Vec::new(),
        string_params: Vec::new(),
        audio_sends: Vec::new(),
    };
    let mut panel = ScenePanel::new();
    panel.open();
    panel.configure(SceneSetupState::Live(Box::new(vm)));
    panel.configure_params(Some(surface));
    panel.set_selection(LayerId::new("layer-1"), SceneSelection::Item(ItemId::new("cube")));
    let mut tree = UITree::new();
    panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 900.0));
    (panel, tree)
}

fn click(panel: &mut ScenePanel, tree: &mut UITree, node_id: NodeId) -> Vec<PanelAction> {
    panel.handle_event(&UIEvent::Click { node_id, pos: Vec2::ZERO, modifiers: Modifiers::default() }, tree).1
}

/// A field row is an ordinary row of the properties card; a click on it
/// opens the shared menu, each option carrying the `SceneFieldSet` it sends.
#[test]
fn a_field_row_click_opens_a_menu_of_field_sets() {
    let (mut panel, mut tree) = live_panel(vec![physics_field()]);
    let button = panel.properties_card.row_host.toggle_ids[0].as_ref().expect("the field row is a button row").button_id;
    let actions = click(&mut panel, &mut tree, button);
    let [PanelAction::Root(RootAction::SceneSetupActionMenuClicked { items, .. })] = actions.as_slice() else {
        panic!("expected one menu, got {actions:?}");
    };
    assert_eq!(items.len(), 2);
    assert!(matches!(
        &items[1].action,
        Some(PanelAction::Project(ProjectAction::SceneFieldSet { item, field, pick: FieldPick::Choice(key), .. }))
            if item.as_str() == "cube" && field == "physics" && key == "body"
    ));
}

/// A disabled field row keeps its place but its click does nothing; the
/// reason rides the row's hover text.
#[test]
fn a_disabled_field_row_does_nothing_and_keeps_its_reason_on_hover() {
    let mut field = physics_field();
    field.disabled = Some("A group above is already a Body".into());
    let (mut panel, mut tree) = live_panel(vec![field]);
    let button = panel.properties_card.row_host.toggle_ids[0].as_ref().unwrap().button_id;
    assert!(click(&mut panel, &mut tree, button).is_empty());
    assert_eq!(tree.get_node(button).unwrap().tooltip.as_deref(), Some("A group above is already a Body"));
}
