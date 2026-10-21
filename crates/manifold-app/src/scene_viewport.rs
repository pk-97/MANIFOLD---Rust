//! Transient editor navigation and frame-matched observations of the live scene.
use manifold_core::{GraphTarget, NodeId};
use {manifold_node_engine::runtime::frame_status::FrameRenderStatus, manifold_node_engine::scene::viewport_camera::ViewportCamera, manifold_core::fluid_domain::FluidDomainSnapshot, manifold_node_engine::scene::scene_viewport::SceneViewportConfig, manifold_node_engine::scene::scene_viewport::SceneViewportHostError, manifold_node_engine::runtime::ModifierPreviewContext};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

type Domains = [(NodeId, FluidDomainSnapshot)];
pub(crate) type SceneViewportFrames = [Option<SceneViewportFrame>; 3];

/// Which view owns the one scene viewport request (D7a). One host
/// navigates at a time: whichever starts navigating ends the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ViewportHost {
    /// The graph editor's 3D viewport pane.
    GraphEditor,
    /// The main window's output preview, navigating away from the output
    /// camera without moving it (D9).
    OutputPreview,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SceneViewportRequest {
    pub host: ViewportHost,
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
    pub live_columns: crate::physics_overlay::LiveColumns,
    /// The outline mask bridge generation when this frame drew a selection
    /// mask into the same surface slot (`selection_outline.rs`).
    pub outline: Option<u64>,
}

impl SceneViewportFrame {
    pub fn matches(&self, request: &SceneViewportRequest, frame: u64, generation: u64) -> bool {
        self.frame == frame
            && self.surface_generation == generation
            && self.request.host == request.host
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
    pub host: ViewportHost,
    pub session: u64,
    pub config: SceneViewportConfig,
    pub displayed: Option<SceneViewportFrame>,
}

impl SceneViewportNavigation {
    pub fn new(host: ViewportHost, width: u32, height: u32) -> Self {
        static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);
        Self {
            host,
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
            && previous.host == self.host
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
            host: self.host,
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

/// The frame `request` produced on the viewport bridge's published surface,
/// for either host. With an image, the leased surface's texture comes back
/// and the lease joins `leases`, retired when the sampling GPU work does;
/// otherwise the lease retires at once. The surface slot comes back too, for
/// the selection mask beside it. `None` when the surface holds no frame of
/// this request.
#[cfg(target_os = "macos")]
pub(crate) fn lease_frame(
    pane: &mut crate::texture_pane::TexturePane,
    device: &manifold_gpu::GpuDevice,
    frames: &SceneViewportFrames,
    request: &SceneViewportRequest,
    leases: &mut crate::shared_texture::BridgeLeaseBatch,
) -> Option<(SceneViewportFrame, Option<manifold_gpu::GpuTexture>, usize)> {
    let bridge = pane.bridge()?.clone();
    let lease = bridge.acquire_read();
    let matched = bridge.leased_frame(lease).and_then(|frame| {
        frames[lease.slot()].as_ref().filter(|observation| observation.matches(request, frame, bridge.generation()))
    });
    let texture = matched
        .filter(|frame| frame.has_image())
        .and_then(|_| pane.leased(device, lease.slot()).cloned());
    if texture.is_some() {
        leases.push((bridge, lease));
    } else {
        bridge.retire_read(lease);
    }
    matched.map(|frame| (frame.clone(), texture, lease.slot()))
}

/// Reuse the domain buffer each frame; allocate a published snapshot only when
/// accepted layout/epoch/readiness changes, not when simulation time advances.
pub(crate) struct SceneViewportObservations {
    pub scratch: Vec<(NodeId, FluidDomainSnapshot)>,
    domains: Arc<Domains>,
    pub frames: SceneViewportFrames,
}

impl SceneViewportObservations {
    /// Stamp surface `slot`'s frame with its selection mask generation.
    pub fn set_outline(&mut self, slot: usize, outline: Option<u64>) {
        if let Some(frame) = self.frames[slot].as_mut() {
            frame.outline = outline;
        }
    }
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
        let live_columns = crate::physics_overlay::share_domains(&mut self.scratch, &mut self.domains);
        self.frames[slot] = Some(SceneViewportFrame {
            frame,
            surface_generation,
            request,
            status,
            domains: self.domains.clone(),
            live_columns,
            outline: None,
        });
    }
}

pub(crate) fn texture_handle() -> manifold_ui::node::TextureHandle {
    manifold_ui::node::texture_handle_for_key("__manifold_shared_scene_viewport__")
}

pub(crate) struct SceneViewportPaint<'a> {
    pub rect: manifold_ui::Rect,
    /// The selection outline pass and this frame's mask (D-102).
    pub outline: Option<(&'a crate::selection_outline::SelectionOutline, &'a manifold_gpu::GpuTexture)>,
}

impl SceneViewportPaint<'_> {
    pub fn draw_image(&self, ui: &mut manifold_ui_paint::ui_renderer::UIRenderer) {
        use manifold_ui_paint::ui_renderer::Depth;
        let r = self.rect;
        ui.push_depth(Depth::CONTENT);
        ui.push_immediate_clip(r.x, r.y, r.width, r.height);
        ui.draw_image_uv(r.x, r.y, r.width, r.height, texture_handle(), [0.0, 0.0, 1.0, 1.0], 0.0);
        ui.pop_immediate_clip();
        ui.pop_depth();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::fluid_domain::{FluidDomainState, domain_layout};

    fn target() -> GraphTarget {
        GraphTarget::Generator(manifold_core::LayerId::new("water-layer"))
    }

    #[test]
    fn scene_viewport_reuses_requests_but_reopen_has_a_new_identity() {
        let mut navigation = SceneViewportNavigation::new(ViewportHost::GraphEditor, 320, 200);
        let node = NodeId::new("scene");
        let first = navigation.request(&target(), &node, None, None);
        let same = navigation.request(&target(), &node, None, Some(&first));
        assert!(Arc::ptr_eq(&first, &same));
        navigation.camera_mut().orbit(20.0, 0.0, 0.01);
        let moved = navigation.request(&target(), &node, None, Some(&same));
        assert!(!Arc::ptr_eq(&same, &moved));
        assert_eq!(same.session, moved.session);
        let reopened =
            SceneViewportNavigation::new(ViewportHost::GraphEditor, 320, 200).request(&target(), &node, None, Some(&first));
        assert_ne!(first.session, reopened.session);
    }

    #[test]
    fn scene_viewport_matches_the_leased_frame_and_uses_its_camera() {
        let mut navigation = SceneViewportNavigation::new(ViewportHost::GraphEditor, 320, 200);
        let node = NodeId::new("scene");
        let old_request = navigation.request(&target(), &node, None, None);
        let frame = SceneViewportFrame {
            frame: 20,
            surface_generation: 2,
            request: old_request.clone(),
            status: Ok(FrameRenderStatus::Complete),
            domains: Arc::from([]),
            live_columns: Default::default(),
            outline: None,
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
        let navigation = SceneViewportNavigation::new(ViewportHost::GraphEditor, 320, 200);
        let request = navigation.request(&target(), &NodeId::new("scene"), None, None);
        let mut observations = SceneViewportObservations::default();
        observations.scratch.push((
            NodeId::new("water"),
            FluidDomainSnapshot {
                epoch: 1,
                state: FluidDomainState::Initializing,
                accepted_layout: None,
                fit: Default::default(),
                column_box: None,
                refusal: None,
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
            Some(domain_layout(None, 4.0, 24).unwrap());
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
