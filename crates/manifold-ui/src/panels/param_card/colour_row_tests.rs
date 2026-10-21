//! The colour row on the scene card (`param_slider_shared::colour_row`).

use super::*;
use crate::param_surface::{ColourGroup, ParamSurface, RgbMembers, RowMapping, RowSpec, RowValue};
use crate::view::UiParamSlot;
use manifold_foundation::{EffectId, ParamId};

const IDS: [&str; 3] = ["7_color_r", "7_color_g", "7_color_b"];

fn row(id: &'static str, name: &str, value: f32) -> ParamRow {
    ParamRow {
        id: std::borrow::Cow::Borrowed(id),
        spec: RowSpec {
            tooltip: None,
            name: name.into(),
            min: 0.0,
            max: 1.0,
            default: 1.0,
            whole_numbers: false,
            is_angle: false,
            is_toggle: false,
            is_trigger: false,
            is_trigger_gate: false,
            value_labels: None,
            unit: None,
            section: None,
            disabled: None,
            material_role: None,
            inactive_reason: None,
            setup_warning: None,
            scene: None,
            limit: Default::default(),
        },
        value: RowValue { base: value, effective: value, exposed: true, driven: false },
        audio: AudioRowState::default(),
        clip_trigger: None,
        modulation: RowMod::default(),
        mapping: RowMapping { osc_address: None, ableton_display: None, ableton_range: None, mappable: false },
        rgb_members: None,
        modulator_stored: false,
    }
}

/// An intensity fader, then a light colour's three channels.
fn light_rows(editable: bool) -> Vec<ParamRow> {
    let mut red = row(IDS[0], "Colour R", 1.0);
    red.rgb_members = Some(RgbMembers {
        group: ColourGroup::Light,
        ids: IDS.map(ParamId::from),
        values: [1.0, 0.5, 0.25],
        editable,
    });
    vec![row("7_intensity", "Intensity", 1.0), red, row(IDS[1], "Colour G", 0.5), row(IDS[2], "Colour B", 0.25)]
}

/// A scene card holding [`light_rows`].
fn scene_card(editable: bool) -> (ParamCardPanel, UITree) {
    let rows = light_rows(editable);
    let surface = ParamSurface {
        kind: ParamCardKind::Generator,
        title: "Scene".into(),
        collapsed: false,
        effect_index: 0,
        effect_id: EffectId::new("gen-scene-1"),
        enabled: true,
        supports_envelopes: true,
        has_graph_mod: false,
        layer_id: None,
        modifier: None,
        rows,
        string_params: vec![],
        audio_sends: Vec::new(),
    };
    let mut panel = ParamCardPanel::new();
    panel.configure(&surface);
    panel.set_scene_sections(Some(&[]));
    panel.set_host(super::CardHost::SceneCard);
    let mut tree = UITree::new();
    panel.build(&mut tree, Rect::new(0.0, 0.0, 340.0, 600.0));
    (panel, tree)
}

fn rebuild(panel: &mut ParamCardPanel) -> UITree {
    let mut tree = UITree::new();
    panel.build(&mut tree, Rect::new(0.0, 0.0, 340.0, 600.0));
    tree
}

#[test]
fn hex_is_one_upper_case_format() {
    assert_eq!(colour_hex([1.0, 0.0, 170.0 / 255.0]), "#FF00AA");
    assert_eq!(colour_hex([-1.0, 2.0, 0.5]), "#00FF80");
    assert_eq!(colour_name("Base Colour R"), "Base Colour");
    assert_eq!(colour_name("Tint"), "Tint");
}

#[test]
fn only_the_red_channel_of_a_grouped_colour_is_the_colour_row() {
    let rows = light_rows(true);
    let host = RowHost::new();
    assert_eq!(rows[1].kind(true), RowKind::Colour);
    assert_eq!(rows[1].kind(false), RowKind::Number, "off a scene surface the channels stay sliders");
    assert_eq!(rows[2].kind(true), RowKind::Number);
    assert!(host.is_colour_tail(&rows, 2, true) && host.is_colour_tail(&rows, 3, true));
    assert!(!host.is_colour_tail(&rows, 0, true) && !host.is_colour_tail(&rows, 2, false));
    assert_eq!(RowHost::colour_channels(&rows, 1), [Some(1), Some(2), Some(3)]);
    let mut fader = rows[0].clone();
    fader.rgb_members = rows[1].rgb_members.clone();
    assert_eq!(fader.kind(true), RowKind::Number, "a fader carrying its light's swatch stays a fader");
}

/// The colour row replaces the three loose sliders: one swatch, one hex
/// field, channels folded away until the fold opens.
#[test]
fn editable_colour_folds_its_channels_and_opens_on_the_fold() {
    let (mut panel, tree) = scene_card(true);
    let (swatch, _) = panel.row_host.swatch_ids[1].expect("the colour row has a swatch");
    let nodes = panel.row_host.colour_ids[1].expect("the colour row has fold and hex");
    assert_eq!(tree.get_node(nodes.hex).unwrap().text.as_deref(), Some("#FF8040"));
    assert!(panel.row_host.slider_ids[1..].iter().all(Option::is_none), "channels start folded");
    assert!(panel.row_host.slider_ids[0].is_some(), "the fader is untouched");
    assert!(panel.row_host.swatch_ids[2..].iter().all(Option::is_none), "one swatch per colour");

    let actions = panel.handle_click(nodes.hex, &tree);
    assert!(
        matches!(actions.as_slice(), [PanelAction::Root(RootAction::BeginColourTextInput { param_ids, anchor, value, .. })]
            if param_ids == &IDS.map(ParamId::from) && *anchor == tree.get_bounds(nodes.hex) && *value == [1.0, 0.5, 0.25]),
        "{actions:?}"
    );
    let actions = panel.handle_click(swatch, &tree);
    assert!(matches!(actions.as_slice(), [PanelAction::Root(RootAction::OpenColourMenu { .. })]), "{actions:?}");

    let actions = panel.handle_click(nodes.fold, &tree);
    assert!(matches!(actions.as_slice(), [PanelAction::Params(crate::ParamsAction::SectionFoldToggled)]));
    let tree = rebuild(&mut panel);
    assert!(panel.row_host.slider_ids[1..].iter().all(Option::is_some), "the fold holds all three sliders");
    assert!(panel.row_host.swatch_ids[1].is_some(), "the R slider does not take the header's swatch");
    let header = tree.get_bounds(panel.row_host.colour_ids[1].unwrap().fold);
    let red = tree.get_bounds(panel.row_host.slider_ids[1].as_ref().unwrap().track);
    assert!(red.y > header.y, "the channels sit under their colour row");
}

/// A channel with a stored modulator keeps its slider and lane reachable:
/// the colour row opens on its channels and the hex only shows.
#[test]
fn colour_one_write_cannot_own_opens_and_only_shows() {
    let (mut panel, tree) = scene_card(false);
    assert!(panel.row_host.slider_ids[1..].iter().all(Option::is_some), "open by default");
    assert!(panel.row_host.driver_btn_ids[2].is_some(), "each channel keeps its D lane");
    let nodes = panel.row_host.colour_ids[1].unwrap();
    assert!(panel.handle_click(nodes.hex, &tree).is_empty());
    let (swatch, _) = panel.row_host.swatch_ids[1].unwrap();
    assert!(panel.handle_click(swatch, &tree).is_empty());
}

#[test]
fn swatch_and_hex_follow_the_live_channels() {
    let (mut panel, mut tree) = scene_card(true);
    let mut slots = [IDS[0], IDS[1], IDS[2], "7_intensity"].map(|id| (id, UiParamSlot::exposed(0.0)));
    slots[0].1 = UiParamSlot::exposed(1.0);
    panel.sync_values(&mut tree, &mut slots.into_iter());
    let nodes = panel.row_host.colour_ids[1].unwrap();
    assert_eq!(tree.get_node(nodes.hex).unwrap().text.as_deref(), Some("#FF0000"));
    let (swatch, _) = panel.row_host.swatch_ids[1].unwrap();
    assert_eq!(tree.get_node(swatch).unwrap().style.bg_color, swatch_colour([1.0, 0.0, 0.0]));
}
