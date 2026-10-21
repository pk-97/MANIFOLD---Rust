//! Where gizmo lines are drawn. The UI composes the lines (it owns the
//! selection and the handles) and sends them as a [`GizmoOverlay`]; the
//! content thread draws them through the one gizmo pass
//! (`manifold_nodes_scene::node_graph::gizmo_pass`) onto the workspace
//! preview, the scene viewport frame and, when the export asks, the export
//! frame, testing them against the scene's own depth in the picked draw
//! mode. Never into the output picture the show sends out.

use std::sync::Arc;

use manifold_core::LayerId;
use manifold_foundation::scene_surface::GizmoDrawMode;
use manifold_nodes_scene::node_graph::viewport_overlay::WorldLine;

use crate::scene_viewport::ViewportHost;

/// What the UI wants drawn, as last sent. Equal lines share one `Arc`, so
/// comparing is a pointer check.
#[derive(Debug, Clone, Default)]
pub(crate) struct GizmoOverlay {
    pub mode: GizmoDrawMode,
    /// The scene the Scene panel shows, with Show gizmos on or off: the
    /// export draws this scene's gizmos when it includes them.
    pub scene_layer: Option<LayerId>,
    /// Lines for the output preview, with the scene layer they belong to.
    pub output: Option<(LayerId, Arc<[WorldLine]>)>,
    /// Lines for the scene viewport frame, with the host that shows it.
    pub viewport: Option<(ViewportHost, Arc<[WorldLine]>)>,
}

impl PartialEq for GizmoOverlay {
    fn eq(&self, other: &Self) -> bool {
        self.mode == other.mode
            && self.scene_layer == other.scene_layer
            && match (&self.output, &other.output) {
                (None, None) => true,
                (Some((a, lines_a)), Some((b, lines_b))) => a == b && Arc::ptr_eq(lines_a, lines_b),
                _ => false,
            }
            && match (&self.viewport, &other.viewport) {
                (None, None) => true,
                (Some((a, lines_a)), Some((b, lines_b))) => a == b && Arc::ptr_eq(lines_a, lines_b),
                _ => false,
            }
    }
}

/// `lines` as a shared slice, the same one as `kept` when nothing moved.
pub(crate) fn intern(kept: &Option<Arc<[WorldLine]>>, lines: &[WorldLine]) -> Arc<[WorldLine]> {
    match kept {
        Some(kept) if kept.as_ref() == lines => Arc::clone(kept),
        _ => Arc::from(lines),
    }
}

#[cfg(target_os = "macos")]
pub(crate) use capture::GizmoCapture;

#[cfg(target_os = "macos")]
mod capture {
    use std::cell::RefCell;

    use manifold_compositor::generator_renderer::GeneratorRenderer;
    use manifold_core::LayerId;
    use manifold_foundation::scene_surface::GizmoDrawMode;
    use manifold_gpu::{GpuDevice, GpuEncoder, GpuTexture};
    use manifold_node_engine::scene::camera::Camera;
    use manifold_nodes_scene::node_graph::gizmo_pass::GizmoPass;
    use manifold_nodes_scene::node_graph::viewport_gizmo::{GizmoChrome, GizmoMode, GizmoView, compose_lines};
    use manifold_nodes_scene::node_graph::viewport_overlay::WorldLine;

    use super::GizmoOverlay;
    use crate::physics_overlay::PhysicsOverlayFrame;
    use crate::scene_viewport::ViewportHost;

    /// The content thread's gizmo state: the standing overlay, the gizmo
    /// pass, and the depth the scenes kept this frame.
    #[derive(Default)]
    pub(crate) struct GizmoCapture {
        overlay: GizmoOverlay,
        pass: RefCell<GizmoPass>,
        output_depth_layer: Option<LayerId>,
        viewport_depth_layer: Option<LayerId>,
        /// The output scene's depth, taken after the generators render.
        frame_depth: Option<GpuTexture>,
        /// Set while an export includes gizmos.
        exporting: bool,
        scratch: Vec<WorldLine>,
    }

    impl GizmoCapture {
        pub fn set(&mut self, overlay: GizmoOverlay) {
            self.overlay = overlay;
        }

        /// An export includes the gizmos of the Scene panel's scene while
        /// `on`. Returns that scene.
        pub fn set_exporting(&mut self, on: bool) -> Option<LayerId> {
            self.exporting = on;
            self.overlay.scene_layer.clone()
        }

        /// Before the generators render: ask the scene shown in the output
        /// (`output_layer`) and the one in the viewport (`viewport_layer`)
        /// to keep their depth, when a draw mode needs it and lines will be
        /// drawn; clear a layer the request left. A rebuilt graph starts
        /// without the request, so a standing one is set every frame.
        pub fn apply(&mut self, generators: &mut GeneratorRenderer, output_layer: Option<&LayerId>, viewport_layer: Option<&LayerId>) {
            let needs_depth = self.overlay.mode != GizmoDrawMode::OnTop;
            let output = output_layer.filter(|_| needs_depth && (self.exporting || self.overlay.output.is_some()));
            let viewport = viewport_layer.filter(|_| needs_depth && self.overlay.viewport.is_some());
            if self.output_depth_layer.as_ref() != output
                && let Some(old) = self.output_depth_layer.take()
            {
                generators.set_gizmo_depth(&old, false);
            }
            if self.viewport_depth_layer.as_ref() != viewport
                && let Some(old) = self.viewport_depth_layer.take()
            {
                generators.set_scene_viewport_gizmo_depth(&old, false);
            }
            self.output_depth_layer = output.cloned();
            self.viewport_depth_layer = viewport.cloned();
            if let Some(layer) = output {
                generators.set_gizmo_depth(layer, true);
            }
            if let Some(layer) = viewport {
                generators.set_scene_viewport_gizmo_depth(layer, true);
            }
        }

        /// After the generators render: take the output scene's kept depth.
        pub fn take_frame_depth(&mut self, generators: Option<&GeneratorRenderer>, output_layer: Option<&LayerId>) {
            self.frame_depth = generators.zip(output_layer).and_then(|(g, layer)| g.gizmo_depth(layer).cloned());
        }

        /// The viewport scene's kept depth, for the copy that carries it.
        pub fn viewport_depth(generators: Option<&GeneratorRenderer>, viewport_layer: Option<&LayerId>) -> Option<GpuTexture> {
            generators.zip(viewport_layer).and_then(|(g, layer)| g.scene_viewport_gizmo_depth(layer).cloned())
        }

        /// The output preview's lines over `target`, through the camera the
        /// scene rendered with. Nothing when the lines belong to another
        /// scene than the frame.
        pub fn draw_output(&self, device: &GpuDevice, enc: &mut GpuEncoder, target: &GpuTexture, frame: Option<&PhysicsOverlayFrame>) {
            let (Some((layer, lines)), Some(frame)) = (self.overlay.output.as_ref(), frame) else { return };
            if &frame.layer != layer {
                return;
            }
            self.pass.borrow_mut().draw(device, enc, target, self.frame_depth.as_ref(), &frame.camera, lines, self.overlay.mode);
        }

        /// The viewport frame's lines over `target`, when `host` shows them.
        pub fn draw_viewport(&self, device: &GpuDevice, enc: &mut GpuEncoder, target: &GpuTexture, host: ViewportHost, depth: Option<&GpuTexture>, camera: &Camera) {
            let Some((lines_host, lines)) = self.overlay.viewport.as_ref() else { return };
            if *lines_host != host {
                return;
            }
            self.pass.borrow_mut().draw(device, enc, target, depth, camera, lines, self.overlay.mode);
        }

        /// The export frame's lines: the scene's own chrome, grid, lights,
        /// domains and physics shapes, with nothing selected, composed from
        /// what this frame's solvers reported.
        pub fn draw_export(&mut self, device: &GpuDevice, enc: &mut GpuEncoder, target: &GpuTexture, frame: Option<&PhysicsOverlayFrame>) {
            let Some(frame) = frame else { return };
            let view = GizmoView::new(frame.camera, target.width, target.height);
            compose_lines(
                &GizmoChrome {
                    view,
                    lights: &frame.lights,
                    domains: &frame.domains,
                    live_columns: &frame.live_columns,
                    physics: &frame.gizmos,
                    problems: &[],
                    selection: &crate::physics_overlay::NO_SELECTION,
                    mode: GizmoMode::default(),
                    fade_handles: false,
                },
                &mut self.scratch,
            );
            self.pass.borrow_mut().draw(device, enc, target, self.frame_depth.as_ref(), &frame.camera, &self.scratch, self.overlay.mode);
        }
    }
}

impl crate::app::Application {
    /// Send the gizmo lines and draw mode when they move: the output
    /// preview's lines while Show gizmos is on, and the scene viewport's from
    /// whichever host owns it (the preview while it navigates, else the graph
    /// editor). The lines are the ones last composed for what is on screen.
    pub(crate) fn sync_gizmo_overlay(&mut self) {
        let panel = &self.ws.ui_root.scene_setup_panel;
        let (mode, scene_layer) = (panel.gizmo_draw_mode(), panel.live_layer_id().cloned());
        let kept_output = self.ws.gizmo_sent.output.as_ref().map(|(_, lines)| Arc::clone(lines));
        let kept_viewport = self.ws.gizmo_sent.viewport.as_ref().map(|(_, lines)| Arc::clone(lines));
        let preview = self.ws.preview_gizmo.composed_lines();
        let (output, viewport) = if self.ws.preview_navigation.active() {
            (None, preview.map(|lines| (ViewportHost::OutputPreview, intern(&kept_viewport, lines))))
        } else {
            let output = self.ws.preview_gizmo.shown().zip(preview).map(|(frame, lines)| (frame.layer.clone(), intern(&kept_output, lines)));
            let editor = self.graph_editor.as_ref().filter(|ed| ed.viewport_session.is_some());
            (output, editor.map(|ed| (ViewportHost::GraphEditor, intern(&kept_viewport, &ed.viewport_overlay_lines))))
        };
        let overlay = GizmoOverlay { mode, scene_layer, output, viewport };
        if overlay == self.ws.gizmo_sent {
            return;
        }
        self.ws.gizmo_sent = overlay.clone();
        self.send_content_cmd(crate::content_command::ContentCommand::SetGizmoOverlay(Box::new(overlay)));
    }
}
