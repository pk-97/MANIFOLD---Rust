//! Physics gizmos over the editor's master preview (`docs/REALTIME_3D_DESIGN.md`
//! D7a). Editor chrome only: the content thread reports, per output surface,
//! the camera the toggled scene rendered through, and the UI projects
//! world-space lines through it onto the master pane. Nothing here feeds the
//! render.

use std::sync::Arc;

use manifold_core::fluid_domain::FluidDomainSnapshot;
use manifold_core::{GraphTarget, LayerId, NodeId};
use manifold_node_engine::scene::camera::Camera;
use manifold_nodes_scene::node_graph::viewport_overlay::WorldLine;

type Domains = [(NodeId, FluidDomainSnapshot)];

/// What one output surface's scene was rendered with.
#[derive(Clone)]
pub(crate) struct PhysicsOverlayFrame {
    pub layer: LayerId,
    pub camera: Camera,
    pub domains: Arc<Domains>,
    /// Moves whenever `domains` changes.
    pub domains_version: u64,
}

/// One entry per output surface, indexed like the master preview bridge, so
/// the lines match the exact frame the pane shows.
pub(crate) type PhysicsOverlayFrames = [Option<PhysicsOverlayFrame>; 3];

/// Content-thread side. The domain buffer is reused each frame; a new
/// published snapshot is allocated only when the accepted domains change.
pub(crate) struct PhysicsOverlayObservations {
    layer: Option<LayerId>,
    scratch: Vec<(NodeId, FluidDomainSnapshot)>,
    domains: Arc<Domains>,
    domains_version: u64,
    pub frames: PhysicsOverlayFrames,
}

impl Default for PhysicsOverlayObservations {
    fn default() -> Self {
        Self { layer: None, scratch: Vec::new(), domains: Arc::from([]), domains_version: 0, frames: std::array::from_fn(|_| None) }
    }
}

impl PhysicsOverlayObservations {
    pub fn set_layer(&mut self, layer: Option<LayerId>) {
        if layer != self.layer {
            self.frames.fill(None);
            self.layer = layer;
        }
    }

    /// Record what surface `slot` was rendered with. Call after the
    /// generators rendered this frame.
    pub fn record(&mut self, slot: usize, generators: Option<&manifold_compositor::generator_renderer::GeneratorRenderer>) {
        self.frames[slot] = None;
        let (Some(layer), Some(generators)) = (self.layer.as_ref(), generators) else { return };
        self.scratch.clear();
        let Some(camera) = generators.write_physics_overlay(layer, &mut self.scratch) else { return };
        if self.scratch.as_slice() != self.domains.as_ref() {
            self.domains = Arc::from(self.scratch.as_slice());
            self.domains_version += 1;
        }
        self.frames[slot] = Some(PhysicsOverlayFrame { layer: layer.clone(), camera, domains: self.domains.clone(), domains_version: self.domains_version });
    }
}

/// UI side: the overlay's world-space lines, rebuilt only when the scene's
/// authored graph or its accepted domains change.
#[derive(Default)]
pub(crate) struct PhysicsOverlayLines {
    key: Option<(LayerId, u64, u64, u64)>,
    lines: Vec<WorldLine>,
}

impl PhysicsOverlayLines {
    pub fn update(
        &mut self,
        project: &manifold_core::project::Project,
        frame: &PhysicsOverlayFrame,
        parts_key: u64,
    ) -> &[WorldLine] {
        let generation = manifold_node_engine::load::preset_loader::catalog_generation();
        let fresh = self.key.as_ref().is_some_and(|(layer, parts, catalog, version)| {
            *layer == frame.layer && *parts == parts_key && *catalog == generation && *version == frame.domains_version
        });
        if !fresh {
            self.lines.clear();
            let target = GraphTarget::Generator(frame.layer.clone());
            if let Some(def) = crate::fluid_domain_edit::authored_def(project, &target)
                && let Some(mut scene) = manifold_nodes_scene::node_graph::scene_vm::SceneVm::from_def(&def)
            {
                crate::fluid_domain_edit::apply_runtime_domains(&mut scene, &frame.domains);
                manifold_nodes_scene::node_graph::viewport_overlay::physics_overlay_lines(&scene, &mut self.lines);
            }
            self.key = Some((frame.layer.clone(), parts_key, generation, frame.domains_version));
        }
        &self.lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::fluid_domain::{FluidDomainLayout, FluidDomainState};
    use manifold_core::{PresetTypeId, project::Project, types::LayerType};
    use manifold_nodes_scene::node_graph::scene_vm::{SceneObjectVm, SceneVm};

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
    fn every_accepted_fluid_domain_draws_one_box_and_unaccepted_draws_none() {
        let mut project = Project::default();
        let index = project.timeline.add_layer(
            "Fluid",
            LayerType::Generator,
            PresetTypeId::from_string("WaterDamBreakGpuFlip".to_string()),
        );
        let layer = project.timeline.layers[index].layer_id.clone();
        let def = crate::fluid_domain_edit::authored_def(&project, &GraphTarget::Generator(layer.clone())).unwrap();
        let scene = SceneVm::from_def(&def).unwrap();
        let domain = scene.objects.iter().find_map(|object| match object {
            SceneObjectVm::Known(row) => row.liquid_domain.as_ref().map(|domain| domain.node.clone()),
            _ => None,
        }).expect("the dam break has a fluid");
        let layout = FluidDomainLayout { min: [-1.0, 0.0, -2.0], size: [2.0, 3.0, 4.0], cells: [8, 8, 8], cell_size: 0.25 };
        let ready = FluidDomainSnapshot { epoch: 1, state: FluidDomainState::Ready, accepted_layout: Some(layout) };
        let mut frame = PhysicsOverlayFrame {
            layer,
            camera: Camera::default_perspective(),
            domains: Arc::from([(domain, ready)]),
            domains_version: 1,
        };
        let mut lines = PhysicsOverlayLines::default();
        let drawn = lines.update(&project, &frame, 0);
        assert_eq!(drawn.len(), 12);
        assert!(drawn.iter().all(|line| (0..3).all(|i| line.a[i] >= layout.min[i] && line.a[i] <= layout.min[i] + layout.size[i])));

        frame.domains = Arc::from([]);
        frame.domains_version = 2;
        assert!(lines.update(&project, &frame, 0).is_empty(), "a domain the runtime has not accepted draws nothing");
    }
}
