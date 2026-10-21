//! The flow runner's main preview gizmo, fed and driven as the App's
//! `ws.preview_gizmo` is: the content thread's per-frame gizmo report, the
//! present-time show and compose, and the preview's pointer handlers.

use manifold_core::project::Project;
use manifold_ui::node::{Rect, Vec2};

use super::super::fixtures::SceneData;
use super::Runner;
use crate::physics_overlay::{PhysicsOverlayObservations, PreviewGizmo};
use crate::ui_root::UIRoot;

#[derive(Default)]
pub(super) struct RunnerGizmo {
    /// The main preview's gizmo host, as the App's `ws.preview_gizmo`.
    pub(super) host: PreviewGizmo,
    /// The content thread's gizmo report for the Scene panel's scene, one
    /// entry per frame. No scene renders here, so each entry carries
    /// [`stand_in_camera`], a stand-in scene node and no lights, domains or
    /// physics shapes.
    overlay: PhysicsOverlayObservations,
    overlay_frame: u64,
    /// The preview's handle grips this frame, the `gizmo` surface.
    pub(super) grips: Vec<(String, Vec2)>,
}

impl RunnerGizmo {
    /// The Scene panel's gizmo report, as the content thread records one
    /// per rendered frame (D7a).
    pub(super) fn report(&mut self, ui: &UIRoot, data: &mut SceneData) {
        let gizmo_layer = ui.scene_setup_panel.gizmo_layer().cloned();
        self.overlay.set_layer(gizmo_layer.clone());
        if gizmo_layer.is_some() {
            self.overlay_frame += 1;
            self.overlay.record(0, self.overlay_frame, |_, _, _, _| Some((manifold_core::NodeId::new("scene"), stand_in_camera())));
        }
        data.content.physics_overlay_frames = self.overlay.frames.clone();
    }

    /// The preview host's present-time read of the report: no bridge here,
    /// so the newest entry is the one shown.
    pub(super) fn present(&mut self, ui: &UIRoot, data: &SceneData, stamps: &manifold_core::change::PartStamps) {
        self.host.show(data.content.physics_overlay_frames[0].as_ref(), preview_dims(&data.project), None);
        let _ = self.host.refresh(&data.project, &ui.scene_setup_panel, stamps);
        self.grips = preview_fit(ui, &data.project).map_or_else(Vec::new, |fit| self.host.grips(fit));
    }
}

impl Runner {
    /// Run `f` with the main preview's gizmo host, a dispatch context and
    /// the picture's fit, as `window_input`'s preview handlers do. A press
    /// over an open overlay never reaches the preview.
    pub(super) fn preview_gizmo(
        &mut self,
        ui: &mut UIRoot,
        data: &mut SceneData,
        f: impl FnOnce(&mut PreviewGizmo, &mut crate::ui_bridge::DispatchCtx, Option<Rect>) -> bool,
    ) -> bool {
        let fit = preview_fit(ui, &data.project);
        let mut dctx = crate::ui_bridge::DispatchCtx {
            project: crate::ui_bridge::ProjectRef::new(&mut data.project, &mut self.edits, &self.content_tx),
            content_tx: &self.content_tx,
            content_state: &self.content_state,
            ui: &mut *ui,
            selection: &mut data.selection,
            active_layer: &mut self.active_layer,
            user_prefs: &mut self.user_prefs,
            editor_target: None,
            scrub: &mut self.scrub,
            dialogs: Default::default(),
        };
        f(&mut self.gizmo.host, &mut dctx, fit)
    }
}

/// The render size the preview shows: the project's output, as the App's
/// content pipeline renders it.
fn preview_dims(project: &Project) -> (u32, u32) {
    (project.settings.output_width.max(1) as u32, project.settings.output_height.max(1) as u32)
}

/// Where the show picture sits in the main window, fitted as the App's
/// draw and input paths fit it (`ui_frame::video_fit_rect`).
fn preview_fit(ui: &UIRoot, project: &Project) -> Option<Rect> {
    let (w, h) = preview_dims(project);
    crate::ui_frame::video_fit_rect(ui.layout.video_area(), (w as f32, h as f32))
}

/// The camera the runner's gizmo report says the scene rendered through:
/// a front view of the origin a few units back, where the fixtures' scene
/// objects sit.
fn stand_in_camera() -> manifold_node_engine::scene::camera::Camera {
    manifold_node_engine::scene::camera::Camera::look_at(
        [0.0, 1.5, 6.0],
        [0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        std::f32::consts::FRAC_PI_3,
        0.05,
        200.0,
    )
}
