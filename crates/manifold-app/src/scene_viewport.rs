//! Transient editor navigation and frame-matched observations of the live scene.
use manifold_core::{GraphTarget, NodeId};
use manifold_renderer::{
    frame_status::FrameRenderStatus,
    node_graph::{
        ViewportCamera, WorldLine,
        fluid::FluidDomainSnapshot,
        scene_viewport::{SceneViewportConfig, SceneViewportHostError},
    },
    preset_runtime::ModifierPreviewContext,
};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

type Domains = [(NodeId, FluidDomainSnapshot)];
pub(crate) type SceneViewportFrames = [Option<SceneViewportFrame>; 3];

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SceneViewportRequest {
    pub session: u64,
    pub target: GraphTarget,
    pub node: NodeId,
    pub config: SceneViewportConfig,
    pub modifier: Option<Arc<ModifierPreviewContext>>,
}

#[derive(Debug, Clone)]
pub(crate) struct SceneViewportFrame {
    pub frame: u64,
    pub surface_generation: u64,
    pub request: Arc<SceneViewportRequest>,
    pub status: Result<FrameRenderStatus, SceneViewportHostError>,
    pub domains: Arc<Domains>,
}

impl SceneViewportFrame {
    pub fn matches(&self, request: &SceneViewportRequest, frame: u64, generation: u64) -> bool {
        self.frame == frame
            && self.surface_generation == generation
            && self.request.session == request.session
            && self.request.target == request.target
            && self.request.node == request.node
            && self.request.modifier == request.modifier
    }

    pub fn has_image(&self) -> bool {
        matches!(self.status, Ok(status) if status.presentable())
    }

    pub fn diagnostic(&self) -> Option<&'static str> {
        match self.status {
            Ok(FrameRenderStatus::Complete) => None,
            Ok(FrameRenderStatus::PendingGeometry) => Some("Waiting for the scene to render…"),
            Ok(FrameRenderStatus::Failed(_)) => {
                Some("The scene could not be rendered. Check its simulation and geometry.")
            }
            Err(error) => Some(error.message()),
        }
    }
}

/// UI owns navigation only. Picking reads `displayed`, never a newer request.
pub(crate) struct SceneViewportNavigation {
    pub session: u64,
    pub config: SceneViewportConfig,
    pub displayed: Option<SceneViewportFrame>,
}

impl SceneViewportNavigation {
    pub fn new(width: u32, height: u32) -> Self {
        static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);
        Self {
            session: NEXT_SESSION.fetch_add(1, Ordering::Relaxed),
            config: SceneViewportConfig {
                camera: ViewportCamera::default(),
                width,
                height,
            },
            displayed: None,
        }
    }

    pub fn camera_mut(&mut self) -> &mut ViewportCamera {
        &mut self.config.camera
    }
    pub fn displayed_config(&self) -> Option<SceneViewportConfig> {
        self.displayed
            .as_ref()
            .filter(|frame| frame.has_image())
            .map(|frame| frame.request.config)
    }

    pub fn request(
        &self,
        target: &GraphTarget,
        node: &NodeId,
        modifier: Option<&(
            Vec<NodeId>,
            Option<manifold_core::scene_modifier_preset::SceneNodeRef>,
        )>,
        previous: Option<&Arc<SceneViewportRequest>>,
    ) -> Arc<SceneViewportRequest> {
        let modifier_parts = match target {
            GraphTarget::SceneModifier { modifier_id, .. } => Some((
                modifier_id,
                modifier.map_or(&[][..], |(scope, _)| scope.as_slice()),
                modifier.and_then(|(_, object)| object.as_ref()),
            )),
            _ => None,
        };
        if let Some(previous) = previous
            && previous.session == self.session
            && previous.target == *target
            && previous.node == *node
            && previous.config == self.config
            && previous
                .modifier
                .as_deref()
                .map(|m| (&m.modifier_id, m.scope.as_slice(), m.object.as_ref()))
                == modifier_parts
        {
            return previous.clone();
        }
        Arc::new(SceneViewportRequest {
            session: self.session,
            target: target.clone(),
            node: node.clone(),
            config: self.config,
            modifier: modifier_parts.map(|(modifier_id, scope, object)| {
                Arc::new(ModifierPreviewContext {
                    modifier_id: modifier_id.clone(),
                    scope: scope.to_vec(),
                    object: object.cloned(),
                })
            }),
        })
    }
}

/// Reuse the domain buffer each frame; allocate a published snapshot only when
/// accepted layout/epoch/readiness changes, not when simulation time advances.
pub(crate) struct SceneViewportObservations {
    pub scratch: Vec<(NodeId, FluidDomainSnapshot)>,
    domains: Arc<Domains>,
    pub frames: SceneViewportFrames,
}

impl Default for SceneViewportObservations {
    fn default() -> Self {
        Self {
            scratch: Vec::new(),
            domains: Arc::from([]),
            frames: std::array::from_fn(|_| None),
        }
    }
}

impl SceneViewportObservations {
    pub fn record(
        &mut self,
        slot: usize,
        frame: u64,
        surface_generation: u64,
        request: Arc<SceneViewportRequest>,
        status: Result<FrameRenderStatus, SceneViewportHostError>,
    ) {
        if self.scratch.as_slice() != self.domains.as_ref() {
            self.domains = Arc::from(self.scratch.as_slice());
        }
        self.frames[slot] = Some(SceneViewportFrame {
            frame,
            surface_generation,
            request,
            status,
            domains: self.domains.clone(),
        });
    }
}

pub(crate) fn texture_handle() -> manifold_ui::node::TextureHandle {
    manifold_ui::node::texture_handle_for_key("__manifold_shared_scene_viewport__")
}

pub(crate) struct SceneViewportPaint<'a> {
    pub rect: manifold_ui::Rect,
    pub frame: &'a SceneViewportFrame,
    pub lines: &'a [WorldLine],
}

impl SceneViewportPaint<'_> {
    pub fn draw(&self, ui: &mut manifold_renderer::ui_renderer::UIRenderer) {
        use manifold_renderer::ui_renderer::Depth;
        let r = self.rect;
        ui.push_depth(Depth::CONTENT);
        ui.push_immediate_clip(r.x, r.y, r.width, r.height);
        ui.draw_image_uv(
            r.x,
            r.y,
            r.width,
            r.height,
            texture_handle(),
            [0.0, 0.0, 1.0, 1.0],
            0.0,
        );
        ui.push_depth(Depth(Depth::CONTENT.0 + 1));
        let config = self.frame.request.config;
        let camera = config.camera.to_camera();
        for line in self.lines {
            if let Some(a) = camera.project_to_pixel(line.a, config.width, config.height)
                && let Some(b) = camera.project_to_pixel(line.b, config.width, config.height)
            {
                ui.draw_line(
                    r.x + a.px * r.width / config.width as f32,
                    r.y + a.py * r.height / config.height as f32,
                    r.x + b.px * r.width / config.width as f32,
                    r.y + b.py * r.height / config.height as f32,
                    1.0,
                    manifold_ui::node::Color32::new(
                        line.color[0],
                        line.color[1],
                        line.color[2],
                        line.color[3],
                    ),
                );
            }
        }
        ui.pop_depth();
        ui.pop_immediate_clip();
        ui.pop_depth();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_renderer::node_graph::fluid::{FluidDomainState, FluidSettings};

    fn target() -> GraphTarget {
        GraphTarget::Generator(manifold_core::LayerId::new("water-layer"))
    }

    #[test]
    fn scene_viewport_reuses_requests_but_reopen_has_a_new_identity() {
        let mut navigation = SceneViewportNavigation::new(320, 200);
        let node = NodeId::new("scene");
        let first = navigation.request(&target(), &node, None, None);
        let same = navigation.request(&target(), &node, None, Some(&first));
        assert!(Arc::ptr_eq(&first, &same));
        navigation.camera_mut().orbit(20.0, 0.0, 0.01);
        let moved = navigation.request(&target(), &node, None, Some(&same));
        assert!(!Arc::ptr_eq(&same, &moved));
        assert_eq!(same.session, moved.session);
        let reopened =
            SceneViewportNavigation::new(320, 200).request(&target(), &node, None, Some(&first));
        assert_ne!(first.session, reopened.session);
    }

    #[test]
    fn scene_viewport_matches_the_leased_frame_and_uses_its_camera() {
        let mut navigation = SceneViewportNavigation::new(320, 200);
        let node = NodeId::new("scene");
        let old_request = navigation.request(&target(), &node, None, None);
        let frame = SceneViewportFrame {
            frame: 20,
            surface_generation: 2,
            request: old_request.clone(),
            status: Ok(FrameRenderStatus::Complete),
            domains: Arc::from([]),
        };
        navigation.camera_mut().orbit(80.0, 40.0, 0.01);
        let request = navigation.request(&target(), &node, None, Some(&old_request));
        assert!(
            frame.matches(&request, 20, 2),
            "older camera frame stays usable with its own projection"
        );
        assert!(!frame.matches(&request, 21, 2));
        assert!(!frame.matches(&request, 20, 3));
        let mut other = (*request).clone();
        other.target = GraphTarget::Generator(manifold_core::LayerId::new("other-layer"));
        assert!(!frame.matches(&other, 20, 2));
        other = (*request).clone();
        other.session += 1;
        assert!(!frame.matches(&other, 20, 2));
        navigation.displayed = Some(frame);
        assert_eq!(navigation.displayed_config(), Some(old_request.config));
        assert_ne!(navigation.displayed_config(), Some(navigation.config));
        navigation.displayed.as_mut().unwrap().status = Ok(FrameRenderStatus::PendingGeometry);
        assert!(navigation.displayed_config().is_none());
    }

    #[test]
    fn scene_viewport_domains_share_storage_until_accepted_state_changes() {
        let navigation = SceneViewportNavigation::new(320, 200);
        let request = navigation.request(&target(), &NodeId::new("scene"), None, None);
        let mut observations = SceneViewportObservations::default();
        observations.scratch.push((
            NodeId::new("water"),
            FluidDomainSnapshot {
                epoch: 1,
                state: FluidDomainState::Initializing,
                accepted_layout: None,
            },
        ));
        observations.record(
            0,
            1,
            0,
            request.clone(),
            Ok(FrameRenderStatus::PendingGeometry),
        );
        let initial = observations.frames[0].as_ref().unwrap().domains.clone();
        observations.record(
            1,
            2,
            0,
            request.clone(),
            Ok(FrameRenderStatus::PendingGeometry),
        );
        assert!(Arc::ptr_eq(
            &initial,
            &observations.frames[1].as_ref().unwrap().domains
        ));
        observations.scratch[0].1.state = FluidDomainState::Ready;
        observations.scratch[0].1.accepted_layout =
            Some(FluidSettings::default().domain_layout().unwrap());
        observations.record(2, 3, 0, request, Ok(FrameRenderStatus::Complete));
        assert!(!Arc::ptr_eq(
            &initial,
            &observations.frames[2].as_ref().unwrap().domains
        ));
        assert_eq!(
            initial[0].1.state,
            FluidDomainState::Initializing,
            "queued old frames keep their own bounds state"
        );
    }
}
