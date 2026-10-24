//! UI_INVALIDATION P4d batch d: selection gestures return `handled()`; the
//! selection versions alone drive the planner.

use crate::frame_plan::{FrameInputs, FramePlanner, MainRegion, Projection};
use manifold_core::{Beats, ClipId, LayerId};
use manifold_ui::panels::scene_setup_panel::SceneSelection;
use manifold_ui::{EditingAction, InspectorTab, Modifiers, PanelAction, RootAction};

struct Rig {
    project: manifold_core::project::Project,
    content_tx: crossbeam_channel::Sender<crate::content_command::ContentCommand>,
    content_rx: crossbeam_channel::Receiver<crate::content_command::ContentCommand>,
    content_state: crate::content_state::ContentState,
    ui: crate::ui_root::UIRoot,
    selection: manifold_ui::UIState,
    active_layer: Option<LayerId>,
    user_prefs: crate::user_prefs::UserPrefs,
    scrub: crate::ui_bridge::ScrubState,
    edits: crate::gesture_preview::UiEdits,
    planner: FramePlanner,
}

impl Rig {
    /// Two layers: clips "a" and "b" on the first, "c" on the second.
    fn new() -> Self {
        let mut project = manifold_core::project::Project::default();
        for (index, ids) in [["a", "b"].as_slice(), ["c"].as_slice()]
            .into_iter()
            .enumerate()
        {
            let mut layer =
                manifold_core::layer::Layer::new_video(format!("L{index}"), index as i32);
            for (n, id) in ids.iter().enumerate() {
                let mut clip = manifold_core::clip::TimelineClip::new_generator(
                    Beats(4.0 * n as f64),
                    Beats(4.0),
                );
                clip.id = ClipId::new(*id);
                layer.clips.push(clip);
            }
            project.timeline.layers.push(layer);
        }
        let (content_tx, content_rx) = crossbeam_channel::unbounded();
        let mut rig = Self {
            project,
            content_tx,
            content_rx,
            content_state: crate::content_state::ContentState::default(),
            ui: crate::ui_root::UIRoot::new(),
            selection: manifold_ui::UIState::new(),
            active_layer: None,
            user_prefs: crate::user_prefs::UserPrefs::in_memory(),
            scrub: crate::ui_bridge::ScrubState::default(),
            edits: Default::default(),
            planner: FramePlanner::default(),
        };
        let first = rig.inputs();
        rig.planner.plan(first);
        rig
    }

    fn dispatch(&mut self, action: PanelAction) -> crate::ui_bridge::DispatchResult {
        let mut ctx = crate::ui_bridge::DispatchCtx {
            project: crate::ui_bridge::ProjectRef::new(
                &mut self.project,
                &mut self.edits,
                &self.content_tx,
            ),
            content_tx: &self.content_tx,
            content_state: &self.content_state,
            ui: &mut self.ui,
            selection: &mut self.selection,
            active_layer: &mut self.active_layer,
            user_prefs: &mut self.user_prefs,
            editor_target: None,
            scrub: &mut self.scrub,
            dialogs: Default::default(),
        };
        crate::ui_bridge::dispatch(&action, &mut ctx)
    }

    /// The selection-side planner inputs, read as `gather_frame_inputs` does.
    fn inputs(&mut self) -> FrameInputs {
        FrameInputs {
            parts: self.edits.stamps.parts(),
            selection_identity: self.selection.selection_identity_version(),
            selection_visual: self.selection.selection_visual_version(),
            cursor: self.selection.cursor_version(),
            active_layer: self.planner.active_layer_version(&self.active_layer),
            automation_view: self.selection.automation_view(),
            automation_point_lane: self.selection.automation_point_lane(),
            inspector_view: self.ui.inspector.view_version(),
            scene_selection: self.ui.scene_setup_panel.selection_version(),
            ..Default::default()
        }
    }

    /// Dispatch one gesture and plan the frame after it.
    fn frame(&mut self, action: PanelAction) -> crate::frame_plan::FramePlan {
        self.dispatch(action.clone());
        assert!(
            self.content_rx.try_recv().is_err(),
            "{action:?} sends nothing"
        );
        let next = self.inputs();
        self.planner.plan(next)
    }
}

fn click(id: &str) -> PanelAction {
    PanelAction::Editing(EditingAction::ClipClicked(id.into(), Modifiers::default()))
}

/// The performer gesture: click between three clips quickly mid-playback.
/// Each click is one inspector sync; re-clicking the selected clip is none.
#[test]
fn clicking_between_clips_is_one_inspector_sync_per_click() {
    let mut rig = Rig::new();
    rig.content_state.is_playing = true;
    for id in ["a", "c", "b", "a"] {
        let plan = rig.frame(click(id));
        assert!(plan.projections.contains(Projection::InspectorSync), "{id}");
        assert!(plan.projections.contains(Projection::SelectionPush), "{id}");
        assert!(plan.rebuild.contains(MainRegion::Inspector), "{id}");
        assert_eq!(
            rig.selection.primary_selected_clip_id(),
            Some(&ClipId::new(id))
        );
        // The frame after the click has nothing left to do.
        let idle = rig.inputs();
        assert!(
            rig.planner.plan(idle).projections == Default::default(),
            "{id}: settled"
        );
    }
    let plan = rig.frame(click("a"));
    assert!(
        !plan.projections.contains(Projection::InspectorSync),
        "same clip again"
    );
}

/// A tab click pins the scope: the identity version re-projects the inspector.
#[test]
fn inspector_tab_click_is_a_selection_change() {
    let mut rig = Rig::new();
    rig.frame(click("c"));
    for tab in [InspectorTab::Master, InspectorTab::Layer] {
        let plan = rig.frame(PanelAction::Root(RootAction::SelectInspectorTab(tab)));
        assert!(
            plan.projections.contains(Projection::InspectorSync),
            "{tab:?}"
        );
    }
    let plan = rig.frame(PanelAction::Root(RootAction::SelectInspectorTab(
        InspectorTab::Layer,
    )));
    assert!(
        !plan.projections.contains(Projection::InspectorSync),
        "same tab again"
    );
}

fn item(id: &str) -> manifold_core::scene_doc::ItemId {
    manifold_core::scene_doc::ItemId::new(id)
}

/// The Scene panel's own selection re-runs the inspector projection (it fills
/// the Properties card), and only when the selection changed.
#[test]
fn scene_selection_reprojects_only_on_change() {
    let mut rig = Rig::new();
    let layer = rig.project.timeline.layers[0].layer_id.clone();
    for (selection, moves) in [
        (SceneSelection::Item(item("object-4")), true),
        (SceneSelection::Item(item("object-4")), false),
        (SceneSelection::Item(item("light-7")), true),
    ] {
        rig.ui
            .scene_setup_panel
            .set_selection(layer.clone(), selection.clone());
        let plan = rig.frame(PanelAction::Root(RootAction::SceneSetupSelectionChanged(
            layer.clone(),
        )));
        assert_eq!(
            plan.rebuild.contains(MainRegion::Inspector),
            moves,
            "{selection:?}"
        );
        assert_eq!(
            plan.projections.contains(Projection::InspectorSync),
            moves,
            "{selection:?}"
        );
        assert!(
            !plan.projections.contains(Projection::ProjectSync),
            "{selection:?}"
        );
    }
}
