//! Preview-only navigation on the main output preview: orbit, pan and dolly
//! through the camera session the graph editor's viewport uses
//! ([`SceneViewportNavigation`], `viewport_input`), seeded from the output
//! camera so the first gesture starts from the picture on screen. The
//! output camera and the rendered output never move
//! (`docs/REALTIME_3D_DESIGN.md` D9): while navigating, the preview shows
//! the scene's second view from the viewport bridge instead of the output.
//! Escape, Show gizmos off, another scene, or the editor's viewport opening
//! (D7a) ends it, and the preview shows the output again.

use std::sync::Arc;

use manifold_core::{GraphTarget, LayerId, NodeId};
use manifold_node_engine::scene::viewport_camera::ViewportCamera;
use manifold_ui::{Rect, Vec2};
use winit::event::{MouseButton, MouseScrollDelta};

use crate::physics_overlay::PhysicsOverlayFrame;
use crate::scene_gizmo::GizmoView;
use crate::scene_viewport::{SceneViewportFrame, SceneViewportFrames, SceneViewportNavigation, SceneViewportRequest, ViewportHost};
use crate::viewport_input::{self, ViewportGesture, ViewportInputSensitivity};

/// The output preview's navigation host.
#[derive(Default)]
pub(crate) struct PreviewNavigation {
    session: Option<SceneViewportNavigation>,
    /// The scene node the session looks at: the one that rendered the output.
    target: Option<(GraphTarget, NodeId)>,
    /// Button and last pointer position of an armed drag. A press arms it;
    /// navigation starts on the first move, so a click stays a click.
    drag: Option<(MouseButton, f32, f32)>,
    request_sent: Option<Arc<SceneViewportRequest>>,
}

/// What a frame's request sync asks of the content thread.
#[derive(Debug, PartialEq)]
pub(crate) enum RequestSync {
    Unchanged,
    Send(Arc<SceneViewportRequest>),
    /// Navigation ended while this host owned the request.
    Clear,
}

impl PreviewNavigation {
    pub fn active(&self) -> bool {
        self.session.is_some()
    }

    /// A left or middle press inside the picture `fit` with `pickable`
    /// gizmos on: arm a drag.
    pub fn press(&mut self, fit: Option<Rect>, pos: Vec2, button: MouseButton, pickable: bool) -> bool {
        if !pickable || !matches!(button, MouseButton::Left | MouseButton::Middle) || !fit.is_some_and(|fit| fit.contains(pos)) {
            return false;
        }
        self.drag = Some((button, pos.x, pos.y));
        true
    }

    /// Pointer motion with a drag armed: orbit, or pan with Shift or the
    /// middle button. `None` when no drag is armed; `Some(true)` when this
    /// move started navigation.
    pub fn drag_to(&mut self, pos: Vec2, shift: bool, shown: Option<&PhysicsOverlayFrame>, size: (u32, u32)) -> Option<bool> {
        let (button, x, y) = self.drag?;
        let (dx, dy) = (pos.x - x, pos.y - y);
        if dx == 0.0 && dy == 0.0 {
            return Some(false);
        }
        self.drag = Some((button, pos.x, pos.y));
        let gesture = viewport_input::classify_mouse_drag(button, shift, dx, dy)?;
        Some(self.navigate(gesture, shown, size).unwrap_or(false))
    }

    pub fn release(&mut self) -> bool {
        self.drag.take().is_some()
    }

    /// A wheel over the picture dollies (a Ctrl pixel scroll too); a bare
    /// pixel scroll pans. `None` when the wheel is not the preview's.
    pub fn wheel(
        &mut self,
        over_picture: bool,
        delta: MouseScrollDelta,
        ctrl: bool,
        shown: Option<&PhysicsOverlayFrame>,
        size: (u32, u32),
    ) -> Option<bool> {
        if !over_picture {
            return None;
        }
        self.navigate(viewport_input::classify_wheel(delta, ctrl), shown, size)
    }

    /// Apply `gesture`, starting navigation from the shown output camera
    /// when none runs. `Some(true)` when it started here; `None` when there
    /// is neither a session nor a shown frame to seed one.
    fn navigate(&mut self, gesture: ViewportGesture, shown: Option<&PhysicsOverlayFrame>, size: (u32, u32)) -> Option<bool> {
        let started = self.session.is_none();
        if started {
            let shown = shown?;
            let mut session = SceneViewportNavigation::new(ViewportHost::OutputPreview, size.0, size.1);
            *session.camera_mut() = ViewportCamera::seeded_from(&shown.camera);
            self.session = Some(session);
            self.target = Some((GraphTarget::Generator(shown.layer.clone()), shown.scene_node.clone()));
        }
        let session = self.session.as_mut()?;
        viewport_input::apply(session.camera_mut(), gesture, &ViewportInputSensitivity::default());
        Some(started)
    }

    /// End navigation. True when this host owned the content request, so
    /// the caller clears it unless another host is taking it over.
    pub fn end(&mut self) -> bool {
        self.session = None;
        self.target = None;
        self.drag = None;
        self.request_sent.take().is_some()
    }

    /// The request this frame wants. Navigation ends when the Scene panel's
    /// gizmos leave the scene it looks at.
    pub fn sync_request(&mut self, gizmo_layer: Option<&LayerId>, size: (u32, u32)) -> RequestSync {
        let Some((target, node)) = self.target.as_ref() else { return RequestSync::Unchanged };
        if !matches!(target, GraphTarget::Generator(layer) if Some(layer) == gizmo_layer) {
            return if self.end() { RequestSync::Clear } else { RequestSync::Unchanged };
        }
        let Some(session) = self.session.as_mut() else { return RequestSync::Unchanged };
        (session.config.width, session.config.height) = (size.0.max(1), size.1.max(1));
        let request = session.request(target, node, None, self.request_sent.as_ref());
        if self.request_sent.as_ref().is_some_and(|sent| Arc::ptr_eq(sent, &request)) {
            return RequestSync::Unchanged;
        }
        self.request_sent = Some(request.clone());
        RequestSync::Send(request)
    }

    pub fn request_sent(&self) -> Option<&Arc<SceneViewportRequest>> {
        self.request_sent.as_ref()
    }

    /// The viewport frame on screen this present, matched by its lease.
    pub fn show(&mut self, frame: Option<SceneViewportFrame>) {
        if let Some(session) = self.session.as_mut() {
            session.displayed = frame;
        }
    }

    /// The picture the preview shows while navigating: the camera and size
    /// of the displayed frame. `None` shows the output through its camera.
    pub fn view(&self) -> Option<GizmoView> {
        let config = self.session.as_ref()?.displayed_config()?;
        Some(GizmoView { camera: config.camera.to_camera(), width: config.width, height: config.height })
    }

    /// A frame-planner input: the session and the newest frame published for
    /// it, so a new navigated frame, or the end of navigation, redraws.
    pub fn frame_input(&self, frames: &SceneViewportFrames) -> (u64, u64) {
        let Some(session) = self.session.as_ref() else { return (0, 0) };
        let newest = frames.iter().flatten()
            .filter(|frame| frame.request.host == ViewportHost::OutputPreview && frame.request.session == session.session)
            .map(|frame| frame.frame)
            .max()
            .unwrap_or(0);
        (session.session, newest)
    }
}

/// The navigated frame the present shows instead of the output: the leased
/// viewport surface's texture when this host's frame has an image. The
/// lease joins `leases` and retires with the present's sampling work.
#[cfg(target_os = "macos")]
pub(crate) fn lease_navigated_frame(
    nav: &mut PreviewNavigation,
    pane: Option<&mut crate::texture_pane::TexturePane>,
    device: &manifold_gpu::GpuDevice,
    frames: &SceneViewportFrames,
    leases: &mut crate::shared_texture::BridgeLeaseBatch,
) -> Option<manifold_gpu::GpuTexture> {
    let (Some(pane), Some(request)) = (pane, nav.request_sent().cloned()) else {
        nav.show(None);
        return None;
    };
    let leased = crate::scene_viewport::lease_frame(pane, device, frames, &request, leases);
    let (frame, texture) = leased.map_or((None, None), |(frame, texture)| (Some(frame), texture));
    nav.show(frame);
    texture
}

impl crate::app::Application {
    /// The size the navigated view renders at: the preview's own pixels.
    fn preview_navigation_size(&self) -> (u32, u32) {
        #[cfg(target_os = "macos")]
        return self.workspace_preview_size;
        #[cfg(not(target_os = "macos"))]
        {
            let (w, h) = self.video_source_dims();
            (w as u32, h as u32)
        }
    }

    /// The pointer is over the picture, the Scene panel's gizmos are on and
    /// no overlay covers it.
    fn pointer_on_navigable_preview(&self) -> bool {
        let pos = self.cursor_pos;
        self.ws.ui_root.scene_setup_panel.gizmo_layer().is_some()
            && self.preview_fit().is_some_and(|fit| fit.contains(pos))
            && !self.ws.ui_root.overlay_contains_point(pos)
    }

    /// A left press the gizmos did not take, or a middle press, on the
    /// picture: arm a navigation drag. True when the preview took it.
    pub(crate) fn preview_navigation_press(&mut self, button: MouseButton) -> bool {
        let pickable = self.pointer_on_navigable_preview();
        let (fit, pos) = (self.preview_fit(), self.cursor_pos);
        self.ws.preview_navigation.press(fit, pos, button, pickable)
    }

    /// Pointer motion while a navigation drag holds the pointer.
    pub(crate) fn preview_navigation_move(&mut self) -> bool {
        let size = self.preview_navigation_size();
        let shift = self.modifiers.shift;
        let Some(started) = self.ws.preview_navigation.drag_to(self.cursor_pos, shift, self.ws.preview_gizmo.shown(), size) else {
            return false;
        };
        if started {
            self.end_editor_viewport();
        }
        true
    }

    pub(crate) fn preview_navigation_release(&mut self) -> bool {
        self.ws.preview_navigation.release()
    }

    /// The wheel over the picture with gizmos on dollies the preview.
    pub(crate) fn preview_navigation_wheel(&mut self, delta: MouseScrollDelta) -> bool {
        let over = self.pointer_on_navigable_preview();
        let (size, ctrl) = (self.preview_navigation_size(), self.modifiers.ctrl);
        let Some(started) = self.ws.preview_navigation.wheel(over, delta, ctrl, self.ws.preview_gizmo.shown(), size) else {
            return false;
        };
        if started {
            self.end_editor_viewport();
        }
        true
    }

    /// Escape: back to the output camera. True when navigation was running.
    pub(crate) fn end_preview_navigation(&mut self) -> bool {
        let active = self.ws.preview_navigation.active();
        if self.ws.preview_navigation.end() {
            self.send_content_cmd(crate::content_command::ContentCommand::SetSceneViewport(None));
        }
        active
    }

    /// Starting preview navigation ends the editor viewport's (D7a): its
    /// pane closes, and the preview's request replaces its request.
    fn end_editor_viewport(&mut self) {
        if let Some(editor) = self.graph_editor.as_mut() {
            editor.viewport_open = false;
            editor.end_viewport_navigation();
        }
    }

    /// Send what this frame's navigation asks of the content thread.
    pub(crate) fn sync_preview_navigation(&mut self) {
        let size = self.preview_navigation_size();
        let layer = self.ws.ui_root.scene_setup_panel.gizmo_layer();
        match self.ws.preview_navigation.sync_request(layer, size) {
            RequestSync::Unchanged => {}
            RequestSync::Send(request) => {
                self.send_content_cmd(crate::content_command::ContentCommand::SetSceneViewport(Some(request)));
            }
            RequestSync::Clear => self.send_content_cmd(crate::content_command::ContentCommand::SetSceneViewport(None)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_node_engine::runtime::frame_status::FrameRenderStatus;
    use manifold_node_engine::scene::camera::Camera;

    const SIZE: (u32, u32) = (640, 360);

    fn output_frame() -> PhysicsOverlayFrame {
        PhysicsOverlayFrame {
            layer: LayerId::new("scene"),
            frame: 7,
            scene_node: NodeId::new("render"),
            camera: Camera::look_at([0.0, 1.5, 6.0], [0.0; 3], [0.0, 1.0, 0.0], std::f32::consts::FRAC_PI_3, 0.05, 200.0),
            domains: Arc::from([]),
            lights: Arc::from([]),
            gizmos: Arc::from([]),
        }
    }

    fn fit() -> Rect {
        Rect::new(100.0, 50.0, 640.0, 360.0)
    }

    fn displayed(request: &Arc<SceneViewportRequest>) -> SceneViewportFrame {
        SceneViewportFrame {
            frame: 9,
            surface_generation: 1,
            request: request.clone(),
            status: Ok(FrameRenderStatus::Complete),
            domains: Arc::from([]),
        }
    }

    #[test]
    fn a_click_without_motion_never_starts_navigation() {
        let mut nav = PreviewNavigation::default();
        let shown = output_frame();
        assert!(nav.press(Some(fit()), Vec2::new(300.0, 200.0), MouseButton::Left, true));
        assert_eq!(nav.drag_to(Vec2::new(300.0, 200.0), false, Some(&shown), SIZE), Some(false));
        assert!(nav.release());
        assert!(!nav.active());
        assert_eq!(nav.sync_request(Some(&shown.layer), SIZE), RequestSync::Unchanged);
        assert!(!nav.press(Some(fit()), Vec2::new(10.0, 10.0), MouseButton::Left, true), "outside the picture");
        assert!(!nav.press(Some(fit()), Vec2::new(300.0, 200.0), MouseButton::Left, false), "gizmos off");
    }

    /// D9: navigating the preview moves only the preview's own camera. The
    /// output camera the overlay reports is untouched, the request names the
    /// output preview and the scene node that rendered the output, and ending
    /// navigation returns the gizmos to the output camera.
    #[test]
    fn preview_navigation_never_moves_the_output_camera() {
        let mut nav = PreviewNavigation::default();
        let shown = output_frame();
        let output_camera = shown.camera;
        nav.press(Some(fit()), Vec2::new(300.0, 200.0), MouseButton::Left, true);
        assert_eq!(nav.drag_to(Vec2::new(360.0, 180.0), false, Some(&shown), SIZE), Some(true), "the first move starts it");
        assert_eq!(nav.drag_to(Vec2::new(380.0, 170.0), true, Some(&shown), SIZE), Some(false));
        assert!(nav.release());
        assert_eq!(nav.wheel(true, MouseScrollDelta::LineDelta(0.0, 2.0), false, Some(&shown), SIZE), Some(false));

        let RequestSync::Send(request) = nav.sync_request(Some(&shown.layer), SIZE) else { panic!("navigation sends a request") };
        assert_eq!(request.host, ViewportHost::OutputPreview);
        assert_eq!(request.target, GraphTarget::Generator(shown.layer.clone()));
        assert_eq!(request.node, shown.scene_node);
        assert_eq!((request.config.width, request.config.height), SIZE);
        assert_ne!(request.config.camera.to_camera().pos, output_camera.pos, "the preview looks from elsewhere");
        assert_eq!(nav.sync_request(Some(&shown.layer), SIZE), RequestSync::Unchanged, "an unchanged camera sends nothing");

        nav.show(Some(displayed(&request)));
        let view = nav.view().expect("a displayed navigated frame is what the gizmos project through");
        assert_eq!(view.camera.pos, request.config.camera.to_camera().pos);
        assert_eq!(shown.camera.pos, output_camera.pos);
        assert_eq!(shown.camera.fwd, output_camera.fwd);
        assert_eq!(shown.camera.view, output_camera.view);

        assert!(nav.end(), "it owned the request");
        assert!(nav.view().is_none(), "ended: the preview shows the output through its camera");
        assert!(!nav.end());
    }

    #[test]
    fn navigation_starts_where_the_output_camera_looks() {
        let mut nav = PreviewNavigation::default();
        let shown = output_frame();
        nav.wheel(true, MouseScrollDelta::LineDelta(0.0, 0.0), false, Some(&shown), SIZE);
        let RequestSync::Send(request) = nav.sync_request(Some(&shown.layer), SIZE) else { panic!() };
        let seeded = request.config.camera.to_camera();
        for i in 0..3 {
            assert!((seeded.pos[i] - shown.camera.pos[i]).abs() < 1e-4);
            assert!((seeded.fwd[i] - shown.camera.fwd[i]).abs() < 1e-4);
        }
    }

    #[test]
    fn leaving_the_scene_ends_navigation_and_clears_the_request() {
        let mut nav = PreviewNavigation::default();
        let shown = output_frame();
        nav.wheel(true, MouseScrollDelta::LineDelta(0.0, 1.0), false, Some(&shown), SIZE);
        assert!(matches!(nav.sync_request(Some(&shown.layer), SIZE), RequestSync::Send(_)));
        assert_eq!(nav.sync_request(None, SIZE), RequestSync::Clear, "Show gizmos off");
        assert!(!nav.active());
        assert_eq!(nav.sync_request(None, SIZE), RequestSync::Unchanged);
    }

    #[test]
    fn nothing_starts_without_a_shown_output_frame() {
        let mut nav = PreviewNavigation::default();
        assert_eq!(nav.wheel(true, MouseScrollDelta::LineDelta(0.0, 1.0), false, None, SIZE), None);
        assert_eq!(nav.wheel(false, MouseScrollDelta::LineDelta(0.0, 1.0), false, Some(&output_frame()), SIZE), None);
        assert!(!nav.active());
    }
}
