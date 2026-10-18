//! The Scene panel's "Show gizmos" overlay on the main window's output
//! preview (`docs/REALTIME_3D_DESIGN.md` D7a). Editor chrome only: the
//! content thread reports, per output surface, the camera the panel's scene
//! rendered through, its light positions and its accepted fluid domains; the
//! UI projects the ground grid, light markers and domain boxes onto the
//! preview. Nothing here feeds the render.

use std::sync::Arc;

use manifold_core::fluid_domain::{FluidDomainLayout, FluidDomainSnapshot, FluidDomainState};
use manifold_core::physics_gizmo::PhysicsGizmo;
use manifold_core::{LayerId, NodeId};
use manifold_node_engine::scene::camera::Camera;
use manifold_nodes_scene::node_graph::viewport_overlay::{
    PHYSICS_GIZMO_LINES, ViewportOverlayConfig, WorldLine, fluid_domain_lines, light_billboard_lines,
    physics_gizmo_lines,
};

type Domains = [(NodeId, FluidDomainSnapshot)];
type Lights = [[f32; 3]];
type Gizmos = [PhysicsGizmo];

/// What one output surface's scene was rendered with.
#[derive(Clone)]
pub(crate) struct PhysicsOverlayFrame {
    pub layer: LayerId,
    /// The content frame that wrote the surface; matched against the frame
    /// the preview bridge published so lines never sit on another frame.
    pub frame: u64,
    pub camera: Camera,
    pub domains: Arc<Domains>,
    pub lights: Arc<Lights>,
    /// Forces, colliders, bodies, sources, drains and the fill, as the
    /// simulation used them.
    pub gizmos: Arc<Gizmos>,
}

impl PhysicsOverlayFrame {
    pub fn matches(&self, layer: Option<&LayerId>, frame: u64) -> bool {
        self.frame == frame && Some(&self.layer) == layer
    }

    /// Every domain the simulation accepted this frame, as its box.
    pub fn accepted_domains(&self) -> impl Iterator<Item = FluidDomainLayout> + '_ {
        self.domains.iter()
            .filter(|(_, snapshot)| snapshot.state == FluidDomainState::Ready)
            .filter_map(|(_, snapshot)| snapshot.accepted_layout)
    }

    /// Hand every gizmo to `draw`, a batch at a time: the ground grid, a
    /// marker per light, a box per accepted fluid domain, then one wireframe
    /// per physics setup shape. No camera frustum: the preview already looks
    /// through that camera.
    pub fn for_each_gizmo(&self, grid: &[WorldLine], mut draw: impl FnMut(&[WorldLine])) {
        draw(grid);
        let size = ViewportOverlayConfig::default().light_billboard_size;
        for &pos in self.lights.iter() {
            draw(&light_billboard_lines(pos, size));
        }
        for layout in self.accepted_domains() {
            draw(&fluid_domain_lines(layout));
        }
        let mut lines = [WorldLine { a: [0.0; 3], b: [0.0; 3], color: [0; 4] }; PHYSICS_GIZMO_LINES];
        for gizmo in self.gizmos.iter() {
            let count = physics_gizmo_lines(gizmo, &mut lines);
            draw(&lines[..count]);
        }
    }
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
    pub fn set_layer(&mut self, layer: Option<LayerId>) {
        if layer != self.layer {
            self.frames.fill(None);
            self.layer = layer;
        }
    }

    /// Record what surface `slot` was rendered with on content frame `frame`.
    /// `observe` appends the layer's domains, light positions and physics
    /// setup shapes and returns
    /// the camera, or `None` when the layer did not render this frame, which
    /// clears the slot.
    pub fn record(
        &mut self,
        slot: usize,
        frame: u64,
        observe: impl FnOnce(&LayerId, &mut Vec<(NodeId, FluidDomainSnapshot)>, &mut Vec<[f32; 3]>, &mut Vec<PhysicsGizmo>) -> Option<Camera>,
    ) {
        self.frames[slot] = None;
        let Some(layer) = self.layer.as_ref() else { return };
        self.domain_scratch.clear();
        self.light_scratch.clear();
        self.gizmo_scratch.clear();
        let Some(camera) = observe(layer, &mut self.domain_scratch, &mut self.light_scratch, &mut self.gizmo_scratch) else { return };
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
            camera,
            domains: self.domains.clone(),
            lights: self.lights.clone(),
            gizmos: self.gizmos.clone(),
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
    fn show_gizmos_draws_grid_lights_and_accepted_domains() {
        let layout = FluidDomainLayout { min: [0.0; 3], size: [1.0; 3], cells: [8; 3], cell_size: 0.125 };
        let ready = FluidDomainSnapshot { epoch: 1, state: FluidDomainState::Ready, accepted_layout: Some(layout) };
        let pending = FluidDomainSnapshot { state: FluidDomainState::Initializing, ..ready };
        let mut observations = PhysicsOverlayObservations::default();
        observations.set_layer(Some(LayerId::new("scene")));
        observations.record(1, 3, |_, domains, lights, _| {
            domains.extend([(NodeId::new("a"), ready), (NodeId::new("b"), pending)]);
            lights.extend([[0.0, 4.0, 0.0], [2.0, 1.0, -1.0]]);
            Some(Camera::default_perspective())
        });
        let frame = observations.frames[1].as_ref().unwrap();
        let grid = manifold_nodes_scene::node_graph::viewport_overlay::grid_lines(2.0, 1.0);
        let mut batches = Vec::new();
        frame.for_each_gizmo(&grid, |lines| batches.push(lines.len()));
        // The grid, two 3-line light crosses, one 12-edge box (the pending
        // domain draws nothing).
        assert_eq!(batches, [grid.len(), 3, 3, 12]);
    }

    #[test]
    fn show_gizmos_draws_each_physics_shape_after_the_domains() {
        use manifold_core::physics_gizmo::{PhysicsGizmoFamily, PhysicsGizmoShape};
        let collider = PhysicsGizmo::aabb(PhysicsGizmoFamily::Collider, [0.0; 3], [1.0; 3]);
        let force = PhysicsGizmo { family: PhysicsGizmoFamily::Force, shape: PhysicsGizmoShape::Sphere { center: [0.0; 3], radius: 1.0 } };
        let wind = PhysicsGizmo { family: PhysicsGizmoFamily::Force, shape: PhysicsGizmoShape::Arrow { from: [0.0; 3], to: [1.0, 0.0, 0.0] } };
        let mut observations = PhysicsOverlayObservations::default();
        observations.set_layer(Some(LayerId::new("water")));
        let observe = |gizmos: &mut Vec<PhysicsGizmo>| {
            gizmos.extend([collider, force, wind]);
            Some(Camera::default_perspective())
        };
        observations.record(0, 1, |_, _, _, gizmos| observe(gizmos));
        let first = observations.frames[0].as_ref().unwrap().gizmos.clone();
        let mut batches = Vec::new();
        observations.frames[0].as_ref().unwrap().for_each_gizmo(&[], |lines| if let Some(line) = lines.first() { batches.push((lines.len(), line.color)) });
        let color = manifold_nodes_scene::node_graph::viewport_overlay::physics_gizmo_color;
        assert_eq!(batches, [
            (12, color(PhysicsGizmoFamily::Collider)),
            (PHYSICS_GIZMO_LINES, color(PhysicsGizmoFamily::Force)),
            (5, color(PhysicsGizmoFamily::Force)),
        ]);
        // An unchanged setup publishes no new snapshot.
        observations.record(1, 2, |_, _, _, gizmos| observe(gizmos));
        assert!(Arc::ptr_eq(&first, &observations.frames[1].as_ref().unwrap().gizmos));
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
        observations.record(2, 41, |_, _, _, _| Some(Camera::default_perspective()));
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
            Some(Camera::default_perspective())
        });
        let frame = observations.frames[0].as_ref().expect("a rendered layer reports");
        assert!(frame.matches(Some(&layer), 7) && !frame.matches(Some(&layer), 8));

        // The clip ended or the layer was occluded: the same slot's next write
        // must not keep last frame's camera, lights or boxes.
        observations.record(0, 10, |_, _, _, _| None);
        assert!(observations.frames[0].is_none());
    }
}
