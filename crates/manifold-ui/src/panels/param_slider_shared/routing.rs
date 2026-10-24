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
    let current_index = label_index(current_value, min, count);
    if count <= 2 {
        let next = (current_index + 1) % count;
        let new_value = min + next as f32;
        vec![
            PanelAction::Scrub(
                ValueRef::Param(target.clone(), param_id.clone()),
                ScrubPhase::Begin,
            ),
            PanelAction::Scrub(
                ValueRef::Param(target.clone(), param_id.clone()),
                ScrubPhase::Move(ScrubValue::Scalar(new_value)),
            ),
            PanelAction::Scrub(ValueRef::Param(target, param_id), ScrubPhase::Commit),
        ]
    } else {
        vec![enum_dropdown(
            target,
            param_id,
            labels,
            current_index,
            cell_node_id,
        )]
    }
}

/// A pick list's click: the shared dropdown over the row's labels, anchored
/// under the pick list, checked at the current value. Every label count
/// opens the list (a pick list never cycles). `None` for a row without labels.
pub(crate) fn pick_list_action(
    target: crate::panels::GraphParamTarget,
    param_id: manifold_foundation::ParamId,
    spec: &crate::param_surface::RowSpec,
    current_value: f32,
    button: NodeId,
) -> Option<crate::panels::PanelAction> {
    let labels = spec
        .value_labels
        .as_deref()
        .filter(|labels| !labels.is_empty())?;
    let current_index = label_index(current_value, spec.min, labels.len());
    Some(enum_dropdown(
        target,
        param_id,
        labels,
        current_index,
        button,
    ))
}

/// A click on a scene item's structural field row (the section 8 action
/// rows): a pick list opens the shared menu under the row's button, every
/// option carrying the `SceneFieldSet` it sends; a switch sends its flip; a
/// file field asks the app
/// for its file dialog. A row with a reason (`inactive_reason`) is disabled,
/// so its click does nothing.
pub(crate) fn field_row_action(
    target: &crate::panels::GraphParamTarget,
    row: &crate::param_surface::ParamRow,
    button: NodeId,
) -> Vec<crate::panels::PanelAction> {
    use crate::panels::PanelAction;
    use crate::panels::dropdown::DropdownItem;
    use crate::panels::scene_setup_panel::FieldControlVm;
    let (
        Some(crate::param_surface::RowAffordance::Field {
            item,
            field,
            control,
        }),
        crate::panels::GraphParamTarget::GeneratorOf(layer),
    ) = (&row.affordance, target)
    else {
        return Vec::new();
    };
    if row.spec.inactive_reason.is_some() {
        return Vec::new();
    }
    match control {
        FieldControlVm::Pick { options, .. } => {
            let items = options
                .iter()
                .map(|option| DropdownItem {
                    // No hover text on a menu item: a disabled choice names its reason.
                    label: match &option.disabled {
                        Some(reason) => format!("{} \u{2014} {reason}", option.label),
                        None => option.label.clone(),
                    },
                    enabled: option.disabled.is_none(),
                    separator_after: false,
                    checked: option.checked,
                    action: Some(PanelAction::Project(crate::ProjectAction::SceneFieldSet {
                        layer_id: layer.clone(),
                        item: item.clone(),
                        field: field.clone(),
                        pick: option.pick.clone(),
                    })),
                })
                .collect();
            vec![PanelAction::Root(RootAction::SceneSetupActionMenuClicked {
                button_node_id: button,
                items,
            })]
        }
        FieldControlVm::Switch { toggle, .. } => {
            vec![PanelAction::Project(crate::ProjectAction::SceneFieldSet {
                layer_id: layer.clone(),
                item: item.clone(),
                field: field.clone(),
                pick: toggle.clone(),
            })]
        }
        FieldControlVm::File { extensions, .. } => {
            vec![PanelAction::Root(RootAction::SceneFieldFileClicked {
                layer_id: layer.clone(),
                item: item.clone(),
                field: field.clone(),
                extensions: extensions.clone(),
            })]
        }
    }
}

fn enum_dropdown(
    target: crate::panels::GraphParamTarget,
    param_id: manifold_foundation::ParamId,
    labels: &[String],
    current_index: usize,
    cell_node_id: NodeId,
) -> crate::panels::PanelAction {
    crate::panels::PanelAction::Root(RootAction::ParamEnumDropdown {
        target,
        param_id,
        labels: labels.to_vec(),
        current_index: current_index as u32,
        cell_node_id,
    })
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
        matches!(
            action,
            crate::panels::PanelAction::Root(crate::RootAction::BeginDriverPeriodTextInput { .. })
        )
        .then_some(pi)
    })
}
