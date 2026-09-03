//! SCENE_LOOP_DESIGN P2: the "Scene Loop" fold section in the Scene panel.
//!
//! Lives apart from `scene_setup_panel.rs` — that file is at its 4150-line
//! ceiling, so section logic for new panels goes in sibling modules like this
//! one (precedent: `scene_setup_actions.rs`, `scene_setup_skin.rs`).
//!
//! Three states (section 3.3):
//! - **Not applied** → "Enable Scene Loop" button dispatching the apply-command.
//! - **Applied** → manifest-backed rows (the D6 P4 whitelist: Bars, Copies,
//!   Height, Lateral) plus a remove button and wrap-debug toggle.
//! - **Hand-edited graph** → the structural trace (`SceneLoopInfo`) is
//!   all-or-nothing on the three core nodes; a hand-edit that removes one
//!   shows "Not applied" (re-apply is then a fresh splice, never a silent
//!   partial fix). A future partial-trace refinement could split this state.

use crate::node::NodeId;
use crate::tree::UITree;

use super::PanelAction;
use super::scene_setup_panel::{
    ROW_H, ScenePanel, SceneSetupVm,
    btn_style, label_style,
};

/// SCENE_LOOP_DESIGN P2: the Scene Loop section's view model. `Some` = loop
/// is applied; the panel renders manifest-backed rows. `None` = not applied;
/// the panel renders the "Enable Scene Loop" button.
#[derive(Clone, Debug, PartialEq)]
pub struct SceneLoopRow {
    /// The section string for manifest filtering (always "Scene Loop").
    pub section: String,
    /// The beat_ramp's doc id — the wrap-debug toggle parks the camera at
    /// phase 0 by writing its `bars` param to 0 through the same
    /// `SceneSetupParamChanged` write path every manifest row uses.
    pub beat_ramp_doc_id: u32,
    /// The beat_ramp's current `bars` (0 = parked). Writing this value back
    /// resumes the loop.
    pub bars: f32,
}

/// Stable key for the "Enable Scene Loop" button.
const KEY_ENABLE_LOOP: u64 = 80_030;
/// Stable key for the "Remove Scene Loop" button.
const KEY_REMOVE_LOOP: u64 = 80_031;
/// Stable key for the wrap-debug toggle.
const KEY_WRAP_DEBUG: u64 = 80_032;

/// The Scene Loop section's UI-local ids on `ScenePanel` — kept OUT of the
/// godfile (`scene_setup_panel.rs`) so the panel's registration glue stays
/// one field + one Default line.
#[derive(Default)]
pub(crate) struct SceneLoopUi {
    pub(crate) enable_id: Option<NodeId>,
    pub(crate) remove_id: Option<NodeId>,
    pub(crate) wrap_debug_id: Option<NodeId>,
    /// The `bars` value to write when the wrap-debug toggle resumes — the
    /// pre-park value, stashed the first time the toggle parks (the applied
    /// bars value is otherwise lost once bars=0 lands in the graph). `None`
    /// until armed.
    pub(crate) wrap_debug_resume_bars: Option<f32>,
}

/// Build the Scene Loop properties section. Called when `SceneSelection::SceneLoop`
/// is the active selection.
pub(crate) fn build_properties(
    panel: &mut ScenePanel,
    tree: &mut UITree,
    inner_x: f32,
    inner_w: f32,
    mut cy: f32,
    vm: &SceneSetupVm,
) -> f32 {
    match &vm.scene_loop {
        None => {
            // Not applied: "Enable Scene Loop" button.
            tree.add_label(
                Some(panel.content_parent),
                inner_x,
                cy,
                inner_w,
                ROW_H,
                "Scene Loop: Not applied",
                label_style(),
            );
            cy += ROW_H;
            panel.scene_loop.enable_id = Some(tree.add_button_keyed(
                Some(panel.content_parent),
                inner_x,
                cy,
                inner_w,
                ROW_H,
                btn_style(),
                "Enable Scene Loop",
                KEY_ENABLE_LOOP,
            ));
            cy += ROW_H;
            cy
        }
        Some(loop_row) => {
            // Applied: manifest-backed rows via build_filtered_properties,
            // plus remove button and wrap-debug toggle.
            let sections = vec!["Scene Loop".to_string()];
            cy = panel.build_filtered_properties(tree, inner_x, inner_w, cy, &sections);

            // Wrap-debug toggle (parks camera at phase 0, D-3.3): the state is
            // the beat_ramp's REAL `bars` — 0 means parked. The label reads the
            // VM, never a panel-side flag (a stale flag would report ON when
            // the performer already resumed, or OFF when the project reloaded
            // parked — the rebuild-less UI class). The pre-park bars value is
            // stashed here (build-time, `&mut self`) so a parked loop can
            // resume to its real bars value — the graph no longer carries it
            // once bars=0 lands.
            let parked = loop_row.bars.abs() < f32::EPSILON;
            if !parked {
                panel.scene_loop.wrap_debug_resume_bars = Some(loop_row.bars);
            }
            tree.add_label(
                Some(panel.content_parent),
                inner_x,
                cy,
                inner_w,
                ROW_H,
                "Wrap Debug (phase 0)",
                label_style(),
            );
            cy += ROW_H;
            panel.scene_loop.wrap_debug_id = Some(tree.add_button_keyed(
                Some(panel.content_parent),
                inner_x,
                cy,
                inner_w,
                ROW_H,
                btn_style(),
                if parked { "ON" } else { "OFF" },
                KEY_WRAP_DEBUG,
            ));
            cy += ROW_H;

            // Remove button.
            panel.scene_loop.remove_id = Some(tree.add_button_keyed(
                Some(panel.content_parent),
                inner_x,
                cy,
                inner_w,
                ROW_H,
                btn_style(),
                "Remove Scene Loop",
                KEY_REMOVE_LOOP,
            ));
            cy += ROW_H;
            cy
        }
    }
}

/// Click dispatch for Scene Loop buttons. Returns `Some(PanelAction)` if the
/// click was handled.
pub(crate) fn click_dispatch(
    panel: &ScenePanel,
    node_id: NodeId,
    vm: &SceneSetupVm,
) -> Option<PanelAction> {
    if panel.scene_loop.enable_id == Some(node_id) {
        return Some(PanelAction::Project(
            super::ProjectAction::SceneSetupApplyLoop(
                vm.layer_id.clone(),
                vm.scene_root_node_id,
            ),
        ));
    }
    if panel.scene_loop.remove_id == Some(node_id) {
        return Some(PanelAction::Project(
            super::ProjectAction::SceneSetupRemoveLoop(
                vm.layer_id.clone(),
                vm.scene_root_node_id,
            ),
        ));
    }
    if panel.scene_loop.wrap_debug_id == Some(node_id) {
        // Wrap-debug toggle: parks the camera at phase 0 (the seam is
        // inspectable) by writing loop_phase `bars` to 0 — a REAL param
        // write through the same `SceneSetupParamChanged` path every manifest
        // row uses, never a UI-local flag. The VM carries the applied bars
        // value so toggling OFF resumes the loop exactly as it was. (bars = 0
        // falls back to the minted rate = 0, so the phase holds at 0.)
        if let Some(loop_row) = &vm.scene_loop {
            let parked = loop_row.bars.abs() < f32::EPSILON;
            // Resume to the STASHED pre-park bars value (the graph no longer
            // carries it while parked); park at 0 when running.
            let bars_target = if parked {
                panel
                    .scene_loop
                    .wrap_debug_resume_bars
                    .unwrap_or(8.0)
            } else {
                0.0
            };
            return Some(PanelAction::Project(
                super::ProjectAction::SceneSetupParamChanged(
                    vm.layer_id.clone(),
                    Vec::new(),
                    loop_row.beat_ramp_doc_id,
                    "bars".to_string(),
                    bars_target,
                ),
            ));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::NodeId;
    use super::super::ProjectAction;

    fn test_id(i: u32) -> NodeId {
        NodeId::from_parts(i, 1)
    }

    fn vm(applied: bool, bars: f32) -> SceneSetupVm {
        SceneSetupVm {
            layer_id: manifold_foundation::LayerId::new("layer-9"),
            scene_name: "Loop Scene".to_string(),
            multiple_scenes: false,
            object_count: 0,
            light_count: 0,
            shadow_caster_count: 0,
            scene_root_node_id: 7,
            environment: super::super::scene_setup_panel::EnvironmentRowVm::None,
            atmosphere: super::super::scene_setup_panel::AtmosphereRowVm::None,
            audio_send_labels: Vec::new(),
            audio_send_ids: Vec::new(),
            objects: Vec::new(),
            lights: Vec::new(),
            camera: super::super::scene_setup_panel::CameraRowVm::None,
            camera_sections: Vec::new(),
            world_sections: Vec::new(),
            scene_loop: applied.then(|| SceneLoopRow {
                section: "Scene Loop".to_string(),
                beat_ramp_doc_id: 40,
                bars,
            }),
            scene_bounds: None,
        }
    }

    /// The "Enable Scene Loop" button, when clicked, dispatches the apply
    /// command's ProjectAction at the panel's OWN layer + render_scene id. This
    /// is the BUG-292/INV-5 addressing: the write path stays `GeneratorOf
    /// (vm.layer_id)` — never the active-layer-resolved plain `Generator`.
    #[test]
    fn enable_button_dispatches_apply_at_panel_layer() {
        let mut panel = ScenePanel::new();
        panel.scene_loop.enable_id = Some(test_id(9000));
        let act = click_dispatch(&panel, test_id(9000), &vm(false, 0.0)).expect("handled");
        match act {
            PanelAction::Project(ProjectAction::SceneSetupApplyLoop(layer, root)) => {
                assert_eq!(layer.as_str(), "layer-9");
                assert_eq!(root, 7);
            }
            other => panic!("expected apply dispatch, got {other:?}"),
        }
    }

    /// The wrap-debug toggle (loop applied, un-parked) dispatches a real
    /// `SceneSetupParamChanged` to the beat_ramp's `bars` at 0 — parking the
    /// camera at phase 0 through the SAME GeneratorOf write path a manifest
    /// row uses, never a UI-local flag. Selecting SceneLoop is not required for
    /// the dispatch itself; the write is what this nets.
    #[test]
    fn wrap_debug_park_writes_beat_ramp_bars_zero() {
        let mut panel = ScenePanel::new();
        panel.scene_loop.wrap_debug_id = Some(test_id(9001));
        // Loop applied, bars 8 (running) → park writes bars 0.
        let act = click_dispatch(&panel, test_id(9001), &vm(true, 8.0)).expect("handled");
        match act {
            PanelAction::Project(ProjectAction::SceneSetupParamChanged(
                _,
                scope,
                node,
                param,
                value,
            )) => {
                assert_eq!(scope, Vec::<u32>::new(), "beat_ramp is a top-level node");
                assert_eq!(node, 40, "writes to the beat_ramp's doc id");
                assert_eq!(param, "bars");
                assert_eq!(value, 0.0, "park = bars 0 (rate fallback is the minted 0)");
            }
            other => panic!("expected wrap-debug param write, got {other:?}"),
        }
    }

    /// Wrap-debug resume: when the loop is already parked (bars 0), clicking
    /// resumes to the STASHED pre-park bars value — the graph no longer
    /// carries it.
    #[test]
    fn wrap_debug_resume_uses_stashed_bars() {
        let mut panel = ScenePanel::new();
        panel.scene_loop.wrap_debug_id = Some(test_id(9002));
        panel.scene_loop.wrap_debug_resume_bars = Some(16.0);
        let act = click_dispatch(&panel, test_id(9002), &vm(true, 0.0)).expect("handled");
        match act {
            PanelAction::Project(ProjectAction::SceneSetupParamChanged(_, _, node, param, value)) => {
                assert_eq!(node, 40);
                assert_eq!(param, "bars");
                assert_eq!(value, 16.0, "resume restores the stashed pre-park bars value");
            }
            other => panic!("expected wrap-debug param write, got {other:?}"),
        }
    }
}
