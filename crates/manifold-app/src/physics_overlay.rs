//! Physics gizmos over the editor's master preview (`docs/REALTIME_3D_DESIGN.md`
//! D7a). Editor chrome only: the content thread reports, per output surface,
//! the camera the toggled scene rendered through and its accepted fluid
//! domains, and the UI projects their boxes onto the master pane. Nothing here
//! feeds the render.

use std::sync::Arc;

use manifold_core::fluid_domain::{FluidDomainSnapshot, FluidDomainState};
use manifold_core::{LayerId, NodeId};
use manifold_node_engine::scene::camera::Camera;

type Domains = [(NodeId, FluidDomainSnapshot)];

/// What one output surface's scene was rendered with.
#[derive(Clone)]
pub(crate) struct PhysicsOverlayFrame {
    pub layer: LayerId,
    /// The content frame that wrote the surface; matched against the frame
    /// the master bridge published so lines never sit on another frame.
    pub frame: u64,
    pub camera: Camera,
    pub domains: Arc<Domains>,
}

impl PhysicsOverlayFrame {
    pub fn matches(&self, layer: Option<&LayerId>, frame: u64) -> bool {
        self.frame == frame && Some(&self.layer) == layer
    }

    /// Every domain the simulation accepted this frame, as its box.
    pub fn accepted_domains(&self) -> impl Iterator<Item = manifold_core::fluid_domain::FluidDomainLayout> + '_ {
        self.domains.iter()
            .filter(|(_, snapshot)| snapshot.state == FluidDomainState::Ready)
            .filter_map(|(_, snapshot)| snapshot.accepted_layout)
    }
}

/// One entry per output surface, indexed like the master preview bridge.
pub(crate) type PhysicsOverlayFrames = [Option<PhysicsOverlayFrame>; 3];

/// Content-thread side. The domain buffer is reused each frame; a new
/// published snapshot is allocated only when the accepted domains change.
pub(crate) struct PhysicsOverlayObservations {
    layer: Option<LayerId>,
    scratch: Vec<(NodeId, FluidDomainSnapshot)>,
    domains: Arc<Domains>,
    pub frames: PhysicsOverlayFrames,
}

impl Default for PhysicsOverlayObservations {
    fn default() -> Self {
        Self { layer: None, scratch: Vec::new(), domains: Arc::from([]), frames: std::array::from_fn(|_| None) }
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
    /// `observe` appends the layer's domains and returns the camera, or
    /// `None` when the layer did not render this frame, which clears the slot.
    pub fn record(
        &mut self,
        slot: usize,
        frame: u64,
        observe: impl FnOnce(&LayerId, &mut Vec<(NodeId, FluidDomainSnapshot)>) -> Option<Camera>,
    ) {
        self.frames[slot] = None;
        let Some(layer) = self.layer.as_ref() else { return };
        self.scratch.clear();
        let Some(camera) = observe(layer, &mut self.scratch) else { return };
        if self.scratch.as_slice() != self.domains.as_ref() {
            self.domains = Arc::from(self.scratch.as_slice());
        }
        self.frames[slot] = Some(PhysicsOverlayFrame { layer: layer.clone(), frame, camera, domains: self.domains.clone() });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::fluid_domain::FluidDomainLayout;
    use manifold_nodes_scene::node_graph::viewport_overlay::WorldLine;

    #[test]
    fn a_world_point_lands_on_its_pixel_in_the_master_pane() {
        // A 16:9 show at 1920x1080 shown in a 320x180 pane at (10, 20).
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
    fn a_layer_that_did_not_render_leaves_no_overlay() {
        let layer = LayerId::new("water");
        let layout = FluidDomainLayout { min: [0.0; 3], size: [1.0; 3], cells: [8; 3], cell_size: 0.125 };
        let ready = FluidDomainSnapshot { epoch: 1, state: FluidDomainState::Ready, accepted_layout: Some(layout) };
        let mut observations = PhysicsOverlayObservations::default();
        observations.set_layer(Some(layer.clone()));

        observations.record(0, 7, |_, domains| {
            domains.push((NodeId::new("domain"), ready));
            Some(Camera::default_perspective())
        });
        let frame = observations.frames[0].as_ref().expect("a rendered layer reports");
        assert!(frame.matches(Some(&layer), 7) && !frame.matches(Some(&layer), 8));
        assert_eq!(frame.accepted_domains().count(), 1);

        // The clip ended or the layer was occluded: the same slot's next write
        // must not keep last frame's camera or boxes.
        observations.record(0, 10, |_, _| None);
        assert!(observations.frames[0].is_none());
    }
}
