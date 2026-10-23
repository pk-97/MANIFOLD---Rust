//! Where gizmo lines are drawn. The UI sends a [`GizmoOverlay`]: which scene
//! to draw chrome for, what is selected and the handle mode. It sends no
//! lines. The content thread builds them in the frame that renders the
//! scene, from its own project and that frame's solver reports, so a handle
//! and the object it moves always come from one snapshot and cannot drift
//! apart mid-drag. The UI keeps only the pointer-hit math. The lines go
//! through the one gizmo pass (`manifold_nodes_scene::node_graph::gizmo_pass`)
//! onto the workspace preview, the scene viewport frame and, when the export
//! asks, the export frame, tested against the scene's own depth in the
//! picked draw mode. Never into the output picture the show sends out.

use std::sync::Arc;

use manifold_core::change::{ChangeSet, Part, PartStamps};
use manifold_core::project::Project;
use manifold_core::{GraphTarget, LayerId};
use manifold_core::scene_surface::GizmoDrawMode;
use manifold_nodes_scene::node_graph::scene_vm::SceneVm;
use manifold_nodes_scene::node_graph::viewport_gizmo::{GizmoChrome, GizmoMode, GizmoSelection, GizmoView, compose_lines};
use manifold_nodes_scene::node_graph::viewport_overlay::WorldLine;
use manifold_ui::panels::scene_setup_panel::SceneSelection;

use crate::physics_overlay::{NO_SELECTION, SharedDomains, effect_follows};
use crate::scene_viewport::ViewportHost;

/// The handles one host wants over its scene: the Scene panel's selected
/// items there, the one selection store every host reads.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct GizmoRequest {
    pub mode: GizmoMode,
    pub items: Arc<[SceneSelection]>,
}

impl GizmoRequest {
    /// The Scene panel's selection. A slice equal to `kept`'s is shared with
    /// it, so a steady frame allocates nothing and compares by content.
    pub fn panel(mode: GizmoMode, items: &[SceneSelection], kept: Option<&Self>) -> Self {
        let items = match kept {
            Some(kept) if kept.items.as_ref() == items => Arc::clone(&kept.items),
            _ => Arc::from(items),
        };
        Self { mode, items }
    }
}

/// What the UI wants drawn, as last sent.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct GizmoOverlay {
    pub mode: GizmoDrawMode,
    /// The scene the Scene panel shows, with Show gizmos on or off: the
    /// export draws this scene's gizmos when it includes them.
    pub scene_layer: Option<LayerId>,
    /// The handles for the output preview, with the scene layer they belong to.
    pub output: Option<(LayerId, GizmoRequest)>,
    /// The handles for the scene viewport frame, with the host that shows it.
    pub viewport: Option<(ViewportHost, GizmoRequest)>,
}

/// Parts that decide whether an effect follows the scene layer.
const CHAIN_PARTS: ChangeSet = ChangeSet::of(&[Part::Layers, Part::Effects, Part::Settings]);

/// What one frame's solvers and camera reported, borrowed from where the
/// frame recorded it.
pub(crate) struct FrameChrome<'a> {
    pub view: GizmoView,
    pub lights: &'a [[f32; 3]],
    pub domains: &'a SharedDomains,
    pub live_columns: &'a [Option<([f32; 3], [f32; 3])>],
    pub physics: &'a [manifold_core::physics_gizmo::PhysicsGizmo],
}

/// The handles' targets for the selection last composed, kept while the
/// scene, the request and the effect chain hold.
struct Chosen {
    scene: Arc<SceneVm>,
    request: GizmoRequest,
    chain: u64,
    selection: GizmoSelection,
    fade: bool,
}

/// One host's lines, built on the content thread from its project.
#[derive(Default)]
pub(crate) struct SceneLines {
    scene: crate::editor_graph_cache::EditorGraphCache,
    problems: crate::setup_check::GizmoProblems,
    chosen: Option<Chosen>,
    lines: Vec<WorldLine>,
}

impl SceneLines {
    /// The chrome of `target`'s scene and the handles of `request`, over the
    /// scene as `project` holds it now and the frame's own reports. Rebuilds
    /// the scene and the targets only when what they read moved. With
    /// `chain_fades`, an effect that moves the picture off the scene fades
    /// the handles.
    pub fn compose(
        &mut self,
        project: &Project,
        stamps: &PartStamps,
        target: &GraphTarget,
        request: &GizmoRequest,
        chain_fades: bool,
        frame: FrameChrome<'_>,
    ) -> &[WorldLine] {
        let scene = self.scene.viewport_def(project, Some(target), stamps).scene;
        let chain = stamps.key(CHAIN_PARTS);
        let stale = match (&self.chosen, &scene) {
            (Some(chosen), Some(scene)) => !Arc::ptr_eq(&chosen.scene, scene) || chosen.request != *request || chosen.chain != chain,
            (None, None) => false,
            _ => true,
        };
        if stale {
            self.chosen = scene.clone().map(|scene| {
                let fade = chain_fades && matches!(target, GraphTarget::Generator(layer) if effect_follows(project, layer));
                let selection = GizmoSelection::new(&scene, crate::scene_gizmo::selected_targets(&scene, &request.items));
                Chosen { scene, request: request.clone(), chain, selection, fade }
            });
        }
        self.problems.update(scene.as_ref(), Some(frame.domains));
        let (selection, fade_handles) = self.chosen.as_ref().map_or((&NO_SELECTION, false), |chosen| (&chosen.selection, chosen.fade));
        compose_lines(
            &GizmoChrome {
                view: frame.view,
                lights: frame.lights,
                domains: frame.domains,
                live_columns: frame.live_columns,
                physics: frame.physics,
                problems: self.problems.problems(),
                selection,
                mode: request.mode,
                fade_handles,
            },
            &mut self.lines,
        );
        &self.lines
    }

    pub fn lines(&self) -> &[WorldLine] {
        &self.lines
    }
}

#[cfg(target_os = "macos")]
pub(crate) use capture::GizmoCapture;

#[cfg(target_os = "macos")]
mod capture {
    use manifold_compositor::generator_renderer::GeneratorRenderer;
    use manifold_core::change::PartStamps;
    use manifold_core::project::Project;
    use manifold_core::{GraphTarget, LayerId};
    use manifold_core::scene_surface::GizmoDrawMode;
    use manifold_gpu::{GpuDevice, GpuEncoder, GpuTexture};
    use manifold_node_engine::scene::camera::Camera;
    use manifold_nodes_scene::node_graph::gizmo_pass::GizmoPass;
    use manifold_nodes_scene::node_graph::viewport_gizmo::{GizmoChrome, GizmoMode, GizmoView, compose_lines};
    use manifold_nodes_scene::node_graph::viewport_overlay::WorldLine;
    use std::cell::RefCell;

    use super::{FrameChrome, GizmoOverlay, SceneLines};
    use crate::physics_overlay::PhysicsOverlayFrame;
    use crate::scene_viewport::{SceneViewportFrame, ViewportHost};

    /// The content thread's gizmo state: the standing overlay, the lines
    /// built this frame, the gizmo pass, and the depth the scenes kept.
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
        /// The output scene's graph target, kept so a frame clones nothing.
        output_target: Option<GraphTarget>,
        output: SceneLines,
        /// True when `output` holds this frame's lines.
        output_ready: bool,
        viewport: SceneLines,
        viewport_ready: bool,
    }

    impl GizmoCapture {
        pub fn set(&mut self, overlay: GizmoOverlay) {
            let layer = overlay.output.as_ref().map(|(layer, _)| layer);
            let kept = match &self.output_target {
                Some(GraphTarget::Generator(kept)) => Some(kept),
                _ => None,
            };
            if kept != layer {
                self.output_target = layer.map(|layer| GraphTarget::Generator(layer.clone()));
            }
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

        /// This frame's output lines, built from the project the frame
        /// rendered and the frame's own camera, lights, domains and physics
        /// shapes. `size` is the output the camera rendered at. Nothing when
        /// the request belongs to another scene than the frame.
        pub fn compose_output(&mut self, project: Option<&Project>, stamps: &PartStamps, frame: Option<&PhysicsOverlayFrame>, size: (u32, u32)) {
            self.output_ready = false;
            let (Some(project), Some((layer, request)), Some(frame), Some(target)) = (project, self.overlay.output.as_ref(), frame, self.output_target.as_ref()) else { return };
            if &frame.layer != layer {
                return;
            }
            self.output.compose(
                project,
                stamps,
                target,
                request,
                true,
                FrameChrome {
                    view: GizmoView::new(frame.camera, size.0, size.1),
                    lights: &frame.lights,
                    domains: &frame.domains,
                    live_columns: &frame.live_columns,
                    physics: &frame.gizmos,
                },
            );
            self.output_ready = true;
        }

        /// This frame's viewport lines, built from the project the frame
        /// rendered. The output preview's navigated view carries the output
        /// frame's lights, solver reports and physics shapes (`output`), the
        /// graph editor's carries its own frame's domains.
        pub fn compose_viewport(
            &mut self,
            project: Option<&Project>,
            stamps: &PartStamps,
            frame: &SceneViewportFrame,
            output: Option<&PhysicsOverlayFrame>,
        ) {
            self.viewport_ready = false;
            let (Some(project), Some((host, request))) = (project, self.overlay.viewport.as_ref()) else { return };
            let scene = &frame.request;
            if *host != scene.host {
                return;
            }
            let view = GizmoView::new(scene.config.camera.to_camera(), scene.config.width, scene.config.height);
            let chrome = match scene.host {
                ViewportHost::OutputPreview => {
                    let Some(output) = output.filter(|output| matches!(&scene.target, GraphTarget::Generator(layer) if layer == &output.layer)) else { return };
                    FrameChrome { view, lights: &output.lights, domains: &output.domains, live_columns: &output.live_columns, physics: &output.gizmos }
                }
                ViewportHost::GraphEditor => FrameChrome { view, lights: &[], domains: &frame.domains, live_columns: &frame.live_columns, physics: &[] },
            };
            self.viewport.compose(project, stamps, &scene.target, request, scene.host == ViewportHost::OutputPreview, chrome);
            self.viewport_ready = true;
        }

        /// The output preview's lines over `target`, through the camera the
        /// scene rendered with.
        pub fn draw_output(&self, device: &GpuDevice, enc: &mut GpuEncoder, target: &GpuTexture, frame: Option<&PhysicsOverlayFrame>) {
            let (true, Some(frame)) = (self.output_ready, frame) else { return };
            self.pass.borrow_mut().draw(device, enc, target, self.frame_depth.as_ref(), &frame.camera, self.output.lines(), self.overlay.mode);
        }

        /// The viewport frame's lines over `target`, when `host` shows them.
        pub fn draw_viewport(&self, device: &GpuDevice, enc: &mut GpuEncoder, target: &GpuTexture, host: ViewportHost, depth: Option<&GpuTexture>, camera: &Camera) {
            let Some((lines_host, _)) = self.overlay.viewport.as_ref() else { return };
            if !self.viewport_ready || *lines_host != host {
                return;
            }
            self.pass.borrow_mut().draw(device, enc, target, depth, camera, self.viewport.lines(), self.overlay.mode);
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
    /// Send what the gizmos should show when it moves: the draw mode, the
    /// output preview's handles while Show gizmos is on, and the scene
    /// viewport's from whichever host owns it (the preview while it
    /// navigates, else the graph editor). Only the selection goes: the
    /// content thread builds the lines in the frame that renders the scene.
    pub(crate) fn sync_gizmo_overlay(&mut self) {
        let panel = &self.ws.ui_root.scene_setup_panel;
        let (mode, scene_layer) = (panel.gizmo_draw_mode(), panel.live_layer_id().cloned());
        let kept = &self.ws.gizmo_sent;
        let preview = self.ws.preview_gizmo.shown().filter(|_| panel.gizmo_layer().is_some()).map(|frame| (frame.layer.clone(), self.ws.preview_gizmo.request(panel, kept.output.as_ref().map(|(_, request)| request))));
        let (output, viewport) = if self.ws.preview_navigation.active() {
            (None, preview.map(|(_, request)| (ViewportHost::OutputPreview, request)))
        } else {
            let editor_layer = match self.watched_graph_target.as_ref() {
                Some(GraphTarget::Generator(layer)) => Some(layer),
                _ => None,
            };
            let editor = self.graph_editor.as_ref().filter(|ed| ed.viewport_session.is_some()).zip(editor_layer);
            let editor = editor.map(|(ed, layer)| {
                let kept = kept.viewport.as_ref().map(|(_, request)| request);
                (ViewportHost::GraphEditor, GizmoRequest::panel(ed.viewport_gizmo_mode, panel.selected_of(layer), kept))
            });
            (preview, editor)
        };
        let overlay = GizmoOverlay { mode, scene_layer, output, viewport };
        if overlay == self.ws.gizmo_sent {
            return;
        }
        self.ws.gizmo_sent = overlay.clone();
        self.send_content_cmd(crate::content_command::ContentCommand::SetGizmoOverlay(Box::new(overlay)));
    }
}
