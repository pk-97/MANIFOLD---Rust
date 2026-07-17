//! [`ViewportSession`] — the persistent, input-driven state behind the P5
//! interactive viewport (`docs/REALTIME_3D_DESIGN.md` D7/D9, P5).
//!
//! `viewport_render::render_viewport_frame` (the P5 gate) builds a brand-new
//! `PresetRuntime` on every call — correct for a one-shot gate proof, wrong
//! for drag-navigation: rebuilding the whole graph (topological sort,
//! execution plan, pipeline compilation) on every mouse-move pixel would
//! jank the UI thread that owns the viewport. This module keeps a session's
//! `PresetRuntime` alive across frames and pays that rebuild cost only twice:
//! once when the viewport opens, and once again if the AUTHORED graph
//! changes underneath it (the performer edited the node graph while the
//! viewport was open — [`ViewportSession::sync_def`]).
//!
//! Camera moves (orbit/pan/dolly) are cheap by construction: the synthetic
//! `node.free_camera` node `viewport_render::override_camera_def` splices in
//! is a REAL node in the built [`crate::node_graph::Graph`], so a camera drag
//! is just [`Graph::set_param`] on that one node's `pos_x`/`yaw`/`pitch`/…
//! params — the same compare-on-write, `param_epoch`-bumping path every
//! live-bound param in the show already uses every frame. The executor's
//! existing memo/dirty tracking does the rest: only the camera node and its
//! downstream (the `render_scene` node) re-evaluate, nothing rebuilds.
//!
//! Never touches the compositor, the content thread, or the live `Project` —
//! same isolation guarantee `viewport_render` documents (D9). A session is
//! editor-context-only state; dropping it (viewport closed) tears down its
//! `PresetRuntime`/`MetalBackend` and releases the GPU resources.

use std::sync::Arc;

use manifold_core::NodeId;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_gpu::{GpuDevice, GpuTextureFormat};

use crate::gpu_encoder::GpuEncoder;
use crate::headless_readback::readback_tonemapped_rgba8;
use crate::node_graph::persistence::PrimitiveRegistry;
use crate::node_graph::viewport_camera::ViewportCamera;
use crate::node_graph::viewport_overlay::{ViewportOverlayConfig, build_overlay_lines, composite_overlay_lines_rgba8, project_lines};
use crate::node_graph::viewport_render::{OVERRIDE_CAMERA_NODE_ID, ViewportRenderError, override_camera_def};
use crate::node_graph::{NodeInstanceId, ParamValue};
use crate::preset_context::PresetContext;
use crate::preset_runtime::PresetRuntime;
use crate::render_target::RenderTarget;

/// Content-hash of an [`EffectGraphDef`] — used to detect "the authored
/// graph changed while the viewport was open" ([`ViewportSession::sync_def`]).
/// Hashes the serialized bytes rather than deriving `Hash` on the def itself
/// (its param values are `f32`, not `Hash`-friendly, and the wire format is
/// already the def's canonical content representation). `serde_json`
/// serialization of a well-formed loaded def cannot fail in practice; a
/// failure degrades to "always looks changed" (hashes to a fixed sentinel),
/// which just costs an extra rebuild — never a stale/wrong render.
fn hash_def(def: &EffectGraphDef) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    match serde_json::to_vec(def) {
        Ok(bytes) => bytes.hash(&mut hasher),
        Err(_) => 0xDEAD_BEEFu64.hash(&mut hasher),
    }
    hasher.finish()
}

/// Persistent per-open-viewport state: an isolated `PresetRuntime` (its own
/// `Graph`/`ExecutionPlan`/`Executor`/`MetalBackend`, per D9), a navigation
/// camera, and a dirty flag gating re-render to "camera moved or the
/// authored graph changed" — never per display tick (the whole point of
/// this struct existing instead of calling `render_viewport_frame` per
/// frame).
pub struct ViewportSession {
    runtime: PresetRuntime,
    device: Arc<GpuDevice>,
    target: RenderTarget,
    camera: ViewportCamera,
    render_scene_node: NodeId,
    /// Runtime instance id of the spliced-in `node.free_camera` — resolved
    /// once per graph build via `Graph::instance_by_node_id`, reused by every
    /// subsequent camera move so navigation never re-walks the node map.
    camera_instance: NodeInstanceId,
    def_hash: u64,
    width: u32,
    height: u32,
    /// `true` when `cached_rgba` is stale w.r.t. `camera`/the built graph —
    /// set by every camera-mutating method and by [`Self::sync_def`] on a
    /// real def change; cleared by [`Self::render_if_dirty`]. Starts `true`
    /// so the viewport's first frame always renders.
    dirty: bool,
    cached_rgba: Vec<u8>,
}

impl ViewportSession {
    /// Open a viewport session on `def`, overriding `render_scene_node`'s
    /// camera input with a fresh default-framed [`ViewportCamera`]. Builds a
    /// brand-new, throwaway-from-the-show's-perspective `PresetRuntime` (the
    /// same production constructor `render_viewport_frame`/the compositor
    /// use) — the one-time cost this struct exists to amortize across the
    /// navigation session that follows.
    pub fn open(
        def: &EffectGraphDef,
        render_scene_node: &NodeId,
        registry: &PrimitiveRegistry,
        device: Arc<GpuDevice>,
        width: u32,
        height: u32,
        frame_ctx: &PresetContext,
    ) -> Result<Self, ViewportRenderError> {
        let camera = ViewportCamera::default();
        let (runtime, target, camera_instance) = Self::build(
            def,
            render_scene_node,
            &camera,
            registry,
            Arc::clone(&device),
            width,
            height,
            frame_ctx,
        )?;
        Ok(Self {
            runtime,
            device,
            target,
            camera,
            render_scene_node: render_scene_node.clone(),
            camera_instance,
            def_hash: hash_def(def),
            width,
            height,
            dirty: true,
            cached_rgba: Vec::new(),
        })
    }

    fn build(
        def: &EffectGraphDef,
        render_scene_node: &NodeId,
        camera: &ViewportCamera,
        registry: &PrimitiveRegistry,
        device: Arc<GpuDevice>,
        width: u32,
        height: u32,
        frame_ctx: &PresetContext,
    ) -> Result<(PresetRuntime, RenderTarget, NodeInstanceId), ViewportRenderError> {
        let overridden = override_camera_def(def, render_scene_node, camera)?;
        let format = GpuTextureFormat::Rgba16Float;
        let mut runtime = PresetRuntime::from_def_with_device(
            overridden,
            registry,
            Arc::clone(&device),
            width,
            height,
            format,
            None,
        )?;
        let camera_instance = runtime
            .graph
            .instance_by_node_id(&NodeId::new(OVERRIDE_CAMERA_NODE_ID))
            .expect("override_camera_def always splices this node id in");
        let target = RenderTarget::new(&device, width, height, format, "viewport-session");

        // Two committed frames — pipeline warm-up, same convention
        // `render_viewport_frame`/`render_scene_shadows.rs` use. Paid once
        // per `build()` call (open, or a def-change rebuild), never per
        // camera move.
        for frame in 0..2 {
            let mut enc = device.create_encoder("viewport-session-warmup-enc");
            {
                let mut gpu = GpuEncoder::new(&mut enc, &device);
                let mut ctx = *frame_ctx;
                ctx.frame_count = frame;
                runtime.render(&mut gpu, &target.texture, &ctx, &manifold_core::params::ParamManifest::default());
            }
            enc.commit_and_wait_completed();
        }
        Ok((runtime, target, camera_instance))
    }

    /// Re-sync against the AUTHORED def, e.g. called once per UI frame the
    /// viewport is open with the live snapshot's def. Cheap no-op
    /// (`def_hash` compare) unless the graph actually changed since the last
    /// call — a real change (topology, a param edit outside the camera
    /// splice, a new node) rebuilds the whole session exactly like
    /// [`Self::open`], carrying the CURRENT camera forward so navigating
    /// doesn't reset on every graph edit.
    pub fn sync_def(
        &mut self,
        def: &EffectGraphDef,
        registry: &PrimitiveRegistry,
        frame_ctx: &PresetContext,
    ) -> Result<(), ViewportRenderError> {
        let h = hash_def(def);
        if h == self.def_hash {
            return Ok(());
        }
        let (runtime, target, camera_instance) = Self::build(
            def,
            &self.render_scene_node,
            &self.camera,
            registry,
            Arc::clone(&self.device),
            self.width,
            self.height,
            frame_ctx,
        )?;
        self.runtime = runtime;
        self.target = target;
        self.camera_instance = camera_instance;
        self.def_hash = h;
        self.dirty = true;
        Ok(())
    }

    pub fn camera(&self) -> &ViewportCamera {
        &self.camera
    }

    /// Push the current `camera` fields into the live graph's spliced camera
    /// node via `Graph::set_param` — no rebuild, just a param write (the
    /// mechanism every doc comment on this struct promises). Marks dirty.
    fn push_camera_to_graph(&mut self) {
        let pos = self.camera.position();
        let sets: [(&str, f32); 8] = [
            ("pos_x", pos[0]),
            ("pos_y", pos[1]),
            ("pos_z", pos[2]),
            ("yaw", self.camera.yaw),
            ("pitch", self.camera.pitch),
            ("fov_y", self.camera.fov_y),
            ("near", self.camera.near),
            ("far", self.camera.far),
        ];
        for (name, value) in sets {
            // `set_param` is Result<(), GraphError> — both error variants
            // (NodeNotFound / ParamNotFound) are impossible here: the node
            // was just resolved via `instance_by_node_id` in `build()`, and
            // these are exactly `node.free_camera`'s own params (D6).
            let _ = self.runtime.graph.set_param(self.camera_instance, name, ParamValue::Float(value));
        }
        self.dirty = true;
    }

    /// LMB-drag orbit — see [`ViewportCamera::orbit`].
    pub fn orbit(&mut self, dx: f32, dy: f32, sensitivity: f32) {
        self.camera.orbit(dx, dy, sensitivity);
        self.push_camera_to_graph();
    }

    /// Shift/MMB-drag pan — see [`ViewportCamera::pan`].
    pub fn pan(&mut self, dx: f32, dy: f32, sensitivity: f32) {
        self.camera.pan(dx, dy, sensitivity);
        self.push_camera_to_graph();
    }

    /// Trackpad two-finger pan — see [`ViewportCamera::trackpad_pan`].
    pub fn trackpad_pan(&mut self, dx: f32, dy: f32, sensitivity: f32) {
        self.camera.trackpad_pan(dx, dy, sensitivity);
        self.push_camera_to_graph();
    }

    /// Scroll-wheel dolly — see [`ViewportCamera::dolly`].
    pub fn dolly(&mut self, delta: f32, sensitivity: f32) {
        self.camera.dolly(delta, sensitivity);
        self.push_camera_to_graph();
    }

    /// Trackpad pinch dolly — see [`ViewportCamera::trackpad_pinch_dolly`].
    pub fn trackpad_pinch_dolly(&mut self, delta: f32, sensitivity: f32) {
        self.camera.trackpad_pinch_dolly(delta, sensitivity);
        self.push_camera_to_graph();
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Render one frame IF `dirty` (camera moved or `sync_def` rebuilt the
    /// graph), then composite the D7 overlays (grid + show-camera frustum +
    /// light billboards) on top and cache the result; otherwise return the
    /// cached bytes untouched — the debounce that keeps this struct off the
    /// per-display-tick path. `show_camera`/`light_positions` are the
    /// caller's decode of the CURRENT def's wired show camera/lights (same
    /// contract as `viewport_overlay::build_overlay_lines`); pass `None`/`&[]`
    /// if the caller hasn't decoded them.
    pub fn render_if_dirty(
        &mut self,
        frame_ctx: &PresetContext,
        overlay_cfg: &ViewportOverlayConfig,
        show_camera: Option<(&crate::node_graph::camera::Camera, f32)>,
        light_positions: &[[f32; 3]],
    ) -> &[u8] {
        if self.dirty {
            let mut enc = self.device.create_encoder("viewport-session-render-enc");
            {
                let mut gpu = GpuEncoder::new(&mut enc, &self.device);
                self.runtime.render(
                    &mut gpu,
                    &self.target.texture,
                    frame_ctx,
                    &manifold_core::params::ParamManifest::default(),
                );
            }
            enc.commit_and_wait_completed();
            let mut rgba = readback_tonemapped_rgba8(&self.device, &self.target.texture, self.width, self.height);

            let editor_cam = self.camera.to_camera();
            let world_lines = build_overlay_lines(overlay_cfg, show_camera, light_positions);
            let screen_lines = project_lines(&editor_cam, self.width, self.height, &world_lines);
            composite_overlay_lines_rgba8(&mut rgba, self.width, self.height, &screen_lines);

            self.cached_rgba = rgba;
            self.dirty = false;
        }
        &self.cached_rgba
    }

    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::persistence::PrimitiveRegistry;
    use manifold_core::effect_graph_def::{
        EFFECT_GRAPH_VERSION, EffectGraphDef, EffectGraphNode, EffectGraphWire,
    };
    use std::collections::{BTreeMap, BTreeSet};

    fn node(id: u32, node_id: &str, type_id: &str) -> EffectGraphNode {
        EffectGraphNode {
            id,
            node_id: NodeId::new(node_id),
            type_id: type_id.to_string(),
            handle: None,
            params: BTreeMap::new(),
            exposed_params: BTreeSet::new(),
            editor_pos: None,
            wgsl_source: None,
            title: None,
            output_formats: BTreeMap::new(),
            output_canvas_scales: BTreeMap::new(),
            group: None,
        }
    }

    fn def_with_render_scene() -> EffectGraphDef {
        EffectGraphDef {
            version: EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![
                node(0, "cam", "node.orbit_camera"),
                node(1, "scene", "node.render_scene"),
            ],
            wires: vec![EffectGraphWire {
                from_node: 0,
                from_port: "out".to_string(),
                to_node: 1,
                to_port: "camera".to_string(),
            }],
        }
    }

    #[test]
    fn hash_def_is_stable_and_change_sensitive() {
        let a = def_with_render_scene();
        let mut b = def_with_render_scene();
        assert_eq!(hash_def(&a), hash_def(&b));
        b.nodes[0].handle = Some("renamed".to_string());
        assert_ne!(hash_def(&a), hash_def(&b));
    }

    // GPU-backed behavior (open/orbit/pan/dolly/sync_def/render_if_dirty)
    // is proven in the `gpu-proofs` feature — see
    // `tests/gpu_proofs/scene_viewport_session.rs` — not here (this crate's
    // default `--lib` sweep is GPU-free; `crate::test_device()` doesn't
    // compile without the feature).
    #[test]
    fn registry_construction_smoke() {
        // Cheap sanity check this module's imports resolve against the real
        // registry type without needing a device.
        let _registry = PrimitiveRegistry::with_builtin();
    }
}
