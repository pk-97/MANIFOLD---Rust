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
        draw_again: false,
        add_loop_camera: false,
        row_reasons: Vec::new(),
        body: false,
        look: None,
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
        header: false,
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
        items: vec![item("world", ItemKind::World, "World", 0, None), cube.clone()],
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

/// A tree of a Group and a cube beside it.
fn tree_panel() -> (ScenePanel, UITree) {
    let mut rig = item("rig", ItemKind::Group, "Rig", 0, None);
    rig.has_children = false;
    let vm = SceneSetupVm {
        layer_id: LayerId::new("layer-1"),
        items: vec![item("world", ItemKind::World, "World", 0, None), rig, item("cube", ItemKind::Object, "Cube", 0, None)],
        add_entries: Vec::new(),
    };
    let mut panel = ScenePanel::new();
    panel.open();
    panel.configure(SceneSetupState::Live(Box::new(vm)));
    let mut tree = UITree::new();
    panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 900.0));
    (panel, tree)
}

fn tree_row(panel: &ScenePanel, id: &str) -> NodeId {
    let item = SceneSelection::Item(ItemId::new(id));
    panel.outliner_row_ids.iter().find(|(_, sel)| *sel == item).expect("the item has a row").0
}

/// Dragging a row onto the middle of a Group's row moves it into the
/// Group; onto a row's top half, before that row.
#[test]
fn a_tree_row_drag_moves_the_item_into_a_group_or_beside_a_row() {
    let (mut panel, mut tree) = tree_panel();
    let (cube, rig) = (tree_row(&panel, "cube"), tree_row(&panel, "rig"));
    let from = tree.get_bounds(cube);
    let target = tree.get_bounds(rig);
    for (at, parent, before) in [(0.5, Some("rig"), None), (0.1, None, Some("rig"))] {
        let start = Vec2::new(from.x + 4.0, from.y + from.height * 0.5);
        assert!(panel.handle_event(&drag_begin(Some(cube), start), &mut tree).0, "the row drag begins");
        let drop = Vec2::new(target.x + 4.0, target.y + target.height * at);
        let (_, actions) = panel.handle_event(&UIEvent::DragEnd { node_id: None, pos: drop }, &mut tree);
        let [PanelAction::Project(ProjectAction::SceneTreeMove { item, parent: to, before: ahead, .. })] = actions.as_slice() else {
            panic!("expected one move, got {actions:?}");
        };
        assert_eq!(item.as_str(), "cube");
        assert_eq!(to.as_ref().map(ItemId::as_str), parent);
        assert_eq!(ahead.as_ref().map(ItemId::as_str), before);
    }
    // Releasing in the panel below the last row moves the item to the end
    // of the top level; releasing outside the panel (over the viewport)
    // cancels.
    let start = Vec2::new(from.x + 4.0, from.y + from.height * 0.5);
    assert!(panel.handle_event(&drag_begin(Some(cube), start), &mut tree).0);
    let (_, actions) = panel.handle_event(&UIEvent::DragEnd { node_id: None, pos: Vec2::new(from.x + 4.0, 890.0) }, &mut tree);
    assert!(
        matches!(actions.as_slice(), [PanelAction::Project(ProjectAction::SceneTreeMove { parent: None, before: None, .. })]),
        "a drop below the rows goes to the end of the top level, got {actions:?}"
    );
    let drop = Vec2::new(600.0, target.y + target.height * 0.5);
    assert!(panel.handle_event(&drag_begin(Some(cube), start), &mut tree).0);
    let (_, actions) = panel.handle_event(&UIEvent::DragEnd { node_id: None, pos: drop }, &mut tree);
    assert!(actions.is_empty(), "a drop at {drop:?} moves nothing, got {actions:?}");
}

/// Double-clicking a tree row renames its item, the name field over the row.
#[test]
fn a_tree_row_double_click_renames_in_place() {
    let (mut panel, mut tree) = tree_panel();
    let row = tree_row(&panel, "cube");
    let event = UIEvent::DoubleClick { node_id: row, pos: Vec2::ZERO, modifiers: Modifiers::default() };
    let (_, actions) = panel.handle_event(&event, &mut tree);
    let [PanelAction::Root(RootAction::SceneRenameClicked(_, item, name))] = actions.as_slice() else {
        panic!("expected a rename, got {actions:?}");
    };
    assert_eq!((item.as_str(), name.as_str()), ("cube", "Cube"));
    let anchor = panel.item_name_rect(&tree, item).expect("an anchor");
    let bounds = tree.get_bounds(row);
    assert_eq!((anchor.x, anchor.y), (bounds.x, bounds.y), "the name field opens over the row");
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

/// A structural field row is no manifest param: right-clicking it opens no
/// param menu, so it can never be mapped (ruling 3).
#[test]
fn a_field_row_opens_no_param_menu() {
    let (panel, tree) = live_panel(vec![physics_field()]);
    let ids = panel.properties_card.row_host.toggle_ids.iter().flatten().next().expect("the Physics row is a button row");
    let mut intents = crate::intent::IntentRegistry::new();
    panel.register_intents(&mut intents);
    for node in std::iter::once(ids.button_id).chain(ids.label_id) {
        let action = intents.resolve(&tree, Some(node), crate::intent::Gesture::RightClick);
        assert!(
            !matches!(action, Some(PanelAction::Params(ParamsAction::ParamLabelRightClick(..)))),
            "a field row opened the param menu: {action:?}"
        );
    }
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

fn switch_field(id: &str, label: &str, on: bool) -> FieldRowVm {
    FieldRowVm {
        id: id.into(),
        label: label.into(),
        section: ControlGroup::Copies,
        control: FieldControlVm::Switch { on, toggle: FieldPick::Choice(if on { "off" } else { "on" }.into()) },
        disabled: None,
        header: false,
    }
}

/// An on/off field is a switch: a click sends its flip at once, no menu.
#[test]
fn an_on_off_field_is_a_switch_that_flips() {
    let (mut panel, mut tree) = live_panel(vec![switch_field("copies", "Copies", false)]);
    let ids = panel.properties_card.row_host.toggle_ids[0].as_ref().expect("the switch is a button row");
    assert_eq!(ids.kind, crate::param_surface::RowKind::Switch);
    let button = ids.button_id;
    let actions = click(&mut panel, &mut tree, button);
    assert!(
        matches!(actions.as_slice(), [PanelAction::Project(ProjectAction::SceneFieldSet { field, pick: FieldPick::Choice(key), .. })]
            if field == "copies" && key == "on"),
        "{actions:?}"
    );
}

/// A Modifier's Type and On ride its header, not its rows.
#[test]
fn a_modifier_header_carries_its_type_and_switch() {
    let mut card = item("cube", ItemKind::Modifier, "Bend", 0, None);
    let mut kind = physics_field();
    kind.id = "modifier_type".into();
    kind.header = true;
    let mut on = switch_field("on", "On", true);
    on.header = true;
    card.fields = vec![kind, on];
    let (mut panel, mut tree) = live_panel_with(card);
    assert!(panel.properties_card.rows.is_empty(), "no Type or On row on the card");
    let switch = named(&tree, "scene_setup.properties.header_field.on").expect("the header switch");
    let actions = click(&mut panel, &mut tree, switch);
    assert!(
        matches!(actions.as_slice(), [PanelAction::Project(ProjectAction::SceneFieldSet { field, pick: FieldPick::Choice(key), .. })]
            if field == "on" && key == "off"),
        "{actions:?}"
    );
    assert!(named(&tree, "scene_setup.properties.header_field.modifier_type").is_some());
}

fn add_entry(key: &str, group: &str, label: &str, refused: bool) -> AddEntryVm {
    AddEntryVm {
        key: key.into(),
        group: group.into(),
        label: label.into(),
        top_level: true,
        holders: vec![ItemKind::Group],
        disabled: refused.then(|| "Turn on Copies to move them".into()),
    }
}

fn add_menu(panel: &mut ScenePanel, tree: &mut UITree, section: &str) -> Vec<crate::panels::dropdown::DropdownItem> {
    let button = named(tree, &format!("scene_tree.add.{section}")).expect("the section has a +");
    let actions = click(panel, tree, button);
    let [PanelAction::Root(RootAction::SceneSetupActionMenuClicked { items, .. })] = actions.as_slice() else {
        panic!("expected one menu, got {actions:?}");
    };
    items.clone()
}

/// Each section's + offers only what belongs there, and a refused entry
/// says why (SCENE_REFINEMENT_DESIGN rulings 2 and 4).
#[test]
fn each_section_adds_its_own_and_says_why_one_is_refused() {
    let cube = item("cube", ItemKind::Object, "Cube", 0, None);
    let vm = |refused: bool| SceneSetupVm {
        layer_id: LayerId::new("layer-1"),
        items: vec![item("world", ItemKind::World, "World", 0, None), cube.clone()],
        add_entries: vec![
            add_entry("object.cube", "Object", "Cube", false),
            add_entry("group", "Group", "Group", false),
            add_entry("light.sun", "Light", "Sun", false),
            add_entry("modifier.bend", "Modifier", "Bend", false),
            add_entry("modifier.copy_wave", "Modifier", "Copy Wave", refused),
        ],
    };
    let (mut panel, mut tree) = live_panel_with(cube.clone());
    panel.configure(SceneSetupState::Live(Box::new(vm(true))));
    tree.clear();
    panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 900.0));
    assert!(named(&tree, "scene_tree.add").is_none(), "the top Add is gone");
    let labels = |items: &[crate::panels::dropdown::DropdownItem]| items.iter().map(|item| item.label.clone()).collect::<Vec<_>>();
    assert_eq!(labels(&add_menu(&mut panel, &mut tree, "Objects")), ["Cube", "Group"]);
    // A menu of one thing is no choice: the + adds it at the top.
    let lighting = named(&tree, "scene_tree.add.Lighting").expect("Lighting has a +");
    let actions = click(&mut panel, &mut tree, lighting);
    assert!(
        matches!(actions.as_slice(), [PanelAction::Project(ProjectAction::SceneAdd { entry, parent: None, .. })] if entry == "light.sun"),
        "{actions:?}"
    );
    let modifiers = add_menu(&mut panel, &mut tree, "Modifiers");
    assert_eq!(labels(&modifiers), ["Bend", "Copy Wave \u{2014} Turn on Copies to move them"]);
    assert!(!modifiers[1].enabled);
    panel.configure(SceneSetupState::Live(Box::new(vm(false))));
    tree.clear();
    panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 900.0));
    let modifiers = add_menu(&mut panel, &mut tree, "Modifiers");
    assert!(modifiers[1].enabled && modifiers[1].label == "Copy Wave", "{:?}", modifiers[1].label);
}

/// Lighting lines up with every other top-level row, and a Material shows
/// as its own row only when two or more items use it.
#[test]
fn lighting_lines_up_and_only_shared_materials_are_rows() {
    let material = |id: &str, used_by: usize| {
        let mut row = item(id, ItemKind::Material, id, 0, None);
        row.material = Some(MaterialBlockVm {
            item: ItemId::new(id),
            name: id.into(),
            used_by,
            features: Vec::new(),
            parameter_ids: Vec::new(),
            fields: Vec::new(),
            look: None,
            pbr: true,
        });
        row
    };
    let vm = SceneSetupVm {
        layer_id: LayerId::new("layer-1"),
        items: vec![
            item("world", ItemKind::World, "World", 0, None),
            item("sun", ItemKind::Light, "Sun", 0, None),
            item("cube", ItemKind::Object, "Cube", 0, None),
            material("own", 1),
            material("shared", 2),
        ],
        add_entries: Vec::new(),
    };
    let mut panel = ScenePanel::new();
    panel.open();
    panel.configure(SceneSetupState::Live(Box::new(vm)));
    let mut tree = UITree::new();
    panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 900.0));
    let row_x = |sel: SceneSelection| tree.get_bounds(panel.outliner_row_ids.iter().find(|(_, row)| *row == sel).expect("a row").0).x;
    assert_eq!(row_x(SceneSelection::Lighting), row_x(SceneSelection::Item(ItemId::new("world"))));
    let has_row = |id: &str| panel.outliner_row_ids.iter().any(|(_, sel)| *sel == SceneSelection::Item(ItemId::new(id)));
    assert!(!has_row("own"), "a Material one item uses shows only on that item's card");
    assert!(has_row("shared"));
}

/// Dragging a Modifier card draws a line where it will land.
#[test]
fn a_card_drag_shows_where_it_lands() {
    let (mut panel, mut tree) = chain_panel();
    let (bend, twist) = (panel.chain_rows[0].0, panel.chain_rows[1].0);
    let from = tree.get_bounds(bend);
    let below = tree.get_bounds(twist);
    let start = Vec2::new(from.x + 4.0, from.y + from.height * 0.5);
    assert!(panel.handle_event(&drag_begin(Some(bend), start), &mut tree).0);
    let past = Vec2::new(start.x, below.y + below.height);
    let version = panel.view_version();
    panel.handle_event(&UIEvent::Drag { node_id: None, pos: past, delta: Vec2::ZERO, modifiers: Modifiers::default() }, &mut tree);
    assert!(panel.view_version() > version, "a new landing place redraws");
    tree.clear();
    panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 900.0));
    let line = named(&tree, "scene_tree.drop_line").expect("the landing line");
    let twist_now = tree.get_bounds(panel.chain_rows[1].0);
    assert_eq!(tree.get_bounds(line).y + 1.0, twist_now.y + twist_now.height, "the line sits under the last card");
    panel.handle_event(&UIEvent::DragEnd { node_id: None, pos: past }, &mut tree);
    tree.clear();
    panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 900.0));
    assert!(named(&tree, "scene_tree.drop_line").is_none(), "the line leaves with the drag");
}

/// The live node registered under `name`, in any region.
fn named(tree: &UITree, name: &str) -> Option<NodeId> {
    (0..=u8::MAX).find_map(|region| {
        (0..).map(|slot| tree.id_in(region, slot)).take_while(|id| *id != NodeId::PLACEHOLDER).find(|id| tree.name_of(*id) == Some(name))
    })
}
