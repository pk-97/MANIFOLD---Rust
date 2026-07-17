//! Isolated render path for the P5 3D viewport
//! (`docs/REALTIME_3D_DESIGN.md` D7/D9, P5).
//!
//! D9 is the load-bearing constraint: "the content-thread render is
//! byte-identical with the viewport open or closed." The live show render
//! runs through the compositor's own long-lived `PresetRuntime` instances
//! (`layer_compositor.rs`), which ALSO back the existing node-output preview
//! (`set_preview_target` captures an extra texture from that SAME execution
//! — see `execution.rs`'s `preview_target` doc comment: "the live render path
//! never sets a preview target"). Splicing an editor camera into that shared
//! execution would corrupt the live `camera` port value the show itself
//! reads — the one thing D9 forbids.
//!
//! So this module never touches the compositor. [`override_camera_def`]
//! clones the generator's `EffectGraphDef` (pure data — nothing here reads
//! or writes the live `Graph`/`Executor`/`Project`) and splices in a
//! synthetic `node.free_camera` node feeding the target `render_scene`
//! node's `camera` port, replacing whatever was wired. [`render_viewport_frame`]
//! then builds a BRAND NEW, throwaway `PresetRuntime` from that cloned def via
//! `PresetRuntime::from_def_with_device` — the same production "def → real
//! MetalBackend" constructor the live compositor itself uses to build a
//! generator's runtime in the first place, just called a second time with
//! its own `Graph`/`ExecutionPlan`/`Executor`/`MetalBackend`. Two structurally
//! separate runtimes on the same `GpuDevice` cannot share execution state —
//! this is what makes the D9 guarantee mechanical rather than a hoped-for
//! ordering. `docs/REALTIME_3D_DESIGN_P5_GATE.md` proof lives in
//! `crates/manifold-renderer/tests/gpu_proofs/scene_viewport_navigate.rs`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use manifold_core::NodeId;
use manifold_core::effect_graph_def::{EffectGraphDef, EffectGraphNode, EffectGraphWire, SerializedParamValue};
use manifold_gpu::{GpuDevice, GpuTextureFormat};

use crate::gpu_encoder::GpuEncoder;
use crate::headless_readback::readback_tonemapped_rgba8;
use crate::node_graph::persistence::PrimitiveRegistry;
use crate::node_graph::viewport_camera::ViewportCamera;
use crate::preset_context::PresetContext;
use crate::preset_runtime::{JsonGeneratorLoadError, PresetRuntime};

/// `type_id` the splice injects — the same primitive `node.free_camera`
/// (`docs/REALTIME_3D_DESIGN.md` D6) every other free-look camera source
/// uses; the viewport asks for no new primitive.
const OVERRIDE_CAMERA_TYPE_ID: &str = "node.free_camera";
/// `pub(crate)`: `viewport_session.rs` resolves this exact node id to a
/// runtime `NodeInstanceId` once per graph build (`Graph::instance_by_node_id`)
/// so subsequent camera moves are a `Graph::set_param` call, not a rebuild.
pub(crate) const OVERRIDE_CAMERA_NODE_ID: &str = "__viewport_editor_camera__";

#[derive(Debug)]
pub enum ViewportRenderError {
    /// `render_scene_node` isn't present in `def` — caller error (the panel
    /// should only request a viewport render for a node it just saw in the
    /// snapshot). Returned rather than silently rendering the un-overridden
    /// graph (no-silent-fallbacks).
    RenderSceneNodeNotFound,
    Load(JsonGeneratorLoadError),
}

impl From<JsonGeneratorLoadError> for ViewportRenderError {
    fn from(e: JsonGeneratorLoadError) -> Self {
        Self::Load(e)
    }
}

impl std::fmt::Display for ViewportRenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RenderSceneNodeNotFound => {
                write!(f, "viewport override: render_scene node not found in def")
            }
            Self::Load(e) => write!(f, "viewport override: {e}"),
        }
    }
}
impl std::error::Error for ViewportRenderError {}

/// Clone `def` and replace whatever feeds `render_scene_node`'s `camera`
/// input with a synthetic `node.free_camera` carrying `vp_cam`'s exact
/// pos/yaw/pitch/fov/near/far — see module docs for why this is a pure data
/// transform on a CLONE, never the live def. `roll` is always 0 (the
/// viewport camera never rolls, matching `ViewportCamera::to_camera`).
pub fn override_camera_def(
    def: &EffectGraphDef,
    render_scene_node: &NodeId,
    vp_cam: &ViewportCamera,
) -> Result<EffectGraphDef, ViewportRenderError> {
    let mut out = def.clone();
    let target_id = out
        .nodes
        .iter()
        .find(|n| &n.node_id == render_scene_node)
        .map(|n| n.id)
        .ok_or(ViewportRenderError::RenderSceneNodeNotFound)?;

    // Drop whatever was wired into `camera` — the override replaces it
    // entirely, it doesn't blend with it.
    out.wires.retain(|w| !(w.to_node == target_id && w.to_port == "camera"));

    let synthetic_id = out.nodes.iter().map(|n| n.id).max().unwrap_or(0).saturating_add(1);
    let pos = vp_cam.position();
    let mut params = BTreeMap::new();
    params.insert("pos_x".to_string(), SerializedParamValue::Float { value: pos[0] });
    params.insert("pos_y".to_string(), SerializedParamValue::Float { value: pos[1] });
    params.insert("pos_z".to_string(), SerializedParamValue::Float { value: pos[2] });
    params.insert("yaw".to_string(), SerializedParamValue::Float { value: vp_cam.yaw });
    params.insert("pitch".to_string(), SerializedParamValue::Float { value: vp_cam.pitch });
    params.insert("roll".to_string(), SerializedParamValue::Float { value: 0.0 });
    params.insert("fov_y".to_string(), SerializedParamValue::Float { value: vp_cam.fov_y });
    params.insert("near".to_string(), SerializedParamValue::Float { value: vp_cam.near });
    params.insert("far".to_string(), SerializedParamValue::Float { value: vp_cam.far });

    out.nodes.push(EffectGraphNode {
        id: synthetic_id,
        node_id: NodeId::new(OVERRIDE_CAMERA_NODE_ID),
        type_id: OVERRIDE_CAMERA_TYPE_ID.to_string(),
        handle: Some("Viewport Editor Camera".to_string()),
        params,
        exposed_params: BTreeSet::new(),
        editor_pos: None,
        wgsl_source: None,
        title: Some("Viewport Editor Camera".to_string()),
        output_formats: BTreeMap::new(),
        output_canvas_scales: BTreeMap::new(),
        group: None,
    });
    out.wires.push(EffectGraphWire {
        from_node: synthetic_id,
        from_port: "out".to_string(),
        to_node: target_id,
        to_port: "camera".to_string(),
    });

    Ok(out)
}

/// Render one frame of `def` (already camera-overridden via
/// [`override_camera_def`]) through a BRAND NEW, throwaway `PresetRuntime` —
/// its own `Graph`/`ExecutionPlan`/`Executor`/`MetalBackend`, built and torn
/// down entirely inside this call. Returns tonemapped RGBA8 pixels (the same
/// convention `headless_readback::readback_to_srgb_png` uses) plus the
/// dimensions rendered. `frame_ctx` follows `PresetContext`'s normal
/// contract (time/beat/aspect/etc.) — pass the SAME values the compositor
/// would for `def`'s live generator so the scene reads identically (same
/// beat position, same triggers) except for the camera.
pub fn render_viewport_frame(
    overridden_def: EffectGraphDef,
    registry: &PrimitiveRegistry,
    device: Arc<GpuDevice>,
    width: u32,
    height: u32,
    frame_ctx: &PresetContext,
) -> Result<(Vec<u8>, u32, u32), ViewportRenderError> {
    let format = GpuTextureFormat::Rgba16Float;
    let mut runtime = PresetRuntime::from_def_with_device(
        overridden_def,
        registry,
        Arc::clone(&device),
        width,
        height,
        format,
        None,
    )?;

    let target = crate::render_target::RenderTarget::new(&device, width, height, format, "viewport-editor-preview");

    // Two committed frames, same convention `render_scene_shadows.rs` uses:
    // the first frame pays pipeline warm-up, `commit_and_wait_completed`
    // hard-checks for GPU errors so a bad splice surfaces here, not as a
    // silently wrong frame.
    for frame in 0..2 {
        let mut enc = device.create_encoder("viewport-editor-preview-enc");
        {
            let mut gpu = GpuEncoder::new(&mut enc, &device);
            let mut ctx = *frame_ctx;
            ctx.frame_count = frame;
            runtime.render(&mut gpu, &target.texture, &ctx, &manifold_core::params::ParamManifest::default());
        }
        enc.commit_and_wait_completed();
    }

    let rgba8 = readback_tonemapped_rgba8(&device, &target.texture, width, height);
    Ok((rgba8, width, height))
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::effect_graph_def::{EFFECT_GRAPH_VERSION, EffectGraphDef};

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
    fn splice_removes_old_camera_wire_and_adds_free_camera() {
        let def = def_with_render_scene();
        let vp = ViewportCamera::default();
        let out = override_camera_def(&def, &NodeId::new("scene"), &vp).expect("splice ok");

        // Old wire from the orbit_camera into `camera` must be gone.
        assert!(
            !out.wires.iter().any(|w| w.to_node == 1
                && w.to_port == "camera"
                && w.from_node == 0),
            "old camera wire should be removed"
        );
        // A new free_camera node feeds `camera` on the render_scene node.
        let new_wire = out
            .wires
            .iter()
            .find(|w| w.to_node == 1 && w.to_port == "camera")
            .expect("a new camera wire must exist");
        let src = out.nodes.iter().find(|n| n.id == new_wire.from_node).expect("src node exists");
        assert_eq!(src.type_id, OVERRIDE_CAMERA_TYPE_ID);
        assert_eq!(
            src.params.get("pos_x"),
            Some(&SerializedParamValue::Float { value: vp.position()[0] })
        );

        // The original def is untouched (splice operates on a clone) — the
        // mechanical half of the D9 guarantee: nothing here can mutate the
        // live generator's def.
        assert_eq!(def.nodes.len(), 2);
        assert_eq!(def.wires.len(), 1);
        assert_eq!(def.wires[0].from_node, 0);
    }

    #[test]
    fn splice_errors_when_render_scene_node_missing() {
        let def = def_with_render_scene();
        let vp = ViewportCamera::default();
        let err = override_camera_def(&def, &NodeId::new("does_not_exist"), &vp);
        assert!(matches!(err, Err(ViewportRenderError::RenderSceneNodeNotFound)));
    }
}
