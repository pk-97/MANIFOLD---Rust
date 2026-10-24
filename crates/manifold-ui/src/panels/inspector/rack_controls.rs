//! Rack header controls: the bypass switch and the wet/dry slider.
//!
//! The rack is not a manifest card, so its slider is the plain chrome slider
//! (`View::slider_row`, `docs/WIDGET_TREE_DESIGN.md` section 5b), driven by one
//! `SliderDragState` and addressed by `ValueRef::RackWetDry`. The switch is the
//! shared `components::toggle`. Both leave as `PanelAction` values; the app
//! turns them into `ToggleGroupCommand` and `ChangeGroupWetDryCommand`.

use super::*;
use crate::chrome::components;
use crate::chrome::{Align, Sizing, SliderSpec};
use crate::panels::{ScrubPhase, ScrubValue, ValueRef};
use crate::slider::{BitmapSlider, SliderColors, SliderDragState, SliderNodeIds};

/// Structural rack projection for one effect group. Membership is inferred by
/// the app from each effect's stable group id; the inspector only needs the
/// ordered member ids to add the header and indentation without touching the
/// parameter surface.
#[derive(Debug, Clone, PartialEq)]
pub struct RackGroupConfig {
    pub id: EffectGroupId,
    pub name: String,
    pub collapsed: bool,
    pub member_ids: Vec<EffectId>,
    /// The composable mask modifier in this group, when present.
    pub mask_effect_id: Option<EffectId>,
    /// False when the rack is bypassed: its effects are skipped in the chain.
    pub enabled: bool,
    /// Wet/dry mix, 0 (dry) to 1 (wet).
    pub wet_dry: f32,
}

pub(super) struct RackGroupNodes {
    pub group_id: EffectGroupId,
    pub frame: NodeId,
    pub header: NodeId,
    pub collapse: NodeId,
    pub bypass: Option<NodeId>,
    pub wet_dry: Option<SliderNodeIds>,
}

/// The live wet/dry drag: the group it started on (the address is captured
/// once, so a rebuild or selection change mid-drag cannot retarget it).
pub(super) struct RackDrag {
    address: ValueRef,
    group_id: EffectGroupId,
    slider: SliderDragState,
}

pub(super) const RACK_CONTROLS_H: f32 = 24.0;
const WET_DRY_LABEL_W: f32 = 54.0;
const BYPASS_W: f32 = 62.0;
pub(super) const BYPASS_NAME: &str = "inspector.effect_group.bypass";

fn wet_dry_text(v: f32) -> String {
    format!("{:.0}%", v * 100.0)
}

/// The wet/dry scrub address for one rack.
pub(super) fn rack_address(
    tab: InspectorTab,
    layer_id: &Option<LayerId>,
    group_id: &EffectGroupId,
) -> ValueRef {
    ValueRef::RackWetDry {
        tab,
        layer_id: layer_id.clone(),
        group_id: group_id.clone(),
    }
}

/// The second header row: bypass switch, then the wet/dry slider.
pub(super) fn rack_controls_row(group: &RackGroupConfig, address: &ValueRef) -> View {
    let reset = PanelAction::slider_reset(
        PanelAction::Scrub(address.clone(), ScrubPhase::Begin),
        PanelAction::Scrub(address.clone(), ScrubPhase::Move(ScrubValue::Scalar(1.0))),
        PanelAction::Scrub(address.clone(), ScrubPhase::Commit),
    );
    let wet = group.wet_dry.clamp(0.0, 1.0);
    View::row(4.0)
        .fill_w()
        .h(Sizing::Fixed(RACK_CONTROLS_H))
        .cross_align(Align::Center)
        .pad(Pad {
            l: 4.0,
            t: 0.0,
            r: 4.0,
            b: 4.0,
        })
        .child(
            components::toggle(
                if group.enabled { "Bypass" } else { "Bypassed" },
                !group.enabled,
            )
            .w(Sizing::Fixed(BYPASS_W))
            .fill_h()
            .name(BYPASS_NAME)
            .key(super::render::rack_group_key(&group.id, "bypass"))
            .inert(),
        )
        .child(
            View::slider_row(SliderSpec {
                label: Some("Wet/Dry".to_string()),
                value: wet,
                default: 1.0,
                value_text: wet_dry_text(wet),
                colors: SliderColors::default_slider(),
                font_size: color::FONT_BODY,
                label_width: WET_DRY_LABEL_W,
                reset,
            })
            .fill_w()
            .fill_h()
            .key(super::render::rack_group_key(&group.id, "wet_dry")),
        )
}

impl InspectorCompositePanel {
    /// Where the group's header controls address their scrubs and edits.
    pub(super) fn rack_address(&self, group_id: &EffectGroupId) -> ValueRef {
        rack_address(self.active_tab, &self.inspecting_layer_id, group_id)
    }

    /// A click on the bypass switch flips the rack; a click on the slider
    /// track is the end of a press the drag already handled.
    pub(super) fn rack_click(&self, node_id: NodeId) -> Option<Vec<PanelAction>> {
        if self
            .group_nodes
            .iter()
            .any(|n| n.wet_dry.is_some_and(|ids| ids.track == node_id))
        {
            return Some(Vec::new());
        }
        let nodes = self
            .group_nodes
            .iter()
            .find(|n| n.bypass == Some(node_id))?;
        let group = self.rack_groups[Self::scope_idx(self.active_tab)]
            .iter()
            .find(|g| g.id == nodes.group_id)?;
        Some(vec![PanelAction::Params(ParamsAction::EffectGroupBypass {
            tab: self.active_tab,
            layer_id: self.inspecting_layer_id.clone(),
            group_id: group.id.clone(),
            enabled: !group.enabled,
        })])
    }

    /// Pointer-down on a wet/dry track starts the scrub: Begin captures the
    /// undo baseline app-side, the first Move applies the grabbed value.
    pub(super) fn rack_press(&mut self, node_id: NodeId, pos: Vec2) -> Option<Vec<PanelAction>> {
        let nodes = self
            .group_nodes
            .iter()
            .find(|n| n.wet_dry.is_some_and(|ids| ids.track == node_id))?;
        let group_id = nodes.group_id.clone();
        let mut slider = SliderDragState::default();
        slider.set_ids(nodes.wet_dry?);
        let value = slider.try_start_drag(node_id, pos.x)?;
        let address = self.rack_address(&group_id);
        let actions = vec![
            PanelAction::Scrub(address.clone(), ScrubPhase::Begin),
            PanelAction::Scrub(address.clone(), ScrubPhase::Move(ScrubValue::Scalar(value))),
        ];
        self.rack_drag = Some(RackDrag {
            address,
            group_id,
            slider,
        });
        self.pressed_target = Some(PressedTarget::RackWetDry);
        Some(actions)
    }

    pub(super) fn rack_drag_move(&mut self, pos: Vec2, tree: &mut UITree) -> Vec<PanelAction> {
        let Some(drag) = self.rack_drag.as_mut() else {
            return Vec::new();
        };
        match drag.slider.apply_drag(pos.x, tree, &wet_dry_text) {
            Some(v) => vec![PanelAction::Scrub(
                drag.address.clone(),
                ScrubPhase::Move(ScrubValue::Scalar(v)),
            )],
            None => Vec::new(),
        }
    }

    pub(super) fn rack_drag_end(&mut self) -> Vec<PanelAction> {
        let Some(mut drag) = self.rack_drag.take() else {
            return Vec::new();
        };
        if drag.slider.end_drag() {
            vec![PanelAction::Scrub(drag.address, ScrubPhase::Commit)]
        } else {
            Vec::new()
        }
    }

    /// After a rebuild the dragged slider has new node ids; point the drag at
    /// them so the live gesture keeps tracking.
    pub(super) fn rack_rewire_drag(&mut self) {
        let Some(drag) = self.rack_drag.as_mut() else {
            return;
        };
        match self
            .group_nodes
            .iter()
            .find(|n| n.group_id == drag.group_id)
            .and_then(|n| n.wet_dry)
        {
            Some(ids) => drag.slider.set_ids(ids),
            None => drag.slider.clear(),
        }
    }

    /// Right-click on a wet/dry track resets it to fully wet.
    pub(super) fn rack_register_intents(&self, intents: &mut crate::intent::IntentRegistry) {
        for nodes in &self.group_nodes {
            let Some(ids) = nodes.wet_dry.as_ref() else {
                continue;
            };
            let address = self.rack_address(&nodes.group_id);
            let reset = PanelAction::slider_reset(
                PanelAction::Scrub(address.clone(), ScrubPhase::Begin),
                PanelAction::Scrub(address.clone(), ScrubPhase::Move(ScrubValue::Scalar(1.0))),
                PanelAction::Scrub(address, ScrubPhase::Commit),
            );
            BitmapSlider::register_track_reset(ids, &reset, intents);
        }
    }
}

pub(super) const WET_DRY_SLIDER_NAME: &str = "inspector.effect_group.wet_dry.slider";
pub(super) const WET_DRY_VALUE_NAME: &str = "inspector.effect_group.wet_dry.value";

/// Name the slider's track and value cell so flows and tests can address them.
pub(super) fn name_wet_dry_slider(tree: &mut UITree, ids: &SliderNodeIds) {
    tree.set_name(ids.track, WET_DRY_SLIDER_NAME);
    tree.set_name(ids.value_text, WET_DRY_VALUE_NAME);
}

#[cfg(test)]
mod tests {
    use super::super::tests::grouped_fixture;
    use super::*;
    use crate::input::Modifiers;

    #[test]
    fn bypass_click_asks_for_the_flipped_state() {
        let (mut panel, tree) = grouped_fixture(false);
        let bypass = panel.group_nodes[0]
            .bypass
            .expect("header builds the switch");
        let actions = panel.route_click(bypass, Modifiers::NONE, &tree);
        assert!(matches!(
            &actions[..],
            [PanelAction::Params(ParamsAction::EffectGroupBypass {
                enabled: false,
                ..
            })]
        ));
    }

    #[test]
    fn bypassed_rack_reads_as_bypassed() {
        let (mut panel, _) = grouped_fixture(false);
        let mut group = panel.rack_groups[0][0].clone();
        group.enabled = false;
        panel.configure_rack_groups(InspectorTab::Master, &[group]);
        let mut tree = UITree::new();
        panel.build(&mut tree, &super::super::tests::inspector_layout());
        let bypass = panel.group_nodes[0].bypass.unwrap();
        assert_eq!(
            tree.get_node(bypass).unwrap().text.as_deref(),
            Some("Bypassed")
        );
        let actions = panel.route_click(bypass, Modifiers::NONE, &tree);
        assert!(matches!(
            &actions[..],
            [PanelAction::Params(ParamsAction::EffectGroupBypass {
                enabled: true,
                ..
            })]
        ));
    }

    #[test]
    fn wet_dry_drag_is_one_begin_moves_one_commit() {
        let (mut panel, mut tree) = grouped_fixture(false);
        let ids = panel.group_nodes[0]
            .wet_dry
            .expect("header builds the slider");
        let span = ids.track_span;
        let y = tree.get_bounds(ids.track).y + 2.0;
        let down = panel.route_pointer_down(
            ids.track,
            Vec2::new(span.x + span.width * 0.5, y),
            Modifiers::NONE,
            &tree,
        );
        assert!(matches!(
            &down[..],
            [
                PanelAction::Scrub(ValueRef::RackWetDry { .. }, ScrubPhase::Begin),
                PanelAction::Scrub(_, ScrubPhase::Move(ScrubValue::Scalar(v)))
            ] if (*v - 0.5).abs() < 0.05
        ));
        let moved = panel.handle_drag(Vec2::new(span.x - 50.0, y), &mut tree, false);
        assert!(matches!(
            &moved[..],
            [PanelAction::Scrub(_, ScrubPhase::Move(ScrubValue::Scalar(v)))] if *v == 0.0
        ));
        let end = panel.handle_drag_end(&mut tree);
        assert!(matches!(
            &end[..],
            [PanelAction::Scrub(_, ScrubPhase::Commit)]
        ));
        assert!(
            panel.handle_drag_end(&mut tree).is_empty(),
            "one commit per drag"
        );
    }

    #[test]
    fn wet_dry_drag_survives_a_rebuild_under_the_cursor() {
        let (mut panel, tree) = grouped_fixture(false);
        let ids = panel.group_nodes[0].wet_dry.unwrap();
        let y = tree.get_bounds(ids.track).y + 2.0;
        let x = ids.track_span.x + 10.0;
        panel.route_pointer_down(ids.track, Vec2::new(x, y), Modifiers::NONE, &tree);
        let mut rebuilt = UITree::new();
        panel.build(&mut rebuilt, &super::super::tests::inspector_layout());
        let moved = panel.handle_drag(Vec2::new(x + 40.0, y), &mut rebuilt, false);
        assert_eq!(
            moved.len(),
            1,
            "the drag keeps tracking after the inspector rebuilds"
        );
    }
}
