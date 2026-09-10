//! Click-resolution helpers for parameter slider row drawers.
//! Split out of `param_slider_shared` (P-S1, UI funnel decomposition).

use super::*;


// ── Shared helper functions ─────────────────────────────────────

/// BUG-250: the click-to-change action set for an enum (`value_labels`)
/// row's value cell — the behavior SCENE_OBJECT_AND_PANEL_V2 D9 committed
/// to, restored in the shared card core after C-P1c/d deleted the bespoke
/// producers. A 2-label row cycles to the next value through the
/// `ParamSnapshot`/`ParamChanged`/`ParamCommit` trio (one undo unit; the
/// scene id_map interception comes free); a 3+-label row opens the shared
/// dropdown via [`PanelAction::ParamEnumDropdown`]. `current_value` is the
/// row's base value, `min` the param's range minimum (enum index = value −
/// min, same encoding as [`format_param_value`]).
pub(crate) fn enum_value_cell_actions(
    target: crate::panels::GraphParamTarget,
    param_id: manifold_foundation::ParamId,
    labels: &[String],
    current_value: f32,
    min: f32,
    cell_node_id: NodeId,
) -> Vec<crate::panels::PanelAction> {
    use crate::panels::PanelAction;
    let count = labels.len();
    if count == 0 {
        return Vec::new();
    }
    let current_index =
        ((current_value - min).round() as i32).clamp(0, count as i32 - 1) as usize;
    if count <= 2 {
        let next = (current_index + 1) % count;
        let new_value = min + next as f32;
        vec![
            PanelAction::Scrub(ValueRef::Param(target.clone(), param_id.clone()), ScrubPhase::Begin),
            PanelAction::Scrub(
                ValueRef::Param(target.clone(), param_id.clone()),
                ScrubPhase::Move(ScrubValue::Scalar(new_value)),
            ),
            PanelAction::Scrub(ValueRef::Param(target, param_id), ScrubPhase::Commit),
        ]
    } else {
        vec![PanelAction::Root(RootAction::ParamEnumDropdown {
            target,
            param_id,
            labels: labels.to_vec(),
            current_index: current_index as u32,
            cell_node_id,
        })]
    }
}


// ── Shared event helpers ────────────────────────────────────────

/// If `node_id` is a driver drawer's Free-period field, return its param index.
/// The Free field opens a beats type-in (free mode) rather than issuing a config
/// command, so it's matched separately from the drawer action resolver.
pub(crate) fn driver_free_field_index(
    node_id: NodeId,
    drawers: &[Option<crate::panels::drawer::DrawerIds>],
) -> Option<usize> {
    drawers.iter().enumerate().find_map(|(pi, drawer)| {
        let drawer = drawer.as_ref()?;
        let action = drawer.resolve_action(node_id)?;
        matches!(action, crate::panels::PanelAction::Root(crate::RootAction::BeginDriverPeriodTextInput { .. })).then_some(pi)
    })
}
