//! "Use this view" (SCENE_SURFACES D14). The editor viewport's camera lives
//! on the UI thread (`Workspace::viewport_session`), so the app resolves it
//! when the Camera header's button is clicked and writes the scene camera
//! once, as one undoable command. Moving the viewport never writes the
//! project.

use manifold_core::scene_doc::ItemId;
use manifold_core::{GraphTarget, LayerId};
use manifold_node_engine::scene::viewport_camera::ViewportCamera;

const NO_VIEWPORT: &str = "Open this scene in the 3D viewport first";

/// The viewport camera, when the open viewport shows `layer_id`'s scene.
pub(crate) fn live_view(ws: &crate::workspace::Workspace, layer_id: &LayerId) -> Option<ViewportCamera> {
    let session = ws.viewport_session.as_ref()?;
    match &ws.viewport_target {
        Some((GraphTarget::Generator(layer), _)) if layer == layer_id => Some(session.config.camera),
        _ => None,
    }
}

/// The click: copy the view into `camera`, or say why not.
pub(crate) fn run(
    project: &mut crate::ui_bridge::ProjectRef<'_>,
    ui: &mut crate::ui_root::UIRoot,
    layer_id: &LayerId,
    camera: &ItemId,
    view: Option<&ViewportCamera>,
) {
    let Some(view) = view else {
        ui.toast.show(NO_VIEWPORT);
        return;
    };
    match crate::scene_edit::copy_view(project, layer_id, camera, view) {
        Ok(Some(edit)) => {
            project.execute(edit.command);
        }
        Ok(None) => {}
        Err(reason) => ui.toast.show(&reason),
    }
}
