//! The scene items of the one param menu (cross-surface gestures): Show on Card from the Scene tab, Remove from
//! Card and Show in Scene tab from the scene card; and the slider range items
//! every param row carries, on both surfaces (D15 (slider range and hard
//! limit)).

use manifold_core::effects::ParamId;
use manifold_core::scene_surface::CardPin;
use manifold_ui::{
    DropdownItem, GraphParamTarget, PanelAction, ParamRow, ParamsAction, Rect, RootAction,
};

/// Shown in place of an action on a mapped row (D6 (attached rows)).
pub(crate) const MAPPED_REASON: &str = "Mapped \u{2014} unmap to remove";

/// Where the right-clicked row lives.
pub(crate) enum PinSurface<'a> {
    /// A row on the scene card.
    Card(&'a ParamRow),
    /// A row in the Scene tab, and whether the card already shows it.
    SceneTab { on_card: bool },
}

/// The scene items for a param menu, in menu order. Empty when the row is
/// already where Show on Card would put it.
pub(crate) fn scene_items(
    target: &GraphParamTarget,
    param: &ParamId,
    surface: PinSurface<'_>,
) -> Vec<DropdownItem> {
    let pin = |pin| {
        PanelAction::Params(ParamsAction::SetCardPin(
            target.clone(),
            param.clone(),
            Some(pin),
        ))
    };
    match surface {
        PinSurface::SceneTab { on_card: true } => Vec::new(),
        PinSurface::SceneTab { on_card: false } => {
            vec![
                DropdownItem::new("Show on Card")
                    .with_action(pin(CardPin::Shown))
                    .with_separator(),
            ]
        }
        PinSurface::Card(row) => {
            let mut items = Vec::with_capacity(3);
            if row.is_attached() {
                items.push(DropdownItem::disabled("Remove from Card"));
                items.push(DropdownItem::disabled(MAPPED_REASON));
            } else {
                items
                    .push(DropdownItem::new("Remove from Card").with_action(pin(CardPin::Removed)));
            }
            items.push(
                DropdownItem::new("Show in Scene tab")
                    .with_action(PanelAction::Params(ParamsAction::RevealInSceneTab(
                        target.clone(),
                        param.clone(),
                    )))
                    .with_separator(),
            );
            items
        }
    }
}

/// Set slider range… and Reset slider range. `anchor` is where the type-in
/// opens: under the right-click.
pub(crate) fn slider_range_items(
    target: &GraphParamTarget,
    param: &ParamId,
    anchor: Rect,
) -> [DropdownItem; 2] {
    [
        DropdownItem::new("Set slider range\u{2026}").with_action(PanelAction::Root(
            RootAction::BeginSliderRangeInput {
                target: target.clone(),
                param_id: param.clone(),
                anchor,
            },
        )),
        DropdownItem::new("Reset slider range")
            .with_action(PanelAction::Params(ParamsAction::SetSliderRange(
                target.clone(),
                param.clone(),
                None,
            )))
            .with_separator(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row() -> ParamRow {
        ParamRow {
            id: std::borrow::Cow::Borrowed("light_intensity"),
            spec: manifold_ui::RowSpec {
                tooltip: None,
                name: "Sun".into(),
                min: 0.0,
                max: 10.0,
                default: 1.0,
                whole_numbers: false,
                is_angle: false,
                is_toggle: false,
                is_trigger: false,
                is_trigger_gate: false,
                value_labels: None,
                unit: None,
                section: None,
                material_role: None,
                inactive_reason: None,
                setup_warning: None,
                scene: None,
                limit: Default::default(),
                disabled: None,
            },
            value: manifold_ui::RowValue {
                base: 1.0,
                effective: 1.0,
                exposed: true,
                driven: false,
            },
            modulation: manifold_ui::RowMod::default(),
            mapping: manifold_ui::RowMapping {
                osc_address: None,
                ableton_display: None,
                ableton_range: None,
                mappable: true,
            },
            rgb_members: None,
            affordance: None,
            modulator_stored: false,
            audio: Default::default(),
            clip_trigger: None,
        }
    }

    fn labelled(items: &[DropdownItem]) -> Vec<(&str, bool)> {
        items
            .iter()
            .map(|item| (item.label.as_str(), item.enabled))
            .collect()
    }

    #[test]
    fn remove_disabled_on_mapped_row() {
        let target = GraphParamTarget::Generator;
        let param: ParamId = "light_intensity".into();

        let free = row();
        let items = scene_items(&target, &param, PinSurface::Card(&free));
        assert_eq!(
            labelled(&items),
            [("Remove from Card", true), ("Show in Scene tab", true)]
        );
        assert!(matches!(
            &items[0].action,
            Some(PanelAction::Params(ParamsAction::SetCardPin(_, id, Some(CardPin::Removed)))) if *id == param
        ));

        for attach in [
            |row: &mut ParamRow| row.modulation.driver_active = true,
            |row: &mut ParamRow| row.modulation.envelope_active = true,
            |row: &mut ParamRow| row.modulation.automation_active = true,
            |row: &mut ParamRow| row.audio.active = true,
        ] {
            let mut mapped = row();
            attach(&mut mapped);
            let items = scene_items(&target, &param, PinSurface::Card(&mapped));
            assert_eq!(
                labelled(&items),
                [
                    ("Remove from Card", false),
                    (MAPPED_REASON, false),
                    ("Show in Scene tab", true)
                ]
            );
            assert!(items[0].action.is_none(), "a disabled item fires nothing");
        }
    }

    #[test]
    fn scene_tab_offers_show_on_card_only_when_off_the_card() {
        let target = GraphParamTarget::GeneratorOf(manifold_core::LayerId::new("layer"));
        let param: ParamId = "roughness".into();
        let items = scene_items(&target, &param, PinSurface::SceneTab { on_card: false });
        assert_eq!(labelled(&items), [("Show on Card", true)]);
        assert!(matches!(
            &items[0].action,
            Some(PanelAction::Params(ParamsAction::SetCardPin(
                GraphParamTarget::GeneratorOf(_),
                _,
                Some(CardPin::Shown)
            )))
        ));
        assert!(scene_items(&target, &param, PinSurface::SceneTab { on_card: true }).is_empty());
    }
}
