//! The Scene panel's "Show gizmos" host on the main window's output preview
//! (`docs/REALTIME_3D_DESIGN.md` D7a). Editor chrome only: the content
//! thread reports, per output surface, the camera the panel's scene rendered
//! through, its light positions, its accepted fluid domains and the physics
//! shapes it simulated ([`PhysicsOverlayFrame`]); [`PreviewGizmo`] draws
//! them with the selected item's handles through the gizmo core's one
//! composition entry, and picks and drags through the same core and scrub
//! path as the graph editor's viewport. Nothing here feeds the render.

use std::sync::Arc;

use manifold_core::change::{ChangeSet, Part, PartStamps};
use manifold_core::fluid_domain::FluidDomainSnapshot;
use manifold_core::physics_gizmo::PhysicsGizmo;
use manifold_core::project::Project;
use manifold_core::{GraphTarget, LayerId, NodeId};
use manifold_node_engine::scene::camera::Camera;
use manifold_nodes_scene::node_graph::scene_vm::SceneVm;
use manifold_nodes_scene::node_graph::viewport_gizmo::{
    GizmoChrome, GizmoMode, GizmoTarget, GizmoTargetId, compose_lines,
};
use manifold_nodes_scene::node_graph::viewport_overlay::WorldLine;
use manifold_ui::panels::scene_setup_panel::SceneSelection;
use manifold_ui::{Rect, Vec2};

use crate::scene_gizmo::{GizmoGesture, GizmoPress, GizmoView};
use crate::ui_bridge::DispatchCtx;

type Domains = [(NodeId, FluidDomainSnapshot)];
type Lights = [[f32; 3]];
type Gizmos = [PhysicsGizmo];

/// Lines a host frame draws without reallocating: the grid, a domain box
/// and the largest handle set.
pub(crate) const CHROME_LINES: usize = 128;

/// What one output surface's scene was rendered with.
#[derive(Clone)]
pub(crate) struct PhysicsOverlayFrame {
    pub layer: LayerId,
    /// The content frame that wrote the surface; matched against the frame
    /// the preview bridge published so lines never sit on another frame.
    pub frame: u64,
    /// The scene node that rendered the picture: what the preview navigates.
    pub scene_node: NodeId,
    pub camera: Camera,
    pub domains: Arc<Domains>,
    pub lights: Arc<Lights>,
    /// Forces, colliders, bodies, sources, drains and the fill, as the
    /// simulation used them.
    pub gizmos: Arc<Gizmos>,
    /// The outline mask bridge generation when this frame drew a selection
    /// mask into the same surface slot (`selection_outline.rs`).
    pub outline: Option<u64>,
}

impl PhysicsOverlayFrame {
    pub fn matches(&self, layer: Option<&LayerId>, frame: u64) -> bool {
        self.frame == frame && Some(&self.layer) == layer
    }
}

/// The main output preview as a gizmo host (D7a): the second host of the
/// gizmo core beside the graph editor's viewport, with the camera of the
/// overlay frame on screen and targets from the Scene panel's selection
/// over `SceneVm`. Lives behind the Show gizmos switch; every drag is the
/// knob's scrub (`scene_gizmo.rs`).
#[derive(Default)]
pub(crate) struct PreviewGizmo {
    mode: GizmoMode,
    /// The shape last picked on the preview. Its handles show while it
    /// belongs to the Scene panel's selection; otherwise the selection's
    /// first shape shows.
    picked: Option<GizmoTargetId>,
    drag: Option<PreviewDrag>,
    /// The preview took the last left press, so W, E and R pick the mode.
    focused: bool,
    /// Moves with the mode and the picked shape: a frame-planner input.
    version: u64,
    /// The overlay frame the last present showed, and the output size it
    /// was rendered at.
    shown: Option<(PhysicsOverlayFrame, (u32, u32))>,
    /// The navigated picture shown instead of the output, if any.
    navigated: Option<GizmoView>,
    scene: crate::editor_graph_cache::EditorGraphCache,
    target: Option<(TargetKey, Option<GizmoTarget>, bool)>,
    lines: Vec<WorldLine>,
    /// Render slots of the selected object, for its outline (D-102); empty
    /// while an effect moves the picture off the scene.
    outline: Vec<u32>,
}

/// What the shown handles read: the scene, the panel selection, this host's
/// version and the parts an effect chain lives in.
type TargetKey = (Arc<SceneVm>, u64, u64, u64);

/// Parts that decide whether an effect follows the scene layer.
const CHAIN_PARTS: ChangeSet = ChangeSet::of(&[Part::Layers, Part::Effects, Part::Settings]);

#[derive(Debug)]
struct PreviewDrag {
    gesture: GizmoGesture,
    /// The picture at the press, used while no matched frame shows.
    view: GizmoView,
}

impl PreviewGizmo {
    /// The render slots the selection outline covers.
    pub fn outline(&self) -> &[u32] {
        &self.outline
    }

    /// Moves when the mode or the picked shape changes.
    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    pub fn set_mode(&mut self, mode: GizmoMode) {
        if mode != self.mode {
            self.mode = mode;
            self.version += 1;
        }
    }

    /// The overlay frame the present is about to draw, or `None` when no
    /// frame matches the picture on screen. Input picks against it.
    /// `navigated` is the preview's navigated picture when it shows instead
    /// of the output (`preview_navigation.rs`): the lines and picks follow
    /// its camera, the output camera stays as reported.
    pub fn show(&mut self, frame: Option<&PhysicsOverlayFrame>, output: (u32, u32), navigated: Option<GizmoView>) {
        self.shown = frame.map(|frame| (frame.clone(), output));
        self.navigated = navigated;
    }

    /// The output frame on screen: what navigation seeds from.
    pub fn shown(&self) -> Option<&PhysicsOverlayFrame> {
        self.shown.as_ref().map(|(frame, _)| frame)
    }

    fn view(&self) -> Option<GizmoView> {
        let (frame, (width, height)) = self.shown.as_ref()?;
        Some(self.navigated.unwrap_or(GizmoView::new(frame.camera, *width, *height)))
    }

    /// The shown frame's chrome and the selected item's handles, through
    /// the gizmo core's composition entry, with the camera to project them
    /// through. Rebuilds the handles only when what they read moved.
    pub fn compose(
        &mut self,
        project: &Project,
        panel: &manifold_ui::panels::scene_setup_panel::ScenePanel,
        parts: &PartStamps,
    ) -> Option<(&[WorldLine], GizmoView)> {
        let view = self.view()?;
        let (frame, _) = self.shown.as_ref()?;
        let layer = frame.layer.clone();
        let target = GraphTarget::Generator(layer.clone());
        let scene = self.scene.viewport_def(project, Some(&target), parts).scene;
        let key = scene.map(|scene| (scene, panel.selection_version(), self.version, parts.key(CHAIN_PARTS)));
        let fresh = match (&self.target, &key) {
            (Some((cached, ..)), Some(key)) => Arc::ptr_eq(&cached.0, &key.0) && cached.1 == key.1 && cached.2 == key.2 && cached.3 == key.3,
            _ => false,
        };
        if !fresh {
            self.target = key.map(|key| {
                let selected = panel
                    .live_selection()
                    .and_then(|selection| crate::scene_gizmo::selected_target(&key.0, selection, self.picked.as_ref()));
                let fade = effect_follows(project, &layer);
                self.outline.clear();
                if let Some(SceneSelection::Object(object)) = panel.live_selection().filter(|_| !fade) {
                    crate::selection_outline::outline_slots(&key.0, *object, &mut self.outline);
                }
                (key, selected, fade)
            });
            if self.target.is_none() {
                self.outline.clear();
            }
        }
        let (selected, fade_handles) = self.target.as_ref().map_or((None, false), |(_, target, fade)| (target.as_ref(), *fade));
        let (frame, _) = self.shown.as_ref()?;
        let chrome = GizmoChrome {
            view,
            lights: &frame.lights,
            domains: &frame.domains,
            physics: &frame.gizmos,
            selected,
            mode: self.mode,
            fade_handles,
        };
        compose_lines(&chrome, &mut self.lines);
        Some((&self.lines, view))
    }

    /// A left press at `pos` in the window. `fit` is where the show picture
    /// sits (`ui_frame::video_fit_rect`). True when the preview took it: a
    /// handle grabbed or refused, or a shape picked. Every press inside the
    /// picture focuses the preview; one outside unfocuses it.
    pub fn press(&mut self, ctx: &mut DispatchCtx, fit: Option<Rect>, pos: Vec2) -> bool {
        let layer = ctx.ui.scene_setup_panel.gizmo_layer().cloned();
        // An open overlay over the picture takes its own presses.
        let inside = fit.is_some_and(|fit| fit.contains(pos)) && !ctx.ui.overlay_contains_point(pos);
        self.focused = inside && layer.is_some();
        let (Some(layer), Some(fit), Some(view)) = (layer, fit.filter(|_| inside), self.view()) else { return false };
        let Some((view, point)) = crate::viewport_input::gizmo_pointer(fit, view, (pos.x, pos.y)) else { return false };
        let graph = GraphTarget::Generator(layer.clone());
        let Some(scene) = crate::scene_gizmo::authored_def(&ctx.project, &graph).as_ref().and_then(SceneVm::from_def) else {
            return false;
        };
        let selected = ctx
            .ui
            .scene_setup_panel
            .live_selection()
            .and_then(|selection| crate::scene_gizmo::selected_target(&scene, selection, self.picked.as_ref()))
            .map(|target| target.id);
        match crate::scene_gizmo::press(ctx, &layer, &scene, selected.as_ref(), self.mode, &view, point) {
            GizmoPress::Handle(gesture) => {
                self.drag = gesture.map(|gesture| PreviewDrag { gesture, view });
                true
            }
            GizmoPress::Picked(Some(id)) => {
                if self.picked.as_ref() != Some(&id) {
                    self.picked = Some(id);
                    self.version += 1;
                }
                true
            }
            GizmoPress::Picked(None) => false,
        }
    }

    /// Pointer motion during a drag: the knob scrub moves live. True when a
    /// drag took it.
    pub fn drag_to(&mut self, ctx: &mut DispatchCtx, fit: Option<Rect>, pos: Vec2) -> bool {
        let view = self.view();
        let Some(drag) = self.drag.as_mut() else { return false };
        // Follow the camera frame for frame; hold the press's while no
        // matched frame shows.
        if let Some(view) = view {
            drag.view = view;
        }
        let view = drag.view;
        if let Some((view, point)) = fit.and_then(|fit| crate::viewport_input::gizmo_pointer(fit, view, (pos.x, pos.y))) {
            drag.gesture.drag_to(ctx, &view, point);
        }
        true
    }

    /// Release, or the drag is cut short (Escape, perform mode): one undo
    /// entry for the whole drag. True when a drag ended.
    pub fn release(&mut self, ctx: &mut DispatchCtx) -> bool {
        let Some(drag) = self.drag.take() else { return false };
        drag.gesture.commit(ctx);
        true
    }

    /// W, E or R with the preview focused picks the gizmo mode, as in the
    /// graph editor's viewport. True when the key was taken.
    pub fn mode_key(&mut self, key: &str) -> bool {
        let mode = if key.eq_ignore_ascii_case("w") {
            GizmoMode::Move
        } else if key.eq_ignore_ascii_case("e") {
            GizmoMode::Rotate
        } else if key.eq_ignore_ascii_case("r") {
            GizmoMode::Scale
        } else {
            return false;
        };
        if !self.focused {
            return false;
        }
        self.set_mode(mode);
        true
    }

    /// The shown handles as screen points in the window, for the ui-snap
    /// runner's `gizmo` surface: each grip, labelled `"<Mode> <Axis>"`, the
    /// shape's origin, and `"Outline"` at the origin while the selected
    /// object asks for one. Call after [`Self::compose`].
    #[cfg(feature = "ui-snapshot")]
    pub fn grips(&self, fit: Rect) -> Vec<(String, Vec2)> {
        let (Some(view), Some((_, Some(target), _))) = (self.view(), self.target.as_ref()) else { return Vec::new() };
        let at = |point: [f32; 3]| {
            let p = view.camera.project_to_pixel(point, view.width, view.height)?;
            Some(Vec2::new(fit.x + p.px * fit.width / view.width as f32, fit.y + p.py * fit.height / view.height as f32))
        };
        let mut out: Vec<_> = at(target.origin).map(|p| ("Origin".to_string(), p)).into_iter().collect();
        if !self.outline.is_empty()
            && let Some(p) = at(target.origin)
        {
            out.push(("Outline".to_string(), p));
        }
        use manifold_nodes_scene::node_graph::viewport_gizmo::GizmoAxis;
        for axis in [GizmoAxis::X, GizmoAxis::Y, GizmoAxis::Z, GizmoAxis::Uniform] {
            if let Some(p) = target.grip(self.mode, axis, &view).and_then(at) {
                out.push((format!("{:?} {:?}", self.mode, axis), p));
            }
        }
        out
    }
}

/// The preview's handle grips as a hit-test surface the automation dump
/// can address (`UI_AUTOMATION_DESIGN.md` D5): they are drawn immediate-mode,
/// outside any tree. The ui-snap runner is its one reader.
#[cfg(feature = "ui-snapshot")]
pub(crate) struct GizmoHitTargets<'a>(pub &'a [(String, Vec2)]);

#[cfg(feature = "ui-snapshot")]
impl manifold_ui::hit_targets::HitTargets for GizmoHitTargets<'_> {
    fn surface_id(&self) -> &'static str {
        "gizmo"
    }

    fn enumerate(&self, out: &mut Vec<manifold_ui::hit_targets::HitTargetEntry>) {
        for (label, at) in self.0 {
            out.push(manifold_ui::hit_targets::HitTargetEntry {
                kind: if label == "Outline" { "outline" } else { "handle" },
                label: label.clone(),
                rect: Rect::new(at.x - 3.0, at.y - 3.0, 6.0, 6.0),
                payload: label.clone(),
            });
        }
    }
}

/// D-69: an enabled effect on the scene layer, a group it sits in, or the
/// master chain moves the picture off the scene.
fn effect_follows(project: &Project, layer: &LayerId) -> bool {
    let enabled = |effects: &[manifold_core::effects::PresetInstance]| effects.iter().any(|fx| fx.enabled);
    let mut next = Some(layer.clone());
    // A parent chain is at most every layer long.
    for _ in 0..=project.timeline.layers.len() {
        let Some((_, found)) = next.as_ref().and_then(|id| project.timeline.find_layer_by_id(id)) else { break };
        if found.effects.as_deref().is_some_and(enabled) {
            return true;
        }
        next = found.parent_layer_id.clone();
    }
    enabled(&project.settings.master_effects)
}

/// One entry per output surface, indexed like the preview bridge.
pub(crate) type PhysicsOverlayFrames = [Option<PhysicsOverlayFrame>; 3];

/// The newest content frame with a gizmo entry for `layer`, 0 for none. A
/// frame-planner input: an entry arriving after its frame was already shown
/// moves it, so that frame is drawn again with its lines.
pub(crate) fn newest_frame(frames: &PhysicsOverlayFrames, layer: Option<&LayerId>) -> u64 {
    frames.iter().flatten()
        .filter(|entry| Some(&entry.layer) == layer)
        .map(|entry| entry.frame)
        .max()
        .unwrap_or(0)
}

/// Content-thread side. The buffers are reused each frame; a new published
/// snapshot is allocated only when the domains or light positions change.
pub(crate) struct PhysicsOverlayObservations {
    layer: Option<LayerId>,
    domain_scratch: Vec<(NodeId, FluidDomainSnapshot)>,
    light_scratch: Vec<[f32; 3]>,
    gizmo_scratch: Vec<PhysicsGizmo>,
    domains: Arc<Domains>,
    lights: Arc<Lights>,
    gizmos: Arc<Gizmos>,
    pub frames: PhysicsOverlayFrames,
}

impl Default for PhysicsOverlayObservations {
    fn default() -> Self {
        Self {
            layer: None,
            domain_scratch: Vec::new(),
            light_scratch: Vec::new(),
            gizmo_scratch: Vec::new(),
            domains: Arc::from([]),
            lights: Arc::from([]),
            gizmos: Arc::from([]),
            frames: std::array::from_fn(|_| None),
        }
    }
}

impl PhysicsOverlayObservations {
    pub fn layer(&self) -> Option<&LayerId> {
        self.layer.as_ref()
    }

    /// Stamp surface `slot`'s entry with its selection mask generation.
    pub fn set_outline(&mut self, slot: usize, outline: Option<u64>) {
        if let Some(frame) = self.frames[slot].as_mut() {
            frame.outline = outline;
        }
    }

    pub fn set_layer(&mut self, layer: Option<LayerId>) {
        if layer != self.layer {
            self.frames.fill(None);
            self.layer = layer;
        }
    }

    /// Record what surface `slot` was rendered with on content frame `frame`.
    /// `observe` appends the layer's domains, light positions and physics
    /// setup shapes and returns the scene node and camera that rendered it,
    /// or `None` when the layer did not render this frame, which clears the
    /// slot.
    pub fn record(
        &mut self,
        slot: usize,
        frame: u64,
        observe: impl FnOnce(&LayerId, &mut Vec<(NodeId, FluidDomainSnapshot)>, &mut Vec<[f32; 3]>, &mut Vec<PhysicsGizmo>) -> Option<(NodeId, Camera)>,
    ) {
        self.frames[slot] = None;
        let Some(layer) = self.layer.as_ref() else { return };
        self.domain_scratch.clear();
        self.light_scratch.clear();
        self.gizmo_scratch.clear();
        let Some((scene_node, camera)) = observe(layer, &mut self.domain_scratch, &mut self.light_scratch, &mut self.gizmo_scratch) else { return };
        if self.domain_scratch.as_slice() != self.domains.as_ref() {
            self.domains = Arc::from(self.domain_scratch.as_slice());
        }
        if self.light_scratch.as_slice() != self.lights.as_ref() {
            self.lights = Arc::from(self.light_scratch.as_slice());
        }
        if self.gizmo_scratch.as_slice() != self.gizmos.as_ref() {
            self.gizmos = Arc::from(self.gizmo_scratch.as_slice());
        }
        self.frames[slot] = Some(PhysicsOverlayFrame {
            layer: layer.clone(),
            frame,
            scene_node,
            camera,
            domains: self.domains.clone(),
            lights: self.lights.clone(),
            gizmos: self.gizmos.clone(),
            outline: None,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_world_point_lands_on_its_pixel_in_the_preview() {
        // A 16:9 show at 1920x1080 shown in a 320x180 preview at (10, 20).
        let camera = Camera::orbit_perspective(0.7, 0.3, 6.0, std::f32::consts::FRAC_PI_3, 0.5, 0.0, 0.05, 200.0);
        let (right, up, depth) = (1.0_f32, 0.5_f32, 3.0_f32);
        let point = [0, 1, 2].map(|i| camera.pos[i] + depth * camera.fwd[i] + right * camera.right[i] + up * camera.up[i]);
        let rect = manifold_ui::Rect::new(10.0, 20.0, 320.0, 180.0);
        let line = WorldLine { a: point, b: point, color: [255; 4] };
        let [x, y, ..] = crate::scene_viewport::project_line(&camera, 1920, 1080, rect, &line).unwrap();

        // Hand derivation: perspective divide by depth, then NDC to the pane,
        // y down. The horizontal extent scales with the show's aspect.
        let half_h = (std::f32::consts::FRAC_PI_3 * 0.5).tan() * depth;
        let ndc = [right / (half_h * 16.0 / 9.0), up / half_h];
        let expected = [10.0 + (ndc[0] * 0.5 + 0.5) * 320.0, 20.0 + (0.5 - ndc[1] * 0.5) * 180.0];
        assert!((x - expected[0]).abs() < 1e-3 && (y - expected[1]).abs() < 1e-3, "{x},{y} vs {expected:?}");
    }

    #[test]
    fn a_camera_standing_inside_the_grid_keeps_every_line_along_its_view() {
        // The show camera at (0, 2, 6) looking at the origin: every grid line
        // running along z passes beside and behind it.
        let camera = Camera::look_at([0.0, 2.0, 6.0], [0.0; 3], [0.0, 1.0, 0.0], std::f32::consts::FRAC_PI_3, 0.05, 200.0);
        let rect = manifold_ui::Rect::new(0.0, 0.0, 320.0, 180.0);
        let grid = manifold_nodes_scene::node_graph::viewport_overlay::grid_lines(10.0, 1.0);
        let along_z: Vec<_> = grid.iter().filter(|line| line.a[2] != line.b[2]).collect();
        assert_eq!(along_z.len(), 21);
        for line in &along_z {
            let [x0, y0, x1, y1] = crate::scene_viewport::project_line(&camera, 1920, 1080, rect, line)
                .unwrap_or_else(|| panic!("line {line:?} vanished"));
            // The far end projects exactly as an unclipped point would.
            let far = camera.project_to_pixel(line.a, 1920, 1080).unwrap();
            assert!((x0 - far.px / 6.0).abs() < 1e-3 && (y0 - far.py / 6.0).abs() < 1e-3);
            // The clipped near end runs toward the bottom of the picture.
            assert!(y1.is_finite() && x1.is_finite() && y1 > y0);
        }
        // A line wholly behind the camera still draws nothing.
        let behind = WorldLine { a: [-1.0, 0.0, 8.0], b: [1.0, 0.0, 8.0], color: [255; 4] };
        assert!(crate::scene_viewport::project_line(&camera, 1920, 1080, rect, &behind).is_none());
    }

    #[test]
    fn the_preview_fits_the_show_inside_the_video_area() {
        let area = manifold_ui::Rect::new(0.0, 0.0, 400.0, 400.0);
        let fit = crate::ui_frame::video_fit_rect(area, (1920.0, 1080.0)).unwrap();
        assert_eq!((fit.x, fit.y, fit.width, fit.height), (0.0, 87.5, 400.0, 225.0));
    }

    #[test]
    fn an_unchanged_physics_setup_publishes_no_new_snapshot() {
        use manifold_core::physics_gizmo::{PhysicsGizmoFamily, PhysicsGizmoShape};
        let collider = PhysicsGizmo::aabb(PhysicsGizmoFamily::Collider, [0.0; 3], [1.0; 3]);
        let wind = PhysicsGizmo { family: PhysicsGizmoFamily::Force, shape: PhysicsGizmoShape::Arrow { from: [0.0; 3], to: [1.0, 0.0, 0.0] } };
        let mut observations = PhysicsOverlayObservations::default();
        observations.set_layer(Some(LayerId::new("water")));
        let observe = |gizmos: &mut Vec<PhysicsGizmo>| {
            gizmos.extend([collider, wind]);
            Some((NodeId::new("scene"), Camera::default_perspective()))
        };
        observations.record(0, 1, |_, _, _, gizmos| observe(gizmos));
        let first = observations.frames[0].as_ref().unwrap().gizmos.clone();
        assert_eq!(first.as_ref(), [collider, wind]);
        observations.record(1, 2, |_, _, _, gizmos| observe(gizmos));
        assert!(Arc::ptr_eq(&first, &observations.frames[1].as_ref().unwrap().gizmos));
    }

    #[test]
    fn an_effect_after_the_scene_fades_its_handles() {
        let mut project = crate::ui_bridge::force_project("RadialForce", &["force_a"]);
        let layer = LayerId::new("scene");
        assert!(!effect_follows(&project, &layer));
        let mut fx = manifold_core::effects::PresetInstance::new(manifold_core::PresetTypeId::from_string("Mirror".to_string()));
        fx.enabled = true;
        project.settings.master_effects.push(fx);
        assert!(effect_follows(&project, &layer), "the master chain follows every layer");
        project.settings.master_effects[0].enabled = false;
        assert!(!effect_follows(&project, &layer), "a bypassed effect leaves the picture on the scene");
    }

    #[test]
    fn a_late_gizmo_entry_redraws_the_frame_already_shown() {
        let layer = LayerId::new("scene");
        let mut observations = PhysicsOverlayObservations::default();
        observations.set_layer(Some(layer.clone()));
        let inputs = |frames: &PhysicsOverlayFrames| crate::frame_plan::FrameInputs {
            gizmo_frame: newest_frame(frames, Some(&layer)),
            ..Default::default()
        };
        // The surface was published and drawn before its entry arrived.
        let shown = inputs(&observations.frames);
        observations.record(2, 41, |_, _, _, _| Some((NodeId::new("scene"), Camera::default_perspective())));
        let arrived = inputs(&observations.frames);
        assert!(crate::frame_plan::plan_frame(&shown, &arrived).redraw);
        assert!(!crate::frame_plan::plan_frame(&arrived, &arrived).redraw);
        // Another scene's entries never move it.
        assert_eq!(newest_frame(&observations.frames, Some(&LayerId::new("other"))), 0);
    }

    #[test]
    fn a_layer_that_did_not_render_leaves_no_overlay() {
        let layer = LayerId::new("water");
        let mut observations = PhysicsOverlayObservations::default();
        observations.set_layer(Some(layer.clone()));
        observations.record(0, 7, |_, _, lights, _| {
            lights.push([0.0; 3]);
            Some((NodeId::new("scene"), Camera::default_perspective()))
        });
        let frame = observations.frames[0].as_ref().expect("a rendered layer reports");
        assert!(frame.matches(Some(&layer), 7) && !frame.matches(Some(&layer), 8));

        // The clip ended or the layer was occluded: the same slot's next write
        // must not keep last frame's camera, lights or boxes.
        observations.record(0, 10, |_, _, _, _| None);
        assert!(observations.frames[0].is_none());
    }
}
