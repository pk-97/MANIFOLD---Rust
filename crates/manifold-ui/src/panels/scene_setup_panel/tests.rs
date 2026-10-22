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
                FieldOptionVm { label: "None".into(), pick: FieldPick::Choice("none".into()), checked: true, disabled: None },
                FieldOptionVm { label: "Body".into(), pick: FieldPick::Choice("body".into()), checked: false, disabled: None },
            ],
        },
        disabled: None,
    }
}

fn live_panel(fields: Vec<FieldRowVm>) -> (ScenePanel, UITree) {
    let mut cube = item("cube", ItemKind::Object, "Cube", 0, None);
    cube.fields = fields;
    live_panel_with(cube)
}

fn live_panel_with(cube: SceneItemVm) -> (ScenePanel, UITree) {
    let vm = SceneSetupVm {
        layer_id: LayerId::new("layer-1"),
        items: vec![item("world", ItemKind::World, "World", 0, None), cube],
        add_entries: vec![AddEntryVm {
            key: "object.cube".into(),
            group: "Object".into(),
            label: "Cube".into(),
            top_level: true,
            holders: vec![ItemKind::Group],
            disabled: None,
        }],
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

/// A panel showing the World and two Modifier cards.
fn chain_panel() -> (ScenePanel, UITree) {
    let vm = SceneSetupVm {
        layer_id: LayerId::new("layer-1"),
        items: vec![
            item("world", ItemKind::World, "World", 0, None),
            item("bend", ItemKind::Modifier, "Bend", 0, None),
            item("twist", ItemKind::Modifier, "Twist", 0, None),
        ],
        add_entries: Vec::new(),
    };
    let mut panel = ScenePanel::new();
    panel.open();
    panel.configure(SceneSetupState::Live(Box::new(vm)));
    let mut tree = UITree::new();
    panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 900.0));
    (panel, tree)
}

fn drag_begin(node_id: Option<NodeId>, pos: Vec2) -> UIEvent {
    UIEvent::DragBegin { node_id, pos, origin: pos, modifiers: Modifiers::default() }
}

/// Closing the tab mid card drag ends the drag: the release the panel never
/// saw cannot reorder the chain when the tab is next open.
#[test]
fn closing_the_tab_mid_card_drag_drops_the_drag() {
    let (mut panel, mut tree) = chain_panel();
    let row = panel.chain_rows[0].0;
    let bounds = tree.get_bounds(row);
    let at = Vec2::new(bounds.x + bounds.width * 0.5, bounds.y + bounds.height * 0.5);
    assert!(panel.handle_event(&drag_begin(Some(row), at), &mut tree).0, "the card drag begins");
    panel.close();
    panel.open();
    panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 900.0));
    let (_, actions) = panel.handle_event(&UIEvent::DragEnd { node_id: None, pos: Vec2::new(at.x, 2000.0) }, &mut tree);
    assert!(
        !actions.iter().any(|action| matches!(action, PanelAction::Project(ProjectAction::SceneChainMove { .. }))),
        "a stale card drag reordered the chain: {actions:?}"
    );
}

/// The inline Material's structural rows on an Object card edit the
/// Material, not the Object that draws with it.
#[test]
fn an_inline_material_row_sets_the_material_not_its_owner() {
    let mut cube = item("cube", ItemKind::Object, "Cube", 0, None);
    let mut material_type = physics_field();
    material_type.id = "material_type".into();
    material_type.label = "Type".into();
    material_type.section = ControlGroup::Material;
    cube.material = Some(MaterialBlockVm {
        item: ItemId::new("cube-material"),
        name: "Material".into(),
        used_by: 1,
        features: Vec::new(),
        parameter_ids: Vec::new(),
        fields: vec![material_type],
        look: None,
        pbr: true,
    });
    let (mut panel, mut tree) = live_panel_with(cube);
    let button = panel.properties_card.row_host.toggle_ids.iter().flatten().next().expect("the Type row is a button row").button_id;
    let actions = click(&mut panel, &mut tree, button);
    let [PanelAction::Root(RootAction::SceneSetupActionMenuClicked { items, .. })] = actions.as_slice() else {
        panic!("expected one menu, got {actions:?}");
    };
    assert!(matches!(
        &items[0].action,
        Some(PanelAction::Project(ProjectAction::SceneFieldSet { item, field, .. })) if item.as_str() == "cube-material" && field == "material_type"
    ), "{:?}", items[0].action);
}

/// Every base scene names its cube "cube": unlinking one layer's cube scale
/// leaves another layer's cube linked.
#[test]
fn the_scale_link_belongs_to_one_layers_item() {
    let (mut panel, _tree) = live_panel(Vec::new());
    let cube = ItemId::new("cube");
    panel.properties_card.rows.push(panel.scale_link_row(&cube));
    let row = panel.properties_card.rows.len() - 1;
    assert!(panel.scale_link_click(row));
    assert!(!panel.scale_linked(&LayerId::new("layer-1"), &cube));
    assert!(panel.scale_linked(&LayerId::new("layer-2"), &cube), "another layer's cube keeps its own link");
}
